use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, InMemoryActionRegistry, WorkflowAction,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::ObjectStore;
use dagger_workflow_core::definition::{
    ActionReference, BackoffPolicy, Binding, BindingSource, NodeDefinition, PublishableDefinition,
    RetryPolicy, TimeoutPolicy, WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::ids::{CostUnits, Digest, Id, Timestamp, TopologicalRank};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::run::{RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::store::{
    CreateDefinition, CreateRun, EventPageRequest, PublishRevision, ResolvedActionSchemas,
    WorkflowStore,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;

fn block_on<T>(future: impl Future<Output = T>) -> T {
    struct ThreadWake(thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match Pin::new(&mut future).poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => thread::park(),
        }
    }
}

fn hash(bytes: &[u8]) -> Digest {
    Digest::new(format!(
        "sha256:{}",
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
    .unwrap()
}

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}

fn scope() -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new("tenant").unwrap(),
        namespace: ScopeAtom::new("engine").unwrap(),
    }
}

struct RetryOnce {
    descriptor: ActionDescriptor,
    fail: AtomicBool,
}

impl WorkflowAction for RetryOnce {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    fn invoke<'a>(
        &'a self,
        _context: ActionContext,
        canonical_bound_input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            if self.fail.swap(false, Ordering::AcqRel) {
                ActionOutcome::retryable(
                    "fixture.transient".to_owned(),
                    "retry once".to_owned(),
                    None,
                    CostUnits(1),
                )
                .unwrap()
            } else {
                let value: Value = serde_json::from_slice(canonical_bound_input).unwrap();
                ActionOutcome::success(value, Vec::new(), CostUnits(1), None).unwrap()
            }
        })
    }
}

