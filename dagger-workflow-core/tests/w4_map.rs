use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, InMemoryActionRegistry, WorkflowAction,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::ObjectStore;
use dagger_workflow_core::definition::{
    ActionReference, BackoffPolicy, BindingSource, MapBinding, MapBindingSource, NodeDefinition,
    PublishableDefinition, RetryPolicy, TimeoutPolicy, WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::event::EventType;
use dagger_workflow_core::ids::{map_child_id, CostUnits, Digest, Id, Timestamp};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::run::{NodeFailureKind, NodeState, RunFailureKind, RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::store::{
    CreateDefinition, CreateRun, EventPageRequest, PageRequest, PublishRevision,
    ResolvedActionSchemas, WorkflowStore,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

fn block_on<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Runtime::new().unwrap().block_on(future)
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
        tenant_id: ScopeAtom::new("w4-tenant").unwrap(),
        namespace: ScopeAtom::new("map").unwrap(),
    }
}

#[derive(Clone)]
struct MapAction {
    descriptor: ActionDescriptor,
}

impl WorkflowAction for MapAction {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    fn invoke<'a>(
        &'a self,
        _context: ActionContext,
        canonical_bound_input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            let value: Value = serde_json::from_slice(canonical_bound_input).unwrap();
            if value.pointer("/item") == Some(&Value::String("fail".to_owned())) {
                ActionOutcome::permanent(
                    "fixture.map_permanent".to_owned(),
                    "requested permanent Map child failure".to_owned(),
                    None,
                    CostUnits(1),
                )
                .unwrap()
            } else if value.pointer("/item") == Some(&Value::String("invalid-output".to_owned())) {
                ActionOutcome::success(json!({"invalid": true}), Vec::new(), CostUnits(1), None)
                    .unwrap()
            } else {
                ActionOutcome::success(value, Vec::new(), CostUnits(1), None).unwrap()
            }
        })
    }
}

/// The narrowest typed object schema the supported subset accepts: section 14.3
/// requires a closed field set on every object schema. `{}` is the separate opaque
/// marker for accepting any JSON value.
const SCHEMA: &[u8] = br#"{"additionalProperties":false,"properties":{"item":{"additionalProperties":false,"properties":{"same":{"type":"boolean"}},"type":["integer","object","string"]}},"required":["item"],"type":"object"}"#;

const RUN_INPUT_SCHEMA: &[u8] = br#"{"additionalProperties":false,"type":"object"}"#;

/// The Succeed node here is bound to the Map, so its output is the ordered
/// aggregate of the children (), which is an ARRAY and
/// can never satisfy `SCHEMA`. `resolve_terminal_node` validates that output
/// against the pinned root output schema per N16, so the root output needs its
/// own document. `items` is the union of the map items these fixtures echo back:
/// integers, strings, and `{"same": bool}` objects.
const ROOT_OUTPUT_SCHEMA: &[u8] = br#"{"items":{"additionalProperties":false,"properties":{"item":{"additionalProperties":false,"properties":{"same":{"type":"boolean"}},"type":["integer","object","string"]}},"required":["item"],"type":"object"},"type":"array"}"#;

fn descriptor() -> ActionDescriptor {
    ActionDescriptor {
        name: "fixture.w4-map".to_owned(),
        contract_version: "1".to_owned(),
        input_schema_digest: hash(SCHEMA),
        output_schema_digest: hash(SCHEMA),
        implementation_compatibility_digest: hash(b"w4-map-implementation"),
    }
}

