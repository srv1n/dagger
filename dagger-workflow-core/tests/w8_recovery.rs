#![cfg(feature = "sqlite")]

//! W8 crash-recovery fixtures. Every fixture kills the process seam the same way the plan
//! requires: the SQLite pool is closed and dropped, the object store handle is dropped, and
//! everything after the kill runs against a FRESH store instance (and, where the scheduler is
//! involved, a fresh `WorkflowEngine`) opened on the same durable files. Nothing survives in
//! process memory, so every post-crash assertion is a statement about the ledger.
//!
//! Contract sections 5.1, 5.3, 5.4, 5.5, 12.1, and 12.3.

use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, CompatibilityReport, CompletionCredential,
    InMemoryActionRegistry, WorkflowAction,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::{ObjectStore, VerifiedObjectRef};
use dagger_workflow_core::definition::{
    canonical_topological_ranks, ActionReference, BackoffPolicy, Binding, BindingSource,
    MapBinding, MapBindingSource, NodeDefinition, PublishableDefinition, RetryPolicy,
    TimeoutPolicy, WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::event::EventType;
use dagger_workflow_core::fs_object_store::FsObjectStore;
use dagger_workflow_core::ids::{
    map_child_id, map_expansion_digest, CostUnits, Digest, Id, MapChildIdentity, NodeInstanceId,
    Timestamp, Version,
};
use dagger_workflow_core::run::{AttemptState, NodeState, RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::sqlite::SqliteWorkflowStore;
use dagger_workflow_core::store::{
    ClaimNodeAttempt, ClaimNodeAttemptResult, CompleteAttempt, CompleteAttemptResult,
    CompletionObjects, CreateDefinition, CreateRun, EventPageRequest, ExpandMap, OrderedMapItem,
    PageRequest, PublishRevision, RecoverAbandonedAttemptsForRun, ReleaseRetry,
    ResolvedActionSchemas, StartRun, StoreError, WorkflowStore,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// The engine lease is database-clock authoritative and 20s long; a crashed generation is only
/// reclaimable once the persisted clock has moved past it. Contract section 6.
const CLAIM_LIFETIME_MS: i64 = 20_000;

type Objects = FsObjectStore<TestClock>;
type Store = SqliteWorkflowStore<TestClock, Objects>;

fn scope(tenant: &str) -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new(tenant).unwrap(),
        namespace: ScopeAtom::new("recovery").unwrap(),
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

/// The one closed schema every fixture node reads and writes, so schema conformance is never
/// what a recovery assertion is actually measuring.
fn schema_bytes() -> Vec<u8> {
    br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#.to_vec()
}

fn action_reference(name: &str, schema: &Digest) -> ActionReference {
    ActionReference {
        name: name.to_owned(),
        contract_version: "w8-1".to_owned(),
        input_schema_digest: schema.clone(),
        output_schema_digest: schema.clone(),
        compatible_implementation_requirement: hash(format!("w8:{name}").as_bytes()),
    }
}

fn descriptor(name: &str, schema: &Digest) -> ActionDescriptor {
    ActionDescriptor {
        name: name.to_owned(),
        contract_version: "w8-1".to_owned(),
        input_schema_digest: schema.clone(),
        output_schema_digest: schema.clone(),
        implementation_compatibility_digest: hash(format!("w8:{name}").as_bytes()),
    }
}

#[allow(clippy::too_many_arguments)]
fn action_node(
    node: &str,
    name: &str,
    schema: &Digest,
    source: BindingSource,
    max_attempts: u32,
    cost: u64,
    next: &[&str],
) -> NodeDefinition {
    NodeDefinition::Action {
        id: id(node),
        action: action_reference(name, schema),
        bindings: vec![Binding {
            target: "/value".to_owned(),
            source,
        }],
        retry: RetryPolicy {
            max_attempts,
            backoff: BackoffPolicy::Fixed { delay_ms: 0 },
        },
        timeout: TimeoutPolicy { timeout_ms: 60_000 },
        declared_max_cost_units: CostUnits(cost),
        next: next.iter().map(|target| id(target)).collect(),
    }
}

/// One durable deployment: a SQLite file plus a filesystem object root, both reopenable.
struct Deployment {
    _directory: TempDir,
    database: PathBuf,
    object_root: PathBuf,
    clock: Arc<TestClock>,
    scope: ExecutionScope,
    principal: AuthenticatedPrincipal,
    schema: Digest,
}

impl Deployment {
    fn new(tenant: &str) -> Self {
        let directory = TempDir::new().unwrap();
        let workflow_scope = scope(tenant);
        let schema = hash(&schema_bytes());
        Self {
            database: directory.path().join("workflow.sqlite"),
            object_root: directory.path().join("objects"),
            clock: Arc::new(TestClock::new(Timestamp(1_000_000))),
            principal: AuthenticatedPrincipal::mint(
                workflow_scope.clone(),
                format!("{tenant}-host"),
                Vec::new(),
                schema.clone(),
            )
            .unwrap(),
            scope: workflow_scope,
            schema,
            _directory: directory,
        }
    }

    /// Opens a brand-new store and object-store instance over the durable files. Every use after
    /// a simulated kill goes through this, so no post-crash assertion can read process memory.
    async fn open(&self) -> (Arc<Store>, Arc<Objects>) {
        let objects = Arc::new(FsObjectStore::open(&self.object_root, self.clock.clone()).unwrap());
        let store = Arc::new(
            SqliteWorkflowStore::open(&self.database, self.clock.clone(), objects.clone())
                .await
                .unwrap(),
        );
        (store, objects)
    }

    async fn put(&self, objects: &Objects, value: &Value) -> VerifiedObjectRef {
        objects
            .put(
                &self.scope,
                &serde_jcs::to_vec(value).unwrap(),
                "application/json",
            )
            .await
            .unwrap()
    }

    async fn publish(
        &self,
        store: &Store,
        objects: &Objects,
        definition: WorkflowDefinition,
        schema_locations: &[&str],
    ) -> Digest {
        let schema_object = objects
            .put(&self.scope, &schema_bytes(), "application/json")
            .await
            .unwrap();
        assert_eq!(schema_object.digest(), &self.schema);
        store
            .create_definition(
                &self.scope,
                CreateDefinition {
                    definition_id: definition.definition_id.clone(),
                    display_name: definition.name.clone(),
                    description: String::new(),
                    principal: self.principal.clone(),
                },
            )
            .await
            .unwrap();
        let canonical = objects
            .put(
                &self.scope,
                &serde_jcs::to_vec(&definition).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        let topological_ranks = canonical_topological_ranks(&definition).unwrap();
        store
            .publish_revision(
                &self.scope,
                PublishRevision {
                    definition_id: definition.definition_id.clone(),
                    expected_definition_version: Version(1),
                    canonical_definition: canonical.clone(),
                    run_input_schema: schema_object.clone(),
                    run_output_schema: schema_object.clone(),
                    resolved_action_schema_objects: schema_locations
                        .iter()
                        .map(|location| {
                            (
                                (*location).to_owned(),
                                ResolvedActionSchemas {
                                    input_schema: schema_object.clone(),
                                    output_schema: schema_object.clone(),
                                },
                            )
                        })
                        .collect(),
                    parsed_revision: PublishableDefinition {
                        definition,
                        topological_ranks,
                    },
                    principal: self.principal.clone(),
                },
            )
            .await
            .unwrap();
        canonical.digest().clone()
    }

    async fn create_run(
        &self,
        store: &Store,
        objects: &Objects,
        definition_id: &Id,
        revision_hash: &Digest,
        run_id: &str,
        budget_limit: u64,
    ) {
        let input = self.put(objects, &json!({"value": 1})).await;
        store
            .create_run(
                &self.scope,
                CreateRun {
                    run_id: id(run_id),
                    definition_id: definition_id.clone(),
                    revision_hash: revision_hash.clone(),
                    input,
                    budget_limit: CostUnits(budget_limit),
                    limits: RunLimits {
                        max_dynamic_node_instances: 16,
                        max_total_attempts: 16,
                        max_total_events: 1_000,
                        max_inline_json_bytes_per_value: 10_000,
                        max_artifacts_per_attempt: 4,
                        max_aggregate_object_bytes_per_run: 100_000,
                        max_run_lifetime_ms: 1_000_000,
                    },
                    principal: self.principal.clone(),
                    idempotency_token: format!("w8-create-{run_id}-token-long-enough"),
                },
            )
            .await
            .unwrap();
    }

    fn evidence(&self) -> CompatibilityReport {
        CompatibilityReport {
            evidence_digest: self.schema.clone(),
            incompatible_reference_locations: Vec::new(),
            evidence: Vec::new(),
        }
    }
}

/// Closes the pool and drops both handles, then moves the persisted clock past the lease so the
/// next instance can take the scope over exactly as a restarted host would.
async fn kill(store: Arc<Store>, objects: Arc<Objects>) {
    store
        .advance_database_clock_ms(CLAIM_LIFETIME_MS + 1)
        .await
        .unwrap();
    store.pool().close().await;
    let weak = Arc::downgrade(&store);
    drop(store);
    drop(objects);
    assert!(
        weak.upgrade().is_none(),
        "a store handle outlived the simulated kill, so the fixture is not testing recovery"
    );
}

async fn ready_node_ids(store: &Store, workflow_scope: &ExecutionScope) -> Vec<String> {
    let mut ids = store
        .scan_ready_nodes(
            workflow_scope,
            PageRequest {
                cursor: None,
                page_size: 32,
            },
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|node| node.node_instance_id.as_str().to_owned())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

// ---------------------------------------------------------------------------------------------
// Scheduler-driven fixtures.
// ---------------------------------------------------------------------------------------------

/// Records every real invocation so "never re-run" is a claim about the action, not about a
/// counter the store happens to keep.
#[derive(Clone, Default)]
struct InvocationLog(Arc<Mutex<Vec<(String, String, String)>>>);

impl InvocationLog {
    fn entries(&self) -> Vec<(String, String, String)> {
        self.0.lock().unwrap().clone()
    }

    fn count(&self, node: &str) -> usize {
        self.entries()
            .iter()
            .filter(|(instance, _, _)| instance == node)
            .count()
    }

    fn idempotency_keys(&self, node: &str) -> Vec<String> {
        self.entries()
            .iter()
            .filter(|(instance, _, _)| instance == node)
            .map(|(_, key, _)| key.clone())
            .collect()
    }
}

struct RecordingAction {
    descriptor: ActionDescriptor,
    log: InvocationLog,
}

impl WorkflowAction for RecordingAction {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        canonical_bound_input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        let input: Value = serde_json::from_slice(canonical_bound_input).unwrap();
        self.log.0.lock().unwrap().push((
            context.node_instance_id.as_str().to_owned(),
            context.idempotency_key.clone(),
            context.attempt_id.as_str().to_owned(),
        ));
        Box::pin(async move {
            ActionOutcome::success(
                json!({"value": input.get("value").cloned().unwrap_or(Value::Null)}),
                Vec::new(),
                CostUnits(1),
                None,
            )
            .unwrap()
        })
    }
}

fn registry(names: &[&str], schema: &Digest, log: &InvocationLog) -> Arc<InMemoryActionRegistry> {
    let registry = Arc::new(InMemoryActionRegistry::new());
    for name in names {
        registry
            .register(Arc::new(RecordingAction {
                descriptor: descriptor(name, schema),
                log: log.clone(),
            }))
            .unwrap();
    }
    registry
}

fn engine(
    store: Arc<Store>,
    objects: Arc<Objects>,
    registry: Arc<InMemoryActionRegistry>,
    instance: &str,
) -> WorkflowEngine<Store, Objects, InMemoryActionRegistry> {
    WorkflowEngine::new(
        store,
        objects,
        registry,
        EngineConfig {
            instance_id: id(instance),
            max_concurrency: 2,
        },
    )
    .unwrap()
}

fn chain_definition(schema: &Digest) -> WorkflowDefinition {
    WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: id("chain"),
        name: "chain".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.clone(),
        run_output_schema_digest: schema.clone(),
        entry_node_id: id("alfa"),
        nodes: vec![
            action_node(
                "alfa",
                "w8.alfa",
                schema,
                BindingSource::RunInput {
                    pointer: "/value".to_owned(),
                },
                2,
                2,
                &["bravo"],
            ),
            action_node(
                "bravo",
                "w8.bravo",
                schema,
                BindingSource::NodeOutput {
                    node_id: id("alfa"),
                    pointer: "/value".to_owned(),
                },
                3,
                2,
                &["done"],
            ),
            NodeDefinition::Succeed {
                id: id("done"),
                output: BindingSource::NodeOutput {
                    node_id: id("bravo"),
                    pointer: String::new(),
                },
            },
        ],
    }
}

/// Fixture 1 and fixture 2. The frontier a restarted scheduler resumes from is derived only from
/// the persisted ledger, and work the crashed generation already committed is never re-invoked.
///
/// The pre-crash frontier is captured, the deployment is killed, and a fresh store is inspected
/// BEFORE any tick: the ready set must already be correct with no engine having run against it.
/// Contract sections 5.1 and 5.4.
#[tokio::test]
async fn restart_reconstructs_the_frontier_and_never_reruns_committed_work() {
    let deployment = Deployment::new("frontier");
    let log = InvocationLog::default();
    let actions = registry(&["w8.alfa", "w8.bravo"], &deployment.schema, &log);

    let (store, objects) = deployment.open().await;
    let revision_hash = deployment
        .publish(
            &store,
            &objects,
            chain_definition(&deployment.schema),
            &["alfa", "bravo"],
        )
        .await;
    deployment
        .create_run(&store, &objects, &id("chain"), &revision_hash, "run", 20)
        .await;

    let first = engine(
        store.clone(),
        objects.clone(),
        actions.clone(),
        "engine-one",
    );
    first.acquire_scope(&deployment.scope).await.unwrap();
    first.start(&deployment.scope, &id("run")).await.unwrap();
    first.tick(&deployment.scope).await.unwrap();

    // The crash point: alfa is committed Succeeded, bravo has never been claimed.
    let alfa_before = store
        .get_node(&deployment.scope, &id("run"), &id("alfa"))
        .await
        .unwrap();
    assert_eq!(alfa_before.status, NodeState::Succeeded);
    assert_eq!(alfa_before.attempt_count, 1);
    assert_eq!(log.count("alfa"), 1);
    let watermark = store
        .get_run(&deployment.scope, &id("run"))
        .await
        .unwrap()
        .run
        .last_event_seq;
    drop(first);
    kill(store, objects).await;

    let (store, objects) = deployment.open().await;
    assert_eq!(
        ready_node_ids(&store, &deployment.scope).await,
        vec!["bravo".to_owned()],
        "a fresh store reconstructed the wrong frontier from the ledger alone"
    );
    let alfa_after = store
        .get_node(&deployment.scope, &id("run"), &id("alfa"))
        .await
        .unwrap();
    assert_eq!(alfa_after.status, NodeState::Succeeded);
    assert_eq!(alfa_after.result_ref, alfa_before.result_ref);
    assert_eq!(alfa_after.version, alfa_before.version);

    let second = engine(store.clone(), objects.clone(), actions, "engine-two");
    second.acquire_scope(&deployment.scope).await.unwrap();
    second.run_until_idle(&deployment.scope, 16).await.unwrap();

    let run = store
        .get_run(&deployment.scope, &id("run"))
        .await
        .unwrap()
        .run;
    assert_eq!(run.status, RunState::Succeeded);
    assert_eq!(
        log.count("alfa"),
        1,
        "recovery re-invoked an action whose result was already committed"
    );
    assert_eq!(log.count("bravo"), 1);
    // The key the recovered generation delivered is the persisted scope-bound derivation, not a
    // per-process value the second engine invented. Contract section 7.1.
    assert_eq!(
        log.idempotency_keys("bravo"),
        vec![dagger_workflow_core::ids::idempotency_key(
            &deployment.scope,
            &id("run"),
            &NodeInstanceId::from(id("bravo")),
        )]
    );
    assert_eq!(
        store
            .get_node(&deployment.scope, &id("run"), &id("alfa"))
            .await
            .unwrap()
            .attempt_count,
        1
    );
    // Nothing the second generation committed may rewrite the first generation's event history.
    let replayed = store
        .list_events_after(
            &deployment.scope,
            &id("run"),
            EventPageRequest {
                after_event_seq: 0,
                page_size: 1_000,
                hard_response_byte_limit: 4_000_000,
            },
        )
        .await
        .unwrap();
    assert!(
        replayed
            .iter()
            .take_while(|event| event.event_seq <= watermark)
            .count()
            > 0
    );
    assert!(replayed
        .windows(2)
        .all(|pair| pair[0].event_seq < pair[1].event_seq));
}

// ---------------------------------------------------------------------------------------------
// Store-level kill fixtures.
// ---------------------------------------------------------------------------------------------

struct StartedRun {
    store: Arc<Store>,
    objects: Arc<Objects>,
    permit: dagger_workflow_core::store::EnginePermit,
}

/// Publishes, creates, and starts one run, leaving the scope claimed by `instance`.
async fn start_run(
    deployment: &Deployment,
    definition: WorkflowDefinition,
    schema_locations: &[&str],
    run_id: &str,
    budget_limit: u64,
    instance: &str,
) -> StartedRun {
    let definition_id = definition.definition_id.clone();
    let (store, objects) = deployment.open().await;
    let revision_hash = deployment
        .publish(&store, &objects, definition, schema_locations)
        .await;
    deployment
        .create_run(
            &store,
            &objects,
            &definition_id,
            &revision_hash,
            run_id,
            budget_limit,
        )
        .await;
    let acquired = store
        .acquire_engine_claim(&deployment.scope, id(instance))
        .await
        .unwrap();
    store
        .start_run(
            &deployment.scope,
            StartRun {
                permit: acquired.permit.clone(),
                run_id: id(run_id),
                compatibility_evidence: deployment.evidence(),
            },
        )
        .await
        .unwrap();
    StartedRun {
        store,
        objects,
        permit: acquired.permit,
    }
}

async fn claim(
    deployment: &Deployment,
    store: &Store,
    permit: &dagger_workflow_core::store::EnginePermit,
    run_id: &str,
    node: &NodeInstanceId,
    attempt_id: &str,
    bound_input: VerifiedObjectRef,
) -> CompletionCredential {
    let current = store
        .get_node(&deployment.scope, &id(run_id), node)
        .await
        .unwrap();
    match store
        .claim_node_attempt(
            &deployment.scope,
            ClaimNodeAttempt {
                permit: permit.clone(),
                run_id: id(run_id),
                node_id: node.clone(),
                expected_node_version: current.version,
                attempt_id: id(attempt_id),
                worker_id: id("w8-worker"),
                bound_input,
                binding_derivation_digest: deployment.schema.clone(),
            },
        )
        .await
        .unwrap()
    {
        ClaimNodeAttemptResult::Claimed {
            completion_credential,
            ..
        } => completion_credential,
        _ => panic!("the fixture required an unconditional claim"),
    }
}

/// Fixture 3. A kill between the object-store put and the SQLite commit leaves an orphan object
/// and no committed reference. The recovered generation re-invokes the node under the SAME
/// versioned scope-bound idempotency key, and the orphan is tolerated rather than reconciled.
/// Contract sections 5.3, 7.1, and 12.1.
#[tokio::test]
async fn a_kill_between_object_put_and_commit_reuses_the_versioned_idempotency_key() {
    let deployment = Deployment::new("orphan");
    let started = start_run(
        &deployment,
        chain_definition(&deployment.schema),
        &["alfa", "bravo"],
        "run",
        20,
        "engine-one",
    )
    .await;
    let bound = deployment.put(&started.objects, &json!({"value": 1})).await;
    claim(
        &deployment,
        &started.store,
        &started.permit,
        "run",
        &id("alfa"),
        "attempt-before-crash",
        bound,
    )
    .await;
    // The action's output object is durably published; the SQLite commit that would reference it
    // never happens. This is the exact W7 commit-order window.
    let orphan = deployment
        .put(&started.objects, &json!({"value": 4242}))
        .await;
    let before = started
        .store
        .get_attempt(&deployment.scope, &id("run"), &id("attempt-before-crash"))
        .await
        .unwrap();
    assert_eq!(before.status, AttemptState::Started);
    kill(started.store, started.objects).await;

    let (store, objects) = deployment.open().await;
    let acquired = store
        .acquire_engine_claim(&deployment.scope, id("engine-two"))
        .await
        .unwrap();
    let recovered = store
        .recover_abandoned_attempts_for_run(
            &deployment.scope,
            RecoverAbandonedAttemptsForRun {
                permit: acquired.permit.clone(),
                run_id: id("run"),
            },
        )
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].status, AttemptState::UnknownOutcome);

    let waiting = store
        .get_node(&deployment.scope, &id("run"), &id("alfa"))
        .await
        .unwrap();
    assert_eq!(waiting.status, NodeState::RetryWaiting);
    store
        .release_retry(
            &deployment.scope,
            ReleaseRetry {
                permit: acquired.permit.clone(),
                run_id: id("run"),
                node_id: id("alfa"),
                expected_node_version: waiting.version,
            },
        )
        .await
        .unwrap();
    let bound = deployment.put(&objects, &json!({"value": 1})).await;
    claim(
        &deployment,
        &store,
        &acquired.permit,
        "run",
        &id("alfa"),
        "attempt-after-crash",
        bound,
    )
    .await;
    let after = store
        .get_attempt(&deployment.scope, &id("run"), &id("attempt-after-crash"))
        .await
        .unwrap();

    let expected = dagger_workflow_core::ids::idempotency_key(
        &deployment.scope,
        &id("run"),
        &NodeInstanceId::from(id("alfa")),
    );
    assert!(
        expected.starts_with("dwf-idem-v1:"),
        "key lost its version prefix"
    );
    assert_eq!(before.idempotency_key, expected);
    assert_eq!(
        after.idempotency_key, before.idempotency_key,
        "the re-invoked node did not receive the retry-stable scope-bound key"
    );
    assert_ne!(after.attempt_id, before.attempt_id);
    assert_eq!(after.attempt_number, before.attempt_number + 1);
    // A different scope must derive a different key, so the assertion above is about binding and
    // not about a constant.
    assert_ne!(
        expected,
        dagger_workflow_core::ids::idempotency_key(
            &scope("other-tenant"),
            &id("run"),
            &NodeInstanceId::from(id("alfa")),
        )
    );

    // The orphan is tolerated: still readable, still unreferenced by any committed row.
    let reread = objects
        .get(&deployment.scope, orphan.digest())
        .await
        .unwrap();
    assert_eq!(reread.reference.digest(), orphan.digest());
    for attempt_id in ["attempt-before-crash", "attempt-after-crash"] {
        let attempt = store
            .get_attempt(&deployment.scope, &id("run"), &id(attempt_id))
            .await
            .unwrap();
        assert!(attempt.output_ref.is_none());
        assert!(attempt.artifact_refs.is_empty());
    }
    assert!(store
        .get_node(&deployment.scope, &id("run"), &id("alfa"))
        .await
        .unwrap()
        .result_ref
        .is_none());
}

/// Fixture 4. An attempt whose outcome the crash made unknowable consumes a retry from the
/// ceiling AND settles its whole reservation against the run budget. Neither may be refunded on
/// the optimistic assumption that the action never ran. Contract sections 5.3 and 12.1.
#[tokio::test]
async fn a_crash_unknown_attempt_consumes_the_retry_ceiling_and_the_full_reservation() {
    let deployment = Deployment::new("ceiling");
    let mut definition = chain_definition(&deployment.schema);
    // One attempt only, so the single crash-unknown attempt is the whole ceiling.
    definition.nodes[0] = action_node(
        "alfa",
        "w8.alfa",
        &deployment.schema,
        BindingSource::RunInput {
            pointer: "/value".to_owned(),
        },
        1,
        7,
        &["bravo"],
    );
    let started = start_run(
        &deployment,
        definition,
        &["alfa", "bravo"],
        "run",
        20,
        "engine-one",
    )
    .await;
    let bound = deployment.put(&started.objects, &json!({"value": 1})).await;
    claim(
        &deployment,
        &started.store,
        &started.permit,
        "run",
        &id("alfa"),
        "only-attempt",
        bound,
    )
    .await;
    let reserved = started
        .store
        .get_run(&deployment.scope, &id("run"))
        .await
        .unwrap()
        .run;
    assert_eq!(reserved.budget_reserved, CostUnits(7));
    assert_eq!(reserved.budget_consumed, CostUnits(0));
    kill(started.store, started.objects).await;

    let (store, _objects) = deployment.open().await;
    let acquired = store
        .acquire_engine_claim(&deployment.scope, id("engine-two"))
        .await
        .unwrap();
    store
        .recover_abandoned_attempts_for_run(
            &deployment.scope,
            RecoverAbandonedAttemptsForRun {
                permit: acquired.permit,
                run_id: id("run"),
            },
        )
        .await
        .unwrap();

    let attempt = store
        .get_attempt(&deployment.scope, &id("run"), &id("only-attempt"))
        .await
        .unwrap();
    assert_eq!(attempt.status, AttemptState::UnknownOutcome);
    assert_eq!(
        attempt.settled_cost,
        Some(CostUnits(7)),
        "an unknown outcome settled less than its full reservation"
    );
    let node = store
        .get_node(&deployment.scope, &id("run"), &id("alfa"))
        .await
        .unwrap();
    assert_eq!(
        node.status,
        NodeState::RetriesExhausted,
        "the crash-unknown attempt did not consume the retry ceiling"
    );
    assert_eq!(node.attempt_count, 1);
    assert!(node.next_eligible_at.is_none());
    let run = store
        .get_run(&deployment.scope, &id("run"))
        .await
        .unwrap()
        .run;
    assert_eq!(run.status, RunState::RetriesExhausted);
    assert_eq!(run.budget_consumed, CostUnits(7));
    assert_eq!(run.budget_reserved, CostUnits(0));
}

/// Fixture 5. A result from the killed generation arriving after restart is admitted or refused
/// on its `CompletionCredential` alone. The run/node/attempt triple is not an authenticator, a
/// credential is not transferable to a newer attempt, and a stale admission may not mutate node
/// or run state. Contract sections 5.3 and 5.5.
#[tokio::test]
async fn a_late_completion_is_authenticated_only_by_its_completion_credential() {
    let deployment = Deployment::new("fencing");
    let started = start_run(
        &deployment,
        chain_definition(&deployment.schema),
        &["alfa", "bravo"],
        "run",
        20,
        "engine-one",
    )
    .await;
    let bound = deployment.put(&started.objects, &json!({"value": 1})).await;
    let stale_credential = claim(
        &deployment,
        &started.store,
        &started.permit,
        "run",
        &id("alfa"),
        "attempt-before-crash",
        bound.clone(),
    )
    .await;
    kill(started.store, started.objects).await;

    let (store, objects) = deployment.open().await;
    let acquired = store
        .acquire_engine_claim(&deployment.scope, id("engine-two"))
        .await
        .unwrap();
    store
        .recover_abandoned_attempts_for_run(
            &deployment.scope,
            RecoverAbandonedAttemptsForRun {
                permit: acquired.permit.clone(),
                run_id: id("run"),
            },
        )
        .await
        .unwrap();
    let waiting = store
        .get_node(&deployment.scope, &id("run"), &id("alfa"))
        .await
        .unwrap();
    store
        .release_retry(
            &deployment.scope,
            ReleaseRetry {
                permit: acquired.permit.clone(),
                run_id: id("run"),
                node_id: id("alfa"),
                expected_node_version: waiting.version,
            },
        )
        .await
        .unwrap();
    let bound = deployment.put(&objects, &json!({"value": 1})).await;
    let live_credential = claim(
        &deployment,
        &store,
        &acquired.permit,
        "run",
        &id("alfa"),
        "attempt-after-crash",
        bound.clone(),
    )
    .await;
    assert_ne!(stale_credential.digest(), live_credential.digest());

    let completion = |credential: CompletionCredential, attempt: &str| CompleteAttempt {
        completion_credential: credential,
        run_id: id("run"),
        node_id: id("alfa"),
        attempt_id: id(attempt),
        submitted_outcome: ActionOutcome::Success {
            output: json!({"value": 9}),
            artifacts: Vec::new(),
            actual_cost_units: CostUnits(1),
            diagnostics: None,
        },
        objects: CompletionObjects {
            output: Some(bound.clone()),
            artifacts: Vec::new(),
            diagnostics: None,
        },
    };

    // A forged credential on a real triple is refused before the fence is even consulted.
    assert!(matches!(
        store
            .complete_attempt(
                &deployment.scope,
                completion(
                    CompletionCredential::from_minted_bytes([7u8; 32]),
                    "attempt-before-crash"
                )
            )
            .await,
        Err(StoreError::InvalidCompletionCredential)
    ));
    // The stale credential cannot be redirected onto the newer attempt.
    assert!(matches!(
        store
            .complete_attempt(
                &deployment.scope,
                completion(stale_credential.clone(), "attempt-after-crash")
            )
            .await,
        Err(StoreError::InvalidCompletionCredential)
    ));
    assert_eq!(
        store
            .get_attempt(&deployment.scope, &id("run"), &id("attempt-after-crash"))
            .await
            .unwrap()
            .status,
        AttemptState::Started,
        "a late credential mutated a newer attempt"
    );

    // Presented against its own attempt, the credential authenticates and fencing then records
    // the result as stale without applying it.
    let node_before = store
        .get_node(&deployment.scope, &id("run"), &id("alfa"))
        .await
        .unwrap();
    assert!(matches!(
        store
            .complete_attempt(
                &deployment.scope,
                completion(stale_credential.clone(), "attempt-before-crash")
            )
            .await
            .unwrap(),
        CompleteAttemptResult::StaleRecorded(_)
    ));
    // Exact replay of the same admitted stale result produces no second observation.
    assert!(matches!(
        store
            .complete_attempt(
                &deployment.scope,
                completion(stale_credential, "attempt-before-crash")
            )
            .await
            .unwrap(),
        CompleteAttemptResult::AlreadyObserved(_)
    ));
    let node_after = store
        .get_node(&deployment.scope, &id("run"), &id("alfa"))
        .await
        .unwrap();
    assert_eq!(node_after.status, node_before.status);
    assert_eq!(node_after.active_attempt_id, node_before.active_attempt_id);
    assert!(node_after.result_ref.is_none());
    assert_eq!(
        store
            .get_attempt(&deployment.scope, &id("run"), &id("attempt-before-crash"))
            .await
            .unwrap()
            .status,
        AttemptState::UnknownOutcome,
        "a stale completion overwrote the recovered terminal attempt state"
    );
}

// ---------------------------------------------------------------------------------------------
// Map re-expansion.
// ---------------------------------------------------------------------------------------------

fn map_definition(schema: &Digest) -> WorkflowDefinition {
    WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: id("mapdef"),
        name: "map".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.clone(),
        run_output_schema_digest: schema.clone(),
        entry_node_id: id("fanout"),
        nodes: vec![
            NodeDefinition::Map {
                id: id("fanout"),
                items: BindingSource::Constant {
                    value: json!([{"value": 1}, {"value": 2}, {"value": 3}]),
                },
                max_items: 8,
                max_concurrency: 3,
                action: action_reference("w8.item", schema),
                bindings: vec![MapBinding {
                    target: "/value".to_owned(),
                    source: MapBindingSource::MapItem {
                        pointer: "/value".to_owned(),
                    },
                }],
                retry: RetryPolicy {
                    max_attempts: 2,
                    backoff: BackoffPolicy::Fixed { delay_ms: 0 },
                },
                timeout: TimeoutPolicy { timeout_ms: 60_000 },
                declared_max_cost_units: CostUnits(1),
                next: vec![id("done")],
            },
            NodeDefinition::Succeed {
                id: id("done"),
                output: BindingSource::NodeOutput {
                    node_id: id("fanout"),
                    pointer: String::new(),
                },
            },
        ],
    }
}

