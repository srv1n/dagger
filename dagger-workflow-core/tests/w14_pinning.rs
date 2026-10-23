use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, ActionRegistry, ArtifactOutput,
    InMemoryActionRegistry, WorkflowAction,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::{ArtifactKind, ArtifactRefValue, ObjectStore};
use dagger_workflow_core::definition::{
    canonical_topological_ranks, ActionReference, BackoffPolicy, Binding, BindingSource,
    MapBinding, MapBindingSource, MapChildFailurePolicy, NodeDefinition, PublishableDefinition,
    RetryPolicy, TimeoutPolicy, WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::ids::{
    artifact_ref_id, map_child_id, map_expansion_digest, ArtifactRefIdentity, CostUnits, Digest,
    Id, MapChildIdentity, Timestamp, Version,
};
use dagger_workflow_core::memoization::{CacheMode, ReuseKind, ReuseProvenance};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::pinning::{PinTarget, PinnedOutput, RunOptions};
use dagger_workflow_core::run::{NodeState, RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::store::{
    ClaimNodeAttempt, ClaimNodeAttemptResult, ClaimReusableNodeAttempt, CompleteAttempt,
    CompletionObjects, CreateDefinition, CreateRun, CreateRunWithOptions, ExpandMap,
    OrderedMapItem, PageRequest, PublishRevision, ResolvedActionSchemas, StartRun, StoreError,
    WorkflowStore,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn hash(bytes: &[u8]) -> Digest {
    Digest::new(format!("sha256:{:x}", Sha256::digest(bytes))).unwrap()
}

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}

fn scope(tenant: &str) -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new(tenant).unwrap(),
        namespace: ScopeAtom::new("pinning").unwrap(),
    }
}

fn principal(scope: &ExecutionScope) -> AuthenticatedPrincipal {
    AuthenticatedPrincipal::mint(scope.clone(), "owner".into(), Vec::new(), hash(b"auth")).unwrap()
}

fn limits() -> RunLimits {
    RunLimits {
        max_dynamic_node_instances: 10,
        max_total_attempts: 10,
        max_total_events: 1_000,
        max_inline_json_bytes_per_value: 100_000,
        max_artifacts_per_attempt: 10,
        max_aggregate_object_bytes_per_run: 1_000_000,
        max_run_lifetime_ms: 100_000,
    }
}

fn descriptor(output_schema: &[u8]) -> ActionDescriptor {
    ActionDescriptor {
        name: "fixture.pinned-part".into(),
        contract_version: "1".into(),
        input_schema_digest: hash(b"{}"),
        output_schema_digest: hash(output_schema),
        implementation_compatibility_digest: hash(b"pinning-v1"),
    }
}

struct PartAction<O> {
    objects: Arc<O>,
    descriptor: ActionDescriptor,
    calls: Arc<AtomicUsize>,
}

impl<O: ObjectStore> WorkflowAction for PartAction<O> {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut output: Value = serde_json::from_slice(input).unwrap();
            let object = self
                .objects
                .put(&context.scope, b"locked-part", "audio/wav")
                .await
                .unwrap();
            let reference = ArtifactRefValue {
                artifact_ref_id: artifact_ref_id(ArtifactRefIdentity {
                    scope: &context.scope,
                    digest: object.digest(),
                    kind: ArtifactKind::ActionArtifact,
                    producer_run_id: Some(&context.run_id),
                    producer_node_id: Some(&context.node_instance_id),
                    producer_attempt_id: Some(&context.attempt_id),
                    ordinal: 0,
                }),
                digest: object.digest().clone(),
                size_bytes: object.size_bytes().to_string(),
                media_type: object.media_type().to_owned(),
            };
            output["artifact"] = serde_json::to_value(reference).unwrap();
            ActionOutcome::success(
                output,
                vec![ArtifactOutput {
                    media_type: "audio/wav".into(),
                    object,
                }],
                CostUnits(1),
                None,
            )
            .unwrap()
        })
    }
}

