use dagger_workflow_core::action::manifest::{ActionManifest, ActionManifestError, ManifestAction};
use dagger_workflow_core::action::schema::{opaque_schema_bundle, SchemaValidationMode};
use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, ActionRegistry, InMemoryActionRegistry,
    WorkflowAction,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::ObjectStore;
use dagger_workflow_core::definition::{
    canonical_definition_json, parse_json_definition, resolve_publication, revision_hash,
    validate_definition, WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::event::EventType;
use dagger_workflow_core::ids::{CostUnits, Digest, Id, Timestamp, Version};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::run::{RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::store::{
    CreateDefinition, CreateRun, EventPageRequest, PublishRevision, WorkflowStore,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

struct CommandAction {
    descriptor: ActionDescriptor,
}

impl WorkflowAction for CommandAction {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    fn invoke<'a>(
        &'a self,
        _: ActionContext,
        bytes: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            ActionOutcome::success(
                serde_json::from_slice(bytes).unwrap(),
                Vec::new(),
                CostUnits(0),
                None,
            )
            .unwrap()
        })
    }
}

fn bind(entry: &ManifestAction) -> Result<Arc<dyn WorkflowAction>, String> {
    Ok(Arc::new(CommandAction {
        descriptor: entry.descriptor().clone(),
    }))
}

fn manifest_json(count: usize) -> Value {
    json!({
        "actions": (0..count).map(|index| json!({
            "name": format!("acme.tool:mock.{index}@1"),
            "contract_version": "v1",
            "input_schema": {
                "type": "object",
                "properties": { format!("item{index}"): {"type": "string"} },
                "additionalProperties": false
            },
            "output_schema": {},
            "implementation_digest": revision_hash(format!("implementation-{index}").as_bytes()),
            "memoize": index % 2 == 0,
            "required_capabilities": ["acme.cli"]
        })).collect::<Vec<_>>()
    })
}

fn definition(manifest: &ActionManifest) -> WorkflowDefinition {
    let opaque = revision_hash(b"{}");
    let mut nodes = manifest
        .actions()
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            json!({
                "kind": "Action",
                "id": format!("tool{index}"),
                "action": entry.action_reference(),
                "bindings": [],
                "retry": {"max_attempts": 1, "backoff": {"kind": "fixed", "delay_ms": 0}},
                "timeout": {"timeout_ms": 1},
                "declared_max_cost_units": "0",
                "next": ["done"]
            })
        })
        .collect::<Vec<_>>();
    nodes.push(
        json!({"kind": "Succeed", "id": "done", "output": {"kind": "constant", "value": {}}}),
    );
    parse_json_definition(
        &json!({
            "definition_format_version": "0.1",
            "definition_id": "forty-tools",
            "name": "Forty tools",
            "run_input_schema_digest": manifest.run_input_schema_digest().unwrap_or(&opaque),
            "run_output_schema_digest": manifest.run_output_schema_digest().unwrap_or(&opaque),
            "nodes": nodes
        })
        .to_string(),
    )
    .unwrap()
}

