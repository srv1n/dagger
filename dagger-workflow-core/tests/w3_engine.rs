use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, ActionRegistry, CompatibilityReport,
    InMemoryActionRegistry, WorkflowAction,
};
use dagger_workflow_core::approval::{
    canonical_human_approval_result, ApprovalDecision, ApprovalExpiryPolicy,
    AuthenticatedPrincipal, DecisionAuthorizationPolicy,
};
use dagger_workflow_core::artifact::{
    ObjectReadError, ObjectStore, ObjectStoreError, VerifiedObject, VerifiedObjectRef,
};
use dagger_workflow_core::definition::{
    ActionReference, ApprovalGateConfig, ArtifactLocator, BackoffPolicy, Binding, BindingSource,
    MapBinding, MapBindingSource, NodeDefinition, PublishableDefinition, RetryPolicy,
    TimeoutPolicy, WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::ids::{CostUnits, Digest, Id, Timestamp, TopologicalRank};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::run::{AttemptState, RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::store::{
    ClaimNodeAttempt, ClaimNodeAttemptResult, CreateDefinition, CreateRun, DecideApproval,
    EventPageRequest, PageRequest, PublishRevision, RequestApproval, ResolvedActionSchemas,
    StartRun, SuspendIncompatible, TimeoutAttempt, WorkflowStore,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Condvar, Mutex,
};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use std::time::Duration;

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

async fn prepare_definition(
    store: &InMemoryStore<TestClock>,
    objects: &InMemoryObjectStore<TestClock>,
    execution_scope: &ExecutionScope,
    definition: WorkflowDefinition,
    run_id: &str,
    max_run_lifetime_ms: u64,
) {
    let schema = objects
        .put(execution_scope, b"{}", "application/json")
        .await
        .unwrap();
    let canonical = objects
        .put(
            execution_scope,
            &serde_jcs::to_vec(&definition).unwrap(),
            "application/json",
        )
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
            execution_scope,
            CreateDefinition {
                definition_id: definition.definition_id.clone(),
                display_name: definition.name.clone(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let ranks = definition
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let node_id = match node {
                NodeDefinition::Action { id, .. }
                | NodeDefinition::Map { id, .. }
                | NodeDefinition::Choice { id, .. }
                | NodeDefinition::Approval { id, .. }
                | NodeDefinition::Succeed { id, .. }
                | NodeDefinition::Fail { id, .. } => id.clone(),
            };
            (node_id, TopologicalRank(index as u32))
        })
        .collect();
    let resolved_action_schema_objects = definition
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
            NodeDefinition::Map { id, .. } => Some((
                format!("{}/map_action", id.as_str()),
                ResolvedActionSchemas {
                    input_schema: schema.clone(),
                    output_schema: schema.clone(),
                },
            )),
            _ => None,
        })
        .collect();
    store
        .publish_revision(
            execution_scope,
            PublishRevision {
                definition_id: definition.definition_id.clone(),
                expected_definition_version: dagger_workflow_core::ids::Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema,
                resolved_action_schema_objects,
                parsed_revision: PublishableDefinition {
                    definition: definition.clone(),
                    topological_ranks: ranks,
                },
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let input = objects
        .put(
            execution_scope,
            br#"{"approval":"release"}"#,
            "application/json",
        )
        .await
        .unwrap();
    store
        .create_run(
            execution_scope,
            CreateRun {
                run_id: id(run_id),
                definition_id: definition.definition_id,
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
                    max_run_lifetime_ms,
                },
                principal,
                idempotency_token: format!("{run_id}-create-token"),
            },
        )
        .await
        .unwrap();
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

struct SerialProbe {
    descriptor: ActionDescriptor,
    active: AtomicUsize,
    max_active: AtomicUsize,
    calls: AtomicUsize,
}

impl WorkflowAction for SerialProbe {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    fn invoke<'a>(
        &'a self,
        _context: ActionContext,
        canonical_bound_input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::AcqRel);
            let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
            self.max_active.fetch_max(active, Ordering::AcqRel);
            thread::sleep(Duration::from_millis(10));
            self.active.fetch_sub(1, Ordering::AcqRel);
            let value: Value = serde_json::from_slice(canonical_bound_input).unwrap();
            ActionOutcome::success(value, Vec::new(), CostUnits(1), None).unwrap()
        })
    }
}

struct BlockingExpiryStore {
    inner: Arc<InMemoryObjectStore<TestClock>>,
    armed: AtomicBool,
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl BlockingExpiryStore {
    fn new(inner: Arc<InMemoryObjectStore<TestClock>>) -> Self {
        Self {
            inner,
            armed: AtomicBool::new(false),
            state: Mutex::new((false, false)),
            changed: Condvar::new(),
        }
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }

    fn wait_until_blocked(&self) {
        let state = self.state.lock().unwrap();
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(2), |state| !state.0)
            .unwrap();
        assert!(
            !timeout.timed_out() && state.0,
            "expiry put was not reached"
        );
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        state.1 = true;
        self.changed.notify_all();
    }
}

impl ObjectStore for BlockingExpiryStore {
    async fn put(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        if self.armed.swap(false, Ordering::AcqRel)
            && bytes == dagger_workflow_core::approval::canonical_expiry_approval_result()
        {
            let mut state = self.state.lock().unwrap();
            state.0 = true;
            self.changed.notify_all();
            while !state.1 {
                state = self.changed.wait(state).unwrap();
            }
        }
        self.inner.put(scope, bytes, media_type).await
    }

    async fn get(
        &self,
        scope: &ExecutionScope,
        digest: &Digest,
    ) -> Result<VerifiedObject, ObjectReadError> {
        self.inner.get(scope, digest).await
    }

