#![cfg(feature = "sqlite")]
//! Structured approval payloads remain durable and route rejections forward.

use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, InMemoryActionRegistry, WorkflowAction,
};
use dagger_workflow_core::approval::{
    canonical_human_decision_result, decide_approval_json, ApprovalDecision, ApprovalExpiryPolicy,
    AuthenticatedPrincipal, DecisionAuthorizationPolicy,
};
use dagger_workflow_core::artifact::{ObjectStore, VerifiedObjectRef};
use dagger_workflow_core::definition::{
    canonical_topological_ranks, ActionReference, ApprovalGateConfig, BackoffPolicy, BindingSource,
    ChoiceCase, ChoiceTarget, NodeDefinition, PublishableDefinition, RetryPolicy, TimeoutPolicy,
    WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::ids::{CostUnits, Id, Timestamp, Version};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::run::{GateState, NodeState, RunFailureKind, RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::sqlite::SqliteWorkflowStore;
use dagger_workflow_core::store::{
    CreateDefinition, CreateRun, DecideApproval, EventPageRequest, PublishRevision,
    ResolvedActionSchemas, StoreError, WorkflowStore,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc, time::Duration};

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}
fn scope() -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new("approval").unwrap(),
        namespace: ScopeAtom::new("structured").unwrap(),
    }
}

struct Noop(ActionDescriptor);
impl WorkflowAction for Noop {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.0
    }
    fn invoke<'a>(
        &'a self,
        _: ActionContext,
        _: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async {
            ActionOutcome::success(json!({}), Vec::new(), CostUnits(1), None).unwrap()
        })
    }
}

struct Seed {
    principal: AuthenticatedPrincipal,
    registry: Arc<InMemoryActionRegistry>,
    payload_schema: VerifiedObjectRef,
}