fn definition(definition_id: &str, items: Value, max_items: u32) -> WorkflowDefinition {
    let descriptor = descriptor();
    WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: id(definition_id),
        name: definition_id.to_owned(),
        description: String::new(),
        run_input_schema_digest: hash(RUN_INPUT_SCHEMA),
        run_output_schema_digest: hash(ROOT_OUTPUT_SCHEMA),
        nodes: vec![
            NodeDefinition::Map {
                id: id("map"),
                items: BindingSource::Constant { value: items },
                max_items,
                on_child_failure:
                    dagger_workflow_core::definition::MapChildFailurePolicy::CancelSiblings,
                min_successful_children: None,
                item_key_pointer: None,
                max_concurrency: 1,
                action: ActionReference {
                    name: descriptor.name,
                    contract_version: descriptor.contract_version,
                    input_schema_digest: descriptor.input_schema_digest,
                    output_schema_digest: descriptor.output_schema_digest,
                    compatible_implementation_requirement: descriptor
                        .implementation_compatibility_digest,
                },
                bindings: vec![MapBinding {
                    target: "/item".to_owned(),
                    source: MapBindingSource::MapItem {
                        pointer: String::new(),
                    },
                }],
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

async fn publish_and_create(
    store: &InMemoryStore<TestClock>,
    objects: &InMemoryObjectStore<TestClock>,
    execution_scope: &ExecutionScope,
    workflow: WorkflowDefinition,
    run_id: &str,
) {
    let schema = objects
        .put(execution_scope, SCHEMA, "application/json")
        .await
        .unwrap();
    let run_input_schema = objects
        .put(execution_scope, RUN_INPUT_SCHEMA, "application/json")
        .await
        .unwrap();
    let root_schema: &[u8] = if workflow.run_output_schema_digest == hash(b"{}") {
        b"{}"
    } else {
        ROOT_OUTPUT_SCHEMA
    };
    let root_output_schema = objects
        .put(execution_scope, root_schema, "application/json")
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
    let principal = AuthenticatedPrincipal::mint(
        execution_scope.clone(),
        "w4-tester".to_owned(),
        Vec::new(),
        hash(b"w4-auth"),
    )
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
    let ranks = workflow
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
            (
                node_id,
                dagger_workflow_core::ids::TopologicalRank(index as u32),
            )
        })
        .collect();
    let mut action_schemas = BTreeMap::new();
    action_schemas.insert(
        "map/map_action".to_owned(),
        ResolvedActionSchemas {
            input_schema: schema.clone(),
            output_schema: schema.clone(),
        },
    );
    store
        .publish_revision(
            execution_scope,
            PublishRevision {
                definition_id: workflow.definition_id.clone(),
                expected_definition_version: dagger_workflow_core::ids::Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema,
                run_output_schema: root_output_schema,
                resolved_action_schema_objects: action_schemas,
                parsed_revision: PublishableDefinition {
                    definition: workflow.clone(),
                    topological_ranks: ranks,
                },
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let input = objects
        .put(execution_scope, br#"{}"#, "application/json")
        .await
        .unwrap();
    store
        .create_run(
            execution_scope,
            CreateRun {
                run_id: id(run_id),
                definition_id: workflow.definition_id,
                revision_hash: canonical.digest().clone(),
                input,
                budget_limit: CostUnits(100),
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
                idempotency_token: format!("w4-create-token-{run_id}"),
            },
        )
        .await
        .unwrap();
}

fn engine(
    store: Arc<InMemoryStore<TestClock>>,
    objects: Arc<InMemoryObjectStore<TestClock>>,
    instance_id: &str,
) -> WorkflowEngine<InMemoryStore<TestClock>, InMemoryObjectStore<TestClock>, InMemoryActionRegistry>
{
    let registry = Arc::new(InMemoryActionRegistry::new());
    registry
        .register(Arc::new(MapAction {
            descriptor: descriptor(),
        }))
        .unwrap();
    WorkflowEngine::new(
        store,
        objects,
        registry,
        EngineConfig {
            instance_id: id(instance_id),
            max_concurrency: 1,
            cancellation_grace: std::time::Duration::from_secs(1),
        },
    )
    .unwrap()
}

async fn children(
    store: &InMemoryStore<TestClock>,
    execution_scope: &ExecutionScope,
    run_id: &str,
) -> Vec<dagger_workflow_core::run::NodeRun> {
    store
        .list_nodes(
            execution_scope,
            &id(run_id),
            PageRequest {
                cursor: None,
                page_size: 100,
            },
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|node| node.parent_map_instance_id.as_ref() == Some(&id("map")))
        .collect()
}

#[test]
fn over_limit_map_fails_contract_before_creating_children() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(1_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        publish_and_create(
            &store,
            &objects,
            &execution_scope,
            definition("w4-over-limit", json!([1, 2]), 1),
            "over-limit",
        )
        .await;
        let engine = engine(store.clone(), objects, "w4-over-limit-engine");
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("over-limit"))
            .await
            .unwrap();
        engine.tick(&execution_scope).await.unwrap();
        let run = store
            .get_run(&execution_scope, &id("over-limit"))
            .await
            .unwrap()
            .run;
        assert_eq!(run.status, RunState::ContractFailed);
        assert_eq!(run.failure_kind, Some(RunFailureKind::MapBoundExceeded));
        assert_eq!(
            store
                .get_node(&execution_scope, &id("over-limit"), &id("map"))
                .await
                .unwrap()
                .status,
            NodeState::ContractFailed
        );
        assert!(children(&store, &execution_scope, "over-limit")
            .await
            .is_empty());
        let events = store
            .list_events_after(
                &execution_scope,
                &id("over-limit"),
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1_000,
                    hard_response_byte_limit: 1_000_000,
                },
            )
            .await
            .unwrap();
        assert!(events
            .iter()
            .any(|event| event.event_type == EventType::NodeContractFailed));
        assert!(events
            .iter()
            .any(|event| event.event_type == EventType::RunContractFailed));
    });
}

