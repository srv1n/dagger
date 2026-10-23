use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, ArtifactOutput, DiagnosticsEnvelope,
    InMemoryActionRegistry, WorkflowAction,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::{
    ArtifactKind, ArtifactRefValue, ObjectReadError, ObjectStore, ObjectStoreError, VerifiedObject,
    VerifiedObjectRef,
};
use dagger_workflow_core::definition::{
    canonical_topological_ranks, ActionReference, BackoffPolicy, Binding, BindingSource,
    NodeDefinition, PublishableDefinition, RetryPolicy, TimeoutPolicy, WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::event::EventType;
use dagger_workflow_core::ids::{
    artifact_ref_id, ArtifactRefIdentity, CostUnits, Digest, Id, Timestamp, Version,
};
use dagger_workflow_core::memoization::{CacheMode, ReuseKind};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::pinning::RunOptions;
use dagger_workflow_core::run::{RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::store::{
    CreateDefinition, CreateRun, CreateRunWithOptions, EventPageRequest, PublishRevision,
    ResolvedActionSchemas, WorkflowStore,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

fn hash(bytes: &[u8]) -> Digest {
    Digest::new(format!("sha256:{:x}", Sha256::digest(bytes))).unwrap()
}
fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}
fn scope(tenant: &str, namespace: &str) -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new(tenant).unwrap(),
        namespace: ScopeAtom::new(namespace).unwrap(),
    }
}
fn principal(scope: &ExecutionScope) -> AuthenticatedPrincipal {
    AuthenticatedPrincipal::mint(scope.clone(), "tester".into(), Vec::new(), hash(b"auth")).unwrap()
}
fn descriptor(name: &str, implementation: &[u8]) -> ActionDescriptor {
    ActionDescriptor {
        name: name.into(),
        contract_version: "1".into(),
        input_schema_digest: hash(b"{}"),
        output_schema_digest: hash(b"{}"),
        implementation_compatibility_digest: hash(implementation),
    }
}

struct Probe<O> {
    objects: Arc<O>,
    descriptor: ActionDescriptor,
    calls: Arc<AtomicUsize>,
    artifact: bool,
    memoize: bool,
}
impl<O: ObjectStore> WorkflowAction for Probe<O> {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }
    fn memoize(&self) -> bool {
        self.memoize
    }
    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let input: Value = serde_json::from_slice(input).unwrap();
            let pass_through =
                self.descriptor.name == "cache.source" && input.get("passthrough").is_some();
            let mut output = if self.descriptor.name == "cache.source" {
                if let Some(value) = input.get("passthrough") {
                    value.clone()
                } else if input["artifact"].is_object() {
                    json!({"value":input["artifact"]["artifact_ref_id"]})
                } else {
                    json!({"value":input["input"].as_u64().unwrap()%2})
                }
            } else {
                input
            };
            let mut artifacts = Vec::new();
            if self.artifact && !pass_through {
                let object = self
                    .objects
                    .put_repairing(&context.scope, b"same-wav", "audio/wav")
                    .await
                    .unwrap();
                let refs: Vec<_> = (0..2)
                    .map(|ordinal| ArtifactRefValue {
                        artifact_ref_id: artifact_ref_id(ArtifactRefIdentity {
                            scope: &context.scope,
                            digest: object.digest(),
                            kind: ArtifactKind::ActionArtifact,
                            producer_run_id: Some(&context.run_id),
                            producer_node_id: Some(&context.node_instance_id),
                            producer_attempt_id: Some(&context.attempt_id),
                            ordinal,
                        }),
                        digest: object.digest().clone(),
                        size_bytes: object.size_bytes().to_string(),
                        media_type: "audio/wav".into(),
                    })
                    .collect();
                output["artifacts"] = serde_json::to_value(refs).unwrap();
                artifacts = vec![
                    ArtifactOutput {
                        media_type: "audio/wav".into(),
                        object: object.clone(),
                    },
                    ArtifactOutput {
                        media_type: "audio/wav".into(),
                        object,
                    },
                ];
            }
            let related_artifact_refs = if pass_through {
                vec![serde_json::from_value(output["artifacts"][0].clone()).unwrap()]
            } else {
                Vec::new()
            };
            ActionOutcome::success(
                output,
                artifacts,
                CostUnits(1),
                Some(
                    DiagnosticsEnvelope::new(
                        Some("memoization fixture".into()),
                        Vec::new(),
                        related_artifact_refs,
                    )
                    .unwrap(),
                ),
            )
            .unwrap()
        })
    }
}