    async fn publish_if_absent(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        self.inner.publish_if_absent(scope, bytes, media_type).await
    }
}

struct CorruptingOutputStore {
    inner: Arc<InMemoryObjectStore<TestClock>>,
    target: Vec<u8>,
    matching_puts: AtomicUsize,
}

impl ObjectStore for CorruptingOutputStore {
    async fn put(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        let reference = self.inner.put(scope, bytes, media_type).await?;
        if bytes == self.target && self.matching_puts.fetch_add(1, Ordering::AcqRel) == 1 {
            assert!(self
                .inner
                .corrupt_bytes(scope, reference.digest(), b"corrupt".to_vec()));
        }
        Ok(reference)
    }

    async fn get(
        &self,
        scope: &ExecutionScope,
        digest: &Digest,
    ) -> Result<VerifiedObject, ObjectReadError> {
        self.inner.get(scope, digest).await
    }

    async fn publish_if_absent(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        self.inner.publish_if_absent(scope, bytes, media_type).await
    }
}

fn map_workflow(
    definition_id: &str,
    descriptor: &ActionDescriptor,
    items: Value,
    max_items: u32,
    max_concurrency: u32,
    bindings: Vec<MapBinding>,
) -> WorkflowDefinition {
    WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: id(definition_id),
        name: definition_id.to_owned(),
        description: String::new(),
        run_input_schema_digest: hash(b"{}"),
        run_output_schema_digest: hash(b"{}"),
        entry_node_id: id("map"),
        nodes: vec![
            NodeDefinition::Map {
                id: id("map"),
                items: BindingSource::Constant { value: items },
                max_items,
                max_concurrency,
                action: ActionReference {
                    name: descriptor.name.clone(),
                    contract_version: descriptor.contract_version.clone(),
                    input_schema_digest: descriptor.input_schema_digest.clone(),
                    output_schema_digest: descriptor.output_schema_digest.clone(),
                    compatible_implementation_requirement: descriptor
                        .implementation_compatibility_digest
                        .clone(),
                },
                bindings,
                retry: RetryPolicy {
                    max_attempts: 1,
                    backoff: BackoffPolicy::Fixed { delay_ms: 0 },
                },
                timeout: TimeoutPolicy { timeout_ms: 1_000 },
                declared_max_cost_units: CostUnits(1),
                next: vec![id("succeed")],
            },
            NodeDefinition::Succeed {
                id: id("succeed"),
                output: BindingSource::NodeOutput {
                    node_id: id("map"),
                    pointer: String::new(),
                },
            },
        ],
    }
}

async fn prepare_scoped_run(
    store: &InMemoryStore<TestClock>,
    objects: &InMemoryObjectStore<TestClock>,
    execution_scope: &ExecutionScope,
    descriptor: &ActionDescriptor,
) {
    let schema = objects
        .put(execution_scope, b"{}", "application/json")
        .await
        .unwrap();
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: id("shared-definition"),
        name: "scoped".to_owned(),
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
                    max_attempts: 1,
                    backoff: BackoffPolicy::Fixed { delay_ms: 0 },
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
    let canonical = objects
        .put(
            execution_scope,
            &serde_jcs::to_vec(&definition).unwrap(),
            "application/json",
        )
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
            execution_scope,
            CreateDefinition {
                definition_id: id("shared-definition"),
                display_name: "scoped".to_owned(),
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
            execution_scope,
            PublishRevision {
                definition_id: id("shared-definition"),
                expected_definition_version: dagger_workflow_core::ids::Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema,
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
            execution_scope,
            br#"{"scope":"isolated"}"#,
            "application/json",
        )
        .await
        .unwrap();
    store
        .create_run(
            execution_scope,
            CreateRun {
                run_id: id("shared-run"),
                definition_id: id("shared-definition"),
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
                    max_run_lifetime_ms: 100_000,
                },
                principal,
                idempotency_token: "shared-create-token".to_owned(),
            },
        )
        .await
        .unwrap();
}