fn workflow(definition_id: &str, output_schema: &[u8]) -> WorkflowDefinition {
    let action = descriptor(output_schema);
    WorkflowDefinition {
        definition_format_version: "0.1".into(),
        definition_id: id(definition_id),
        name: definition_id.into(),
        description: String::new(),
        run_input_schema_digest: hash(b"{}"),
        run_output_schema_digest: hash(b"{}"),
        nodes: vec![
            NodeDefinition::Map {
                id: id("parts"),
                items: BindingSource::RunInput {
                    pointer: "/items".into(),
                },
                max_items: 4,
                max_concurrency: 2,
                on_child_failure: MapChildFailurePolicy::CancelSiblings,
                min_successful_children: None,
                item_key_pointer: Some("/id".into()),
                action: ActionReference {
                    name: action.name,
                    contract_version: action.contract_version,
                    input_schema_digest: action.input_schema_digest,
                    output_schema_digest: action.output_schema_digest,
                    compatible_implementation_requirement: action
                        .implementation_compatibility_digest,
                },
                bindings: vec![
                    MapBinding {
                        target: "/item".into(),
                        source: MapBindingSource::MapItem {
                            pointer: "/id".into(),
                        },
                    },
                    MapBinding {
                        target: "/version".into(),
                        source: MapBindingSource::MapItem {
                            pointer: "/version".into(),
                        },
                    },
                ],
                retry: RetryPolicy {
                    max_attempts: 1,
                    backoff: BackoffPolicy::Fixed { delay_ms: 0 },
                },
                timeout: TimeoutPolicy { timeout_ms: 10_000 },
                declared_max_cost_units: CostUnits(1),
                next: vec![id("succeed")],
            },
            NodeDefinition::Succeed {
                id: id("succeed"),
                output: BindingSource::NodeOutput {
                    node_id: id("parts"),
                    pointer: String::new(),
                },
            },
        ],
    }
}

