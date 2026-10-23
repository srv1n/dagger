//! Opt-in 500 MiB acceptance probe. Run the compiled test under `/usr/bin/time -l` on macOS
//! (`/usr/bin/time -v` on Linux) to measure process RSS without including compiler memory.

#![cfg(feature = "sqlite")]

use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, ArtifactOutput, InMemoryActionRegistry,
    WorkflowAction,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::{ObjectStore, VerifiedObjectRef};
use dagger_workflow_core::committed_read::{CommittedObjectReader, CommittedReadOutcome};
use dagger_workflow_core::definition::{
    canonical_topological_ranks, BindingSource, NodeDefinition, PublishableDefinition,
    WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::fs_object_store::FsObjectStore;
use dagger_workflow_core::ids::{CostUnits, Id, Timestamp, Version};
use dagger_workflow_core::memory::InMemoryStore;
use dagger_workflow_core::run::{RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::sqlite::SqliteWorkflowStore;
use dagger_workflow_core::store::{
    CreateDefinition, CreateRun, PublishRevision, ResolvedActionSchemas, StoreError, WorkflowStore,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs::File;
use std::future::Future;
use std::io::Read;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

struct ReturnArtifact {
    descriptor: ActionDescriptor,
    artifact: VerifiedObjectRef,
}

impl WorkflowAction for ReturnArtifact {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    fn invoke<'a>(
        &'a self,
        _context: ActionContext,
        _input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            ActionOutcome::success(
                json!({"value": 1}),
                vec![ArtifactOutput {
                    media_type: self.artifact.media_type().to_owned(),
                    object: self.artifact.clone(),
                }],
                CostUnits(1),
                None,
            )
            .unwrap()
        })
    }
}

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}

#[tokio::test]
#[ignore = "500 MiB disk/RSS acceptance probe; run compiled test under /usr/bin/time"]
async fn large_file_import_verify_and_read_uses_bounded_memory() {
    exercise_import_commit_and_read(500 * 1024 * 1024).await;
}

#[tokio::test]
async fn imported_binary_artifact_commits_and_host_reads_apply_corruption() {
    exercise_import_commit_and_read(1024 * 1024).await;
}

#[tokio::test]
async fn binary_file_capabilities_are_rejected_at_structured_publication_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.wav");
    std::fs::write(&source, b"binary audio").unwrap();
    let clock = Arc::new(TestClock::new(Timestamp(1)));
    let objects =
        Arc::new(FsObjectStore::open(root.path().join("objects"), clock.clone()).unwrap());
    let scope = ExecutionScope {
        tenant_id: ScopeAtom::new("binary-boundary").unwrap(),
        namespace: ScopeAtom::new("demo").unwrap(),
    };
    let binary = objects
        .import_file(&scope, source, "audio/wav")
        .await
        .unwrap();
    let schema = objects
        .put(
            &scope,
            br#"{"additionalProperties":false,"properties":{},"type":"object"}"#,
            "application/json",
        )
        .await
        .unwrap();
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: id("binary-boundary-definition"),
        name: "binary boundary".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.digest().clone(),
        run_output_schema_digest: schema.digest().clone(),
        nodes: vec![NodeDefinition::Succeed {
            id: id("done"),
            output: BindingSource::Constant { value: json!({}) },
        }],
    };
    let canonical = objects
        .put(
            &scope,
            &serde_jcs::to_vec(&definition).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let principal = AuthenticatedPrincipal::mint(
        scope.clone(),
        "demo-host".to_owned(),
        Vec::new(),
        schema.digest().clone(),
    )
    .unwrap();
    let memory = InMemoryStore::new(clock.clone());
    let sqlite = SqliteWorkflowStore::open(root.path().join("workflow.sqlite"), clock, objects)
        .await
        .unwrap();
    assert_binary_publication_rejected(
        &memory,
        &scope,
        &definition,
        &principal,
        &canonical,
        &schema,
        &binary,
    )
    .await;
    assert_binary_publication_rejected(
        &sqlite,
        &scope,
        &definition,
        &principal,
        &canonical,
        &schema,
        &binary,
    )
    .await;
}