#[tokio::test]
async fn forty_distinct_command_descriptors_publish_with_verified_schemas() {
    let manifest = ActionManifest::from_json(&manifest_json(40).to_string()).unwrap();
    let registry = Arc::new(InMemoryActionRegistry::new());
    manifest.register(&registry, bind).unwrap();
    for (index, entry) in manifest.actions().iter().enumerate() {
        let registered = registry.resolve(&entry.descriptor().name).unwrap();
        assert_eq!(registered.descriptor(), entry.descriptor());
        assert_eq!(registered.memoize(), index % 2 == 0);
        assert_eq!(registered.required_capabilities(), ["acme.cli"]);
    }

    let definition = definition(&manifest);
    let parsed_revision = resolve_publication(
        validate_definition(&definition).unwrap(),
        &manifest.publication_resolver(&*registry),
    )
    .unwrap();
    let clock = Arc::new(TestClock::new(Timestamp(100)));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let objects = Arc::new(InMemoryObjectStore::new(clock));
    let scope = ExecutionScope {
        tenant_id: ScopeAtom::new("tenant").unwrap(),
        namespace: ScopeAtom::new("demo").unwrap(),
    };
    let principal = AuthenticatedPrincipal::mint(
        scope.clone(),
        "publisher".to_owned(),
        Vec::new(),
        revision_hash(b"publisher"),
    )
    .unwrap();
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
    let materials = manifest.publish_schemas(&*objects, &scope).await.unwrap();
    let canonical = canonical_definition_json(&parsed_revision.definition).unwrap();
    let canonical_definition = objects
        .put(&scope, &canonical, "application/json")
        .await
        .unwrap();
    let revision = store
        .publish_revision(
            &scope,
            PublishRevision {
                definition_id: definition.definition_id.clone(),
                expected_definition_version: Version(1),
                canonical_definition,
                run_input_schema: materials
                    .schema(&definition.run_input_schema_digest)
                    .unwrap()
                    .clone(),
                run_output_schema: materials
                    .schema(&definition.run_output_schema_digest)
                    .unwrap()
                    .clone(),
                resolved_action_schema_objects: materials
                    .resolve_action_schemas(&definition)
                    .unwrap(),
                parsed_revision,
                principal,
            },
        )
        .await
        .unwrap();
    assert_eq!(revision.action_pins.len(), 40);
    assert!(registry
        .check_pins(&revision.action_pins)
        .incompatible_reference_locations
        .is_empty());
    for pin in &revision.action_pins {
        let canonical_input = objects.get(&scope, &pin.input_schema_digest).await.unwrap();
        let entry = manifest
            .actions()
            .iter()
            .find(|entry| entry.descriptor().name == pin.name)
            .unwrap();
        assert_eq!(
            canonical_input.bytes,
            serde_jcs::to_vec(entry.input_schema()).unwrap()
        );
        assert_eq!(pin.input_schema_ref.0.digest, pin.input_schema_digest);
    }

    let run_id = Id::new("versioned-tools-run").unwrap();
    store
        .create_run(
            &scope,
            CreateRun {
                run_id: run_id.clone(),
                definition_id: definition.definition_id.clone(),
                revision_hash: revision.revision_hash.clone(),
                input: objects
                    .put(&scope, b"{}", "application/json")
                    .await
                    .unwrap(),
                budget_limit: CostUnits(0),
                principal: AuthenticatedPrincipal::mint(
                    scope.clone(),
                    "runner".into(),
                    Vec::new(),
                    revision_hash(b"runner"),
                )
                .unwrap(),
                limits: RunLimits {
                    max_dynamic_node_instances: 0,
                    max_total_attempts: 40,
                    max_total_events: 1000,
                    max_inline_json_bytes_per_value: 100_000,
                    max_artifacts_per_attempt: 0,
                    max_aggregate_object_bytes_per_run: 1_000_000,
                    max_run_lifetime_ms: 60_000,
                },
                idempotency_token: "versioned-tools-run-create".into(),
            },
        )
        .await
        .unwrap();
    let engine = WorkflowEngine::new(
        store.clone(),
        objects.clone(),
        registry,
        EngineConfig {
            instance_id: Id::new("versioned-tools-engine").unwrap(),
            max_concurrency: 8,
            cancellation_grace: std::time::Duration::from_secs(1),
        },
    )
    .unwrap();
    engine.acquire_scope(&scope).await.unwrap();
    engine.start(&scope, &run_id).await.unwrap();
    engine.run_until_idle(&scope, 100).await.unwrap();
    assert_eq!(
        store.get_run(&scope, &run_id).await.unwrap().run.status,
        RunState::Succeeded
    );
    let events = store
        .list_events_after(
            &scope,
            &run_id,
            EventPageRequest {
                after_event_seq: 0,
                page_size: 1000,
                hard_response_byte_limit: 1_000_000,
            },
        )
        .await
        .unwrap();
    for (index, action) in manifest.actions().iter().enumerate() {
        let attempt = store
            .get_latest_attempt(&scope, &run_id, &Id::new(format!("tool{index}")).unwrap())
            .await
            .unwrap()
            .unwrap();
        let invocation = store
            .get_invocation(&scope, &run_id, &attempt.invocation_id)
            .unwrap();
        assert_eq!(invocation.action_name, action.descriptor().name);
        let claim = events
            .iter()
            .find(|event| {
                event.event_type == EventType::NodeAttemptClaimed
                    && event.attempt_id.as_ref() == Some(&attempt.attempt_id)
            })
            .unwrap();
        assert_eq!(claim.payload["action_name"], action.descriptor().name);
    }
}