async fn publish<S: WorkflowStore, O: ObjectStore>(
    store: &S,
    objects: &O,
    scope: &ExecutionScope,
    workflow: WorkflowDefinition,
    output_schema: &[u8],
) -> Digest {
    store
        .create_definition(
            scope,
            CreateDefinition {
                definition_id: workflow.definition_id.clone(),
                display_name: workflow.name.clone(),
                description: String::new(),
                principal: principal(scope),
            },
        )
        .await
        .unwrap();
    let schema = objects.put(scope, b"{}", "application/json").await.unwrap();
    let output_schema = objects
        .put(scope, output_schema, "application/json")
        .await
        .unwrap();
    let canonical = objects
        .put(
            scope,
            &serde_jcs::to_vec(&workflow).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let mut schemas = BTreeMap::new();
    let reference_location = match &workflow.nodes[0] {
        NodeDefinition::Map { .. } => "parts/map_action",
        _ => "parts",
    };
    schemas.insert(
        reference_location.into(),
        ResolvedActionSchemas {
            input_schema: schema.clone(),
            output_schema,
        },
    );
    store
        .publish_revision(
            scope,
            PublishRevision {
                definition_id: workflow.definition_id.clone(),
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

#[allow(clippy::too_many_arguments)]
async fn create<S: WorkflowStore, O: ObjectStore>(
    store: &S,
    objects: &O,
    scope: &ExecutionScope,
    definition_id: &str,
    revision: &Digest,
    run_id: &str,
    input: Value,
    pins: Vec<PinnedOutput>,
) -> Result<(), StoreError> {
    let input = objects
        .put(
            scope,
            &serde_jcs::to_vec(&input).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    store
        .create_run_with_options(
            scope,
            CreateRunWithOptions {
                command: CreateRun {
                    run_id: id(run_id),
                    definition_id: id(definition_id),
                    revision_hash: revision.clone(),
                    input,
                    budget_limit: CostUnits(10),
                    limits: limits(),
                    principal: principal(scope),
                    idempotency_token: format!("pin-create-token-{run_id}"),
                },
                options: RunOptions {
                    cache_mode: CacheMode::Off,
                    pinned_outputs: pins,
                },
            },
        )
        .await?;
    Ok(())
}

fn engine<S: WorkflowStore + 'static, O: ObjectStore + 'static>(
    store: Arc<S>,
    objects: Arc<O>,
    calls: Arc<AtomicUsize>,
    output_schema: &[u8],
) -> WorkflowEngine<S, O, InMemoryActionRegistry> {
    let registry = Arc::new(InMemoryActionRegistry::new());
    registry
        .register(Arc::new(PartAction {
            objects: objects.clone(),
            descriptor: descriptor(output_schema),
            calls,
        }))
        .unwrap();
    WorkflowEngine::new(
        store,
        objects,
        registry,
        EngineConfig {
            instance_id: id("pin-engine"),
            max_concurrency: 2,
            cancellation_grace: std::time::Duration::from_secs(1),
        },
    )
    .unwrap()
}

async fn pin_bass<S: WorkflowStore, O: ObjectStore>(
    store: &S,
    objects: &O,
    scope: &ExecutionScope,
) -> PinnedOutput {
    let nodes = store
        .list_nodes(
            scope,
            &id("source"),
            PageRequest {
                cursor: None,
                page_size: 100,
            },
        )
        .await
        .unwrap()
        .items;
    for node in nodes {
        if node.parent_map_instance_id.is_none() {
            continue;
        }
        let reference = node.result_ref.unwrap();
        let output: Value =
            serde_json::from_slice(&objects.get(scope, &reference.0.digest).await.unwrap().bytes)
                .unwrap();
        if output["item"] == "bass" {
            return PinnedOutput {
                target: PinTarget::MapItem {
                    map_node_id: id("parts"),
                    item_key: "bass".into(),
                },
                source_run_id: id("source"),
                source_node_id: node.node_instance_id,
                source_attempt_id: reference.0.producer_attempt_id.unwrap(),
            };
        }
    }
    panic!("source bass output missing")
}

async fn execute<S: WorkflowStore + 'static, O: ObjectStore + 'static>(
    engine: &WorkflowEngine<S, O, InMemoryActionRegistry>,
    scope: &ExecutionScope,
    run_id: &str,
) {
    engine.start(scope, &id(run_id)).await.unwrap();
    engine.run_until_idle(scope, 20).await.unwrap();
}

async fn source<S: WorkflowStore + 'static, O: ObjectStore + 'static>(
    store: Arc<S>,
    objects: Arc<O>,
    scope: &ExecutionScope,
    calls: Arc<AtomicUsize>,
) -> (Digest, PinnedOutput) {
    let revision = publish(
        store.as_ref(),
        objects.as_ref(),
        scope,
        workflow("parts-workflow", b"{}"),
        b"{}",
    )
    .await;
    create(
        store.as_ref(),
        objects.as_ref(),
        scope,
        "parts-workflow",
        &revision,
        "source",
        json!({"items":[{"id":"bass","version":1},{"id":"drums","version":1}]}),
        Vec::new(),
    )
    .await
    .unwrap();
    let engine = engine(store.clone(), objects.clone(), calls, b"{}");
    engine.acquire_scope(scope).await.unwrap();
    execute(&engine, scope, "source").await;
    engine.release_scope(scope).await.unwrap();
    let pin = pin_bass(store.as_ref(), objects.as_ref(), scope).await;
    (revision, pin)
}

async fn assert_pinned_run<S: WorkflowStore, O: ObjectStore>(
    store: &S,
    objects: &O,
    scope: &ExecutionScope,
    pin: &PinnedOutput,
) {
    let run = store.get_run(scope, &id("locked")).await.unwrap().run;
    assert_eq!(run.status, RunState::Succeeded);
    assert_eq!(run.budget_consumed, CostUnits(1));
    assert_eq!(run.options.pinned_outputs, vec![pin.clone()]);
    let output: Value = serde_json::from_slice(
        &objects
            .get(scope, &run.output_ref.unwrap().0.digest)
            .await
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(output[0]["item"], "drums");
    assert_eq!(output[0]["version"], 2);
    assert_eq!(output[1]["item"], "bass");
    assert_eq!(output[1]["version"], 1);
    let nodes = store
        .list_nodes(
            scope,
            &id("locked"),
            PageRequest {
                cursor: None,
                page_size: 100,
            },
        )
        .await
        .unwrap()
        .items;
    let bass = nodes
        .iter()
        .find(|node| node.parent_map_instance_id.is_some() && node.map_item_index == Some(1))
        .unwrap();
    assert_eq!(bass.status, NodeState::Succeeded);
    let attempt_id = bass
        .result_ref
        .as_ref()
        .unwrap()
        .0
        .producer_attempt_id
        .as_ref()
        .unwrap();
    let attempt = store
        .get_attempt(scope, &id("locked"), attempt_id)
        .await
        .unwrap();
    let provenance = attempt.reuse_provenance.as_ref().unwrap();
    assert_eq!(provenance.kind, ReuseKind::Pinned);
    assert_eq!(provenance.source_run_id, pin.source_run_id);
    assert_eq!(provenance.source_attempt_id, pin.source_attempt_id);
    assert_eq!(attempt.declared_max_cost, CostUnits(0));
    assert_eq!(attempt.settled_cost, Some(CostUnits(0)));
    let source_attempt = store
        .get_attempt(scope, &pin.source_run_id, &pin.source_attempt_id)
        .await
        .unwrap();
    assert_eq!(
        attempt.artifact_refs[0].digest,
        source_attempt.artifact_refs[0].digest
    );
    assert_ne!(
        attempt.artifact_refs[0].artifact_ref_id,
        source_attempt.artifact_refs[0].artifact_ref_id
    );
    assert_eq!(attempt.artifact_refs[0].producer_run_id, Some(id("locked")));
    assert_eq!(
        output[1]["artifact"]["artifact_ref_id"],
        attempt.artifact_refs[0].artifact_ref_id.as_str()
    );
}

#[tokio::test]
async fn one_map_item_stays_pinned_after_reordering_and_artifacts_are_rehomed() {
    let scope = scope("pin-memory");
    let clock = Arc::new(TestClock::new(Timestamp(1_000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(InMemoryObjectStore::new(clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let (revision, pin) = source(store.clone(), objects.clone(), &scope, calls.clone()).await;
    create(
        store.as_ref(),
        objects.as_ref(),
        &scope,
        "parts-workflow",
        &revision,
        "locked",
        json!({"items":[{"id":"drums","version":2},{"id":"bass","version":2}]}),
        vec![pin.clone()],
    )
    .await
    .unwrap();
    let engine = engine(store.clone(), objects.clone(), calls.clone(), b"{}");
    engine.acquire_scope(&scope).await.unwrap();
    execute(&engine, &scope, "locked").await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_pinned_run(store.as_ref(), objects.as_ref(), &scope, &pin).await;
}

#[tokio::test]
async fn a_pinned_authored_action_completes_with_zero_budget() {
    let scope = scope("pin-static");
    let clock = Arc::new(TestClock::new(Timestamp(1_000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(InMemoryObjectStore::new(clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut workflow = workflow("static-workflow", b"{}");
    let action = match workflow.nodes.remove(0) {
        NodeDefinition::Map {
            action,
            retry,
            timeout,
            declared_max_cost_units,
            next,
            ..
        } => NodeDefinition::Action {
            id: id("parts"),
            action,
            retry,
            timeout,
            declared_max_cost_units,
            next,
            bindings: vec![
                Binding {
                    target: "/item".into(),
                    source: BindingSource::Constant {
                        value: json!("bass"),
                    },
                },
                Binding {
                    target: "/version".into(),
                    source: BindingSource::RunInput {
                        pointer: "/version".into(),
                    },
                },
            ],
        },
        _ => unreachable!(),
    };
    workflow.nodes.insert(0, action);
    let revision = publish(store.as_ref(), objects.as_ref(), &scope, workflow, b"{}").await;
    create(
        store.as_ref(),
        objects.as_ref(),
        &scope,
        "static-workflow",
        &revision,
        "source",
        json!({"version":1}),
        Vec::new(),
    )
    .await
    .unwrap();
    let engine = engine(store.clone(), objects.clone(), calls.clone(), b"{}");
    engine.acquire_scope(&scope).await.unwrap();
    execute(&engine, &scope, "source").await;
    let node = store
        .get_node(&scope, &id("source"), &id("parts"))
        .await
        .unwrap();
    let pin = PinnedOutput {
        target: PinTarget::Node {
            node_id: id("parts"),
        },
        source_run_id: id("source"),
        source_node_id: id("parts"),
        source_attempt_id: node.result_ref.unwrap().0.producer_attempt_id.unwrap(),
    };
    let input = objects
        .put(&scope, br#"{"version":2}"#, "application/json")
        .await
        .unwrap();
    store
        .create_run_with_options(
            &scope,
            CreateRunWithOptions {
                command: CreateRun {
                    run_id: id("locked"),
                    definition_id: id("static-workflow"),
                    revision_hash: revision,
                    input,
                    budget_limit: CostUnits(0),
                    limits: limits(),
                    principal: principal(&scope),
                    idempotency_token: "static-pin-token-locked".into(),
                },
                options: RunOptions {
                    cache_mode: CacheMode::Off,
                    pinned_outputs: vec![pin],
                },
            },
        )
        .await
        .unwrap();
    execute(&engine, &scope, "locked").await;
    let run = store.get_run(&scope, &id("locked")).await.unwrap().run;
    assert_eq!(run.status, RunState::Succeeded);
    assert_eq!(run.budget_consumed, CostUnits(0));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let output: Value = serde_json::from_slice(
        &objects
            .get(&scope, &run.output_ref.unwrap().0.digest)
            .await
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(output["version"], 1);
}

#[tokio::test]
async fn corrupt_pin_bytes_are_a_hard_error_before_an_action_is_claimed() {
    let scope = scope("pin-corruption");
    let clock = Arc::new(TestClock::new(Timestamp(1_000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(InMemoryObjectStore::new(clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let (revision, pin) = source(store.clone(), objects.clone(), &scope, calls.clone()).await;
    create(
        store.as_ref(),
        objects.as_ref(),
        &scope,
        "parts-workflow",
        &revision,
        "locked",
        json!({"items":[{"id":"bass","version":2}]}),
        vec![pin.clone()],
    )
    .await
    .unwrap();
    let attempt = store
        .get_attempt(&scope, &pin.source_run_id, &pin.source_attempt_id)
        .await
        .unwrap();
    assert!(objects.corrupt_bytes(&scope, &attempt.artifact_refs[0].digest, b"broken".to_vec()));
    let engine = engine(store.clone(), objects, calls.clone(), b"{}");
    engine.acquire_scope(&scope).await.unwrap();
    engine.start(&scope, &id("locked")).await.unwrap();
    assert!(engine.run_until_idle(&scope, 20).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        store
            .get_run(&scope, &id("locked"))
            .await
            .unwrap()
            .run
            .total_attempt_count,
        0
    );
}

#[tokio::test]
async fn pins_reject_cross_scope_sources_and_changed_output_schemas() {
    let source_scope = scope("pin-owner");
    let other_scope = scope("pin-other");
    let clock = Arc::new(TestClock::new(Timestamp(1_000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(InMemoryObjectStore::new(clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let (_, pin) = source(store.clone(), objects.clone(), &source_scope, calls.clone()).await;
    let revision = publish(
        store.as_ref(),
        objects.as_ref(),
        &other_scope,
        workflow("foreign-target", b"{}"),
        b"{}",
    )
    .await;
    create(
        store.as_ref(),
        objects.as_ref(),
        &other_scope,
        "foreign-target",
        &revision,
        "locked",
        json!({"items":[{"id":"bass","version":2}]}),
        vec![pin.clone()],
    )
    .await
    .unwrap();
    let foreign_engine = engine(store.clone(), objects.clone(), calls.clone(), b"{}");
    foreign_engine.acquire_scope(&other_scope).await.unwrap();
    foreign_engine
        .start(&other_scope, &id("locked"))
        .await
        .unwrap();
    assert!(foreign_engine
        .run_until_idle(&other_scope, 20)
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let changed_schema = br#"{"type":"string"}"#;
    let revision = publish(
        store.as_ref(),
        objects.as_ref(),
        &source_scope,
        workflow("changed-schema", changed_schema),
        changed_schema,
    )
    .await;
    create(
        store.as_ref(),
        objects.as_ref(),
        &source_scope,
        "changed-schema",
        &revision,
        "locked",
        json!({"items":[{"id":"bass","version":2}]}),
        vec![pin],
    )
    .await
    .unwrap();
    let changed_engine = engine(
        store.clone(),
        objects.clone(),
        calls.clone(),
        changed_schema,
    );
    changed_engine.acquire_scope(&source_scope).await.unwrap();
    changed_engine
        .start(&source_scope, &id("locked"))
        .await
        .unwrap();
    assert!(changed_engine
        .run_until_idle(&source_scope, 20)
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn malformed_targets_and_duplicate_or_missing_map_keys_are_rejected() {
    let scope = scope("pin-validation");
    let clock = Arc::new(TestClock::new(Timestamp(1_000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(InMemoryObjectStore::new(clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let (revision, pin) = source(store.clone(), objects.clone(), &scope, calls.clone()).await;
    let mut malformed = pin.clone();
    malformed.target = PinTarget::Node {
        node_id: id("parts"),
    };
    assert!(create(
        store.as_ref(),
        objects.as_ref(),
        &scope,
        "parts-workflow",
        &revision,
        "malformed",
        json!({"items":[]}),
        vec![malformed]
    )
    .await
    .is_err());
    assert!(store.get_run(&scope, &id("malformed")).await.is_err());
    let engine = engine(store.clone(), objects.clone(), calls.clone(), b"{}");
    engine.acquire_scope(&scope).await.unwrap();
    for (run_id, items, pins) in [
        (
            "duplicate",
            json!([{"id":"bass","version":2},{"id":"bass","version":2}]),
            Vec::new(),
        ),
        ("missing", json!([{"id":"drums","version":2}]), vec![pin]),
    ] {
        create(
            store.as_ref(),
            objects.as_ref(),
            &scope,
            "parts-workflow",
            &revision,
            run_id,
            json!({"items":items}),
            pins,
        )
        .await
        .unwrap();
        execute(&engine, &scope, run_id).await;
        let run = store.get_run(&scope, &id(run_id)).await.unwrap().run;
        assert_eq!(run.status, RunState::ContractFailed);
        assert_eq!(run.dynamic_node_count, 0);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

async fn reject_forged_claims<S: WorkflowStore + 'static, O: ObjectStore + 'static>(
    store: Arc<S>,
    objects: Arc<O>,
    scope: &ExecutionScope,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let (revision, pin) = source(store.clone(), objects.clone(), scope, calls.clone()).await;
    let source_drums = store
        .list_nodes(
            scope,
            &id("source"),
            PageRequest {
                cursor: None,
                page_size: 100,
            },
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|node| node.map_item_key.as_deref() == Some("drums"))
        .unwrap();
    let drum_attempt = source_drums
        .result_ref
        .unwrap()
        .0
        .producer_attempt_id
        .unwrap();
    let changed_schema = br#"{"type":"string"}"#;
    let changed_revision = publish(
        store.as_ref(),
        objects.as_ref(),
        scope,
        workflow("changed-schema", changed_schema),
        changed_schema,
    )
    .await;
    let acquired = store
        .acquire_engine_claim(scope, id("direct-pin-engine"))
        .await
        .unwrap();
    for (
        run_id,
        node_index,
        authorized_pins,
        source_node,
        source_attempt,
        output_schema,
        target_revision,
        definition_id,
    ) in [
        (
            "unapproved",
            0_u32,
            Vec::new(),
            pin.source_node_id.clone(),
            pin.source_attempt_id.clone(),
            b"{}".as_slice(),
            &revision,
            "parts-workflow",
        ),
        (
            "wrong-target",
            1,
            vec![pin.clone()],
            pin.source_node_id.clone(),
            pin.source_attempt_id.clone(),
            b"{}".as_slice(),
            &revision,
            "parts-workflow",
        ),
        (
            "wrong-source",
            0,
            vec![pin.clone()],
            source_drums.node_instance_id,
            drum_attempt,
            b"{}".as_slice(),
            &revision,
            "parts-workflow",
        ),
        (
            "wrong-schema",
            0,
            vec![pin.clone()],
            pin.source_node_id.clone(),
            pin.source_attempt_id.clone(),
            changed_schema.as_slice(),
            &changed_revision,
            "changed-schema",
        ),
        (
            "stale-pin",
            0,
            vec![pin.clone()],
            pin.source_node_id.clone(),
            pin.source_attempt_id.clone(),
            b"{}".as_slice(),
            &revision,
            "parts-workflow",
        ),
    ] {
        let items = json!([{"id":"bass","version":2},{"id":"drums","version":2}]);
        create(
            store.as_ref(),
            objects.as_ref(),
            scope,
            definition_id,
            target_revision,
            run_id,
            json!({"items":items}),
            authorized_pins,
        )
        .await
        .unwrap();
        let registry = InMemoryActionRegistry::new();
        registry
            .register(Arc::new(PartAction {
                objects: objects.clone(),
                descriptor: descriptor(output_schema),
                calls: calls.clone(),
            }))
            .unwrap();
        let revision_record = store
            .get_revision(scope, &id(definition_id), target_revision)
            .await
            .unwrap();
        store
            .start_run(
                scope,
                StartRun {
                    permit: acquired.permit.clone(),
                    run_id: id(run_id),
                    compatibility_evidence: registry.check_pins(&revision_record.action_pins),
                },
            )
            .await
            .unwrap();
        let parent = store
            .get_node(scope, &id(run_id), &id("parts"))
            .await
            .unwrap();
        let map_input = objects
            .put(
                scope,
                &serde_jcs::to_vec(&items).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        let ordered = items
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let index = index as u32;
                let digest = hash(&serde_jcs::to_vec(item).unwrap());
                OrderedMapItem {
                    index,
                    child_id: map_child_id(&id(run_id), &id("parts"), index, &digest),
                    item_digest: digest,
                }
            })
            .collect::<Vec<_>>();
        let identities = ordered
            .iter()
            .map(|item| MapChildIdentity {
                item_index: item.index,
                item_digest: item.item_digest.clone(),
                child_id: item.child_id.clone(),
            })
            .collect::<Vec<_>>();
        store
            .expand_map(
                scope,
                ExpandMap {
                    permit: acquired.permit.clone(),
                    run_id: id(run_id),
                    map_node_id: id("parts"),
                    expected_node_version: parent.version,
                    input: map_input,
                    expansion_digest: map_expansion_digest(&identities),
                    ordered_items: ordered.clone(),
                },
            )
            .await
            .unwrap();
        let child_id = &ordered[node_index as usize].child_id;
        let before_node = store.get_node(scope, &id(run_id), child_id).await.unwrap();
        let before_run = store.get_run(scope, &id(run_id)).await.unwrap().run;
        let bound =
            serde_jcs::to_vec(&json!({"item":items[node_index as usize]["id"],"version":2}))
                .unwrap();
        let bound_object = objects
            .put(scope, &bound, "application/json")
            .await
            .unwrap();
        let claim_result = store
            .claim_reusable_node_attempt(
                scope,
                ClaimReusableNodeAttempt {
                    claim: ClaimNodeAttempt {
                        permit: acquired.permit.clone(),
                        run_id: id(run_id),
                        node_id: child_id.clone(),
                        expected_node_version: before_node.version,
                        attempt_id: id("forged-pin-attempt"),
                        worker_id: id("worker"),
                        bound_input: bound_object,
                        binding_derivation_digest: hash(&bound),
                    },
                    memoization_key: None,
                    memoization_artifact_refs: Vec::new(),
                    reuse_provenance: Some(ReuseProvenance {
                        source_run_id: id("source"),
                        source_node_id: source_node,
                        source_attempt_id: source_attempt,
                        kind: ReuseKind::Pinned,
                    }),
                },
            )
            .await;
        if run_id == "stale-pin" {
            let ClaimNodeAttemptResult::Claimed {
                completion_credential,
                ..
            } = claim_result.unwrap()
            else {
                panic!("authorized pin was not claimed");
            };
            let attempt = store
                .get_attempt(scope, &id(run_id), &id("forged-pin-attempt"))
                .await
                .unwrap();
            assert_eq!(attempt.declared_max_cost, CostUnits(0));
            assert_eq!(attempt.reserved_cost, CostUnits(0));
            assert_eq!(
                attempt.reuse_provenance.as_ref().unwrap().kind,
                ReuseKind::Pinned
            );
            store
                .release_engine_claim(scope, &acquired.permit)
                .await
                .unwrap();
            let takeover = store
                .acquire_engine_claim(scope, id("replacement-pin-engine"))
                .await
                .unwrap();
            assert!(takeover.claim.generation > acquired.claim.generation);
            let output = json!({"item":"bass","version":1});
            let object = objects
                .put(
                    scope,
                    &serde_jcs::to_vec(&output).unwrap(),
                    "application/json",
                )
                .await
                .unwrap();
            assert!(matches!(
                store
                    .complete_attempt(
                        scope,
                        CompleteAttempt {
                            completion_credential,
                            run_id: id(run_id),
                            node_id: child_id.clone(),
                            attempt_id: id("forged-pin-attempt"),
                            submitted_outcome: ActionOutcome::success(
                                output,
                                Vec::new(),
                                CostUnits(0),
                                None
                            )
                            .unwrap(),
                            objects: CompletionObjects {
                                output: Some(object),
                                artifacts: Vec::new(),
                                diagnostics: None
                            },
                        }
                    )
                    .await,
                Err(StoreError::AttemptFenced)
            ));
            let current = store
                .get_attempt(scope, &id(run_id), &id("forged-pin-attempt"))
                .await
                .unwrap();
            assert_eq!(current, attempt);
            assert_eq!(current.settled_cost, None);
            assert_eq!(current.output_ref, None);
            assert_eq!(
                store
                    .get_node(scope, &id(run_id), child_id)
                    .await
                    .unwrap()
                    .result_ref,
                None
            );
            assert_eq!(
                store
                    .get_run(scope, &id(run_id))
                    .await
                    .unwrap()
                    .run
                    .budget_consumed,
                CostUnits(0)
            );
            continue;
        }
        assert!(
            claim_result.is_err(),
            "forged direct-store claim accepted: {run_id}"
        );
        assert_eq!(
            store.get_node(scope, &id(run_id), child_id).await.unwrap(),
            before_node
        );
        assert_eq!(
            store.get_run(scope, &id(run_id)).await.unwrap().run,
            before_run
        );
        assert!(store
            .get_attempt(scope, &id(run_id), &id("forged-pin-attempt"))
            .await
            .is_err());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn memory_store_rejects_forged_pinned_provenance_without_reserving_or_advancing() {
    let clock = Arc::new(TestClock::new(Timestamp(1_000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(InMemoryObjectStore::new(clock));
    reject_forged_claims(store, objects, &scope("pin-forgery-memory")).await;
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_store_rejects_forged_pinned_provenance_without_reserving_or_advancing() {
    use dagger_workflow_core::sqlite::SqliteWorkflowStore;
    let clock = Arc::new(TestClock::new(Timestamp(1_000)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = Arc::new(
        SqliteWorkflowStore::open_url("sqlite::memory:", clock, objects.clone())
            .await
            .unwrap(),
    );
    let execution_scope = scope("pin-forgery-sqlite");
    reject_forged_claims(store, objects, &execution_scope).await;
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_pins_survive_restart_and_committed_provenance_survives_a_second_restart() {
    use dagger_workflow_core::fs_object_store::FsObjectStore;
    use dagger_workflow_core::sqlite::SqliteWorkflowStore;
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("control.sqlite");
    let object_root = directory.path().join("objects");
    let scope = scope("pin-durable");
    let clock = Arc::new(TestClock::new(Timestamp(1_000)));
    let calls = Arc::new(AtomicUsize::new(0));
    let objects = Arc::new(FsObjectStore::open(&object_root, clock.clone()).unwrap());
    let store = Arc::new(
        SqliteWorkflowStore::open(&database, clock.clone(), objects.clone())
            .await
            .unwrap(),
    );
    let (revision, pin) = source(store.clone(), objects.clone(), &scope, calls.clone()).await;
    create(
        store.as_ref(),
        objects.as_ref(),
        &scope,
        "parts-workflow",
        &revision,
        "locked",
        json!({"items":[{"id":"drums","version":2},{"id":"bass","version":2}]}),
        vec![pin.clone()],
    )
    .await
    .unwrap();
    store.pool().close().await;
    drop(store);
    drop(objects);
    let objects = Arc::new(FsObjectStore::open(&object_root, clock.clone()).unwrap());
    let store = Arc::new(
        SqliteWorkflowStore::open(&database, clock.clone(), objects.clone())
            .await
            .unwrap(),
    );
    let engine = engine(store.clone(), objects.clone(), calls.clone(), b"{}");
    engine.acquire_scope(&scope).await.unwrap();
    execute(&engine, &scope, "locked").await;
    engine.release_scope(&scope).await.unwrap();
    drop(engine);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    store.pool().close().await;
    drop(store);
    drop(objects);
    let objects = Arc::new(FsObjectStore::open(&object_root, clock.clone()).unwrap());
    let store = SqliteWorkflowStore::open(&database, clock, objects.clone())
        .await
        .unwrap();
    assert_pinned_run(&store, objects.as_ref(), &scope, &pin).await;
}