#[test]
fn runtime_scopes_have_disjoint_attempts_events_and_budgets() {
    block_on(async {
        let scope_a = ExecutionScope {
            tenant_id: dagger_workflow_core::scope::ScopeAtom::new("tenant-a").unwrap(),
            namespace: dagger_workflow_core::scope::ScopeAtom::new("runtime").unwrap(),
        };
        let scope_b = ExecutionScope {
            tenant_id: dagger_workflow_core::scope::ScopeAtom::new("tenant-b").unwrap(),
            namespace: dagger_workflow_core::scope::ScopeAtom::new("runtime").unwrap(),
        };
        let clock = Arc::new(TestClock::new(Timestamp(1_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        let schema_digest = hash(b"{}");
        let descriptor = ActionDescriptor {
            name: "fixture.scoped".to_owned(),
            contract_version: "1".to_owned(),
            input_schema_digest: schema_digest.clone(),
            output_schema_digest: schema_digest,
            implementation_compatibility_digest: hash(b"scoped-implementation"),
        };
        let registry = Arc::new(InMemoryActionRegistry::new());
        registry
            .register(Arc::new(RetryOnce {
                descriptor: descriptor.clone(),
                fail: AtomicBool::new(false),
            }))
            .unwrap();
        prepare_scoped_run(&store, &objects, &scope_a, &descriptor).await;
        prepare_scoped_run(&store, &objects, &scope_b, &descriptor).await;
        let first_page = store
            .list_nodes(
                &scope_a,
                &id("shared-run"),
                PageRequest {
                    cursor: None,
                    page_size: 1,
                },
            )
            .await
            .unwrap();
        let cursor = first_page.next_cursor.clone().unwrap();
        let second_page = store
            .list_nodes(
                &scope_a,
                &id("shared-run"),
                PageRequest {
                    cursor: Some(cursor.clone()),
                    page_size: 1,
                },
            )
            .await
            .unwrap();
        assert_ne!(
            first_page.items[0].node_instance_id,
            second_page.items[0].node_instance_id
        );
        assert!(store
            .list_nodes(
                &scope_b,
                &id("shared-run"),
                PageRequest {
                    cursor: Some(cursor),
                    page_size: 1,
                },
            )
            .await
            .is_err());
        let engine = WorkflowEngine::new(
            store.clone(),
            objects,
            registry,
            EngineConfig {
                instance_id: id("scoped-engine"),
                max_concurrency: 1,
            },
        )
        .unwrap();
        engine.acquire_scope(&scope_a).await.unwrap();
        engine.acquire_scope(&scope_b).await.unwrap();
        engine.start(&scope_a, &id("shared-run")).await.unwrap();
        engine.start(&scope_b, &id("shared-run")).await.unwrap();
        engine.run_until_idle(&scope_a, 8).await.unwrap();
        engine.run_until_idle(&scope_b, 8).await.unwrap();
        let events_a = store
            .list_events_after(
                &scope_a,
                &id("shared-run"),
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1_000,
                    hard_response_byte_limit: 1_000_000,
                },
            )
            .await
            .unwrap();
        let events_b = store
            .list_events_after(
                &scope_b,
                &id("shared-run"),
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1_000,
                    hard_response_byte_limit: 1_000_000,
                },
            )
            .await
            .unwrap();
        let attempt_a = events_a
            .iter()
            .find_map(|event| event.attempt_id.clone())
            .unwrap();
        let attempt_b = events_b
            .iter()
            .find_map(|event| event.attempt_id.clone())
            .unwrap();
        assert_ne!(attempt_a, attempt_b);
        assert!(events_a.iter().all(|event| event.scope == scope_a));
        assert!(events_b.iter().all(|event| event.scope == scope_b));
        let ledger_a = store.budget_ledger(&scope_a, &id("shared-run"));
        let ledger_b = store.budget_ledger(&scope_b, &id("shared-run"));
        assert_eq!(ledger_a.len(), 2);
        assert_eq!(ledger_b.len(), 2);
        assert!(ledger_a.iter().all(|entry| entry.scope == scope_a));
        assert!(ledger_b.iter().all(|entry| entry.scope == scope_b));
    });
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
                fail: AtomicBool::new(false),
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
                    input: input.clone(),
                    budget_limit: CostUnits(10),
                    limits: RunLimits {
                        max_dynamic_node_instances: 10,
                        max_total_attempts: 10,
                        max_total_events: 1_000,
                        max_inline_json_bytes_per_value: 10_000,
                        max_artifacts_per_attempt: 10,
                        max_aggregate_object_bytes_per_run: 100_000,
                        max_run_lifetime_ms: 100_000,
                    },
                    principal: principal.clone(),
                    idempotency_token: "create-token-0001".to_owned(),
                },
            )
            .await
            .unwrap();
        let first_claim = store
            .acquire_engine_claim(&execution_scope, id("engine-1"))
            .await
            .unwrap();
        let revision = store
            .get_revision(&execution_scope, &id("definition"), canonical.digest())
            .await
            .unwrap();
        store
            .create_run(
                &execution_scope,
                CreateRun {
                    run_id: id("capacity-run"),
                    definition_id: id("definition"),
                    revision_hash: canonical.digest().clone(),
                    input: input.clone(),
                    budget_limit: CostUnits(10),
                    limits: RunLimits {
                        max_dynamic_node_instances: 10,
                        max_total_attempts: 10,
                        max_total_events: 9,
                        max_inline_json_bytes_per_value: 10_000,
                        max_artifacts_per_attempt: 10,
                        max_aggregate_object_bytes_per_run: 100_000,
                        max_run_lifetime_ms: 100_000,
                    },
                    principal,
                    idempotency_token: "capacity-create-01".to_owned(),
                },
            )
            .await
            .unwrap();
        store
            .start_run(
                &execution_scope,
                StartRun {
                    permit: first_claim.permit.clone(),
                    run_id: id("capacity-run"),
                    compatibility_evidence: registry.check_pins(&revision.action_pins),
                },
            )
            .await
            .unwrap();
        let capacity_node = store
            .get_node(&execution_scope, &id("capacity-run"), &id("action"))
            .await
            .unwrap();
        assert!(matches!(
            store
                .claim_node_attempt(
                    &execution_scope,
                    ClaimNodeAttempt {
                        permit: first_claim.permit.clone(),
                        run_id: id("capacity-run"),
                        node_id: id("action"),
                        expected_node_version: capacity_node.version,
                        attempt_id: id("must-rollback"),
                        worker_id: id("worker-1"),
                        bound_input: input.clone(),
                        binding_derivation_digest: hash(b"capacity-binding"),
                    },
                )
                .await,
            Err(dagger_workflow_core::store::StoreError::RunLimitApplied { .. })
        ));
        assert_eq!(
            store
                .get_run(&execution_scope, &id("capacity-run"))
                .await
                .unwrap()
                .run
                .status,
            RunState::Cancelled
        );
        assert!(store
            .get_attempt(&execution_scope, &id("capacity-run"), &id("must-rollback"))
            .await
            .is_err());
        store
            .start_run(
                &execution_scope,
                StartRun {
                    permit: first_claim.permit.clone(),
                    run_id: id("run"),
                    compatibility_evidence: registry.check_pins(&revision.action_pins),
                },
            )
            .await
            .unwrap();
        let node = store
            .get_node(&execution_scope, &id("run"), &id("action"))
            .await
            .unwrap();
        let foreign_objects = InMemoryObjectStore::new(clock.clone());
        let foreign_input = foreign_objects
            .put(&execution_scope, br#"{"value":7}"#, "application/json")
            .await
            .unwrap();
        assert!(matches!(
            store
                .claim_node_attempt(
                    &execution_scope,
                    ClaimNodeAttempt {
                        permit: first_claim.permit.clone(),
                        run_id: id("run"),
                        node_id: id("action"),
                        expected_node_version: node.version,
                        attempt_id: id("foreign-capability"),
                        worker_id: id("worker-1"),
                        bound_input: foreign_input,
                        binding_derivation_digest: hash(b"foreign-binding"),
                    },
                )
                .await,
            Err(dagger_workflow_core::store::StoreError::ObjectNotVerified)
        ));
        assert!(store
            .get_attempt(&execution_scope, &id("run"), &id("foreign-capability"))
            .await
            .is_err());
        let claim = store
            .claim_node_attempt(
                &execution_scope,
                ClaimNodeAttempt {
                    permit: first_claim.permit.clone(),
                    run_id: id("run"),
                    node_id: id("action"),
                    expected_node_version: node.version,
                    attempt_id: id("abandoned-attempt"),
                    worker_id: id("worker-1"),
                    bound_input: input,
                    binding_derivation_digest: hash(b"binding"),
                },
            )
            .await
            .unwrap();
        assert!(matches!(claim, ClaimNodeAttemptResult::Claimed { .. }));
        assert_eq!(
            store
                .get_node(&execution_scope, &id("run"), &id("action"))
                .await
                .unwrap()
                .status,
            dagger_workflow_core::run::NodeState::Running
        );
        clock.advance_ms(20_000).unwrap();
        assert!(matches!(
            store
                .timeout_attempt(
                    &execution_scope,
                    TimeoutAttempt {
                        permit: first_claim.permit,
                        run_id: id("run"),
                        node_id: id("action"),
                        attempt_id: id("abandoned-attempt"),
                    },
                )
                .await,
            Err(dagger_workflow_core::store::StoreError::EngineClaimExpired)
        ));

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
        let recovered = store
            .get_attempt(&execution_scope, &id("run"), &id("abandoned-attempt"))
            .await
            .unwrap();
        assert_eq!(recovered.status, AttemptState::UnknownOutcome);
        assert_eq!(recovered.settled_cost, Some(CostUnits(2)));
        assert_eq!(
            store
                .get_node(&execution_scope, &id("run"), &id("action"))
                .await
                .unwrap()
                .status,
            dagger_workflow_core::run::NodeState::RetryWaiting
        );
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
        let created_ready = events
            .iter()
            .find(|event| {
                event.event_type == dagger_workflow_core::event::EventType::NodeCreatedReady
            })
            .unwrap();
        assert!(created_ready.payload.get("incoming_total").is_none());
        let satisfied = events
            .iter()
            .find(|event| event.event_type == dagger_workflow_core::event::EventType::EdgeSatisfied)
            .unwrap();
        assert!(satisfied.payload.get("cause").is_none());
        for settled in events.iter().filter(|event| {
            event.event_type == dagger_workflow_core::event::EventType::BudgetSettled
        }) {
            assert!(settled.payload["ledger_seq"].as_u64().unwrap() > 0);
            assert_ne!(settled.payload["available_after"], "0");
        }
    });
}