#[test]
fn canonical_schema_digest_agrees_between_json_yaml_and_publication() {
    let document = manifest_json(1);
    let json_manifest = ActionManifest::from_json(&document.to_string()).unwrap();
    let identity = document["actions"][0]["implementation_digest"]
        .as_str()
        .unwrap();
    let yaml = format!(
        "actions:\n  - output_schema: {{}}\n    input_schema:\n      additionalProperties: false\n      properties:\n        item0: {{type: string}}\n      type: object\n    implementation_digest: {identity}\n    name: acme.tool:mock.0@1\n    contract_version: v1\n    memoize: true\n    capability_tags: [acme.cli]\n"
    );
    let yaml_manifest = ActionManifest::from_yaml(&yaml).unwrap();
    assert_eq!(json_manifest, yaml_manifest);
    let input = json_manifest.actions()[0].input_schema();
    assert_eq!(
        json_manifest.actions()[0].descriptor().input_schema_digest,
        revision_hash(&serde_jcs::to_vec(input).unwrap())
    );

    let mut changed = document;
    changed["actions"][0]["input_schema"]["properties"]["item0"]["type"] = json!("integer");
    let changed_manifest = ActionManifest::from_json(&changed.to_string()).unwrap();
    assert_ne!(
        json_manifest.actions()[0].descriptor().input_schema_digest,
        changed_manifest.actions()[0]
            .descriptor()
            .input_schema_digest
    );
    assert_eq!(
        json_manifest.actions()[0].descriptor().output_schema_digest,
        changed_manifest.actions()[0]
            .descriptor()
            .output_schema_digest
    );
}

#[test]
fn omitted_metadata_defaults_to_uncached_without_capabilities() {
    let mut document = manifest_json(1);
    document["actions"][0]
        .as_object_mut()
        .unwrap()
        .remove("memoize");
    document["actions"][0]
        .as_object_mut()
        .unwrap()
        .remove("required_capabilities");
    let manifest = ActionManifest::from_json(&document.to_string()).unwrap();
    let registry = InMemoryActionRegistry::new();
    manifest.register(&registry, bind).unwrap();
    let action = registry
        .resolve(&manifest.actions()[0].descriptor().name)
        .unwrap();
    assert!(!action.memoize());
    assert!(action.required_capabilities().is_empty());
}

#[test]
fn descriptor_mismatches_and_factory_errors_install_no_entries() {
    let manifest = ActionManifest::from_json(&manifest_json(2).to_string()).unwrap();
    let mutations: [fn(&mut ActionDescriptor); 5] = [
        |descriptor| descriptor.name = "different".to_owned(),
        |descriptor| descriptor.contract_version = "v2".to_owned(),
        |descriptor| descriptor.input_schema_digest = revision_hash(b"different input"),
        |descriptor| descriptor.output_schema_digest = revision_hash(b"different output"),
        |descriptor| {
            descriptor.implementation_compatibility_digest =
                revision_hash(b"different implementation")
        },
    ];
    for mutate in mutations {
        let registry = InMemoryActionRegistry::new();
        let mut count = 0;
        let result = manifest.register(&registry, |entry| {
            count += 1;
            let mut descriptor = entry.descriptor().clone();
            if count == 2 {
                mutate(&mut descriptor);
            }
            Ok(Arc::new(CommandAction { descriptor }))
        });
        assert!(matches!(
            result,
            Err(ActionManifestError::DescriptorMismatch { .. })
        ));
        assert!(registry
            .resolve(&manifest.actions()[0].descriptor().name)
            .is_none());
    }
    let registry = InMemoryActionRegistry::new();
    let mut count = 0;
    let result = manifest.register(&registry, |entry| {
        count += 1;
        if count == 2 {
            Err("unavailable command".to_owned())
        } else {
            bind(entry)
        }
    });
    assert!(matches!(result, Err(ActionManifestError::Factory { .. })));
    assert!(registry
        .resolve(&manifest.actions()[0].descriptor().name)
        .is_none());
}

#[test]
fn registry_name_conflict_rejects_complete_manifest_batch() {
    let manifest = ActionManifest::from_json(&manifest_json(2).to_string()).unwrap();
    let registry = InMemoryActionRegistry::new();
    let existing = bind(&manifest.actions()[1]).unwrap();
    registry.register(existing.clone()).unwrap();
    assert!(matches!(
        manifest.register(&registry, bind),
        Err(ActionManifestError::Registration(_))
    ));
    assert!(registry
        .resolve(&manifest.actions()[0].descriptor().name)
        .is_none());
    assert!(Arc::ptr_eq(
        &registry.resolve(&existing.descriptor().name).unwrap(),
        &existing
    ));
}