fn workflow(definition_id: &str, implementation: &[u8]) -> WorkflowDefinition {
    let action = |name: &str, node: &str, source: BindingSource, next: &str| {
        let descriptor = descriptor(name, implementation);
        NodeDefinition::Action {
            id: id(node),
            action: ActionReference {
                name: descriptor.name,
                contract_version: descriptor.contract_version,
                input_schema_digest: descriptor.input_schema_digest,
                output_schema_digest: descriptor.output_schema_digest,
                compatible_implementation_requirement: descriptor
                    .implementation_compatibility_digest,
            },
            bindings: vec![Binding {
                target: String::new(),
                source,
            }],
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: BackoffPolicy::Fixed { delay_ms: 0 },
            },
            timeout: TimeoutPolicy { timeout_ms: 10_000 },
            declared_max_cost_units: CostUnits(1),
            next: vec![id(next)],
        }
    };
    WorkflowDefinition {
        definition_format_version: "0.1".into(),
        definition_id: id(definition_id),
        name: definition_id.into(),
        description: String::new(),
        run_input_schema_digest: hash(b"{}"),
        run_output_schema_digest: hash(b"{}"),
        nodes: vec![
            action(
                "cache.source",
                "source",
                BindingSource::RunInput {
                    pointer: String::new(),
                },
                "dependent",
            ),
            action(
                "cache.dependent",
                "dependent",
                BindingSource::NodeOutput {
                    node_id: id("source"),
                    pointer: String::new(),
                },
                "done",
            ),
            NodeDefinition::Succeed {
                id: id("done"),
                output: BindingSource::NodeOutput {
                    node_id: id("dependent"),
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
    name: &str,
    implementation: &[u8],
) -> Digest {
    let workflow = workflow(name, implementation);
    store
        .create_definition(
            scope,
            CreateDefinition {
                definition_id: id(name),
                display_name: name.into(),
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
    let schemas = ["source", "dependent"]
        .into_iter()
        .map(|name| {
            (
                name.into(),
                ResolvedActionSchemas {
                    input_schema: schema.clone(),
                    output_schema: schema.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    store
        .publish_revision(
            scope,
            PublishRevision {
                definition_id: id(name),
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

async fn create<S: WorkflowStore, O: ObjectStore>(
    store: &S,
    objects: &O,
    scope: &ExecutionScope,
    definition: &str,
    revision: &Digest,
    run: &str,
    input: impl Into<Value>,
    mode: CacheMode,
    budget: u64,
) {
    let input = input.into();
    let input = if input.is_object() {
        input
    } else {
        json!({"input":input})
    };
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
                    run_id: id(run),
                    definition_id: id(definition),
                    revision_hash: revision.clone(),
                    input,
                    budget_limit: CostUnits(budget),
                    principal: principal(scope),
                    limits: RunLimits {
                        max_dynamic_node_instances: 10,
                        max_total_attempts: 10,
                        max_total_events: 1000,
                        max_inline_json_bytes_per_value: 100_000,
                        max_artifacts_per_attempt: 10,
                        max_aggregate_object_bytes_per_run: 1_000_000,
                        max_run_lifetime_ms: 100_000,
                    },
                    idempotency_token: format!("memoize-create-token-{run}"),
                },
                options: RunOptions {
                    cache_mode: mode,
                    pinned_outputs: Vec::new(),
                },
            },
        )
        .await
        .unwrap();
}
fn engine<S: WorkflowStore + 'static, O: ObjectStore + 'static>(
    store: Arc<S>,
    objects: Arc<O>,
    calls: [Arc<AtomicUsize>; 2],
    artifact: bool,
    memoize: bool,
    implementation: &[u8],
) -> WorkflowEngine<S, O, InMemoryActionRegistry> {
    let registry = Arc::new(InMemoryActionRegistry::new());
    for (i, name) in ["cache.source", "cache.dependent"].into_iter().enumerate() {
        registry
            .register(Arc::new(Probe {
                objects: objects.clone(),
                descriptor: descriptor(name, implementation),
                calls: calls[i].clone(),
                artifact: artifact && i == 0,
                memoize,
            }))
            .unwrap();
    }
    WorkflowEngine::new(
        store,
        objects,
        registry,
        EngineConfig {
            instance_id: id("cache-engine"),
            max_concurrency: 2,
            cancellation_grace: std::time::Duration::from_secs(1),
        },
    )
    .unwrap()
}
async fn execute<S: WorkflowStore + 'static, O: ObjectStore + 'static>(
    engine: &WorkflowEngine<S, O, InMemoryActionRegistry>,
    store: &S,
    scope: &ExecutionScope,
    run: &str,
) {
    engine.start(scope, &id(run)).await.unwrap();
    engine.run_until_idle(scope, 20).await.unwrap();
    assert_eq!(
        store.get_run(scope, &id(run)).await.unwrap().run.status,
        RunState::Succeeded
    );
}
async fn attempts<S: WorkflowStore>(
    store: &S,
    scope: &ExecutionScope,
    run: &str,
) -> Vec<dagger_workflow_core::run::NodeAttempt> {
    let mut attempts = Vec::new();
    for node in ["source", "dependent"] {
        attempts.push(
            store
                .get_latest_attempt(scope, &id(run), &id(node))
                .await
                .unwrap()
                .unwrap(),
        );
    }
    attempts
}
async fn output<S: WorkflowStore, O: ObjectStore>(
    store: &S,
    objects: &O,
    scope: &ExecutionScope,
    run: &str,
) -> Value {
    let reference = store
        .get_run(scope, &id(run))
        .await
        .unwrap()
        .run
        .output_ref
        .unwrap();
    serde_json::from_slice(&objects.get(scope, &reference.0.digest).await.unwrap().bytes).unwrap()
}

#[tokio::test]
async fn identical_inputs_zero_budget_changed_inputs_identical_dependents_and_modes() {
    let scope = scope("tenant", "cache");
    let clock = Arc::new(TestClock::new(Timestamp(1000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(InMemoryObjectStore::new(clock));
    let calls = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let engine = engine(
        store.clone(),
        objects.clone(),
        calls.clone(),
        false,
        true,
        b"v1",
    );
    engine.acquire_scope(&scope).await.unwrap();
    let revision = publish(&*store, &*objects, &scope, "pipeline", b"v1").await;
    for (run, input, mode, budget, expected) in [
        ("first", 1, CacheMode::Use, 10, [1, 1]),
        ("same", 1, CacheMode::Use, 0, [1, 1]),
        ("different_same_output", 3, CacheMode::Use, 10, [2, 1]),
        ("different_output", 2, CacheMode::Use, 10, [3, 2]),
        ("off", 1, CacheMode::Off, 10, [4, 3]),
        ("refresh", 1, CacheMode::Refresh, 10, [5, 4]),
        ("after_refresh", 1, CacheMode::Use, 0, [5, 4]),
    ] {
        create(
            &*store, &*objects, &scope, "pipeline", &revision, run, input, mode, budget,
        )
        .await;
        execute(&engine, &*store, &scope, run).await;
        assert_eq!(
            [
                calls[0].load(Ordering::SeqCst),
                calls[1].load(Ordering::SeqCst)
            ],
            expected,
            "{run}"
        );
    }
    let run = store.get_run(&scope, &id("same")).await.unwrap().run;
    assert_eq!(run.budget_consumed, CostUnits(0));
    assert_eq!(run.budget_reserved, CostUnits(0));
    let successful_attempts = attempts(&*store, &scope, "after_refresh").await;
    assert!(successful_attempts.iter().all(|a| a
        .reuse_provenance
        .as_ref()
        .is_some_and(|p| p.kind == ReuseKind::CacheHit && p.source_run_id == id("refresh"))));
    let off = attempts(&*store, &scope, "off").await;
    assert!(off.iter().all(|a| a.memoization_key.is_none()));
    let events = store
        .list_events_after(
            &scope,
            &id("same"),
            EventPageRequest {
                after_event_seq: 0,
                page_size: 1000,
                hard_response_byte_limit: 1_000_000,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == EventType::CacheHit)
            .count(),
        2
    );
}

#[tokio::test]
async fn cache_rehomes_duplicate_artifacts_and_pass_through_refs_and_repairs_corruption() {
    let scope = scope("tenant", "artifacts");
    let clock = Arc::new(TestClock::new(Timestamp(1000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(InMemoryObjectStore::new(clock));
    let calls = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let engine = engine(
        store.clone(),
        objects.clone(),
        calls.clone(),
        true,
        true,
        b"v1",
    );
    engine.acquire_scope(&scope).await.unwrap();
    let revision = publish(&*store, &*objects, &scope, "pipeline", b"v1").await;
    create(
        &*store,
        &*objects,
        &scope,
        "pipeline",
        &revision,
        "first",
        1,
        CacheMode::Use,
        10,
    )
    .await;
    execute(&engine, &*store, &scope, "first").await;
    let first = output(&*store, &*objects, &scope, "first").await;
    create(
        &*store,
        &*objects,
        &scope,
        "pipeline",
        &revision,
        "same",
        1,
        CacheMode::Use,
        0,
    )
    .await;
    execute(&engine, &*store, &scope, "same").await;
    let same = output(&*store, &*objects, &scope, "same").await;
    assert_ne!(
        same["artifacts"][0]["artifact_ref_id"],
        same["artifacts"][1]["artifact_ref_id"]
    );
    assert_ne!(
        first["artifacts"][0]["artifact_ref_id"],
        same["artifacts"][0]["artifact_ref_id"]
    );
    for reference in same["artifacts"].as_array().unwrap() {
        let projection: ArtifactRefValue = serde_json::from_value(reference.clone()).unwrap();
        let reference = store
            .lookup_artifact_ref(&scope, &projection.artifact_ref_id)
            .await
            .unwrap();
        assert_eq!(reference.producer_run_id, Some(id("same")));
    }
    assert_eq!(
        [
            calls[0].load(Ordering::SeqCst),
            calls[1].load(Ordering::SeqCst)
        ],
        [1, 1]
    );
    let artifact: ArtifactRefValue = serde_json::from_value(same["artifacts"][0].clone()).unwrap();
    assert!(objects.corrupt_bytes(&scope, &artifact.digest, b"broken".to_vec()));
    create(
        &*store,
        &*objects,
        &scope,
        "pipeline",
        &revision,
        "after_corruption",
        1,
        CacheMode::Use,
        10,
    )
    .await;
    execute(&engine, &*store, &scope, "after_corruption").await;
    assert_eq!(
        [
            calls[0].load(Ordering::SeqCst),
            calls[1].load(Ordering::SeqCst)
        ],
        [2, 1]
    );
    assert_eq!(
        store.get_run(&scope, &id("same")).await.unwrap().run.status,
        RunState::Succeeded
    );
    let source = store
        .get_node(&scope, &id("after_corruption"), &id("source"))
        .await
        .unwrap()
        .result_ref
        .unwrap();
    assert!(objects.corrupt_bytes(&scope, &source.0.digest, b"broken-json".to_vec()));
    create(
        &*store,
        &*objects,
        &scope,
        "pipeline",
        &revision,
        "after_json_corruption",
        1,
        CacheMode::Use,
        10,
    )
    .await;
    execute(&engine, &*store, &scope, "after_json_corruption").await;
    assert_eq!(calls[0].load(Ordering::SeqCst), 3);
    let source = store
        .get_node(&scope, &id("after_json_corruption"), &id("source"))
        .await
        .unwrap();
    let attempt = store
        .get_attempt(
            &scope,
            &id("after_json_corruption"),
            source
                .result_ref
                .unwrap()
                .0
                .producer_attempt_id
                .as_ref()
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(objects.corrupt_bytes(
        &scope,
        &attempt.diagnostics_ref.unwrap().0.digest,
        b"broken-diagnostics".to_vec()
    ));
    create(
        &*store,
        &*objects,
        &scope,
        "pipeline",
        &revision,
        "after_diagnostics_corruption",
        1,
        CacheMode::Use,
        10,
    )
    .await;
    execute(&engine, &*store, &scope, "after_diagnostics_corruption").await;
    assert_eq!(calls[0].load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn memoization_is_opt_in_and_implementation_pins_and_scope_isolate_entries() {
    let scope1 = scope("tenant", "one");
    let clock = Arc::new(TestClock::new(Timestamp(1000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(InMemoryObjectStore::new(clock));
    let calls = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let no_cache = engine(
        store.clone(),
        objects.clone(),
        calls.clone(),
        false,
        false,
        b"v1",
    );
    no_cache.acquire_scope(&scope1).await.unwrap();
    let revision = publish(&*store, &*objects, &scope1, "pipeline", b"v1").await;
    for run in ["optout1", "optout2"] {
        create(
            &*store,
            &*objects,
            &scope1,
            "pipeline",
            &revision,
            run,
            1,
            CacheMode::Use,
            10,
        )
        .await;
        execute(&no_cache, &*store, &scope1, run).await;
    }
    assert_eq!(calls[0].load(Ordering::SeqCst), 2);
    no_cache.release_scope(&scope1).await.unwrap();
    let caching = engine(
        store.clone(),
        objects.clone(),
        calls.clone(),
        false,
        true,
        b"v1",
    );
    caching.acquire_scope(&scope1).await.unwrap();
    create(
        &*store,
        &*objects,
        &scope1,
        "pipeline",
        &revision,
        "cached",
        1,
        CacheMode::Use,
        10,
    )
    .await;
    execute(&caching, &*store, &scope1, "cached").await;
    caching.release_scope(&scope1).await.unwrap();
    let changed = engine(
        store.clone(),
        objects.clone(),
        calls.clone(),
        false,
        true,
        b"v2",
    );
    changed.acquire_scope(&scope1).await.unwrap();
    let revision2 = publish(&*store, &*objects, &scope1, "changed", b"v2").await;
    create(
        &*store,
        &*objects,
        &scope1,
        "changed",
        &revision2,
        "changed",
        1,
        CacheMode::Use,
        10,
    )
    .await;
    execute(&changed, &*store, &scope1, "changed").await;
    assert_eq!(calls[0].load(Ordering::SeqCst), 4);
    changed.release_scope(&scope1).await.unwrap();
    for scope in [scope("other-tenant", "one"), scope("tenant", "two")] {
        let scoped = engine(
            store.clone(),
            objects.clone(),
            calls.clone(),
            false,
            true,
            b"v1",
        );
        scoped.acquire_scope(&scope).await.unwrap();
        let revision = publish(&*store, &*objects, &scope, "pipeline", b"v1").await;
        create(
            &*store,
            &*objects,
            &scope,
            "pipeline",
            &revision,
            "scoped",
            1,
            CacheMode::Use,
            10,
        )
        .await;
        execute(&scoped, &*store, &scope, "scoped").await;
        scoped.release_scope(&scope).await.unwrap();
    }
    assert_eq!(calls[0].load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn ordinary_artifact_shaped_dictionaries_keep_every_input_field_in_the_key() {
    let scope = scope("tenant", "plain-dict");
    let clock = Arc::new(TestClock::new(Timestamp(1000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(InMemoryObjectStore::new(clock));
    let calls = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let engine = engine(
        store.clone(),
        objects.clone(),
        calls.clone(),
        false,
        true,
        b"v1",
    );
    engine.acquire_scope(&scope).await.unwrap();
    let revision = publish(&*store, &*objects, &scope, "pipeline", b"v1").await;
    for value in ["foo", "bar"] {
        let input = json!({"artifact":{"artifact_ref_id":value,"digest":hash(b"fake"),"size_bytes":"4","media_type":"audio/wav"}});
        create(
            &*store,
            &*objects,
            &scope,
            "pipeline",
            &revision,
            value,
            input,
            CacheMode::Use,
            10,
        )
        .await;
        execute(&engine, &*store, &scope, value).await;
        assert_eq!(
            output(&*store, &*objects, &scope, value).await["value"],
            value
        );
    }
    assert_eq!(
        [
            calls[0].load(Ordering::SeqCst),
            calls[1].load(Ordering::SeqCst)
        ],
        [2, 2]
    );
}

struct UnavailableReads<O> {
    inner: Arc<O>,
    denied: Mutex<Option<Digest>>,
    delay_next_binary_verify: AtomicBool,
}
impl<O: ObjectStore> ObjectStore for UnavailableReads<O> {
    async fn put(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        self.inner.put(scope, bytes, media).await
    }
    async fn put_repairing(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        self.inner.put_repairing(scope, bytes, media).await
    }
    async fn get(
        &self,
        scope: &ExecutionScope,
        digest: &Digest,
    ) -> Result<VerifiedObject, ObjectReadError> {
        if self.denied.lock().unwrap().as_ref() == Some(digest) {
            return Err(ObjectReadError::StorageUnavailable);
        }
        self.inner.get(scope, digest).await
    }
    async fn verify(
        &self,
        scope: &ExecutionScope,
        digest: &Digest,
    ) -> Result<VerifiedObjectRef, ObjectReadError> {
        let object = self.inner.verify(scope, digest).await?;
        if object.media_type() != "application/json"
            && self.delay_next_binary_verify.swap(false, Ordering::SeqCst)
        {
            tokio::time::sleep(std::time::Duration::from_secs(25)).await;
        }
        Ok(object)
    }
    async fn publish_if_absent(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        self.inner.publish_if_absent(scope, bytes, media).await
    }
}

#[tokio::test]
async fn unavailable_cache_storage_remains_an_error_without_claiming_or_invoking() {
    let scope = scope("tenant", "unavailable");
    let clock = Arc::new(TestClock::new(Timestamp(1000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(UnavailableReads {
        inner: Arc::new(InMemoryObjectStore::new(clock)),
        denied: Mutex::new(None),
        delay_next_binary_verify: AtomicBool::new(false),
    });
    let calls = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let engine = engine(
        store.clone(),
        objects.clone(),
        calls.clone(),
        false,
        true,
        b"v1",
    );
    engine.acquire_scope(&scope).await.unwrap();
    let revision = publish(&*store, &*objects, &scope, "pipeline", b"v1").await;
    create(
        &*store,
        &*objects,
        &scope,
        "pipeline",
        &revision,
        "first",
        1,
        CacheMode::Use,
        10,
    )
    .await;
    execute(&engine, &*store, &scope, "first").await;
    let reference = store
        .get_node(&scope, &id("first"), &id("source"))
        .await
        .unwrap()
        .result_ref
        .unwrap();
    *objects.denied.lock().unwrap() = Some(reference.0.digest);
    create(
        &*store,
        &*objects,
        &scope,
        "pipeline",
        &revision,
        "unavailable",
        1,
        CacheMode::Use,
        10,
    )
    .await;
    engine.start(&scope, &id("unavailable")).await.unwrap();
    assert!(matches!(
        engine.run_until_idle(&scope, 20).await,
        Err(dagger_workflow_core::engine::EngineError::StorageUnavailable)
    ));
    assert_eq!(calls[0].load(Ordering::SeqCst), 1);
    let run = store.get_run(&scope, &id("unavailable")).await.unwrap().run;
    assert_eq!(run.status, RunState::Running);
    assert_eq!(run.total_attempt_count, 0);
    assert_eq!(
        store
            .get_run(&scope, &id("first"))
            .await
            .unwrap()
            .run
            .status,
        RunState::Succeeded
    );
    *objects.denied.lock().unwrap() = None;
    engine.run_until_idle(&scope, 20).await.unwrap();
    assert_eq!(
        store
            .get_run(&scope, &id("unavailable"))
            .await
            .unwrap()
            .run
            .status,
        RunState::Succeeded
    );
}

#[tokio::test]
async fn missing_cached_json_reexecutes_and_restores_identical_bytes() {
    use dagger_workflow_core::fs_object_store::FsObjectStore;
    let temp = tempfile::tempdir().unwrap();
    let scope = scope("tenant", "missing");
    let clock = Arc::new(TestClock::new(Timestamp(1000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(FsObjectStore::open(temp.path(), clock).unwrap());
    let calls = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let engine = engine(
        store.clone(),
        objects.clone(),
        calls.clone(),
        false,
        true,
        b"v1",
    );
    engine.acquire_scope(&scope).await.unwrap();
    let revision = publish(&*store, &*objects, &scope, "pipeline", b"v1").await;
    create(
        &*store,
        &*objects,
        &scope,
        "pipeline",
        &revision,
        "first",
        1,
        CacheMode::Use,
        10,
    )
    .await;
    execute(&engine, &*store, &scope, "first").await;
    let reference = store
        .get_node(&scope, &id("first"), &id("source"))
        .await
        .unwrap()
        .result_ref
        .unwrap();
    let record = store
        .object_records(&scope)
        .into_iter()
        .find(|record| record.digest == reference.0.digest)
        .unwrap();
    std::fs::remove_file(objects.root().join(record.object_key)).unwrap();
    create(
        &*store,
        &*objects,
        &scope,
        "pipeline",
        &revision,
        "second",
        1,
        CacheMode::Use,
        10,
    )
    .await;
    execute(&engine, &*store, &scope, "second").await;
    assert_eq!(
        [
            calls[0].load(Ordering::SeqCst),
            calls[1].load(Ordering::SeqCst)
        ],
        [2, 1]
    );
    assert_eq!(
        store
            .get_run(&scope, &id("first"))
            .await
            .unwrap()
            .run
            .status,
        RunState::Succeeded
    );
    assert!(objects.get(&scope, &reference.0.digest).await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn slow_binary_cache_verification_heartbeats_before_the_attempt_is_claimed() {
    let scope = scope("tenant", "slow-verify");
    let clock = Arc::new(TestClock::new(Timestamp(1000)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(UnavailableReads {
        inner: Arc::new(InMemoryObjectStore::new(clock.clone())),
        denied: Mutex::new(None),
        delay_next_binary_verify: AtomicBool::new(false),
    });
    let calls = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let engine = engine(
        store.clone(),
        objects.clone(),
        calls.clone(),
        true,
        true,
        b"v1",
    );
    engine.acquire_scope(&scope).await.unwrap();
    let revision = publish(&*store, &*objects, &scope, "pipeline", b"v1").await;
    create(
        &*store,
        &*objects,
        &scope,
        "pipeline",
        &revision,
        "first",
        1,
        CacheMode::Use,
        10,
    )
    .await;
    execute(&engine, &*store, &scope, "first").await;
    let advancing_clock = clock.clone();
    let clock_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            advancing_clock.advance_ms(1000).unwrap();
        }
    });
    objects
        .delay_next_binary_verify
        .store(true, Ordering::SeqCst);
    create(
        &*store,
        &*objects,
        &scope,
        "pipeline",
        &revision,
        "second",
        1,
        CacheMode::Use,
        0,
    )
    .await;
    execute(&engine, &*store, &scope, "second").await;
    clock_task.abort();
    assert_eq!(
        [
            calls[0].load(Ordering::SeqCst),
            calls[1].load(Ordering::SeqCst)
        ],
        [1, 1]
    );
    assert_eq!(
        store
            .get_run(&scope, &id("second"))
            .await
            .unwrap()
            .run
            .budget_consumed,
        CostUnits(0)
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_restart_cache_and_indexed_lookup_are_durable() {
    use dagger_workflow_core::fs_object_store::FsObjectStore;
    use dagger_workflow_core::sqlite::SqliteWorkflowStore;
    let temp = tempfile::tempdir().unwrap();
    let execution_scope = scope("tenant", "sqlite-cache");
    let clock = Arc::new(TestClock::new(Timestamp(1000)));
    let path = temp.path().join("db.sqlite");
    let objects =
        Arc::new(FsObjectStore::open(temp.path().join("objects"), clock.clone()).unwrap());
    let calls = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let store = Arc::new(
        SqliteWorkflowStore::open(&path, clock.clone(), objects.clone())
            .await
            .unwrap(),
    );
    let revision = publish(&*store, &*objects, &execution_scope, "pipeline", b"v1").await;
    let first = engine(
        store.clone(),
        objects.clone(),
        calls.clone(),
        true,
        true,
        b"v1",
    );
    first.acquire_scope(&execution_scope).await.unwrap();
    create(
        &*store,
        &*objects,
        &execution_scope,
        "pipeline",
        &revision,
        "first",
        1,
        CacheMode::Use,
        10,
    )
    .await;
    execute(&first, &*store, &execution_scope, "first").await;
    first.release_scope(&execution_scope).await.unwrap();
    drop(first);
    drop(store);
    drop(objects);
    let objects =
        Arc::new(FsObjectStore::open(temp.path().join("objects"), clock.clone()).unwrap());
    let store = Arc::new(
        SqliteWorkflowStore::open(&path, clock, objects.clone())
            .await
            .unwrap(),
    );
    let second = engine(
        store.clone(),
        objects.clone(),
        calls.clone(),
        true,
        true,
        b"v1",
    );
    second.acquire_scope(&execution_scope).await.unwrap();
    create(
        &*store,
        &*objects,
        &execution_scope,
        "pipeline",
        &revision,
        "second",
        1,
        CacheMode::Use,
        0,
    )
    .await;
    execute(&second, &*store, &execution_scope, "second").await;
    assert_eq!(
        [
            calls[0].load(Ordering::SeqCst),
            calls[1].load(Ordering::SeqCst)
        ],
        [1, 1]
    );
    assert_eq!(
        store
            .get_run(&execution_scope, &id("second"))
            .await
            .unwrap()
            .run
            .budget_consumed,
        CostUnits(0)
    );
    let foreign = scope("other", "sqlite-cache");
    let successful_attempts = attempts(&*store, &execution_scope, "second").await;
    let key = successful_attempts[0].memoization_key.as_ref().unwrap();
    assert!(store
        .lookup_memoized_attempt(&foreign, key)
        .await
        .unwrap()
        .is_none());
    let plan:Vec<(i64,i64,i64,String)>=sqlx::query_as("EXPLAIN QUERY PLAN SELECT attempt_id FROM dagger_workflow_attempts WHERE tenant_id=? AND namespace=? AND cache_key=? AND status='Succeeded' ORDER BY cache_sequence DESC,run_id DESC,attempt_id DESC LIMIT 1")
        .bind(execution_scope.tenant_id.as_str()).bind(execution_scope.namespace.as_str()).bind(key.as_str()).fetch_all(store.pool()).await.unwrap();
    assert!(plan
        .iter()
        .any(|row| row.3.contains("dagger_workflow_attempts_cache_lookup")));

    let original_output = output(&*store, &*objects, &execution_scope, "first").await;
    let original_artifact = original_output["artifacts"][0].clone();
    let mut plain_dictionary = original_artifact.clone();
    plain_dictionary["media_type"] = json!("application/plain-dictionary");
    let pass_through_input =
        json!({"passthrough":{"artifacts":[original_artifact, plain_dictionary.clone()]}});
    for (run, budget) in [("passthrough_source", 10), ("passthrough_consumer", 0)] {
        create(
            &*store,
            &*objects,
            &execution_scope,
            "pipeline",
            &revision,
            run,
            pass_through_input.clone(),
            CacheMode::Use,
            budget,
        )
        .await;
        execute(&second, &*store, &execution_scope, run).await;
    }
    let consumer_output =
        output(&*store, &*objects, &execution_scope, "passthrough_consumer").await;
    let consumer_projection: ArtifactRefValue =
        serde_json::from_value(consumer_output["artifacts"][0].clone()).unwrap();
    assert_eq!(
        store
            .lookup_artifact_ref(&execution_scope, &consumer_projection.artifact_ref_id)
            .await
            .unwrap()
            .producer_run_id,
        Some(id("passthrough_consumer"))
    );
    assert_eq!(consumer_output["artifacts"][1], plain_dictionary);
    let consumer_attempt = store
        .get_latest_attempt(&execution_scope, &id("passthrough_consumer"), &id("source"))
        .await
        .unwrap()
        .unwrap();
    let diagnostics_ref = consumer_attempt.diagnostics_ref.unwrap();
    let diagnostics: DiagnosticsEnvelope = serde_json::from_slice(
        &objects
            .get(&execution_scope, &diagnostics_ref.0.digest)
            .await
            .unwrap()
            .bytes,
    )
    .unwrap();
    let diagnostic_projection = &diagnostics.related_artifact_refs[0];
    assert_eq!(
        store
            .lookup_artifact_ref(&execution_scope, &diagnostic_projection.artifact_ref_id)
            .await
            .unwrap()
            .producer_run_id,
        Some(id("passthrough_consumer"))
    );
    assert_eq!(
        [
            calls[0].load(Ordering::SeqCst),
            calls[1].load(Ordering::SeqCst)
        ],
        [2, 2]
    );

    // Simulate retention removing the original run's control-plane rows and
    // artifact uses. Physical content remains retained by the consumer's uses.
    let mut cleanup = store.pool().begin().await.unwrap();
    sqlx::query("DELETE FROM dagger_workflow_artifact_rows WHERE tenant_id=? AND namespace=? AND entity_id IN (SELECT artifact_ref_id FROM dagger_workflow_artifact_refs WHERE tenant_id=? AND namespace=? AND producer_run_id=?)")
        .bind(execution_scope.tenant_id.as_str())
        .bind(execution_scope.namespace.as_str())
        .bind(execution_scope.tenant_id.as_str())
        .bind(execution_scope.namespace.as_str())
        .bind("first")
        .execute(&mut *cleanup).await.unwrap();
    sqlx::query("DELETE FROM dagger_workflow_artifact_refs WHERE tenant_id=? AND namespace=? AND producer_run_id=?")
        .bind(execution_scope.tenant_id.as_str())
        .bind(execution_scope.namespace.as_str())
        .bind("first")
        .execute(&mut *cleanup).await.unwrap();
    for table in [
        "dagger_workflow_run_rows",
        "dagger_workflow_node_run_rows",
        "dagger_workflow_attempt_rows",
        "dagger_workflow_invocation_rows",
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE tenant_id=? AND namespace=? AND entity_id=?"
        ))
        .bind(execution_scope.tenant_id.as_str())
        .bind(execution_scope.namespace.as_str())
        .bind("first")
        .execute(&mut *cleanup)
        .await
        .unwrap();
    }
    for table in [
        "dagger_workflow_runs",
        "dagger_workflow_nodes",
        "dagger_workflow_attempts",
        "dagger_workflow_action_invocations",
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE tenant_id=? AND namespace=? AND run_id=?"
        ))
        .bind(execution_scope.tenant_id.as_str())
        .bind(execution_scope.namespace.as_str())
        .bind("first")
        .execute(&mut *cleanup)
        .await
        .unwrap();
    }
    cleanup.commit().await.unwrap();
    assert!(store.get_run(&execution_scope, &id("first")).await.is_err());
    let retained_output = output(&*store, &*objects, &execution_scope, "second").await;
    for value in retained_output["artifacts"].as_array().unwrap() {
        let projection: ArtifactRefValue = serde_json::from_value(value.clone()).unwrap();
        let reference = store
            .lookup_artifact_ref(&execution_scope, &projection.artifact_ref_id)
            .await
            .unwrap();
        assert_eq!(reference.producer_run_id, Some(id("second")));
        objects
            .verify(&execution_scope, &reference.digest)
            .await
            .unwrap();
    }
    assert_eq!(
        output(&*store, &*objects, &execution_scope, "passthrough_consumer").await,
        consumer_output
    );
    objects
        .verify(&execution_scope, &consumer_projection.digest)
        .await
        .unwrap();
    store
        .lookup_artifact_ref(&execution_scope, &diagnostic_projection.artifact_ref_id)
        .await
        .unwrap();
    let retained_pass_through_input = json!({"passthrough":consumer_output});
    create(
        &*store,
        &*objects,
        &execution_scope,
        "pipeline",
        &revision,
        "passthrough_after_cleanup",
        retained_pass_through_input,
        CacheMode::Use,
        0,
    )
    .await;
    execute(
        &second,
        &*store,
        &execution_scope,
        "passthrough_after_cleanup",
    )
    .await;
    create(
        &*store,
        &*objects,
        &execution_scope,
        "pipeline",
        &revision,
        "after_cleanup",
        1,
        CacheMode::Use,
        0,
    )
    .await;
    execute(&second, &*store, &execution_scope, "after_cleanup").await;
    assert_eq!(
        [
            calls[0].load(Ordering::SeqCst),
            calls[1].load(Ordering::SeqCst)
        ],
        [2, 2]
    );
}