#[test]
fn approval_expiry_advances_downstream_frontier_through_engine_ticks() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(1_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let definition = WorkflowDefinition {
            definition_format_version: "0.1".to_owned(),
            definition_id: id("approval-definition"),
            name: "approval-expiry".to_owned(),
            description: String::new(),
            run_input_schema_digest: hash(b"{}"),
            run_output_schema_digest: hash(b"{}"),
            entry_node_id: id("approval"),
            nodes: vec![
                NodeDefinition::Approval {
                    id: id("approval"),
                    request: BindingSource::RunInput {
                        pointer: String::new(),
                    },
                    gate: ApprovalGateConfig {
                        expires_after_ms: 50,
                        on_expiry: ApprovalExpiryPolicy::Approve,
                        authorization: DecisionAuthorizationPolicy {
                            allowed_principal_ids: vec!["approver".to_owned()],
                            allowed_role_ids: Vec::new(),
                        },
                    },
                    next: vec![id("succeed")],
                },
                NodeDefinition::Succeed {
                    id: id("succeed"),
                    output: BindingSource::NodeOutput {
                        node_id: id("approval"),
                        pointer: String::new(),
                    },
                },
            ],
        };
        prepare_definition(
            &store,
            &objects,
            &execution_scope,
            definition,
            "approval-run",
            10_000,
        )
        .await;
        let registry = Arc::new(InMemoryActionRegistry::new());
        let engine = WorkflowEngine::new(
            store.clone(),
            objects,
            registry,
            EngineConfig {
                instance_id: id("approval-engine"),
                max_concurrency: 1,
            },
        )
        .unwrap();
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("approval-run"))
            .await
            .unwrap();
        engine.tick(&execution_scope).await.unwrap();
        assert_eq!(
            store
                .get_node(&execution_scope, &id("approval-run"), &id("approval"))
                .await
                .unwrap()
                .status,
            dagger_workflow_core::run::NodeState::WaitingApproval
        );
        clock.advance_ms(51).unwrap();
        engine.tick(&execution_scope).await.unwrap();
        assert_eq!(
            store
                .get_run(&execution_scope, &id("approval-run"))
                .await
                .unwrap()
                .run
                .status,
            RunState::Succeeded
        );
        let events = store
            .list_events_after(
                &execution_scope,
                &id("approval-run"),
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1_000,
                    hard_response_byte_limit: 1_000_000,
                },
            )
            .await
            .unwrap();
        assert!(events.iter().any(|event| {
            event.event_type == dagger_workflow_core::event::EventType::ApprovalGateExpiredApproved
        }));
        assert!(events.iter().any(|event| {
            event.event_type == dagger_workflow_core::event::EventType::RunSucceeded
        }));
    });
}