async fn seed<S: WorkflowStore>(
    store: &S,
    objects: &InMemoryObjectStore<TestClock>,
    structured: bool,
    route_by_payload: bool,
) -> Seed {
    let scope = scope();
    let schema = objects
        .put(
            &scope,
            br#"{"additionalProperties":false,"properties":{},"type":"object"}"#,
            "application/json",
        )
        .await
        .unwrap();
    let payload_schema = objects.put(&scope, br#"{"additionalProperties":false,"properties":{"defect_tags":{"items":{"type":"string"},"type":"array"},"verdict":{"enum":["liked","rejected"],"type":"string"}},"required":["verdict","defect_tags"],"type":"object"}"#, "application/json").await.unwrap();
    let principal = AuthenticatedPrincipal::mint(
        scope.clone(),
        "listener".to_owned(),
        vec![],
        schema.digest().clone(),
    )
    .unwrap();
    store
        .create_definition(
            &scope,
            CreateDefinition {
                definition_id: id("definition"),
                display_name: "structured approval".to_owned(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let registry = Arc::new(InMemoryActionRegistry::new());
    let action = |name: &str| {
        let descriptor = ActionDescriptor {
            name: name.to_owned(),
            contract_version: "1".to_owned(),
            input_schema_digest: schema.digest().clone(),
            output_schema_digest: schema.digest().clone(),
            implementation_compatibility_digest: schema.digest().clone(),
        };
        registry
            .register(Arc::new(Noop(descriptor.clone())))
            .unwrap();
        NodeDefinition::Action {
            id: id(name),
            action: ActionReference {
                name: descriptor.name,
                contract_version: descriptor.contract_version,
                input_schema_digest: descriptor.input_schema_digest,
                output_schema_digest: descriptor.output_schema_digest,
                compatible_implementation_requirement: descriptor
                    .implementation_compatibility_digest,
            },
            bindings: vec![],
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: BackoffPolicy::Fixed { delay_ms: 0 },
            },
            timeout: TimeoutPolicy { timeout_ms: 1_000 },
            declared_max_cost_units: CostUnits(1),
            next: vec![id("done")],
        }
    };
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: id("definition"),
        name: "approval-routing".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.digest().clone(),
        run_output_schema_digest: schema.digest().clone(),
        nodes: vec![
            NodeDefinition::Approval {
                id: id("listen"),
                request: BindingSource::RunInput {
                    pointer: String::new(),
                },
                gate: ApprovalGateConfig {
                    expires_after_ms: 10_000,
                    on_expiry: ApprovalExpiryPolicy::Reject,
                    authorization: DecisionAuthorizationPolicy {
                        allowed_principal_ids: vec!["listener".to_owned()],
                        allowed_role_ids: vec![],
                    },
                    decision_output: structured,
                    decision_schema_digest: structured.then(|| payload_schema.digest().clone()),
                },
                next: vec![id("route")],
            },
            NodeDefinition::Choice {
                id: id("route"),
                input: BindingSource::NodeOutput {
                    node_id: id("listen"),
                    pointer: if route_by_payload {
                        "/payload/verdict"
                    } else {
                        "/decision"
                    }
                    .to_owned(),
                },
                selector: String::new(),
                cases: vec![ChoiceCase::Equals {
                    equals: json!(if route_by_payload { "liked" } else { "approve" }),
                    target: ChoiceTarget::Node {
                        next: id("deliver"),
                    },
                }],
                default: ChoiceTarget::Node { next: id("repair") },
            },
            action("deliver"),
            action("repair"),
            NodeDefinition::Succeed {
                id: id("done"),
                output: BindingSource::Constant { value: json!({}) },
            },
        ],
    };
    let canonical = objects
        .put(
            &scope,
            &serde_jcs::to_vec(&definition).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let make_command = || PublishRevision {
        definition_id: id("definition"),
        expected_definition_version: Version(1),
        canonical_definition: canonical.clone(),
        run_input_schema: schema.clone(),
        run_output_schema: schema.clone(),
        resolved_action_schema_objects: BTreeMap::from([
            (
                "deliver".to_owned(),
                ResolvedActionSchemas {
                    input_schema: schema.clone(),
                    output_schema: schema.clone(),
                },
            ),
            (
                "repair".to_owned(),
                ResolvedActionSchemas {
                    input_schema: schema.clone(),
                    output_schema: schema.clone(),
                },
            ),
        ]),
        parsed_revision: PublishableDefinition {
            topological_ranks: canonical_topological_ranks(&definition).unwrap(),
            definition: definition.clone(),
        },
        principal: principal.clone(),
    };
    if structured {
        assert!(matches!(
            store.publish_revision(&scope, make_command()).await,
            Err(StoreError::NotFound)
        ));
    }
    let revision = store
        .publish_revision_with_approval_schemas(
            &scope,
            make_command(),
            if structured {
                BTreeMap::from([("listen".to_owned(), payload_schema.clone())])
            } else {
                BTreeMap::new()
            },
        )
        .await
        .unwrap();
    if structured {
        assert_eq!(
            revision.approval_schema_refs["listen"].0.digest,
            *payload_schema.digest()
        );
    }
    let input = objects
        .put(&scope, b"{}", "application/json")
        .await
        .unwrap();
    store
        .create_run(
            &scope,
            CreateRun {
                run_id: id("run"),
                definition_id: id("definition"),
                revision_hash: canonical.digest().clone(),
                input,
                budget_limit: CostUnits(100),
                limits: RunLimits {
                    max_dynamic_node_instances: 10,
                    max_total_attempts: 10,
                    max_total_events: 1_000,
                    max_inline_json_bytes_per_value: 10_000,
                    max_artifacts_per_attempt: 10,
                    max_aggregate_object_bytes_per_run: 1_000_000,
                    max_run_lifetime_ms: 100_000,
                },
                principal: principal.clone(),
                idempotency_token: "structured-create-run-token-1234567890".to_owned(),
            },
        )
        .await
        .unwrap();
    Seed {
        principal,
        registry,
        payload_schema,
    }
}

fn make_engine<S: WorkflowStore + 'static>(
    store: Arc<S>,
    objects: Arc<InMemoryObjectStore<TestClock>>,
    seed: &Seed,
) -> WorkflowEngine<S, InMemoryObjectStore<TestClock>, InMemoryActionRegistry> {
    WorkflowEngine::new(
        store,
        objects,
        seed.registry.clone(),
        EngineConfig {
            instance_id: id("engine"),
            max_concurrency: 2,
            cancellation_grace: Duration::from_secs(1),
        },
    )
    .unwrap()
}

async fn open<S: WorkflowStore + 'static>(
    engine: &WorkflowEngine<S, InMemoryObjectStore<TestClock>, InMemoryActionRegistry>,
    store: &S,
) -> Id {
    engine.acquire_scope(&scope()).await.unwrap();
    engine.start(&scope(), &id("run")).await.unwrap();
    engine.tick(&scope()).await.unwrap();
    store
        .get_node(&scope(), &id("run"), &id("listen"))
        .await
        .unwrap()
        .approval_gate_id
        .unwrap()
}

async fn output<S: WorkflowStore>(store: &S, objects: &InMemoryObjectStore<TestClock>) -> Value {
    let node = store
        .get_node(&scope(), &id("run"), &id("listen"))
        .await
        .unwrap();
    assert_eq!(node.status, NodeState::Succeeded);
    let reference = node.result_ref.unwrap();
    serde_json::from_slice(
        &objects
            .get(&scope(), &reference.0.digest)
            .await
            .unwrap()
            .bytes,
    )
    .unwrap()
}

async fn human_routing<S: WorkflowStore + 'static>(
    store: Arc<S>,
    objects: Arc<InMemoryObjectStore<TestClock>>,
    verdict: &str,
) {
    let seed = seed(store.as_ref(), &objects, true, true).await;
    let engine = make_engine(store.clone(), objects.clone(), &seed);
    let gate_id = open(&engine, store.as_ref()).await;
    let decision = if verdict == "liked" {
        ApprovalDecision::Approve
    } else {
        ApprovalDecision::Reject
    };
    let payload = json!({"verdict": verdict, "defect_tags": ["mix.clipping"]});
    let gate = decide_approval_json(
        store.as_ref(),
        objects.as_ref(),
        &scope(),
        id("run"),
        gate_id.clone(),
        decision,
        Some(payload.clone()),
        seed.principal.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        gate.status,
        if verdict == "liked" {
            GateState::Approved
        } else {
            GateState::Rejected
        }
    );
    let view = store.get_run(&scope(), &id("run")).await.unwrap();
    let before = (view.run.version, view.run.last_event_seq);
    let replay = decide_approval_json(
        store.as_ref(),
        objects.as_ref(),
        &scope(),
        id("run"),
        gate_id.clone(),
        decision,
        Some(payload.clone()),
        seed.principal.clone(),
    )
    .await
    .unwrap();
    assert_eq!(gate.version, replay.version);
    let view = store.get_run(&scope(), &id("run")).await.unwrap();
    assert_eq!(before, (view.run.version, view.run.last_event_seq));
    let changed = json!({"verdict": verdict, "defect_tags": ["timing.rushed"]});
    assert!(matches!(
        decide_approval_json(
            store.as_ref(),
            objects.as_ref(),
            &scope(),
            id("run"),
            gate_id,
            decision,
            Some(changed),
            seed.principal.clone()
        )
        .await,
        Err(StoreError::ApprovalAlreadyResolved)
    ));
    let result = output(store.as_ref(), &objects).await;
    assert_eq!(result["payload"], payload);
    assert_eq!(result["principal"], "listener");
    assert!(result["payload_ref"]["digest"].as_str().is_some());
    engine.run_until_idle(&scope(), 12).await.unwrap();
    assert_eq!(
        store
            .get_run(&scope(), &id("run"))
            .await
            .unwrap()
            .run
            .status,
        RunState::Succeeded
    );
    let (chosen, skipped) = if verdict == "liked" {
        ("deliver", "repair")
    } else {
        ("repair", "deliver")
    };
    assert_eq!(
        store
            .get_node(&scope(), &id("run"), &id(chosen))
            .await
            .unwrap()
            .status,
        NodeState::Succeeded
    );
    assert_eq!(
        store
            .get_node(&scope(), &id("run"), &id(skipped))
            .await
            .unwrap()
            .status,
        NodeState::Skipped
    );
    let events = store
        .list_events_after(
            &scope(),
            &id("run"),
            EventPageRequest {
                after_event_seq: 0,
                page_size: 1_000,
                hard_response_byte_limit: 4_000_000,
            },
        )
        .await
        .unwrap();
    assert!(events.iter().any(|event| event.event_type
        == dagger_workflow_core::event::EventType::ApprovalDecisionResolved
        && event.payload["decision"]
            == if verdict == "liked" {
                "approve"
            } else {
                "reject"
            }));
}

