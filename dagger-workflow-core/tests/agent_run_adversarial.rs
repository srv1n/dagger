#![cfg(feature = "sqlite")]

//! Adversarial host-action probes: concurrent scheduler calls, cooperative cancellation,
//! stale-result fencing, and an external-effect retry after a forced store disconnect.

use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, CompatibilityReport, InMemoryActionRegistry,
    WorkflowAction,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::{ObjectStore, VerifiedObjectRef};
use dagger_workflow_core::definition::{
    ActionReference, BackoffPolicy, Binding, BindingSource, NodeDefinition, PublishableDefinition,
    RetryPolicy, TimeoutPolicy, WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::fs_object_store::FsObjectStore;
use dagger_workflow_core::ids::{CostUnits, Digest, Id, Timestamp, TopologicalRank, Version};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::run::{AttemptState, NodeState, RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::sqlite::SqliteWorkflowStore;
use dagger_workflow_core::store::{
    ClaimNodeAttempt, ClaimNodeAttemptResult, CompleteAttempt, CompleteAttemptResult,
    CompletionObjects, CreateDefinition, CreateRun, ExpectedGateVersion, PublishRevision,
    RecoverAbandonedAttemptsForRun, ReleaseRetry, ResolvedActionSchemas, StartRun, WorkflowStore,
};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Condvar, Mutex,
};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const SCHEMA: &[u8] = br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#;
const CLAIM_LIFETIME_MS: i64 = 20_000;

type SqlStore = SqliteWorkflowStore<TestClock, FsObjectStore<TestClock>>;

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}

fn digest(bytes: &[u8]) -> Digest {
    Digest::new(format!("sha256:{:x}", Sha256::digest(bytes))).unwrap()
}

fn scope(name: &str) -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new("agent-adversarial").unwrap(),
        namespace: ScopeAtom::new(name).unwrap(),
    }
}

fn descriptor(name: &str, schema: &Digest) -> ActionDescriptor {
    ActionDescriptor {
        name: name.to_owned(),
        contract_version: "agent-run-v1".to_owned(),
        input_schema_digest: schema.clone(),
        output_schema_digest: schema.clone(),
        implementation_compatibility_digest: digest(format!("agent-run:{name}").as_bytes()),
    }
}

fn definition(
    definition_id: &str,
    action_name: &str,
    schema: &Digest,
    attempts: u32,
) -> WorkflowDefinition {
    WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: id(definition_id),
        name: definition_id.to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.clone(),
        run_output_schema_digest: schema.clone(),
        entry_node_id: id("work"),
        nodes: vec![
            NodeDefinition::Action {
                id: id("work"),
                action: ActionReference {
                    name: action_name.to_owned(),
                    contract_version: "agent-run-v1".to_owned(),
                    input_schema_digest: schema.clone(),
                    output_schema_digest: schema.clone(),
                    compatible_implementation_requirement: digest(
                        format!("agent-run:{action_name}").as_bytes(),
                    ),
                },
                bindings: vec![Binding {
                    target: "/value".to_owned(),
                    source: BindingSource::RunInput {
                        pointer: "/value".to_owned(),
                    },
                }],
                retry: RetryPolicy {
                    max_attempts: attempts,
                    backoff: BackoffPolicy::Fixed { delay_ms: 0 },
                },
                timeout: TimeoutPolicy {
                    timeout_ms: 120_000,
                },
                declared_max_cost_units: CostUnits(1),
                next: vec![id("done")],
            },
            NodeDefinition::Succeed {
                id: id("done"),
                output: BindingSource::NodeOutput {
                    node_id: id("work"),
                    pointer: String::new(),
                },
            },
        ],
    }
}

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
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => thread::park(),
        }
    }
}