#[test]
fn approval_decision_winning_between_due_scan_and_expiry_is_benign() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(1_500)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let inner = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let objects = Arc::new(BlockingExpiryStore::new(inner.clone()));
        let definition = WorkflowDefinition {
            definition_format_version: "0.1".to_owned(),
            definition_id: id("approval-race-definition"),
            name: "approval-race".to_owned(),
            description: String::new(),
            run_input_schema_digest: hash(b"{}"),
            run_output_schema_digest: hash(b"{}"),
            entry_node_id: id("approval"),
            nodes: vec![
                NodeDefinition::Approval {
                    id: id("approval"),
                    request: BindingSource::RunInput {
                        pointer: String::new(),
                    },
                    gate: ApprovalGateConfig {
                        expires_after_ms: 50,
                        on_expiry: ApprovalExpiryPolicy::Approve,
                        authorization: DecisionAuthorizationPolicy {
                            allowed_principal_ids: vec!["approver".to_owned()],
                            allowed_role_ids: Vec::new(),
                        },
                    },
                    next: vec![id("succeed")],
                },
                NodeDefinition::Succeed {
                    id: id("succeed"),
                    output: BindingSource::NodeOutput {
                        node_id: id("approval"),
                        pointer: String::new(),
                    },
                },
            ],
        };
        prepare_definition(
            &store,
            &inner,
            &execution_scope,
            definition,
            "approval-race-run",
            10_000,
        )
        .await;
        let engine = Arc::new(
            WorkflowEngine::new(
                store.clone(),
                objects.clone(),
                Arc::new(InMemoryActionRegistry::new()),
                EngineConfig {
                    instance_id: id("approval-race-engine"),
                    max_concurrency: 1,
                },
            )
            .unwrap(),
        );
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("approval-race-run"))
            .await
            .unwrap();
        engine.tick(&execution_scope).await.unwrap();
        let approval_node = store
            .get_node(&execution_scope, &id("approval-race-run"), &id("approval"))
            .await
            .unwrap();
        let gate_id = approval_node.approval_gate_id.unwrap();
        let gate = store
            .get_gate(&execution_scope, &id("approval-race-run"), &gate_id)
            .await
            .unwrap();
        clock.advance_ms(51).unwrap();
        objects.arm();
        let tick_engine = engine.clone();
        let tick_scope = execution_scope.clone();
        let tick_thread = thread::spawn(move || block_on(tick_engine.tick(&tick_scope)));
        objects.wait_until_blocked();
        let principal = AuthenticatedPrincipal::mint(
            execution_scope.clone(),
            "approver".to_owned(),
            Vec::new(),
            hash(b"approval-race-auth"),
        )
        .unwrap();
        let approval_output = inner
            .put(
                &execution_scope,
                &canonical_human_approval_result(None, &principal),
                "application/json",
            )
            .await
            .unwrap();
        let run = store
            .get_run(&execution_scope, &id("approval-race-run"))
            .await
            .unwrap()
            .run;
        store
            .decide_approval(
                &execution_scope,
                DecideApproval {
                    run_id: id("approval-race-run"),
                    gate_id,
                    expected_run_version: run.version,
                    expected_gate_version: gate.version,
                    decision: ApprovalDecision::Approve,
                    decision_payload: None,
                    approval_output: Some(approval_output),
                    principal,
                },
            )
            .await
            .unwrap();
        objects.release();
        assert!(tick_thread.join().unwrap().is_ok());
        assert_eq!(
            store
                .get_run(&execution_scope, &id("approval-race-run"))
                .await
                .unwrap()
                .run
                .status,
            RunState::Succeeded
        );
    });
}