#[test]
fn retry_delay_survives_engine_recreation_and_then_succeeds() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(1_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let schema = objects
            .put(&execution_scope, b"{}", "application/json")
            .await
            .unwrap();
        let descriptor = ActionDescriptor {
            name: "fixture.retry".to_owned(),
            contract_version: "1".to_owned(),
            input_schema_digest: schema.digest().clone(),
            output_schema_digest: schema.digest().clone(),
            implementation_compatibility_digest: hash(b"implementation"),
        };
        let registry = Arc::new(InMemoryActionRegistry::new());
        registry
            .register(Arc::new(RetryOnce {
                descriptor: descriptor.clone(),
                fail: AtomicBool::new(true),
            }))
            .unwrap();
        let definition = WorkflowDefinition {
            definition_format_version: "0.1".to_owned(),
            definition_id: id("definition"),
            name: "retry".to_owned(),
            description: String::new(),
            run_input_schema_digest: schema.digest().clone(),
            run_output_schema_digest: schema.digest().clone(),
            entry_node_id: id("action"),
            nodes: vec![
                NodeDefinition::Action {
                    id: id("action"),
                    action: ActionReference {
                        name: descriptor.name.clone(),
                        contract_version: descriptor.contract_version.clone(),
                        input_schema_digest: descriptor.input_schema_digest.clone(),
                        output_schema_digest: descriptor.output_schema_digest.clone(),
                        compatible_implementation_requirement: descriptor
                            .implementation_compatibility_digest
                            .clone(),
                    },
                    bindings: vec![Binding {
                        target: String::new(),
                        source: BindingSource::RunInput {
                            pointer: String::new(),
                        },
                    }],
                    retry: RetryPolicy {
                        max_attempts: 2,
                        backoff: BackoffPolicy::Fixed { delay_ms: 100 },
                    },
                    timeout: TimeoutPolicy { timeout_ms: 1_000 },
                    declared_max_cost_units: CostUnits(2),
                    next: vec![id("succeed")],
                },
                NodeDefinition::Succeed {
                    id: id("succeed"),
                    output: BindingSource::NodeOutput {
                        node_id: id("action"),
                        pointer: String::new(),
                    },
                },
            ],
        };
        let definition_bytes = serde_jcs::to_vec(&definition).unwrap();
        let canonical = objects
            .put(&execution_scope, &definition_bytes, "application/json")
            .await
            .unwrap();
        let principal = AuthenticatedPrincipal::mint(
            execution_scope.clone(),
            "tester".to_owned(),
            Vec::new(),
            hash(b"auth"),
        )
        .unwrap();
        store
            .create_definition(
                &execution_scope,
                CreateDefinition {
                    definition_id: id("definition"),
                    display_name: "retry".to_owned(),
                    description: String::new(),
                    principal: principal.clone(),
                },
            )
            .await
            .unwrap();
        let mut ranks = BTreeMap::new();
        ranks.insert(id("action"), TopologicalRank(0));
        ranks.insert(id("succeed"), TopologicalRank(1));
        let mut action_schemas = BTreeMap::new();
        action_schemas.insert(
            "action".to_owned(),
            ResolvedActionSchemas {
                input_schema: schema.clone(),
                output_schema: schema.clone(),
            },
        );
        store
            .publish_revision(
                &execution_scope,
                PublishRevision {
                    definition_id: id("definition"),
                    expected_definition_version: dagger_workflow_core::ids::Version(1),
                    canonical_definition: canonical.clone(),
                    run_input_schema: schema.clone(),
                    run_output_schema: schema.clone(),
                    resolved_action_schema_objects: action_schemas,
                    parsed_revision: PublishableDefinition {
                        definition,
                        topological_ranks: ranks,
                    },
                    principal: principal.clone(),
                },
            )
            .await
            .unwrap();
        let input = objects
            .put(
                &execution_scope,
                &serde_jcs::to_vec(&json!({"value": 7})).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        store
            .create_run(
                &execution_scope,
                CreateRun {
                    run_id: id("run"),
                    definition_id: id("definition"),
                    revision_hash: canonical.digest().clone(),
                    input,
                    budget_limit: CostUnits(10),
                    limits: RunLimits {
                        max_dynamic_node_instances: 10,
                        max_total_attempts: 10,
                        max_total_events: 1_000,
                        max_inline_json_bytes_per_value: 10_000,
                        max_artifacts_per_attempt: 10,
                        max_aggregate_object_bytes_per_run: 100_000,
                        max_run_lifetime_ms: 10_000,
                    },
                    principal,
                    idempotency_token: "create-token-0001".to_owned(),
                },
            )
            .await
            .unwrap();
        let engine = WorkflowEngine::new(
            store.clone(),
            objects.clone(),
            registry.clone(),
            EngineConfig {
                instance_id: id("engine-1"),
                max_concurrency: 2,
            },
        )
        .unwrap();
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine.start(&execution_scope, &id("run")).await.unwrap();
        assert_eq!(engine.tick(&execution_scope).await.unwrap(), 1);
        assert_eq!(
            store
                .get_node(&execution_scope, &id("run"), &id("action"))
                .await
                .unwrap()
                .status,
            dagger_workflow_core::run::NodeState::RetryWaiting
        );
        engine.release_scope(&execution_scope).await.unwrap();

        let restarted = WorkflowEngine::new(
            store.clone(),
            objects,
            registry,
            EngineConfig {
                instance_id: id("engine-2"),
                max_concurrency: 2,
            },
        )
        .unwrap();
        restarted.acquire_scope(&execution_scope).await.unwrap();
        assert_eq!(restarted.tick(&execution_scope).await.unwrap(), 0);
        clock.advance_ms(100).unwrap();
        restarted.run_until_idle(&execution_scope, 8).await.unwrap();
        assert_eq!(
            store
                .get_run(&execution_scope, &id("run"))
                .await
                .unwrap()
                .run
                .status,
            RunState::Succeeded
        );
        let events = store
            .list_events_after(
                &execution_scope,
                &id("run"),
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1_000,
                    hard_response_byte_limit: 1_000_000,
                },
            )
            .await
            .unwrap();
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event.event_seq, index as u64 + 1);
        }
        for batch in events
            .iter()
            .map(|event| event.batch_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
        {
            let members = events
                .iter()
                .filter(|event| event.batch_id == batch)
                .collect::<Vec<_>>();
            assert_eq!(members.len(), members[0].batch_count as usize);
            assert!(members
                .iter()
                .enumerate()
                .all(|(index, event)| event.batch_index == index as u32));
        }
    });
}
