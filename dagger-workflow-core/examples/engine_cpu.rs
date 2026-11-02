//! Engine overhead on a SQLite store with an 85-node fan-in DAG.
//!
//! Shape follows a real host report: 67 small "write" roots, 17 "render" nodes
//! that each join 4 writes, and 1 "mix" node that joins every render.
//! Actions only sleep, so wall and CPU time above the sleeps is engine cost.
//!
//! cargo run -p dagger-workflow-core --release --features sqlite --example engine_cpu
//! Optional args: <write_ms> <render_ms> <mix_ms> (defaults 20 500 1000).
//! Wrap with `/usr/bin/time -l` (macOS) or `/usr/bin/time -v` (Linux) for CPU.

use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, InMemoryActionRegistry, WorkflowAction,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::ObjectStore;
use dagger_workflow_core::definition::{
    canonical_topological_ranks, ActionReference, BackoffPolicy, Binding, BindingSource,
    NodeDefinition, PublishableDefinition, RetryPolicy, TimeoutPolicy, WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::fs_object_store::FsObjectStore;
use dagger_workflow_core::ids::{CostUnits, Digest, Id, Timestamp, Version};
use dagger_workflow_core::memoization::CacheMode;
use dagger_workflow_core::pinning::RunOptions;
use dagger_workflow_core::run::{RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::sqlite::SqliteWorkflowStore;
use dagger_workflow_core::store::{
    CreateDefinition, CreateRun, CreateRunWithOptions, PublishRevision, ResolvedActionSchemas,
    WorkflowStore,
};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

const WRITES: usize = 67;
const RENDERS: usize = 17;
const JOIN: usize = 4;

type Objects = FsObjectStore<TestClock>;
type Store = SqliteWorkflowStore<TestClock, Objects>;
type Engine = WorkflowEngine<Store, Objects, InMemoryActionRegistry>;

fn hash(bytes: &[u8]) -> Digest {
    Digest::new(format!("sha256:{:x}", Sha256::digest(bytes))).unwrap()
}
fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}
fn principal(scope: &ExecutionScope) -> AuthenticatedPrincipal {
    AuthenticatedPrincipal::mint(scope.clone(), "bench".into(), Vec::new(), hash(b"auth")).unwrap()
}
fn descriptor(name: &str) -> ActionDescriptor {
    ActionDescriptor {
        name: name.into(),
        contract_version: "1".into(),
        input_schema_digest: hash(b"{}"),
        output_schema_digest: hash(b"{}"),
        implementation_compatibility_digest: hash(name.as_bytes()),
    }
}

struct Sleep {
    descriptor: ActionDescriptor,
    duration: Duration,
}
impl WorkflowAction for Sleep {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }
    fn memoize(&self) -> bool {
        true
    }
    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            tokio::time::sleep(self.duration).await;
            let output =
                json!({"node": context.node_instance_id.as_str(), "input_bytes": input.len()});
            ActionOutcome::success(output, Vec::new(), CostUnits(1), None).unwrap()
        })
    }
}

fn action(node: String, name: &str, sources: Vec<BindingSource>, next: Vec<Id>) -> NodeDefinition {
    let descriptor = descriptor(name);
    NodeDefinition::Action {
        id: id(&node),
        action: ActionReference {
            name: descriptor.name,
            contract_version: descriptor.contract_version,
            input_schema_digest: descriptor.input_schema_digest,
            output_schema_digest: descriptor.output_schema_digest,
            compatible_implementation_requirement: descriptor.implementation_compatibility_digest,
        },
        bindings: sources
            .into_iter()
            .enumerate()
            .map(|(index, source)| Binding {
                target: format!("/in{index}"),
                source,
            })
            .collect(),
        retry: RetryPolicy {
            max_attempts: 1,
            backoff: BackoffPolicy::Fixed { delay_ms: 0 },
        },
        timeout: TimeoutPolicy {
            timeout_ms: 600_000,
        },
        declared_max_cost_units: CostUnits(1),
        next,
    }
}