#[tokio::test]
async fn both_human_decisions_route_payloads_with_scope_confined_replay() {
    for verdict in ["liked", "rejected"] {
        let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
        let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
        human_routing(Arc::new(InMemoryStore::new(clock)), objects, verdict).await;
        let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
        let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let store = Arc::new(
            SqliteWorkflowStore::open_url("sqlite::memory:", clock, objects.clone())
                .await
                .unwrap(),
        );
        human_routing(store, objects, verdict).await;
    }
}

async fn invalid_payload<S: WorkflowStore + 'static>(
    store: Arc<S>,
    objects: Arc<InMemoryObjectStore<TestClock>>,
) {
    let seed = seed(store.as_ref(), &objects, true, true).await;
    let engine = make_engine(store.clone(), objects.clone(), &seed);
    let gate_id = open(&engine, store.as_ref()).await;
    let run = store.get_run(&scope(), &id("run")).await.unwrap().run;
    let gate = store
        .get_gate(&scope(), &id("run"), &gate_id)
        .await
        .unwrap();
    for payload in [
        None,
        Some(json!({"verdict":"unknown","defect_tags":[]})),
        Some(json!({"verdict":"liked","defect_tags":"bad"})),
        Some(json!({"verdict":"liked","defect_tags":["x".repeat(10_001)]})),
    ] {
        assert!(matches!(
            decide_approval_json(
                store.as_ref(),
                objects.as_ref(),
                &scope(),
                id("run"),
                gate_id.clone(),
                ApprovalDecision::Approve,
                payload,
                seed.principal.clone()
            )
            .await,
            Err(StoreError::ApprovalDecisionPayloadInvalid)
        ));
        let after = store.get_run(&scope(), &id("run")).await.unwrap().run;
        let after_gate = store
            .get_gate(&scope(), &id("run"), &gate_id)
            .await
            .unwrap();
        assert_eq!(
            (run.version, run.last_event_seq, gate.version),
            (after.version, after.last_event_seq, after_gate.version)
        );
        assert_eq!(after_gate.status, GateState::Pending);
    }
    // The direct store path must read the actual payload object, not trust the
    // valid JSON embedded by a caller in a proposed successful output envelope.
    let payload = objects
        .put(
            &scope(),
            br#"{"defect_tags":[],"verdict":"unknown"}"#,
            "application/json",
        )
        .await
        .unwrap();
    let forged = objects
        .put(
            &scope(),
            &canonical_human_decision_result(
                ApprovalDecision::Approve,
                json!({"verdict":"liked","defect_tags":[]}),
                None,
                &seed.principal,
            ),
            "application/json",
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .decide_approval(
                &scope(),
                DecideApproval {
                    run_id: id("run"),
                    gate_id,
                    expected_run_version: run.version,
                    expected_gate_version: gate.version,
                    decision: ApprovalDecision::Approve,
                    decision_payload: Some(payload),
                    approval_output: Some(forged),
                    principal: seed.principal.clone()
                }
            )
            .await,
        Err(StoreError::ApprovalDecisionPayloadInvalid)
    ));
    assert_eq!(
        store
            .get_run(&scope(), &id("run"))
            .await
            .unwrap()
            .run
            .status,
        RunState::Running
    );
    // A valid payload cannot authorize a caller to replace it in the output.
    let payload = objects
        .put(
            &scope(),
            br#"{"defect_tags":[],"verdict":"liked"}"#,
            "application/json",
        )
        .await
        .unwrap();
    let forged = objects
        .put(
            &scope(),
            &canonical_human_decision_result(
                ApprovalDecision::Approve,
                json!({"verdict":"rejected","defect_tags":[]}),
                None,
                &seed.principal,
            ),
            "application/json",
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .decide_approval(
                &scope(),
                DecideApproval {
                    run_id: id("run"),
                    gate_id: gate.gate_id.clone(),
                    expected_run_version: run.version,
                    expected_gate_version: gate.version,
                    decision: ApprovalDecision::Approve,
                    decision_payload: Some(payload),
                    approval_output: Some(forged),
                    principal: seed.principal
                }
            )
            .await,
        Err(StoreError::ContractValidationApplied { .. })
    ));
    assert_eq!(
        store
            .get_run(&scope(), &id("run"))
            .await
            .unwrap()
            .run
            .status,
        RunState::ContractFailed
    );
    assert!(store
        .get_node(&scope(), &id("run"), &id("listen"))
        .await
        .unwrap()
        .result_ref
        .is_none());
}