#[test]
fn child_ids_are_reconstructible_per_run_and_distinct_across_runs() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(2_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        let workflow = definition("w4-identities", json!(["same", "same"]), 2);
        publish_and_create(&store, &objects, &execution_scope, workflow, "first").await;
        let revision_hash = store
            .get_run(&execution_scope, &id("first"))
            .await
            .unwrap()
            .run
            .revision_hash;
        let input = objects
            .put(&execution_scope, br#"{}"#, "application/json")
            .await
            .unwrap();
        let principal = AuthenticatedPrincipal::mint(
            execution_scope.clone(),
            "w4-tester".to_owned(),
            Vec::new(),
            hash(b"w4-auth-second-run"),
        )
        .unwrap();
        store
            .create_run(
                &execution_scope,
                CreateRun {
                    run_id: id("second"),
                    definition_id: id("w4-identities"),
                    revision_hash,
                    input,
                    budget_limit: CostUnits(100),
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
                    idempotency_token: "w4-create-token-second".to_owned(),
                },
            )
            .await
            .unwrap();
        let engine = engine(store.clone(), objects, "w4-identities-engine");
        engine.acquire_scope(&execution_scope).await.unwrap();
        for run_id in ["first", "second"] {
            engine.start(&execution_scope, &id(run_id)).await.unwrap();
            engine.tick(&execution_scope).await.unwrap();
        }
        let first = children(&store, &execution_scope, "first").await;
        let second = children(&store, &execution_scope, "second").await;
        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 2);
        for child in &first {
            assert_eq!(
                child.node_instance_id,
                map_child_id(
                    &id("first"),
                    &id("map"),
                    child.map_item_index.unwrap(),
                    child.map_item_digest.as_ref().unwrap()
                )
            );
        }
        assert_ne!(first[0].node_instance_id, second[0].node_instance_id);
        assert_ne!(first[0].node_instance_id, first[1].node_instance_id);
    });
}