#[test]
fn manifest_rejects_duplicates_unknown_fields_and_unsupported_schemas() {
    let original = manifest_json(1);
    let mut duplicate = original.clone();
    duplicate["actions"]
        .as_array_mut()
        .unwrap()
        .push(original["actions"][0].clone());
    assert!(matches!(
        ActionManifest::from_json(&duplicate.to_string()),
        Err(ActionManifestError::DuplicateName { .. })
    ));

    for (field, value) in [
        ("unrecognized", json!(true)),
        ("implementation_digest", json!("bad digest")),
        (
            "input_schema",
            json!({"type": "object", "patternProperties": {}}),
        ),
        ("output_schema", json!(false)),
        ("contract_version", json!("")),
        ("required_capabilities", json!(["render", "render"])),
    ] {
        let mut document = original.clone();
        document["actions"][0][field] = value;
        assert!(
            ActionManifest::from_json(&document.to_string()).is_err(),
            "accepted invalid {field}"
        );
    }
    let duplicated_schema_key = original.to_string().replace(
        "\"output_schema\":{}",
        "\"output_schema\":{\"type\":\"object\",\"type\":\"string\"}",
    );
    assert!(matches!(
        ActionManifest::from_json(&duplicated_schema_key),
        Err(ActionManifestError::Parse(_))
    ));
    assert!(ActionManifest::from_yaml("actions: []\nactions: []\n").is_err());
    assert!(ActionManifest::from_yaml("actions: []\n---\nactions: []\n").is_err());
    assert!(ActionManifest::from_json("{\"actions\":[],\"unknown\":true}").is_err());
    let identity = original["actions"][0]["implementation_digest"]
        .as_str()
        .unwrap();
    assert!(ActionManifest::from_yaml(&format!(
        "actions:\n  - name: test\n    contract_version: v1\n    implementation_digest: {identity}\n    input_schema:\n      type: object\n      type: string\n    output_schema: {{}}\n"
    )).is_err());
}

#[test]
fn batch_registration_rejects_internal_duplicates_without_mutation() {
    let manifest = ActionManifest::from_json(&manifest_json(1).to_string()).unwrap();
    let action = bind(&manifest.actions()[0]).unwrap();
    let registry = InMemoryActionRegistry::new();
    assert!(registry.register_many([action.clone(), action]).is_err());
    assert!(registry
        .resolve(&manifest.actions()[0].descriptor().name)
        .is_none());
}

#[tokio::test]
async fn definition_schema_materials_reject_unavailable_pins() {
    let manifest = ActionManifest::from_json(&manifest_json(1).to_string()).unwrap();
    let objects = InMemoryObjectStore::new(Arc::new(TestClock::new(Timestamp(1))));
    let scope = ExecutionScope {
        tenant_id: ScopeAtom::new("tenant").unwrap(),
        namespace: ScopeAtom::new("demo").unwrap(),
    };
    let materials = manifest.publish_schemas(&objects, &scope).await.unwrap();
    let mut definition = definition(&manifest);
    let dagger_workflow_core::definition::NodeDefinition::Action { action, .. } =
        &mut definition.nodes[0]
    else {
        unreachable!()
    };
    action.input_schema_digest = Digest::new(format!("sha256:{}", "0".repeat(64))).unwrap();
    assert!(matches!(
        materials.resolve_action_schemas(&definition),
        Err(ActionManifestError::MissingSchema { .. })
    ));
}