#[test]
fn map_nodes_expand_schedule_children_and_complete_parent_through_ticks() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(2_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        let descriptor = ActionDescriptor {
            name: "fixture.map".to_owned(),
            contract_version: "1".to_owned(),
            input_schema_digest: hash(b"{}"),
            output_schema_digest: hash(b"{}"),
            implementation_compatibility_digest: hash(b"map-implementation"),
        };
        let definition = WorkflowDefinition {
            definition_format_version: "0.1".to_owned(),
            definition_id: id("map-definition"),
            name: "map".to_owned(),
            description: String::new(),
            run_input_schema_digest: hash(b"{}"),
            run_output_schema_digest: hash(b"{}"),
            entry_node_id: id("map"),
            nodes: vec![
                NodeDefinition::Map {
                    id: id("map"),
                    items: BindingSource::Constant {
                        value: json!([{"value": 2}, {"value": 1}]),
                    },
                    max_items: 2,
                    max_concurrency: 2,
                    action: ActionReference {
                        name: descriptor.name.clone(),
                        contract_version: descriptor.contract_version.clone(),
                        input_schema_digest: descriptor.input_schema_digest.clone(),
                        output_schema_digest: descriptor.output_schema_digest.clone(),
                        compatible_implementation_requirement: descriptor
                            .implementation_compatibility_digest
                            .clone(),
                    },
                    bindings: vec![
                        MapBinding {
                            target: "/item".to_owned(),
                            source: MapBindingSource::MapItem {
                                pointer: String::new(),
                            },
                        },
                        MapBinding {
                            target: "/index".to_owned(),
                            source: MapBindingSource::MapIndex,
                        },
                    ],
                    retry: RetryPolicy {
                        max_attempts: 1,
                        backoff: BackoffPolicy::Fixed { delay_ms: 0 },
                    },
                    timeout: TimeoutPolicy { timeout_ms: 1_000 },
                    declared_max_cost_units: CostUnits(1),
                    next: vec![id("succeed")],
                },
                NodeDefinition::Succeed {
                    id: id("succeed"),
                    output: BindingSource::NodeOutput {
                        node_id: id("map"),
                        pointer: String::new(),
                    },
                },
            ],
        };
        prepare_definition(
            &store,
            &objects,
            &execution_scope,
            definition,
            "map-run",
            10_000,
        )
        .await;
        let registry = Arc::new(InMemoryActionRegistry::new());
        registry
            .register(Arc::new(RetryOnce {
                descriptor,
                fail: AtomicBool::new(false),
            }))
            .unwrap();
        let engine = WorkflowEngine::new(
            store.clone(),
            objects.clone(),
            registry,
            EngineConfig {
                instance_id: id("map-engine"),
                max_concurrency: 2,
            },
        )
        .unwrap();
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("map-run"))
            .await
            .unwrap();
        engine.run_until_idle(&execution_scope, 8).await.unwrap();
        let run = store
            .get_run(&execution_scope, &id("map-run"))
            .await
            .unwrap()
            .run;
        assert_eq!(
            run.status,
            RunState::Succeeded,
            "failure kind: {:?}",
            run.failure_kind
        );
        let parent = store
            .get_node(&execution_scope, &id("map-run"), &id("map"))
            .await
            .unwrap();
        let aggregate = objects
            .get(
                &execution_scope,
                &parent.result_ref.as_ref().unwrap().0.digest,
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&aggregate.bytes).unwrap(),
            json!([
                {"index": 0, "item": {"value": 2}},
                {"index": 1, "item": {"value": 1}}
            ])
        );
    });
}

#[test]
fn map_max_concurrency_one_serializes_children_with_engine_concurrency_three() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(3_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        let descriptor = ActionDescriptor {
            name: "fixture.serial-map".to_owned(),
            contract_version: "1".to_owned(),
            input_schema_digest: hash(b"{}"),
            output_schema_digest: hash(b"{}"),
            implementation_compatibility_digest: hash(b"serial-map-implementation"),
        };
        let definition = map_workflow(
            "serial-map-definition",
            &descriptor,
            json!([0, 1, 2]),
            3,
            1,
            vec![
                MapBinding {
                    target: "/item".to_owned(),
                    source: MapBindingSource::MapItem {
                        pointer: String::new(),
                    },
                },
                MapBinding {
                    target: "/index".to_owned(),
                    source: MapBindingSource::MapIndex,
                },
            ],
        );
        prepare_definition(
            &store,
            &objects,
            &execution_scope,
            definition,
            "serial-map-run",
            100_000,
        )
        .await;
        let action = Arc::new(SerialProbe {
            descriptor,
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
        });
        let registry = Arc::new(InMemoryActionRegistry::new());
        registry.register(action.clone()).unwrap();
        let engine = WorkflowEngine::new(
            store.clone(),
            objects,
            registry,
            EngineConfig {
                instance_id: id("serial-map-engine"),
                max_concurrency: 3,
            },
        )
        .unwrap();
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("serial-map-run"))
            .await
            .unwrap();
        engine.run_until_idle(&execution_scope, 12).await.unwrap();
        assert_eq!(action.calls.load(Ordering::Acquire), 3);
        assert_eq!(action.max_active.load(Ordering::Acquire), 1);
        assert_eq!(
            store
                .get_run(&execution_scope, &id("serial-map-run"))
                .await
                .unwrap()
                .run
                .status,
            RunState::Succeeded
        );
    });
}

#[test]
fn map_child_accepts_literal_artifact_ref_binding() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(4_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        let seed = WorkflowDefinition {
            definition_format_version: "0.1".to_owned(),
            definition_id: id("artifact-seed-definition"),
            name: "artifact-seed".to_owned(),
            description: String::new(),
            run_input_schema_digest: hash(b"{}"),
            run_output_schema_digest: hash(b"{}"),
            entry_node_id: id("succeed"),
            nodes: vec![NodeDefinition::Succeed {
                id: id("succeed"),
                output: BindingSource::Constant { value: json!({}) },
            }],
        };
        prepare_definition(
            &store,
            &objects,
            &execution_scope,
            seed,
            "artifact-seed-run",
            100_000,
        )
        .await;
        let seed_run = store
            .get_run(&execution_scope, &id("artifact-seed-run"))
            .await
            .unwrap()
            .run;
        let seed_revision = store
            .get_revision(
                &execution_scope,
                &seed_run.definition_id,
                &seed_run.revision_hash,
            )
            .await
            .unwrap();
        let literal = seed_revision.run_input_schema_ref.0;
        let descriptor = ActionDescriptor {
            name: "fixture.artifact-map".to_owned(),
            contract_version: "1".to_owned(),
            input_schema_digest: hash(b"{}"),
            output_schema_digest: hash(b"{}"),
            implementation_compatibility_digest: hash(b"artifact-map-implementation"),
        };
        let definition = map_workflow(
            "artifact-map-definition",
            &descriptor,
            json!([0]),
            1,
            1,
            vec![MapBinding {
                target: "/artifact".to_owned(),
                source: MapBindingSource::ArtifactRef {
                    source: ArtifactLocator::Literal {
                        artifact_ref_id: literal.artifact_ref_id.clone(),
                        digest: literal.digest.clone(),
                        media_type: literal.media_type.clone(),
                    },
                },
            }],
        );
        prepare_definition(
            &store,
            &objects,
            &execution_scope,
            definition,
            "artifact-map-run",
            100_000,
        )
        .await;
        let registry = Arc::new(InMemoryActionRegistry::new());
        registry
            .register(Arc::new(RetryOnce {
                descriptor,
                fail: AtomicBool::new(false),
            }))
            .unwrap();
        let engine = WorkflowEngine::new(
            store.clone(),
            objects.clone(),
            registry,
            EngineConfig {
                instance_id: id("artifact-map-engine"),
                max_concurrency: 2,
            },
        )
        .unwrap();
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("artifact-map-run"))
            .await
            .unwrap();
        engine.run_until_idle(&execution_scope, 8).await.unwrap();
        let parent = store
            .get_node(&execution_scope, &id("artifact-map-run"), &id("map"))
            .await
            .unwrap();
        let aggregate = objects
            .get(
                &execution_scope,
                &parent.result_ref.as_ref().unwrap().0.digest,
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&aggregate.bytes).unwrap(),
            json!([{
                "artifact": {
                    "artifact_ref_id": literal.artifact_ref_id,
                    "digest": literal.digest,
                    "media_type": literal.media_type,
                    "size_bytes": literal.size_bytes.to_string()
                }
            }])
        );
    });
}