#[test]
fn zero_item_map_succeeds_with_a_verified_empty_aggregate() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(3_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        publish_and_create(
            &store,
            &objects,
            &execution_scope,
            definition("w4-zero", json!([]), 3),
            "zero",
        )
        .await;
        let engine = engine(store.clone(), objects.clone(), "w4-zero-engine");
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine.start(&execution_scope, &id("zero")).await.unwrap();
        engine.run_until_idle(&execution_scope, 4).await.unwrap();
        let parent = store
            .get_node(&execution_scope, &id("zero"), &id("map"))
            .await
            .unwrap();
        assert_eq!(parent.status, NodeState::Succeeded);
        let aggregate = objects
            .get(&execution_scope, &parent.result_ref.unwrap().0.digest)
            .await
            .unwrap();
        assert_eq!(aggregate.bytes, b"[]");
        assert_eq!(
            store
                .get_run(&execution_scope, &id("zero"))
                .await
                .unwrap()
                .run
                .status,
            RunState::Succeeded
        );
    });
}

#[test]
fn duplicate_items_produce_distinct_children_and_idempotency_keys() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(4_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        publish_and_create(
            &store,
            &objects,
            &execution_scope,
            definition("w4-duplicates", json!([{"same": true}, {"same": true}]), 2),
            "duplicates",
        )
        .await;
        let engine = engine(store.clone(), objects, "w4-duplicates-engine");
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("duplicates"))
            .await
            .unwrap();
        engine.tick(&execution_scope).await.unwrap();
        let children = children(&store, &execution_scope, "duplicates").await;
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].map_item_digest, children[1].map_item_digest);
        assert_ne!(children[0].node_instance_id, children[1].node_instance_id);
        engine.run_until_idle(&execution_scope, 6).await.unwrap();
        let events = store
            .list_events_after(
                &execution_scope,
                &id("duplicates"),
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1_000,
                    hard_response_byte_limit: 1_000_000,
                },
            )
            .await
            .unwrap();
        let child_attempts = events
            .iter()
            .filter(|event| event.event_type == EventType::AttemptStarted)
            .collect::<Vec<_>>();
        assert_eq!(child_attempts.len(), 2);
        assert_ne!(
            child_attempts[0].node_instance_id,
            child_attempts[1].node_instance_id
        );
        assert_ne!(
            child_attempts[0]
                .payload
                .get("idempotency_key_digest")
                .unwrap(),
            child_attempts[1]
                .payload
                .get("idempotency_key_digest")
                .unwrap()
        );
    });
}

#[test]
fn fail_fast_child_failure_fails_parent_and_cancels_siblings() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(5_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        publish_and_create(
            &store,
            &objects,
            &execution_scope,
            definition("w4-fail-fast", json!(["fail", "later"]), 2),
            "fail-fast",
        )
        .await;
        let engine = engine(store.clone(), objects, "w4-fail-fast-engine");
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("fail-fast"))
            .await
            .unwrap();
        engine.tick(&execution_scope).await.unwrap();
        engine.run_until_idle(&execution_scope, 4).await.unwrap();
        let parent = store
            .get_node(&execution_scope, &id("fail-fast"), &id("map"))
            .await
            .unwrap();
        assert_eq!(parent.status, NodeState::Failed);
        assert_eq!(parent.failure_kind, Some(NodeFailureKind::MapChildFailed));
        assert_eq!(
            store
                .get_run(&execution_scope, &id("fail-fast"))
                .await
                .unwrap()
                .run
                .status,
            RunState::Failed
        );
        assert_eq!(
            store
                .get_run(&execution_scope, &id("fail-fast"))
                .await
                .unwrap()
                .run
                .failure_kind,
            Some(RunFailureKind::MapChildFailed)
        );
        assert!(children(&store, &execution_scope, "fail-fast")
            .await
            .iter()
            .any(|node| node.status == NodeState::Cancelled));
        let events = store
            .list_events_after(
                &execution_scope,
                &id("fail-fast"),
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1_000,
                    hard_response_byte_limit: 1_000_000,
                },
            )
            .await
            .unwrap();
        assert!(events
            .iter()
            .any(|event| event.event_type == EventType::MapFailedFast));
        assert!(events
            .iter()
            .any(|event| event.event_type == EventType::NodeCancelled));
    });
}