/// The three constant items the Map definition fans out over.
fn map_items() -> Vec<Value> {
    vec![
        json!({"value": 1}),
        json!({"value": 2}),
        json!({"value": 3}),
    ]
}

/// Builds the exact ordered expansion the scheduler would derive for `items`.
async fn expansion(
    deployment: &Deployment,
    objects: &Objects,
    run_id: &str,
    items: &[Value],
) -> (VerifiedObjectRef, Vec<OrderedMapItem>, Digest) {
    let input = deployment.put(objects, &Value::Array(items.to_vec())).await;
    let mut ordered = Vec::new();
    let mut identities = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let index = index as u32;
        let item_digest = deployment.put(objects, item).await.digest().clone();
        let child_id = map_child_id(
            &id(run_id),
            &NodeInstanceId::from(id("fanout")),
            index,
            &item_digest,
        );
        ordered.push(OrderedMapItem {
            index,
            item_digest: item_digest.clone(),
            child_id: child_id.clone(),
        });
        identities.push(MapChildIdentity {
            item_index: index,
            item_digest,
            child_id,
        });
    }
    let digest = map_expansion_digest(&identities);
    (input, ordered, digest)
}

/// Fixture 6. A kill in the Map expansion window is survivable in both directions: an expansion
/// that never committed is re-derived to a byte-identical child set, and an expansion that did
/// commit is not duplicated by the retry the restarted scheduler would naturally issue.
/// Contract sections 5.3, 10.1, and 12.1.
#[tokio::test]
async fn map_re_expansion_after_a_crash_converges_on_an_identical_child_set() {
    let deployment = Deployment::new("mapcrash");
    let started = start_run(
        &deployment,
        map_definition(&deployment.schema),
        &["fanout/map_action"],
        "run",
        20,
        "engine-one",
    )
    .await;
    let (input, ordered, digest) =
        expansion(&deployment, &started.objects, "run", &map_items()).await;
    let expected_children = ordered
        .iter()
        .map(|item| item.child_id.as_str().to_owned())
        .collect::<Vec<_>>();
    // Killed inside the expansion window: the items were derived, nothing committed.
    let unexpanded = started
        .store
        .get_node(&deployment.scope, &id("run"), &id("fanout"))
        .await
        .unwrap();
    assert!(unexpanded.map_expansion_digest.is_none());
    kill(started.store, started.objects).await;

    let (store, objects) = deployment.open().await;
    let acquired = store
        .acquire_engine_claim(&deployment.scope, id("engine-two"))
        .await
        .unwrap();
    let node = store
        .get_node(&deployment.scope, &id("run"), &id("fanout"))
        .await
        .unwrap();
    assert!(
        node.map_expansion_digest.is_none(),
        "an uncommitted expansion survived the kill"
    );
    // Re-derived by the restarted generation from the same persisted definition and run.
    let (reinput, reordered, redigest) =
        expansion(&deployment, &objects, "run", &map_items()).await;
    assert_eq!(reinput.digest(), input.digest());
    assert_eq!(redigest, digest);
    assert_eq!(
        reordered
            .iter()
            .map(|item| item.child_id.as_str().to_owned())
            .collect::<Vec<_>>(),
        expected_children
    );
    store
        .expand_map(
            &deployment.scope,
            ExpandMap {
                permit: acquired.permit.clone(),
                run_id: id("run"),
                map_node_id: id("fanout"),
                expected_node_version: node.version,
                input: reinput.clone(),
                ordered_items: reordered.clone(),
                expansion_digest: redigest.clone(),
            },
        )
        .await
        .unwrap();
    let expanded = store
        .get_node(&deployment.scope, &id("run"), &id("fanout"))
        .await
        .unwrap();
    assert_eq!(expanded.map_child_count, Some(3));
    assert_eq!(expanded.map_expansion_digest, Some(redigest.clone()));
    let committed_children = ready_node_ids(&store, &deployment.scope).await;
    assert_eq!(committed_children.len(), 3);

    // Second half: kill AFTER the expansion committed. The restarted generation re-derives the
    // identical expansion and must not be able to fan out a second child set.
    let dynamic_before = store
        .get_run(&deployment.scope, &id("run"))
        .await
        .unwrap()
        .run
        .dynamic_node_count;
    kill(store, objects).await;

    let (store, objects) = deployment.open().await;
    let acquired = store
        .acquire_engine_claim(&deployment.scope, id("engine-three"))
        .await
        .unwrap();
    let hydrated = store
        .get_node(&deployment.scope, &id("run"), &id("fanout"))
        .await
        .unwrap();
    assert_eq!(hydrated.map_expansion_digest, Some(redigest.clone()));
    assert_eq!(hydrated.map_child_count, Some(3));
    assert_eq!(
        ready_node_ids(&store, &deployment.scope).await,
        committed_children,
        "the committed child set did not hydrate identically from the ledger"
    );
    let (reinput, reordered, redigest_again) =
        expansion(&deployment, &objects, "run", &map_items()).await;
    assert_eq!(redigest_again, redigest);
    // Converged re-expansion is absorbed rather than re-applied.
    let replayed = store
        .expand_map(
            &deployment.scope,
            ExpandMap {
                permit: acquired.permit.clone(),
                run_id: id("run"),
                map_node_id: id("fanout"),
                expected_node_version: hydrated.version,
                input: reinput,
                ordered_items: reordered,
                expansion_digest: redigest_again,
            },
        )
        .await
        .unwrap();
    assert_eq!(replayed.map_child_count, Some(3));
    assert_eq!(replayed.version, hydrated.version);

    // The convergence claim only means anything if divergence is refused: a restarted generation
    // that derived a different child set must not be able to rewrite the committed expansion.
    let (bad_input, bad_ordered, bad_digest) = expansion(
        &deployment,
        &objects,
        "run",
        &[json!({"value": 1}), json!({"value": 2})],
    )
    .await;
    assert_ne!(bad_digest, redigest);
    assert!(matches!(
        store
            .expand_map(
                &deployment.scope,
                ExpandMap {
                    permit: acquired.permit,
                    run_id: id("run"),
                    map_node_id: id("fanout"),
                    expected_node_version: hydrated.version,
                    input: bad_input,
                    ordered_items: bad_ordered,
                    expansion_digest: bad_digest,
                },
            )
            .await,
        Err(StoreError::IdempotencyConflict)
    ));
    assert_eq!(
        store
            .get_run(&deployment.scope, &id("run"))
            .await
            .unwrap()
            .run
            .dynamic_node_count,
        dynamic_before,
        "re-expansion after restart minted additional dynamic nodes"
    );
    assert_eq!(
        ready_node_ids(&store, &deployment.scope).await,
        committed_children
    );
}