#[tokio::test]
async fn invalid_payloads_do_not_consume_the_gate_and_cannot_be_laundered() {
    let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    invalid_payload(Arc::new(InMemoryStore::new(clock)), objects).await;
    let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = Arc::new(
        SqliteWorkflowStore::open_url("sqlite::memory:", clock, objects.clone())
            .await
            .unwrap(),
    );
    invalid_payload(store, objects).await;
}

#[tokio::test]
async fn legacy_rejection_remains_a_run_failure() {
    let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = Arc::new(InMemoryStore::new(clock));
    let seed = seed(store.as_ref(), &objects, false, true).await;
    let engine = make_engine(store.clone(), objects.clone(), &seed);
    let gate = open(&engine, store.as_ref()).await;
    decide_approval_json(
        store.as_ref(),
        objects.as_ref(),
        &scope(),
        id("run"),
        gate,
        ApprovalDecision::Reject,
        Some(json!({"verdict":"rejected"})),
        seed.principal,
    )
    .await
    .unwrap();
    let run = store.get_run(&scope(), &id("run")).await.unwrap().run;
    assert_eq!(run.status, RunState::Failed);
    assert_eq!(run.failure_kind, Some(RunFailureKind::ApprovalRejected));
}

async fn expiry<S: WorkflowStore + 'static, F: FnOnce() -> Fut, Fut: Future<Output = ()>>(
    store: Arc<S>,
    objects: Arc<InMemoryObjectStore<TestClock>>,
    advance: F,
) {
    let seed = seed(store.as_ref(), &objects, true, false).await;
    let engine = make_engine(store.clone(), objects.clone(), &seed);
    let gate_id = open(&engine, store.as_ref()).await;
    advance().await;
    // Maintenance succeeds with payload=null although a human must provide an object.
    engine.tick(&scope()).await.unwrap();
    let result = output(store.as_ref(), &objects).await;
    assert_eq!(result["decision"], "reject");
    assert_eq!(result["source"], "expiry");
    assert_eq!(result["payload"], Value::Null);
    assert_eq!(result["principal"], Value::Null);
    assert_eq!(
        store
            .get_gate(&scope(), &id("run"), &gate_id)
            .await
            .unwrap()
            .status,
        GateState::ExpiredRejected
    );
    engine.run_until_idle(&scope(), 12).await.unwrap();
    assert_eq!(
        store
            .get_run(&scope(), &id("run"))
            .await
            .unwrap()
            .run
            .status,
        RunState::Succeeded
    );
    assert_eq!(
        store
            .get_node(&scope(), &id("run"), &id("repair"))
            .await
            .unwrap()
            .status,
        NodeState::Succeeded
    );
}