fn continue_definition(name: &str, items: Value, minimum: Option<u32>) -> WorkflowDefinition {
    let mut workflow = definition(name, items, 3);
    workflow.run_output_schema_digest = hash(b"{}");
    if let NodeDefinition::Map {
        on_child_failure,
        min_successful_children,
        ..
    } = &mut workflow.nodes[0]
    {
        *on_child_failure = dagger_workflow_core::definition::MapChildFailurePolicy::Continue;
        *min_successful_children = minimum;
    }
    workflow
}

#[test]
fn continue_records_every_child_and_successful_projection() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(6_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        let mut workflow = continue_definition("w4-continue", json!(["fail", "later"]), Some(1));
        if let NodeDefinition::Succeed { output, .. } = &mut workflow.nodes[1] {
            *output = BindingSource::MapAggregate {
                node_id: id("map"),
                pointer: "/item".to_owned(),
            };
        }
        publish_and_create(&store, &objects, &execution_scope, workflow, "continue").await;
        let engine = engine(store.clone(), objects.clone(), "w4-continue-engine");
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("continue"))
            .await
            .unwrap();
        engine.run_until_idle(&execution_scope, 8).await.unwrap();
        let parent = store
            .get_node(&execution_scope, &id("continue"), &id("map"))
            .await
            .unwrap();
        assert_eq!(parent.status, NodeState::Succeeded);
        let bytes = objects
            .get(&execution_scope, &parent.result_ref.unwrap().0.digest)
            .await
            .unwrap()
            .bytes;
        let result: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(result[0]["index"], 0);
        assert_eq!(result[0]["status"], "failed");
        assert_eq!(result[0]["error_class"], "permanent");
        assert!(result[0]["message"].as_str().is_some());
        assert!(result[0]["output"].is_null());
        assert_eq!(result[1]["status"], "succeeded");
        assert_eq!(result[1]["output"], json!({"item":"later"}));
        assert!(result[1]["error_class"].is_null());
        let run = store
            .get_run(&execution_scope, &id("continue"))
            .await
            .unwrap()
            .run;
        assert_eq!(run.status, RunState::Succeeded);
        let output = objects
            .get(&execution_scope, &run.output_ref.unwrap().0.digest)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output.bytes).unwrap(),
            json!(["later"])
        );
    });
}

#[test]
fn continue_default_threshold_fails_after_siblings_finish() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(7_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        publish_and_create(
            &store,
            &objects,
            &execution_scope,
            continue_definition("w4-threshold", json!(["fail", "later"]), None),
            "threshold",
        )
        .await;
        let engine = engine(store.clone(), objects, "w4-threshold-engine");
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("threshold"))
            .await
            .unwrap();
        engine.run_until_idle(&execution_scope, 8).await.unwrap();
        let children = children(&store, &execution_scope, "threshold").await;
        assert!(children
            .iter()
            .any(|child| child.status == NodeState::Succeeded));
        assert!(children
            .iter()
            .all(|child| child.status != NodeState::Cancelled));
        let parent = store
            .get_node(&execution_scope, &id("threshold"), &id("map"))
            .await
            .unwrap();
        assert_eq!(parent.status, NodeState::Failed);
        let events = store
            .list_events_after(
                &execution_scope,
                &id("threshold"),
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1000,
                    hard_response_byte_limit: 1_000_000,
                },
            )
            .await
            .unwrap();
        let event = events
            .iter()
            .find(|event| event.event_type == EventType::MapThresholdFailed)
            .unwrap();
        assert_eq!(event.payload["successful_children"], 1);
        assert_eq!(event.payload["min_successful_children"], 2);
    });
}

#[test]
fn threshold_above_actual_count_finishes_children_then_fails_map() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(8_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        publish_and_create(
            &store,
            &objects,
            &execution_scope,
            continue_definition("w4-bad-threshold", json!(["later"]), Some(2)),
            "bad-threshold",
        )
        .await;
        let engine = engine(store.clone(), objects, "w4-bad-threshold-engine");
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("bad-threshold"))
            .await
            .unwrap();
        engine.run_until_idle(&execution_scope, 4).await.unwrap();
        let run = store
            .get_run(&execution_scope, &id("bad-threshold"))
            .await
            .unwrap()
            .run;
        assert_eq!(run.status, RunState::Failed);
        assert_eq!(run.failure_kind, Some(RunFailureKind::MapChildFailed));
        let children = children(&store, &execution_scope, "bad-threshold").await;
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].status, NodeState::Succeeded);
    });
}