// ---------------------------------------------------------------------------------------------
// Multi-attempt takeover and order independence.
// ---------------------------------------------------------------------------------------------

fn fanout_definition(schema: &Digest) -> WorkflowDefinition {
    WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: id("fan"),
        name: "fan".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.clone(),
        run_output_schema_digest: schema.clone(),
        entry_node_id: id("root"),
        nodes: vec![
            action_node(
                "root",
                "w8.root",
                schema,
                BindingSource::RunInput {
                    pointer: "/value".to_owned(),
                },
                3,
                1,
                &["alfa", "zulu"],
            ),
            action_node(
                "alfa",
                "w8.alfa",
                schema,
                BindingSource::NodeOutput {
                    node_id: id("root"),
                    pointer: "/value".to_owned(),
                },
                1,
                1,
                &["done"],
            ),
            action_node(
                "zulu",
                "w8.zulu",
                schema,
                BindingSource::NodeOutput {
                    node_id: id("root"),
                    pointer: "/value".to_owned(),
                },
                1,
                1,
                &["done"],
            ),
            NodeDefinition::Succeed {
                id: id("done"),
                output: BindingSource::NodeOutput {
                    node_id: id("alfa"),
                    pointer: String::new(),
                },
            },
        ],
    }
}

/// The observable recovery outcome: the ordered recovery event batch plus the terminal state of
/// every node and the run.
#[derive(Debug, Eq, PartialEq)]
struct TakeoverOutcome {
    batch: Vec<(EventType, String)>,
    nodes: Vec<(String, NodeState)>,
    run: RunState,
}