#[test]
fn referenced_common_document_changes_every_using_pin_without_rewriting_documents() {
    let common_uri = "https://acme.local/schemas/common.json";
    let common = json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "$id": common_uri,
        "$defs": {"value": {"oneOf": [{"type": "integer"}, {"type": "null"}]}}});
    let schema = json!({"$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://acme.local/schemas/action.json", "description": "preserved exactly",
        "allOf": [{"$ref": "common.json#/$defs/value"}]});
    let mut source = manifest_json(3);
    source["schema_documents"] =
        json!({common_uri: common.clone(), "unused.json": {"type": "string"}});
    for index in [0, 1] {
        source["actions"][index]["input_schema"] = schema.clone();
        source["actions"][index]["output_schema"] = schema.clone();
    }
    source["actions"][2]["input_schema"] = json!({"type": "string"});
    let before = ActionManifest::from_json_with_schema_mode(
        &source.to_string(),
        SchemaValidationMode::Opaque,
    )
    .unwrap();
    let dependencies: BTreeMap<String, Value> =
        serde_json::from_value(source["schema_documents"].clone()).unwrap();
    let bundle = opaque_schema_bundle(schema.clone(), &dependencies).unwrap();
    assert_eq!(bundle["documents"]["root"], schema);
    assert_eq!(bundle["documents"][common_uri], common);
    assert_eq!(bundle["documents"].as_object().unwrap().len(), 2);
    assert_eq!(
        before.actions()[0].descriptor().input_schema_digest,
        revision_hash(&serde_jcs::to_vec(&bundle).unwrap())
    );
    source["schema_documents"][common_uri]["$defs"]["value"]["oneOf"][0]["type"] = json!("string");
    let after = ActionManifest::from_json_with_schema_mode(
        &source.to_string(),
        SchemaValidationMode::Opaque,
    )
    .unwrap();
    for index in [0, 1] {
        assert_ne!(
            before.actions()[index].descriptor().input_schema_digest,
            after.actions()[index].descriptor().input_schema_digest
        );
        assert_ne!(
            before.actions()[index].descriptor().output_schema_digest,
            after.actions()[index].descriptor().output_schema_digest
        );
    }
    assert_eq!(
        before.actions()[2].descriptor(),
        after.actions()[2].descriptor()
    );
}

#[test]
fn closed_bundles_accept_recursive_anchors_and_ignore_literal_dollar_ref_data() {
    let root = json!({"$schema": "https://json-schema.org/draft/2020-12/schema",
    "$id": "https://acme.local/root.json", "$dynamicAnchor": "node",
    "type": "object", "$defs": {"value": {"$anchor": "value", "type": "string"}},
    "properties": {
        "child": {"$dynamicRef": "#node"},
        "value": {"$ref": "#value", "description": "a ref sibling stays intact"},
        "pointer": {"$ref": "#/%24defs/value"},
        "literal": {"const": {"$ref": "https://not-a-schema.invalid/data"}}
    }});
    let bundle = opaque_schema_bundle(root.clone(), &BTreeMap::new()).unwrap();
    assert_eq!(bundle["documents"]["root"], root);
    for invalid in [
        json!({"$ref": "https://missing.invalid/common.json"}),
        json!({"$ref": "#/$defs/missing"}),
        json!({"$ref": "#missing-anchor"}),
        json!({"$ref": "#/%GG"}),
        json!({"$ref": 1}),
        json!({"$schema": "https://json-schema.org/draft-07/schema"}),
        json!({"allOf": {}}),
    ] {
        assert!(opaque_schema_bundle(invalid, &BTreeMap::new()).is_err());
    }
}

#[test]
fn opaque_mode_does_not_silently_weaken_existing_supported_subset() {
    let root = json!({"oneOf": [{"type": "string"}, {"type": "integer"}]});
    let bundle = opaque_schema_bundle(root.clone(), &BTreeMap::new()).unwrap();
    let mut source = manifest_json(1);
    source["actions"][0]["input_schema"] = root;
    assert!(ActionManifest::from_json(&source.to_string()).is_err());
    assert!(ActionManifest::from_json_with_schema_mode(
        &source.to_string(),
        SchemaValidationMode::Opaque
    )
    .is_ok());
    source["actions"][0]["input_schema"] = bundle;
    assert!(ActionManifest::from_json(&source.to_string()).is_err());
    source["actions"][0]["input_schema"] = json!({"$ref": "https://missing.invalid/schema"});
    assert!(ActionManifest::from_json_with_schema_mode(
        &source.to_string(),
        SchemaValidationMode::Opaque
    )
    .is_err());
    source["actions"][0]["input_schema"] = json!({"type": "string"});
    source["actions"][0]["input_schema_digest"] = json!(revision_hash(b"another-schema"));
    assert!(ActionManifest::from_json_with_schema_mode(
        &source.to_string(),
        SchemaValidationMode::Opaque
    )
    .is_err());
}