struct YieldOnce(bool);
impl Future for YieldOnce {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            context.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

#[derive(Default)]
struct Probe {
    started: AtomicBool,
    starts: AtomicUsize,
    stopped: Mutex<Option<Instant>>,
    gate: (Mutex<bool>, Condvar),
}

impl Probe {
    fn wait_started(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.started.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "action did not start within five seconds"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

struct SlowAction {
    descriptor: ActionDescriptor,
    probe: Arc<Probe>,
    duration: Duration,
    hold_first: bool,
}
impl WorkflowAction for SlowAction {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }
    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        _: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        self.probe.starts.fetch_add(1, Ordering::AcqRel);
        self.probe.started.store(true, Ordering::Release);
        let probe = self.probe.clone();
        let duration = self.duration;
        let hold = self.hold_first && context.attempt_number == 1;
        Box::pin(async move {
            if hold {
                let (lock, wake) = &probe.gate;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
            } else {
                let deadline = Instant::now() + duration;
                while Instant::now() < deadline && !context.cancellation_token.is_cancelled() {
                    thread::sleep(Duration::from_millis(20));
                    YieldOnce(false).await;
                }
            }
            *probe.stopped.lock().unwrap() = Some(Instant::now());
            ActionOutcome::success(json!({"value": 1}), Vec::new(), CostUnits(1), None).unwrap()
        })
    }
}

struct FastAction {
    descriptor: ActionDescriptor,
}
impl WorkflowAction for FastAction {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }
    fn invoke<'a>(
        &'a self,
        _: ActionContext,
        _: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async {
            ActionOutcome::success(json!({"value": 1}), Vec::new(), CostUnits(1), None).unwrap()
        })
    }
}

