use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, ActionRegistry, InMemoryActionRegistry,
    WorkflowAction,
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
use dagger_workflow_core::run::{AttemptState, RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::store::{
    ClaimNodeAttempt, ClaimNodeAttemptResult, CreateDefinition, CreateRun, EventPageRequest,
    PageRequest, PublishRevision, ResolvedActionSchemas, StartRun, TimeoutAttempt, WorkflowStore,
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