#[test]
fn continue_zero_items_defaults_to_zero_successes() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(9_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        publish_and_create(
            &store,
            &objects,
            &execution_scope,
            continue_definition("w4-continue-empty", json!([]), None),
            "continue-empty",
        )
        .await;
        let engine = engine(store.clone(), objects, "w4-continue-empty-engine");
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("continue-empty"))
            .await
            .unwrap();
        engine.run_until_idle(&execution_scope, 4).await.unwrap();
        assert_eq!(
            store
                .get_run(&execution_scope, &id("continue-empty"))
                .await
                .unwrap()
                .run
                .status,
            RunState::Succeeded
        );
    });
}

#[test]
fn continue_does_not_tolerate_action_output_contract_failure() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(10_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        publish_and_create(
            &store,
            &objects,
            &execution_scope,
            continue_definition("w4-contract", json!(["invalid-output", "later"]), Some(0)),
            "contract",
        )
        .await;
        let engine = engine(store.clone(), objects, "w4-contract-engine");
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("contract"))
            .await
            .unwrap();
        engine.run_until_idle(&execution_scope, 8).await.unwrap();
        let run = store
            .get_run(&execution_scope, &id("contract"))
            .await
            .unwrap()
            .run;
        assert_eq!(run.status, RunState::ContractFailed);
        assert_eq!(
            run.failure_kind,
            Some(RunFailureKind::ActionOutputSchemaMismatch)
        );
        assert!(children(&store, &execution_scope, "contract")
            .await
            .iter()
            .any(|child| child.status == NodeState::Cancelled));
    });
}

#[test]
fn empty_map_with_positive_threshold_fails_normally() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(11_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        publish_and_create(
            &store,
            &objects,
            &execution_scope,
            continue_definition("w4-empty-threshold", json!([]), Some(1)),
            "empty-threshold",
        )
        .await;
        let engine = engine(store.clone(), objects, "w4-empty-threshold-engine");
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("empty-threshold"))
            .await
            .unwrap();
        engine.run_until_idle(&execution_scope, 4).await.unwrap();
        let run = store
            .get_run(&execution_scope, &id("empty-threshold"))
            .await
            .unwrap()
            .run;
        assert_eq!(run.status, RunState::Failed);
        assert_eq!(run.failure_kind, Some(RunFailureKind::MapChildFailed));
        assert!(children(&store, &execution_scope, "empty-threshold")
            .await
            .is_empty());
        let events = store
            .list_events_after(
                &execution_scope,
                &id("empty-threshold"),
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1000,
                    hard_response_byte_limit: 1_000_000,
                },
            )
            .await
            .unwrap();
        let event = events
            .iter()
            .find(|event| event.event_type == EventType::MapThresholdFailed)
            .unwrap();
        assert_eq!(event.payload["successful_children"], 0);
        assert_eq!(event.payload["min_successful_children"], 1);
    });
}

struct PendingMapAction {
    descriptor: ActionDescriptor,
    started: Arc<tokio::sync::Notify>,
    invocations: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
    cancelled_at_drop: Arc<AtomicBool>,
    sibling_cancelled: Arc<AtomicBool>,
}

struct PendingInvocationGuard {
    context: ActionContext,
    dropped: Arc<AtomicBool>,
    cancelled_at_drop: Arc<AtomicBool>,
}