#[test]
fn corrupt_map_child_output_marks_waiting_parent_and_run_corrupt() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(4_500)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let inner = Arc::new(InMemoryObjectStore::new(clock));
        let objects = Arc::new(CorruptingOutputStore {
            inner: inner.clone(),
            target: serde_jcs::to_vec(&json!({"index": 0, "item": {"value": 2}})).unwrap(),
            matching_puts: AtomicUsize::new(0),
        });
        let descriptor = ActionDescriptor {
            name: "fixture.corrupt-map".to_owned(),
            contract_version: "1".to_owned(),
            input_schema_digest: hash(b"{}"),
            output_schema_digest: hash(b"{}"),
            implementation_compatibility_digest: hash(b"corrupt-map-implementation"),
        };
        let definition = map_workflow(
            "corrupt-map-definition",
            &descriptor,
            json!([{"value": 2}]),
            1,
            1,
            vec![
                MapBinding {
                    target: "/item".to_owned(),
                    source: MapBindingSource::MapItem {
                        pointer: String::new(),
                    },
                },
                MapBinding {
                    target: "/index".to_owned(),
                    source: MapBindingSource::MapIndex,
                },
            ],
        );
        prepare_definition(
            &store,
            &inner,
            &execution_scope,
            definition,
            "corrupt-map-run",
            100_000,
        )
        .await;
        let registry = Arc::new(InMemoryActionRegistry::new());
        registry
            .register(Arc::new(RetryOnce {
                descriptor,
                fail: AtomicBool::new(false),
            }))
            .unwrap();
        let engine = WorkflowEngine::new(
            store.clone(),
            objects,
            registry,
            EngineConfig {
                instance_id: id("corrupt-map-engine"),
                max_concurrency: 1,
            },
        )
        .unwrap();
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("corrupt-map-run"))
            .await
            .unwrap();
        engine.run_until_idle(&execution_scope, 6).await.unwrap();
        assert_eq!(
            store
                .get_run(&execution_scope, &id("corrupt-map-run"))
                .await
                .unwrap()
                .run
                .status,
            RunState::CorruptStorage
        );
        let parent = store
            .get_node(&execution_scope, &id("corrupt-map-run"), &id("map"))
            .await
            .unwrap();
        assert_eq!(
            parent.status,
            dagger_workflow_core::run::NodeState::CorruptStorage
        );
        let events = store
            .list_events_after(
                &execution_scope,
                &id("corrupt-map-run"),
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1_000,
                    hard_response_byte_limit: 1_000_000,
                },
            )
            .await
            .unwrap();
        assert!(events.iter().any(|event| event.transition_id == "N50"));
        assert!(events.iter().any(|event| event.transition_id == "R15"));
    });
}

#[test]
fn run_lifetime_expiry_terminalizes_through_engine_ticks() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(5_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let definition = WorkflowDefinition {
            definition_format_version: "0.1".to_owned(),
            definition_id: id("lifetime-definition"),
            name: "lifetime-expiry".to_owned(),
            description: String::new(),
            run_input_schema_digest: hash(b"{}"),
            run_output_schema_digest: hash(b"{}"),
            entry_node_id: id("approval"),
            nodes: vec![
                NodeDefinition::Approval {
                    id: id("approval"),
                    request: BindingSource::RunInput {
                        pointer: String::new(),
                    },
                    gate: ApprovalGateConfig {
                        expires_after_ms: 10_000,
                        on_expiry: ApprovalExpiryPolicy::Reject,
                        authorization: DecisionAuthorizationPolicy {
                            allowed_principal_ids: vec!["approver".to_owned()],
                            allowed_role_ids: Vec::new(),
                        },
                    },
                    next: vec![id("succeed")],
                },
                NodeDefinition::Succeed {
                    id: id("succeed"),
                    output: BindingSource::Constant { value: json!(null) },
                },
            ],
        };
        prepare_definition(
            &store,
            &objects,
            &execution_scope,
            definition,
            "lifetime-run",
            50,
        )
        .await;
        let engine = WorkflowEngine::new(
            store.clone(),
            objects,
            Arc::new(InMemoryActionRegistry::new()),
            EngineConfig {
                instance_id: id("lifetime-engine"),
                max_concurrency: 1,
            },
        )
        .unwrap();
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("lifetime-run"))
            .await
            .unwrap();
        engine.tick(&execution_scope).await.unwrap();
        clock.advance_ms(51).unwrap();
        engine.tick(&execution_scope).await.unwrap();
        assert_eq!(
            store
                .get_run(&execution_scope, &id("lifetime-run"))
                .await
                .unwrap()
                .run
                .status,
            RunState::Cancelled
        );
        let events = store
            .list_events_after(
                &execution_scope,
                &id("lifetime-run"),
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1_000,
                    hard_response_byte_limit: 1_000_000,
                },
            )
            .await
            .unwrap();
        let cancelled = events
            .iter()
            .find(|event| event.event_type == dagger_workflow_core::event::EventType::RunCancelled)
            .unwrap();
        assert_eq!(cancelled.transition_id, "R12");
        assert_eq!(
            cancelled.payload.get("reason_code").and_then(Value::as_str),
            Some("RunLifetimeExceeded")
        );
        assert!(events.iter().any(|event| {
            event.event_type == dagger_workflow_core::event::EventType::ApprovalGateCancelled
        }));
    });
}