async fn publish<S: WorkflowStore, O: ObjectStore>(
    store: &S,
    objects: &O,
    execution_scope: &ExecutionScope,
    principal: &AuthenticatedPrincipal,
    workflow: WorkflowDefinition,
) -> Digest {
    let schema = objects
        .put(execution_scope, SCHEMA, "application/json")
        .await
        .unwrap();
    let canonical = objects
        .put(
            execution_scope,
            &serde_jcs::to_vec(&workflow).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    store
        .create_definition(
            execution_scope,
            CreateDefinition {
                definition_id: workflow.definition_id.clone(),
                display_name: workflow.name.clone(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let action_locations = workflow
        .nodes
        .iter()
        .filter_map(|node| match node {
            NodeDefinition::Action { id, .. } => Some(id.as_str().to_owned()),
            _ => None,
        })
        .map(|location| {
            (
                location,
                ResolvedActionSchemas {
                    input_schema: schema.clone(),
                    output_schema: schema.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let ranks = workflow
        .nodes
        .iter()
        .enumerate()
        .map(|(rank, node)| {
            let node_id = match node {
                NodeDefinition::Action { id, .. }
                | NodeDefinition::Map { id, .. }
                | NodeDefinition::Choice { id, .. }
                | NodeDefinition::Approval { id, .. }
                | NodeDefinition::Succeed { id, .. }
                | NodeDefinition::Fail { id, .. } => id.clone(),
            };
            (node_id, TopologicalRank(rank as u32))
        })
        .collect();
    store
        .publish_revision(
            execution_scope,
            PublishRevision {
                definition_id: workflow.definition_id.clone(),
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema,
                resolved_action_schema_objects: action_locations,
                parsed_revision: PublishableDefinition {
                    definition: workflow,
                    topological_ranks: ranks,
                },
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    canonical.digest().clone()
}

async fn create_run<S: WorkflowStore, O: ObjectStore>(
    store: &S,
    objects: &O,
    execution_scope: &ExecutionScope,
    principal: &AuthenticatedPrincipal,
    definition_id: &str,
    revision: &Digest,
    run_id: &str,
) {
    let input = objects
        .put(execution_scope, br#"{"value":1}"#, "application/json")
        .await
        .unwrap();
    store
        .create_run(
            execution_scope,
            CreateRun {
                run_id: id(run_id),
                definition_id: id(definition_id),
                revision_hash: revision.clone(),
                input,
                budget_limit: CostUnits(10),
                limits: RunLimits {
                    max_dynamic_node_instances: 10,
                    max_total_attempts: 10,
                    max_total_events: 1_000,
                    max_inline_json_bytes_per_value: 10_000,
                    max_artifacts_per_attempt: 10,
                    max_aggregate_object_bytes_per_run: 100_000,
                    max_run_lifetime_ms: 1_000_000,
                },
                principal: principal.clone(),
                idempotency_token: format!("agent-adversarial-create-{run_id}"),
            },
        )
        .await
        .unwrap();
}

fn principal(execution_scope: &ExecutionScope, schema: &Digest) -> AuthenticatedPrincipal {
    AuthenticatedPrincipal::mint(
        execution_scope.clone(),
        "agent-adversarial".to_owned(),
        Vec::new(),
        schema.clone(),
    )
    .unwrap()
}

async fn start<S, O>(
    engine: &WorkflowEngine<S, O, InMemoryActionRegistry>,
    execution_scope: &ExecutionScope,
    run_id: &str,
) where
    S: WorkflowStore,
    O: ObjectStore,
{
    engine.start(execution_scope, &id(run_id)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_progress_and_cancellation_latency() {
    let execution_scope = scope("live");
    let clock = Arc::new(TestClock::new(Timestamp(1_000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(InMemoryObjectStore::new(clock));
    let schema = digest(SCHEMA);
    let owner = principal(&execution_scope, &schema);
    let slow_probe = Arc::new(Probe::default());
    let cancel_probe = Arc::new(Probe::default());
    let registry = Arc::new(InMemoryActionRegistry::new());
    registry
        .register(Arc::new(SlowAction {
            descriptor: descriptor("slow", &schema),
            probe: slow_probe.clone(),
            duration: Duration::from_secs(30),
            hold_first: false,
        }))
        .unwrap();
    registry
        .register(Arc::new(SlowAction {
            descriptor: descriptor("cancel", &schema),
            probe: cancel_probe.clone(),
            duration: Duration::from_secs(30),
            hold_first: false,
        }))
        .unwrap();
    registry
        .register(Arc::new(FastAction {
            descriptor: descriptor("fast", &schema),
        }))
        .unwrap();
    let slow_revision = publish(
        &*store,
        &*objects,
        &execution_scope,
        &owner,
        definition("slow-def", "slow", &schema, 1),
    )
    .await;
    let fast_revision = publish(
        &*store,
        &*objects,
        &execution_scope,
        &owner,
        definition("fast-def", "fast", &schema, 1),
    )
    .await;
    let cancel_revision = publish(
        &*store,
        &*objects,
        &execution_scope,
        &owner,
        definition("cancel-def", "cancel", &schema, 3),
    )
    .await;
    create_run(
        &*store,
        &*objects,
        &execution_scope,
        &owner,
        "slow-def",
        &slow_revision,
        "run-a",
    )
    .await;
    for run in ["run-b", "run-c", "run-d", "run-e", "run-f"] {
        create_run(
            &*store,
            &*objects,
            &execution_scope,
            &owner,
            "fast-def",
            &fast_revision,
            run,
        )
        .await;
    }
    create_run(
        &*store,
        &*objects,
        &execution_scope,
        &owner,
        "cancel-def",
        &cancel_revision,
        "run-cancel",
    )
    .await;
    let engine = Arc::new(
        WorkflowEngine::new(
            store.clone(),
            objects.clone(),
            registry,
            EngineConfig {
                instance_id: id("agent-adversarial-engine"),
                max_concurrency: 1,
            },
        )
        .unwrap(),
    );
    engine.acquire_scope(&execution_scope).await.unwrap();
    start(&engine, &execution_scope, "run-a").await;
    let tick_engine = engine.clone();
    let tick_scope = execution_scope.clone();
    let a_tick = thread::spawn(move || block_on(tick_engine.tick(&tick_scope)));
    slow_probe.wait_started();
    for run in ["run-b", "run-c", "run-d", "run-e", "run-f"] {
        start(&engine, &execution_scope, run).await;
    }
    let loaded_start = Instant::now();
    for run in ["run-b", "run-c", "run-d", "run-e", "run-f"] {
        engine.run_until_idle(&execution_scope, 8).await.unwrap();
        assert_eq!(
            store
                .get_run(&execution_scope, &id(run))
                .await
                .unwrap()
                .run
                .status,
            RunState::Succeeded
        );
    }
    let loaded_latency = loaded_start.elapsed();
    assert_eq!(
        store
            .get_run(&execution_scope, &id("run-a"))
            .await
            .unwrap()
            .run
            .status,
        RunState::Running,
        "fast runs only completed after long action ended"
    );
    for run in [
        "baseline-b",
        "baseline-c",
        "baseline-d",
        "baseline-e",
        "baseline-f",
    ] {
        create_run(
            &*store,
            &*objects,
            &execution_scope,
            &owner,
            "fast-def",
            &fast_revision,
            run,
        )
        .await;
        start(&engine, &execution_scope, run).await;
    }
    let baseline_start = Instant::now();
    for _ in 0..5 {
        engine.run_until_idle(&execution_scope, 8).await.unwrap();
    }
    let baseline_latency = baseline_start.elapsed();
    eprintln!(
        "CONCURRENT_PROGRESS loaded_fast_runs_ms={} baseline_fast_runs_ms={}",
        loaded_latency.as_millis(),
        baseline_latency.as_millis()
    );
    // Do not wait the full 30 seconds merely to prove this suite's next probe.
    // The action is still live here; cancellation is its completion path.
    let a_before_cancel = Instant::now();
    let live = store
        .get_run(&execution_scope, &id("run-a"))
        .await
        .unwrap()
        .run;
    engine
        .cancel(
            &execution_scope,
            dagger_workflow_core::store::CancelRun {
                run_id: id("run-a"),
                expected_run_version: live.version,
                expected_pending_gate_versions: Vec::<ExpectedGateVersion>::new(),
                principal: owner.clone(),
                reason_code: "test-cleanup".to_owned(),
                idempotency_token: "agent-adversarial-cancel-a".to_owned(),
            },
        )
        .await
        .unwrap();
    a_tick.join().unwrap().unwrap();
    assert!(a_before_cancel.elapsed() < Duration::from_secs(2));
    start(&engine, &execution_scope, "run-cancel").await;
    let cancel_engine = engine.clone();
    let cancel_scope = execution_scope.clone();
    let cancel_tick = thread::spawn(move || block_on(cancel_engine.tick(&cancel_scope)));
    cancel_probe.wait_started();
    let cancellation_started = Instant::now();
    let live = store
        .get_run(&execution_scope, &id("run-cancel"))
        .await
        .unwrap()
        .run;
    engine
        .cancel(
            &execution_scope,
            dagger_workflow_core::store::CancelRun {
                run_id: id("run-cancel"),
                expected_run_version: live.version,
                expected_pending_gate_versions: Vec::new(),
                principal: owner,
                reason_code: "operator-cancel".to_owned(),
                idempotency_token: "agent-adversarial-cancel-run".to_owned(),
            },
        )
        .await
        .unwrap();
    cancel_tick.join().unwrap().unwrap();
    let stop_latency = cancel_probe
        .stopped
        .lock()
        .unwrap()
        .unwrap()
        .duration_since(cancellation_started);
    let cancelled = store
        .get_run(&execution_scope, &id("run-cancel"))
        .await
        .unwrap()
        .run;
    let node = store
        .get_node(&execution_scope, &id("run-cancel"), &id("work"))
        .await
        .unwrap();
    assert_eq!(cancelled.status, RunState::Cancelled);
    assert_eq!(
        cancel_probe.starts.load(Ordering::Acquire),
        1,
        "cancellation spawned another action attempt"
    );
    assert_eq!(
        node.attempt_count, 1,
        "cancellation allowed another attempt"
    );
    assert!(
        stop_latency < Duration::from_secs(1),
        "cooperative cancellation took {stop_latency:?}"
    );
    eprintln!(
        "CANCELLATION_LATENCY stop_ms={} attempts={}",
        stop_latency.as_millis(),
        node.attempt_count
    );
}

#[tokio::test]
async fn stale_completion_is_fenced_after_takeover() {
    let execution_scope = scope("fence");
    let clock = Arc::new(TestClock::new(Timestamp(10_000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let schema = digest(SCHEMA);
    let owner = principal(&execution_scope, &schema);
    let revision = publish(
        &*store,
        &*objects,
        &execution_scope,
        &owner,
        definition("fence-def", "unused", &schema, 2),
    )
    .await;
    create_run(
        &*store,
        &*objects,
        &execution_scope,
        &owner,
        "fence-def",
        &revision,
        "run-fence",
    )
    .await;
    let first = store
        .acquire_engine_claim(&execution_scope, id("fence-one"))
        .await
        .unwrap();
    store
        .start_run(
            &execution_scope,
            StartRun {
                permit: first.permit.clone(),
                run_id: id("run-fence"),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: schema.clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .unwrap();
    let input = objects
        .put(&execution_scope, br#"{"value":1}"#, "application/json")
        .await
        .unwrap();
    let ready = store
        .get_node(&execution_scope, &id("run-fence"), &id("work"))
        .await
        .unwrap();
    let stale = match store
        .claim_node_attempt(
            &execution_scope,
            ClaimNodeAttempt {
                permit: first.permit,
                run_id: id("run-fence"),
                node_id: id("work"),
                expected_node_version: ready.version,
                attempt_id: id("attempt-one"),
                worker_id: id("fence-one"),
                bound_input: input.clone(),
                binding_derivation_digest: schema.clone(),
            },
        )
        .await
        .unwrap()
    {
        ClaimNodeAttemptResult::Claimed {
            completion_credential,
            ..
        } => completion_credential,
        _ => panic!("first attempt was not claimed"),
    };
    clock.advance_ms(CLAIM_LIFETIME_MS + 1).unwrap();
    let second = store
        .acquire_engine_claim(&execution_scope, id("fence-two"))
        .await
        .unwrap();
    store
        .recover_abandoned_attempts_for_run(
            &execution_scope,
            RecoverAbandonedAttemptsForRun {
                permit: second.permit.clone(),
                run_id: id("run-fence"),
            },
        )
        .await
        .unwrap();
    let waiting = store
        .get_node(&execution_scope, &id("run-fence"), &id("work"))
        .await
        .unwrap();
    assert_eq!(waiting.status, NodeState::RetryWaiting);
    store
        .release_retry(
            &execution_scope,
            ReleaseRetry {
                permit: second.permit.clone(),
                run_id: id("run-fence"),
                node_id: id("work"),
                expected_node_version: waiting.version,
            },
        )
        .await
        .unwrap();
    let ready = store
        .get_node(&execution_scope, &id("run-fence"), &id("work"))
        .await
        .unwrap();
    let live = match store
        .claim_node_attempt(
            &execution_scope,
            ClaimNodeAttempt {
                permit: second.permit,
                run_id: id("run-fence"),
                node_id: id("work"),
                expected_node_version: ready.version,
                attempt_id: id("attempt-two"),
                worker_id: id("fence-two"),
                bound_input: input.clone(),
                binding_derivation_digest: schema,
            },
        )
        .await
        .unwrap()
    {
        ClaimNodeAttemptResult::Claimed {
            completion_credential,
            ..
        } => completion_credential,
        _ => panic!("second attempt was not claimed"),
    };
    assert_ne!(stale.digest(), live.digest());
    let result = store
        .complete_attempt(
            &execution_scope,
            CompleteAttempt {
                completion_credential: stale,
                run_id: id("run-fence"),
                node_id: id("work"),
                attempt_id: id("attempt-one"),
                submitted_outcome: ActionOutcome::success(
                    json!({"value":1}),
                    Vec::new(),
                    CostUnits(1),
                    None,
                )
                .unwrap(),
                objects: CompletionObjects {
                    output: Some(input),
                    artifacts: Vec::new(),
                    diagnostics: None,
                },
            },
        )
        .await
        .unwrap();
    assert!(matches!(result, CompleteAttemptResult::StaleRecorded(_)));
    let node = store
        .get_node(&execution_scope, &id("run-fence"), &id("work"))
        .await
        .unwrap();
    assert_eq!(node.active_attempt_id, Some(id("attempt-two")));
    assert_eq!(
        store
            .get_attempt(&execution_scope, &id("run-fence"), &id("attempt-one"))
            .await
            .unwrap()
            .status,
        AttemptState::UnknownOutcome
    );
    eprintln!("STALE_COMPLETION_FENCE result=StaleRecorded active_attempt=attempt-two");
}

#[derive(Default)]
struct MarkerLog {
    entries: Mutex<Vec<(String, String)>>,
    first_started: AtomicBool,
    gate: (Mutex<bool>, Condvar),
}

struct MarkerAction {
    descriptor: ActionDescriptor,
    marker_dir: PathBuf,
    log: Arc<MarkerLog>,
}
impl WorkflowAction for MarkerAction {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }
    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        _: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        let marker = self.marker_dir.join(&context.idempotency_key);
        let key = context.idempotency_key.clone();
        let attempt = context.attempt_id.as_str().to_owned();
        let log = self.log.clone();
        Box::pin(async move {
            std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
            if let Err(error) = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&marker)
            {
                assert_eq!(
                    error.kind(),
                    ErrorKind::AlreadyExists,
                    "marker write failed: {error}"
                );
            }
            log.entries.lock().unwrap().push((key, attempt));
            if context.attempt_number == 1 {
                log.first_started.store(true, Ordering::Release);
                let (lock, wake) = &log.gate;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
            }
            ActionOutcome::success(json!({"value":1}), Vec::new(), CostUnits(1), None).unwrap()
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_retry_reuses_the_external_idempotency_marker() {
    let directory = TempDir::new().unwrap();
    let execution_scope = scope("restart");
    let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
    let database = directory.path().join("workflow.sqlite");
    let object_root = directory.path().join("objects");
    let marker_dir = directory.path().join("markers");
    let objects = Arc::new(FsObjectStore::open(&object_root, clock.clone()).unwrap());
    let store = Arc::new(
        SqlStore::open(&database, clock.clone(), objects.clone())
            .await
            .unwrap(),
    );
    let schema = digest(SCHEMA);
    let owner = principal(&execution_scope, &schema);
    let log = Arc::new(MarkerLog::default());
    let registry = Arc::new(InMemoryActionRegistry::new());
    registry
        .register(Arc::new(MarkerAction {
            descriptor: descriptor("marker", &schema),
            marker_dir: marker_dir.clone(),
            log: log.clone(),
        }))
        .unwrap();
    let revision = publish(
        &*store,
        &*objects,
        &execution_scope,
        &owner,
        definition("restart-def", "marker", &schema, 2),
    )
    .await;
    create_run(
        &*store,
        &*objects,
        &execution_scope,
        &owner,
        "restart-def",
        &revision,
        "run-restart",
    )
    .await;
    let first = Arc::new(
        WorkflowEngine::new(
            store.clone(),
            objects.clone(),
            registry.clone(),
            EngineConfig {
                instance_id: id("restart-one"),
                max_concurrency: 1,
            },
        )
        .unwrap(),
    );
    first.acquire_scope(&execution_scope).await.unwrap();
    start(&first, &execution_scope, "run-restart").await;
    let first_tick = {
        let engine = first.clone();
        let execution_scope = execution_scope.clone();
        thread::spawn(move || block_on(engine.tick(&execution_scope)))
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !log.first_started.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "marker action did not start");
        thread::sleep(Duration::from_millis(5));
    }
    store
        .advance_database_clock_ms(CLAIM_LIFETIME_MS + 1)
        .await
        .unwrap();
    store.pool().close().await;
    {
        let (lock, wake) = &log.gate;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
    assert!(
        first_tick.join().unwrap().is_err(),
        "closed durable store still accepted an in-flight completion"
    );
    drop(first);
    drop(store);
    drop(objects);
    let objects = Arc::new(FsObjectStore::open(&object_root, clock.clone()).unwrap());
    let store = Arc::new(
        SqlStore::open(&database, clock, objects.clone())
            .await
            .unwrap(),
    );
    let second = WorkflowEngine::new(
        store.clone(),
        objects.clone(),
        registry,
        EngineConfig {
            instance_id: id("restart-two"),
            max_concurrency: 1,
        },
    )
    .unwrap();
    second.acquire_scope(&execution_scope).await.unwrap();
    second.run_until_idle(&execution_scope, 16).await.unwrap();
    assert_eq!(
        store
            .get_run(&execution_scope, &id("run-restart"))
            .await
            .unwrap()
            .run
            .status,
        RunState::Succeeded
    );
    let entries = log.entries.lock().unwrap().clone();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, entries[1].0);
    assert_ne!(entries[0].1, entries[1].1);
    assert_eq!(
        std::fs::read_dir(&marker_dir).unwrap().count(),
        1,
        "retry created a second external marker"
    );
    eprintln!(
        "RESTART_REATTACH attempts={} key={} first_attempt={} retry_attempt={} markers=1",
        entries.len(),
        entries[0].0,
        entries[0].1,
        entries[1].1
    );
}