fn output_of(node: &str) -> BindingSource {
    BindingSource::NodeOutput {
        node_id: id(node),
        pointer: String::new(),
    }
}

fn workflow() -> WorkflowDefinition {
    let mut nodes = Vec::new();
    for w in 0..WRITES {
        let seed = BindingSource::RunInput {
            pointer: "/seed".into(),
        };
        nodes.push(action(
            format!("w{w:02}"),
            "bench.write",
            vec![seed],
            Vec::new(),
        ));
    }
    for r in 0..RENDERS {
        let sources = (0..JOIN)
            .map(|k| output_of(&format!("w{:02}", (r * JOIN + k) % WRITES)))
            .collect();
        nodes.push(action(
            format!("r{r:02}"),
            "bench.render",
            sources,
            Vec::new(),
        ));
    }
    let renders = (0..RENDERS)
        .map(|r| output_of(&format!("r{r:02}")))
        .collect();
    nodes.push(action("mix".into(), "bench.mix", renders, vec![id("done")]));
    nodes.push(NodeDefinition::Succeed {
        id: id("done"),
        output: output_of("mix"),
    });
    WorkflowDefinition {
        definition_format_version: "0.1".into(),
        definition_id: id("bench"),
        name: "bench".into(),
        description: String::new(),
        run_input_schema_digest: hash(b"{}"),
        run_output_schema_digest: hash(b"{}"),
        nodes,
    }
}