#[tokio::test]
async fn expiry_outputs_rejection_data_without_applying_the_human_schema() {
    let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    expiry(store, objects, || async {
        clock.advance_ms(10_001).unwrap();
    })
    .await;
    let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = Arc::new(
        SqliteWorkflowStore::open_url("sqlite::memory:", clock, objects.clone())
            .await
            .unwrap(),
    );
    let advance_store = store.clone();
    expiry(store, objects, move || async move {
        advance_store
            .advance_database_clock_ms(10_001)
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn sqlite_restart_hydrates_the_pinned_decision_schema() {
    let temp = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let path = temp.path().join("approval.db");
    let store = Arc::new(
        SqliteWorkflowStore::open(&path, clock.clone(), objects.clone())
            .await
            .unwrap(),
    );
    let seed = seed(store.as_ref(), &objects, true, true).await;
    let engine = make_engine(store.clone(), objects.clone(), &seed);
    let gate_id = open(&engine, store.as_ref()).await;
    engine.release_scope(&scope()).await.unwrap();
    drop(engine);
    drop(store);
    let reopened = Arc::new(
        SqliteWorkflowStore::open(&path, clock, objects.clone())
            .await
            .unwrap(),
    );
    assert_eq!(
        reopened
            .get_revision(
                &scope(),
                &id("definition"),
                &reopened
                    .get_run(&scope(), &id("run"))
                    .await
                    .unwrap()
                    .run
                    .revision_hash
            )
            .await
            .unwrap()
            .approval_schema_refs["listen"]
            .0
            .digest,
        *seed.payload_schema.digest()
    );
    decide_approval_json(
        reopened.as_ref(),
        objects.as_ref(),
        &scope(),
        id("run"),
        gate_id,
        ApprovalDecision::Reject,
        Some(json!({"verdict":"rejected","defect_tags":["mix.clipping"]})),
        seed.principal.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        output(reopened.as_ref(), &objects).await["payload"]["verdict"],
        "rejected"
    );
    let engine = make_engine(reopened.clone(), objects, &seed);
    engine.acquire_scope(&scope()).await.unwrap();
    engine.run_until_idle(&scope(), 12).await.unwrap();
    assert_eq!(
        reopened
            .get_run(&scope(), &id("run"))
            .await
            .unwrap()
            .run
            .status,
        RunState::Succeeded
    );
    assert_eq!(
        reopened
            .get_node(&scope(), &id("run"), &id("repair"))
            .await
            .unwrap()
            .status,
        NodeState::Succeeded
    );
}