#[test]
fn blocked_run_fence_precedes_exact_approval_replay() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(9_000)));
        let store = InMemoryStore::new(clock.clone());
        let objects = InMemoryObjectStore::new(clock);
        let definition = WorkflowDefinition {
            definition_format_version: "0.1".to_owned(),
            definition_id: id("blocked-approval-definition"),
            name: "blocked-approval".to_owned(),
            description: String::new(),
            run_input_schema_digest: hash(b"{}"),
            run_output_schema_digest: hash(b"{}"),
            entry_node_id: id("approval"),
            nodes: vec![
                NodeDefinition::Approval {
                    id: id("approval"),
                    request: BindingSource::RunInput {
                        pointer: String::new(),
                    },
                    gate: ApprovalGateConfig {
                        expires_after_ms: 10_000,
                        on_expiry: ApprovalExpiryPolicy::Reject,
                        authorization: DecisionAuthorizationPolicy {
                            allowed_principal_ids: vec!["approver".to_owned()],
                            allowed_role_ids: Vec::new(),
                        },
                    },
                    next: vec![id("succeed")],
                },
                NodeDefinition::Succeed {
                    id: id("succeed"),
                    output: BindingSource::Constant { value: json!(null) },
                },
            ],
        };
        prepare_definition(
            &store,
            &objects,
            &execution_scope,
            definition,
            "blocked-approval-run",
            100_000,
        )
        .await;
        let claim = store
            .acquire_engine_claim(&execution_scope, id("blocked-engine"))
            .await
            .unwrap();
        store
            .start_run(
                &execution_scope,
                StartRun {
                    permit: claim.permit.clone(),
                    run_id: id("blocked-approval-run"),
                    compatibility_evidence: CompatibilityReport {
                        evidence_digest: hash(b"compatible"),
                        incompatible_reference_locations: Vec::new(),
                        evidence: Vec::new(),
                    },
                },
            )
            .await
            .unwrap();
        let node = store
            .get_node(
                &execution_scope,
                &id("blocked-approval-run"),
                &id("approval"),
            )
            .await
            .unwrap();
        let request = objects
            .put(&execution_scope, br#"{"request":true}"#, "application/json")
            .await
            .unwrap();
        let gate = store
            .request_approval(
                &execution_scope,
                RequestApproval {
                    permit: claim.permit.clone(),
                    run_id: id("blocked-approval-run"),
                    node_id: id("approval"),
                    expected_node_version: node.version,
                    gate_id: id("approval-gate"),
                    request,
                },
            )
            .await
            .unwrap();
        let principal = AuthenticatedPrincipal::mint(
            execution_scope.clone(),
            "approver".to_owned(),
            Vec::new(),
            hash(b"approver-auth"),
        )
        .unwrap();
        let approval_output = objects
            .put(
                &execution_scope,
                &canonical_human_approval_result(None, &principal),
                "application/json",
            )
            .await
            .unwrap();
        let observed_run = store
            .get_run(&execution_scope, &id("blocked-approval-run"))
            .await
            .unwrap()
            .run;
        let decision = DecideApproval {
            run_id: id("blocked-approval-run"),
            gate_id: gate.gate_id.clone(),
            expected_run_version: observed_run.version,
            expected_gate_version: gate.version,
            decision: ApprovalDecision::Approve,
            decision_payload: None,
            approval_output: Some(approval_output.clone()),
            principal: principal.clone(),
        };
        store
            .decide_approval(&execution_scope, decision)
            .await
            .unwrap();
        let incompatibilities = objects
            .put(
                &execution_scope,
                br#"{"missing":["synthetic"]}"#,
                "application/json",
            )
            .await
            .unwrap();
        store
            .suspend_incompatible(
                &execution_scope,
                SuspendIncompatible {
                    permit: claim.permit,
                    run_id: id("blocked-approval-run"),
                    incompatibilities,
                    evidence: CompatibilityReport {
                        evidence_digest: hash(b"incompatible"),
                        incompatible_reference_locations: vec!["synthetic".to_owned()],
                        evidence: Vec::new(),
                    },
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .decide_approval(
                    &execution_scope,
                    DecideApproval {
                        run_id: id("blocked-approval-run"),
                        gate_id: gate.gate_id,
                        expected_run_version: observed_run.version,
                        expected_gate_version: gate.version,
                        decision: ApprovalDecision::Approve,
                        decision_payload: None,
                        approval_output: Some(approval_output),
                        principal,
                    },
                )
                .await,
            Err(dagger_workflow_core::store::StoreError::RunBlockedIncompatible)
        ));
    });
}