async fn publish(store: &Store, objects: &Objects, scope: &ExecutionScope) -> Digest {
    let workflow = workflow();
    store
        .create_definition(
            scope,
            CreateDefinition {
                definition_id: id("bench"),
                display_name: "bench".into(),
                description: String::new(),
                principal: principal(scope),
            },
        )
        .await
        .unwrap();
    let schema = objects.put(scope, b"{}", "application/json").await.unwrap();
    let canonical = objects
        .put(
            scope,
            &serde_jcs::to_vec(&workflow).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let schemas = workflow
        .nodes
        .iter()
        .filter_map(|node| match node {
            NodeDefinition::Action { id, .. } => Some((
                id.as_str().to_owned(),
                ResolvedActionSchemas {
                    input_schema: schema.clone(),
                    output_schema: schema.clone(),
                },
            )),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    store
        .publish_revision(
            scope,
            PublishRevision {
                definition_id: id("bench"),
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema,
                resolved_action_schema_objects: schemas,
                parsed_revision: PublishableDefinition {
                    topological_ranks: canonical_topological_ranks(&workflow).unwrap(),
                    definition: workflow,
                },
                principal: principal(scope),
            },
        )
        .await
        .unwrap();
    canonical.digest().clone()
}

async fn create(
    store: &Store,
    objects: &Objects,
    scope: &ExecutionScope,
    revision: &Digest,
    run: &str,
) {
    let input = objects
        .put(scope, br#"{"seed":"song"}"#, "application/json")
        .await
        .unwrap();
    store
        .create_run_with_options(
            scope,
            CreateRunWithOptions {
                command: CreateRun {
                    run_id: id(run),
                    definition_id: id("bench"),
                    revision_hash: revision.clone(),
                    input,
                    budget_limit: CostUnits(1_000),
                    principal: principal(scope),
                    limits: RunLimits {
                        max_dynamic_node_instances: 200,
                        max_total_attempts: 200,
                        max_total_events: 10_000,
                        max_inline_json_bytes_per_value: 100_000,
                        max_artifacts_per_attempt: 10,
                        max_aggregate_object_bytes_per_run: 100_000_000,
                        max_run_lifetime_ms: 3_600_000,
                    },
                    idempotency_token: format!("bench-create-token-{run}"),
                },
                options: RunOptions {
                    cache_mode: CacheMode::Use,
                    pinned_outputs: Vec::new(),
                },
            },
        )
        .await
        .unwrap();
}

/// Drives one run with the host loop the engine recommends.
async fn drive(engine: &Engine, store: &Store, scope: &ExecutionScope, run: &str) -> Duration {
    let run_id = id(run);
    let started = Instant::now();
    let mut activity = engine.activity();
    engine.start(scope, &run_id).await.unwrap();
    let (mut ticks, mut busy, mut tick_time, mut longest) = (0, 0, Duration::ZERO, Duration::ZERO);
    loop {
        let tick_started = Instant::now();
        let changed = engine.tick(scope).await.unwrap();
        let elapsed = tick_started.elapsed();
        ticks += 1;
        tick_time += elapsed;
        longest = longest.max(elapsed);
        if changed > 0 {
            busy += 1;
            continue;
        }
        let status = store.get_run_record(scope, &run_id).await.unwrap().status;
        if status.is_terminal() {
            assert_eq!(status, RunState::Succeeded);
            break;
        }
        let _ = tokio::time::timeout(Duration::from_secs(1), activity.changed()).await;
    }
    let total = started.elapsed();
    println!(
        "{run}: {:.2}s wall, {ticks} ticks ({busy} committed work), {:.2}s inside tick, longest {:.0} ms",
        total.as_secs_f64(),
        tick_time.as_secs_f64(),
        longest.as_secs_f64() * 1e3,
    );
    total
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error>> {
    if cfg!(debug_assertions) {
        return Err("use --release for comparable timings".into());
    }
    let ms = |index: usize, default: u64| {
        Duration::from_millis(
            std::env::args()
                .nth(index)
                .and_then(|value| value.parse().ok())
                .unwrap_or(default),
        )
    };
    let (write, render, mix) = (ms(1, 20), ms(2, 500), ms(3, 1000));
    let scratch = tempfile::tempdir()?;
    let scope = ExecutionScope {
        tenant_id: ScopeAtom::new("bench-tenant")?,
        namespace: ScopeAtom::new("bench")?,
    };
    let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
    let objects = Arc::new(FsObjectStore::open(
        scratch.path().join("objects"),
        clock.clone(),
    )?);
    let store = Arc::new(
        SqliteWorkflowStore::open(
            &scratch.path().join("workflow.sqlite"),
            clock,
            objects.clone(),
        )
        .await?,
    );
    let registry = Arc::new(InMemoryActionRegistry::new());
    for (name, duration) in [
        ("bench.write", write),
        ("bench.render", render),
        ("bench.mix", mix),
    ] {
        registry.register(Arc::new(Sleep {
            descriptor: descriptor(name),
            duration,
        }))?;
    }
    let engine = WorkflowEngine::new(
        store.clone(),
        objects.clone(),
        registry,
        EngineConfig {
            instance_id: id("bench-engine"),
            max_concurrency: 6,
            cancellation_grace: Duration::from_secs(1),
        },
    )?;
    engine.acquire_scope(&scope).await?;
    let revision = publish(&store, &objects, &scope).await;

    // Critical path of sleeps: write -> render -> mix, with 6-way concurrency.
    let floor = write * WRITES.div_ceil(6) as u32 + render * RENDERS.div_ceil(6) as u32 + mix;
    println!("action sleep floor ~{:.2}s", floor.as_secs_f64());
    create(&store, &objects, &scope, &revision, "cold").await;
    drive(&engine, &store, &scope, "cold").await;
    create(&store, &objects, &scope, &revision, "cached").await;
    drive(&engine, &store, &scope, "cached").await;

    let run_id = id("cold");
    for (label, full) in [("get_run", true), ("get_run_record", false)] {
        let started = Instant::now();
        for _ in 0..20 {
            if full {
                store.get_run(&scope, &run_id).await?;
            } else {
                store.get_run_record(&scope, &run_id).await?;
            }
        }
        println!(
            "{label}: {:.2} ms/call",
            started.elapsed().as_secs_f64() * 1e3 / 20.0
        );
    }
    Ok(())
}