async fn assert_binary_publication_rejected<S: WorkflowStore>(
    store: &S,
    scope: &ExecutionScope,
    definition: &WorkflowDefinition,
    principal: &AuthenticatedPrincipal,
    canonical: &VerifiedObjectRef,
    schema: &VerifiedObjectRef,
    binary: &VerifiedObjectRef,
) {
    for binary_definition in [true, false] {
        let result = store
            .publish_revision(
                scope,
                PublishRevision {
                    definition_id: definition.definition_id.clone(),
                    expected_definition_version: Version(1),
                    canonical_definition: if binary_definition {
                        binary.clone()
                    } else {
                        canonical.clone()
                    },
                    run_input_schema: if binary_definition {
                        schema.clone()
                    } else {
                        binary.clone()
                    },
                    run_output_schema: schema.clone(),
                    resolved_action_schema_objects: BTreeMap::new(),
                    parsed_revision: PublishableDefinition {
                        definition: definition.clone(),
                        topological_ranks: canonical_topological_ranks(definition).unwrap(),
                    },
                    principal: principal.clone(),
                },
            )
            .await;
        assert!(
            matches!(result, Err(StoreError::InvalidField)),
            "binary structured input must return a typed rejection, got {result:?}"
        );
    }
}

async fn exercise_import_commit_and_read(size: u64) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("large.wav");
    File::create(&source).unwrap().set_len(size).unwrap();
    let clock = Arc::new(TestClock::new(Timestamp(1)));
    let objects =
        Arc::new(FsObjectStore::open(root.path().join("objects"), clock.clone()).unwrap());
    let store = Arc::new(
        SqliteWorkflowStore::open(root.path().join("workflow.sqlite"), clock, objects.clone())
            .await
            .unwrap(),
    );
    let scope = ExecutionScope {
        tenant_id: ScopeAtom::new("large-artifact-tenant").unwrap(),
        namespace: ScopeAtom::new("demo").unwrap(),
    };
    let imported = objects
        .import_file(&scope, source, "audio/wav")
        .await
        .unwrap();
    assert_eq!(imported.size_bytes(), size);
    let verified = objects.verify(&scope, imported.digest()).await.unwrap();
    assert_eq!(verified, imported);

    let schema = objects.put(&scope, br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#, "application/json").await.unwrap();
    let principal = AuthenticatedPrincipal::mint(
        scope.clone(),
        "demo-host".to_owned(),
        Vec::new(),
        schema.digest().clone(),
    )
    .unwrap();
    let descriptor = ActionDescriptor {
        name: "demo.return_artifact".to_owned(),
        contract_version: "1".to_owned(),
        input_schema_digest: schema.digest().clone(),
        output_schema_digest: schema.digest().clone(),
        implementation_compatibility_digest: schema.digest().clone(),
    };
    let definition: WorkflowDefinition = serde_json::from_value(json!({
        "definition_format_version": "0.1",
        "definition_id": "large-file",
        "name": "large artifact acceptance",
        "description": "",
        "run_input_schema_digest": schema.digest(),
        "run_output_schema_digest": schema.digest(),
        "nodes": [
            {
                "kind": "Action", "id": "produce",
                "action": {
                    "name": descriptor.name,
                    "contract_version": "1",
                    "input_schema_digest": schema.digest(),
                    "output_schema_digest": schema.digest(),
                    "compatible_implementation_requirement": schema.digest()
                },
                "bindings": [{"target": "/value", "source": {"kind": "run_input", "pointer": "/value"}}],
                "retry": {"max_attempts": 1, "backoff": {"kind": "fixed", "delay_ms": 0}},
                "timeout": {"timeout_ms": 600000},
                "declared_max_cost_units": "1",
                "next": ["done"]
            },
            {"kind": "Succeed", "id": "done", "output": {"kind": "node_output", "node_id": "produce", "pointer": ""}}
        ]
    })).unwrap();
    store
        .create_definition(
            &scope,
            CreateDefinition {
                definition_id: definition.definition_id.clone(),
                display_name: definition.name.clone(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let canonical = objects
        .put(
            &scope,
            &serde_jcs::to_vec(&definition).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    store
        .publish_revision(
            &scope,
            PublishRevision {
                definition_id: definition.definition_id.clone(),
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema.clone(),
                resolved_action_schema_objects: BTreeMap::from([(
                    "produce".to_owned(),
                    ResolvedActionSchemas {
                        input_schema: schema.clone(),
                        output_schema: schema.clone(),
                    },
                )]),
                parsed_revision: PublishableDefinition {
                    topological_ranks: canonical_topological_ranks(&definition).unwrap(),
                    definition: definition.clone(),
                },
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let input = objects
        .put(&scope, br#"{"value":1}"#, "application/json")
        .await
        .unwrap();
    store
        .create_run(
            &scope,
            CreateRun {
                run_id: id("demo-run"),
                definition_id: definition.definition_id,
                revision_hash: canonical.digest().clone(),
                input,
                budget_limit: CostUnits(2),
                limits: RunLimits {
                    max_dynamic_node_instances: 10,
                    max_total_attempts: 10,
                    max_total_events: 1000,
                    max_inline_json_bytes_per_value: 10000,
                    max_artifacts_per_attempt: 10,
                    max_aggregate_object_bytes_per_run: size + 10000,
                    max_run_lifetime_ms: 600000,
                },
                principal,
                idempotency_token: "large-artifact-create-token".to_owned(),
            },
        )
        .await
        .unwrap();
    let registry = Arc::new(InMemoryActionRegistry::new());
    registry
        .register(Arc::new(ReturnArtifact {
            descriptor,
            artifact: imported.clone(),
        }))
        .unwrap();
    let engine = WorkflowEngine::new(
        store.clone(),
        objects.clone(),
        registry,
        EngineConfig {
            instance_id: id("demo-engine"),
            max_concurrency: 1,
            cancellation_grace: Duration::from_secs(1),
        },
    )
    .unwrap();
    engine.acquire_scope(&scope).await.unwrap();
    engine.start(&scope, &id("demo-run")).await.unwrap();
    engine.run_until_idle(&scope, 10).await.unwrap();
    assert_eq!(
        store
            .get_run(&scope, &id("demo-run"))
            .await
            .unwrap()
            .run
            .status,
        RunState::Succeeded
    );
    let attempt = store
        .get_latest_attempt(&scope, &id("demo-run"), &id("produce"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(attempt.artifact_refs.len(), 1);
    let committed = &attempt.artifact_refs[0];
    assert_eq!(&committed.digest, imported.digest());
    assert_eq!(committed.size_bytes, size);
    let reader = CommittedObjectReader::new(store.clone(), objects.clone());
    let CommittedReadOutcome::Verified(mut snapshot) = reader
        .read_file(&scope, &id("demo-run"), committed, None)
        .await
    else {
        panic!("committed binary must read as a verified snapshot");
    };
    assert_eq!(snapshot.reference.size_bytes(), size);
    let mut count = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = snapshot.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        assert!(buffer[..read].iter().all(|byte| *byte == 0));
        count += read as u64;
    }
    assert_eq!(count, size);

    let hex = imported.digest().as_str().strip_prefix("sha256:").unwrap();
    let atom_hex = |text: &str| {
        text.as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let object_path = objects
        .root()
        .join("objects")
        .join(atom_hex(scope.tenant_id.as_str()))
        .join(atom_hex(scope.namespace.as_str()))
        .join(&hex[..2])
        .join(hex);
    std::fs::write(object_path, b"corrupt").unwrap();
    assert!(matches!(
        reader
            .read_file(&scope, &id("demo-run"), committed, None)
            .await,
        CommittedReadOutcome::CorruptionApplied { .. }
    ));
    assert_eq!(
        store
            .get_run(&scope, &id("demo-run"))
            .await
            .unwrap()
            .run
            .status,
        RunState::CorruptStorage
    );
}