impl Drop for PendingInvocationGuard {
    fn drop(&mut self) {
        self.cancelled_at_drop.store(
            self.context.cancellation_token.is_cancelled(),
            Ordering::SeqCst,
        );
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl WorkflowAction for PendingMapAction {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            if self.invocations.fetch_add(1, Ordering::SeqCst) == 0 {
                let _resource = PendingInvocationGuard {
                    context,
                    dropped: self.dropped.clone(),
                    cancelled_at_drop: self.cancelled_at_drop.clone(),
                };
                self.started.notify_one();
                // Models a non-cooperative renderer which releases resources only
                // when its invocation future is dropped after cancellation grace.
                std::future::pending::<ActionOutcome>().await
            } else {
                self.sibling_cancelled
                    .store(context.cancellation_token.is_cancelled(), Ordering::SeqCst);
                ActionOutcome::success(
                    serde_json::from_slice(input).unwrap(),
                    Vec::new(),
                    CostUnits(1),
                    None,
                )
                .unwrap()
            }
        })
    }
}

#[test]
fn continue_timeout_cancels_only_its_attempt_and_releases_slot_for_sibling() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(12_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
        publish_and_create(
            &store,
            &objects,
            &execution_scope,
            continue_definition("w4-timeout-slot", json!(["first", "second"]), Some(1)),
            "timeout-slot",
        )
        .await;
        let started = Arc::new(tokio::sync::Notify::new());
        let invocations = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let cancelled_at_drop = Arc::new(AtomicBool::new(false));
        let sibling_cancelled = Arc::new(AtomicBool::new(false));
        let registry = Arc::new(InMemoryActionRegistry::new());
        registry
            .register(Arc::new(PendingMapAction {
                descriptor: descriptor(),
                started: started.clone(),
                invocations: invocations.clone(),
                dropped: dropped.clone(),
                cancelled_at_drop: cancelled_at_drop.clone(),
                sibling_cancelled: sibling_cancelled.clone(),
            }))
            .unwrap();
        let engine = WorkflowEngine::new(
            store.clone(),
            objects.clone(),
            registry,
            EngineConfig {
                instance_id: id("w4-timeout-slot-engine"),
                max_concurrency: 1,
                cancellation_grace: std::time::Duration::from_millis(10),
            },
        )
        .unwrap();
        engine.acquire_scope(&execution_scope).await.unwrap();
        engine
            .start(&execution_scope, &id("timeout-slot"))
            .await
            .unwrap();
        engine.tick(&execution_scope).await.unwrap();
        engine.tick(&execution_scope).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        clock.advance_ms(1_000).unwrap();
        engine.tick(&execution_scope).await.unwrap();
        assert_eq!(
            store
                .get_run(&execution_scope, &id("timeout-slot"))
                .await
                .unwrap()
                .run
                .status,
            RunState::Running
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            engine.run_until_idle(&execution_scope, 8),
        )
        .await
        .expect("timed-out invocation held the sole execution slot")
        .unwrap();
        assert!(dropped.load(Ordering::SeqCst));
        assert!(cancelled_at_drop.load(Ordering::SeqCst));
        assert!(!sibling_cancelled.load(Ordering::SeqCst));
        assert_eq!(invocations.load(Ordering::SeqCst), 2);
        let run = store
            .get_run(&execution_scope, &id("timeout-slot"))
            .await
            .unwrap()
            .run;
        assert_eq!(run.status, RunState::Succeeded);
        let child_nodes = children(&store, &execution_scope, "timeout-slot").await;
        assert_eq!(
            child_nodes
                .iter()
                .filter(|child| child.status == NodeState::Succeeded)
                .count(),
            1
        );
        assert_eq!(
            child_nodes
                .iter()
                .filter(|child| child.status == NodeState::RetriesExhausted)
                .count(),
            1
        );
        let parent = store
            .get_node(&execution_scope, &id("timeout-slot"), &id("map"))
            .await
            .unwrap();
        let result = objects
            .get(&execution_scope, &parent.result_ref.unwrap().0.digest)
            .await
            .unwrap();
        let envelopes: Value = serde_json::from_slice(&result.bytes).unwrap();
        assert!(envelopes
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["error_class"] == "timeout"));
    });
}