/// Runs one takeover scenario. `claim_order` decides which abandoned attempt is written first and
/// `attempt_ids` decides how the attempt identifiers sort, so a recovery that ordered by row
/// arrival or by attempt ID instead of by persisted rank produces a different outcome.
async fn takeover(claim_order: [&str; 2], attempt_ids: [&str; 2]) -> TakeoverOutcome {
    let deployment = Deployment::new(&format!("takeover-{}-{}", claim_order[0], attempt_ids[0]));
    let started = start_run(
        &deployment,
        fanout_definition(&deployment.schema),
        &["root", "alfa", "zulu"],
        "run",
        20,
        "engine-one",
    )
    .await;
    let bound = deployment.put(&started.objects, &json!({"value": 1})).await;
    let root_credential = claim(
        &deployment,
        &started.store,
        &started.permit,
        "run",
        &id("root"),
        "root-attempt",
        bound.clone(),
    )
    .await;
    started
        .store
        .complete_attempt(
            &deployment.scope,
            CompleteAttempt {
                completion_credential: root_credential,
                run_id: id("run"),
                node_id: id("root"),
                attempt_id: id("root-attempt"),
                submitted_outcome: ActionOutcome::Success {
                    output: json!({"value": 1}),
                    artifacts: Vec::new(),
                    actual_cost_units: CostUnits(1),
                    diagnostics: None,
                },
                objects: CompletionObjects {
                    output: Some(bound.clone()),
                    artifacts: Vec::new(),
                    diagnostics: None,
                },
            },
        )
        .await
        .unwrap();

    let by_node: BTreeMap<&str, &str> = BTreeMap::from([
        (claim_order[0], attempt_ids[0]),
        (claim_order[1], attempt_ids[1]),
    ]);
    for node in claim_order {
        claim(
            &deployment,
            &started.store,
            &started.permit,
            "run",
            &id(node),
            by_node[node],
            bound.clone(),
        )
        .await;
    }
    let watermark = started
        .store
        .get_run(&deployment.scope, &id("run"))
        .await
        .unwrap()
        .run
        .last_event_seq;
    kill(started.store, started.objects).await;

    let (store, _objects) = deployment.open().await;
    // Ranks are read back from the ledger, not assumed, so the fixture asserts against what the
    // store actually persisted at publication. Contract section 1.3.
    let alfa_rank = store
        .get_node(&deployment.scope, &id("run"), &id("alfa"))
        .await
        .unwrap()
        .topological_rank;
    let zulu_rank = store
        .get_node(&deployment.scope, &id("run"), &id("zulu"))
        .await
        .unwrap()
        .topological_rank;
    assert!(alfa_rank < zulu_rank);
    let acquired = store
        .acquire_engine_claim(&deployment.scope, id("engine-two"))
        .await
        .unwrap();
    let recovered = store
        .recover_abandoned_attempts_for_run(
            &deployment.scope,
            RecoverAbandonedAttemptsForRun {
                permit: acquired.permit,
                run_id: id("run"),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        recovered.len(),
        2,
        "takeover terminalized an incomplete abandoned set"
    );
    assert!(recovered
        .iter()
        .all(|attempt| attempt.status == AttemptState::UnknownOutcome));

    let batch = store
        .list_events_after(
            &deployment.scope,
            &id("run"),
            EventPageRequest {
                after_event_seq: watermark,
                page_size: 1_000,
                hard_response_byte_limit: 4_000_000,
            },
        )
        .await
        .unwrap()
        .into_iter()
        .map(|event| {
            (
                event.event_type,
                event
                    .node_instance_id
                    .map(|node| node.as_str().to_owned())
                    .unwrap_or_default(),
            )
        })
        .collect();
    let mut nodes = Vec::new();
    for node in ["alfa", "root", "zulu"] {
        nodes.push((
            node.to_owned(),
            store
                .get_node(&deployment.scope, &id("run"), &id(node))
                .await
                .unwrap()
                .status,
        ));
    }
    TakeoverOutcome {
        batch,
        nodes,
        run: store
            .get_run(&deployment.scope, &id("run"))
            .await
            .unwrap()
            .run
            .status,
    }
}

/// Fixtures 7 and 8. Taking over a scope with several abandoned attempts terminalizes the whole
/// set, and the single primary exhaustion is selected by persisted topological rank rather than
/// by the order the rows happen to arrive in or sort by. The four permutations differ only in
/// row order and attempt-ID collation, so an identical outcome across them is the order
/// independence the plan requires. Contract sections 5.3 and 12.1.
#[tokio::test]
async fn multi_attempt_takeover_is_decided_by_rank_and_not_by_row_order() {
    let mut outcomes = Vec::new();
    for claim_order in [["alfa", "zulu"], ["zulu", "alfa"]] {
        for attempt_ids in [
            ["aaa-attempt", "zzz-attempt"],
            ["zzz-attempt", "aaa-attempt"],
        ] {
            outcomes.push(takeover(claim_order, attempt_ids).await);
        }
    }
    let first = &outcomes[0];
    assert_eq!(
        first.nodes,
        vec![
            ("alfa".to_owned(), NodeState::RetriesExhausted),
            ("root".to_owned(), NodeState::Succeeded),
            ("zulu".to_owned(), NodeState::Cancelled),
        ],
        "the primary exhaustion was not the lowest-ranked abandoned node"
    );
    assert_eq!(first.run, RunState::RetriesExhausted);
    // The whole abandoned set is terminalized, and the run terminalizes exactly once.
    assert_eq!(
        first
            .batch
            .iter()
            .filter(|(kind, _)| *kind == EventType::NodeRetriesExhausted)
            .count(),
        1
    );
    assert_eq!(
        first
            .batch
            .iter()
            .filter(|(kind, _)| *kind == EventType::AttemptOutcomeUnknown)
            .count(),
        2
    );
    for other in &outcomes[1..] {
        assert_eq!(
            other, first,
            "row order or attempt-ID collation changed the bulk-recovery outcome or its batch"
        );
    }
}
