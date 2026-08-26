use dagger_workflow_core::definition::{
    canonical_definition_json, parse_json_definition, parse_yaml_definition, resolve_publication,
    revision_hash, validate_definition, DefinitionValidationError, PublicationResolver,
    PublicationSchemaDocument, ValidationErrorKind,
};
use dagger_workflow_core::ids::Digest;
use std::collections::BTreeMap;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn valid() -> String {
    format!(
        r#"{{"definition_format_version":"0.1","definition_id":"example","name":"Example","run_input_schema_digest":"{DIGEST}","run_output_schema_digest":"{DIGEST}","entry_node_id":"work","nodes":[{{"id":"work","kind":"Action","action":{{"name":"example.action","contract_version":"v1","input_schema_digest":"{DIGEST}","output_schema_digest":"{DIGEST}","compatible_implementation_requirement":"{DIGEST}"}},"bindings":[],"retry":{{"max_attempts":1,"backoff":{{"kind":"fixed","delay_ms":0}}}},"timeout":{{"timeout_ms":1}},"declared_max_cost_units":"0","next":["done"]}},{{"id":"done","kind":"Succeed","output":{{"kind":"constant","value":null}}}}]}}"#
    )
}

fn invalid_kind(input: &str) -> ValidationErrorKind {
    let definition = parse_json_definition(input).expect("syntax should parse");
    validate_definition(&definition).expect_err("definition should be rejected")[0]
        .kind
        .clone()
}

#[test]
fn reference_workflows_parse_and_validate() {
    for fixture in ["legal_research.yaml", "intel_digest.yaml"] {
        let path = format!(
            "{}/../docs/reference_workflows/{fixture}",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(path).unwrap();
        let definition = parse_yaml_definition(&text).unwrap();
        let unresolved = validate_definition(&definition).unwrap();
        assert!(
            resolve_publication(unresolved, &EmptyResolver).is_err(),
            "README fixtures are deliberately non-publishable without schemas and registry pins"
        );
    }
}

struct EmptyResolver;
impl PublicationResolver for EmptyResolver {
    fn schema_document(&self, _: &Digest) -> Option<PublicationSchemaDocument> {
        None
    }
    fn artifact_exists(&self, _: &dagger_workflow_core::ids::Id, _: &Digest, _: &str) -> bool {
        false
    }
    fn action_pin_available(
        &self,
        _: &dagger_workflow_core::definition::ExtractedActionPin,
    ) -> bool {
        false
    }
}

struct FakeResolver {
    schemas: BTreeMap<Digest, serde_json::Value>,
}
impl PublicationResolver for FakeResolver {
    fn schema_document(&self, digest: &Digest) -> Option<PublicationSchemaDocument> {
        self.schemas
            .get(digest)
            .cloned()
            .map(|value| PublicationSchemaDocument {
                digest: digest.clone(),
                value,
            })
    }
    fn artifact_exists(&self, _: &dagger_workflow_core::ids::Id, _: &Digest, _: &str) -> bool {
        true
    }
    fn action_pin_available(
        &self,
        _: &dagger_workflow_core::definition::ExtractedActionPin,
    ) -> bool {
        true
    }
}

fn resolve_action_bindings(
    action_input_schema: serde_json::Value,
    bindings: serde_json::Value,
) -> Result<(), Vec<DefinitionValidationError>> {
    let run_input_schema = serde_json::json!({
        "additionalProperties": false,
        "properties": {
            "date": {"type": "string"},
            "summaries": {"type": "integer"}
        },
        "type": "object"
    });
    let output_schema = serde_json::json!({});
    let run_input_digest = revision_hash(&serde_jcs::to_vec(&run_input_schema).unwrap());
    let action_input_digest = revision_hash(&serde_jcs::to_vec(&action_input_schema).unwrap());
    let output_digest = revision_hash(&serde_jcs::to_vec(&output_schema).unwrap());
    let definition = parse_json_definition(
        &serde_json::json!({
            "definition_format_version": "0.1",
            "definition_id": "deep-bindings",
            "name": "deep bindings",
            "run_input_schema_digest": run_input_digest,
            "run_output_schema_digest": output_digest,
            "entry_node_id": "work",
            "nodes": [
                {
                    "id": "work",
                    "kind": "Action",
                    "action": {
                        "name": "example.action",
                        "contract_version": "v1",
                        "input_schema_digest": action_input_digest,
                        "output_schema_digest": output_digest,
                        "compatible_implementation_requirement": DIGEST
                    },
                    "bindings": bindings,
                    "retry": {"max_attempts": 1, "backoff": {"kind": "fixed", "delay_ms": 0}},
                    "timeout": {"timeout_ms": 1},
                    "declared_max_cost_units": "0",
                    "next": ["done"]
                },
                {"id": "done", "kind": "Succeed", "output": {"kind": "constant", "value": null}}
            ]
        })
        .to_string(),
    )
    .unwrap();
    let unresolved = validate_definition(&definition)?;
    let schemas = BTreeMap::from([
        (run_input_digest, run_input_schema),
        (action_input_digest, action_input_schema),
        (output_digest, output_schema),
    ]);
    resolve_publication(unresolved, &FakeResolver { schemas }).map(|_| ())
}

fn deep_bindings() -> serde_json::Value {
    serde_json::json!([
        {"target": "/data/date", "source": {"kind": "run_input", "pointer": "/date"}},
        {"target": "/data/summaries", "source": {"kind": "run_input", "pointer": "/summaries"}}
    ])
}

#[test]
fn publication_accepts_deep_bindings_below_required_opaque_input() {
    let schema = serde_json::json!({
        "additionalProperties": false,
        "properties": {"data": {}},
        "required": ["data"],
        "type": "object"
    });
    resolve_action_bindings(schema, deep_bindings()).unwrap();
}

#[test]
fn publication_type_checks_deep_bindings_below_typed_input() {
    let schema = |date_type| {
        serde_json::json!({
            "additionalProperties": false,
            "properties": {
                "data": {
                    "additionalProperties": false,
                    "properties": {
                        "date": {"type": date_type},
                        "summaries": {"type": "integer"}
                    },
                    "required": ["date", "summaries"],
                    "type": "object"
                }
            },
            "required": ["data"],
            "type": "object"
        })
    };
    resolve_action_bindings(schema("string"), deep_bindings()).unwrap();
    let errors = resolve_action_bindings(schema("integer"), deep_bindings()).unwrap_err();
    assert!(errors
        .iter()
        .any(|error| error.kind == ValidationErrorKind::BindingTypeMismatch));
}

#[test]
fn publication_rejects_unresolvable_binding_target_with_nearest_ancestor() {
    let schema = serde_json::json!({
        "additionalProperties": false,
        "properties": {"data": {"type": "string"}},
        "type": "object"
    });
    let errors = resolve_action_bindings(
        schema,
        serde_json::json!([
            {"target": "/dta/date", "source": {"kind": "run_input", "pointer": "/date"}}
        ]),
    )
    .unwrap_err();
    let error = errors
        .iter()
        .find(|error| error.kind == ValidationErrorKind::BindingTargetInvalid)
        .unwrap();
    assert!(error.message.contains("work"));
    assert!(error.message.contains("/dta/date"));
    assert!(error.message.contains("<root>"));
}

#[test]
fn validation_rejects_overlapping_binding_targets_and_names_both() {
    let errors = resolve_action_bindings(
        serde_json::json!({
            "additionalProperties": false,
            "properties": {"data": {}},
            "type": "object"
        }),
        serde_json::json!([
            {"target": "/data", "source": {"kind": "run_input", "pointer": "/date"}},
            {"target": "/data/date", "source": {"kind": "run_input", "pointer": "/date"}}
        ]),
    )
    .unwrap_err();
    let error = errors
        .iter()
        .find(|error| error.kind == ValidationErrorKind::BindingTargetInvalid)
        .unwrap();
    assert!(error.message.contains("/data`"));
    assert!(error.message.contains("/data/date`"));
}

#[test]
fn publication_requires_external_resolution_then_accepts_matching_fakes() {
    let schema = serde_json::json!({"type":"object"});
    let digest = revision_hash(&serde_jcs::to_vec(&schema).unwrap());
    let definition = parse_json_definition(&valid().replace(DIGEST, digest.as_str())).unwrap();
    let unresolved = validate_definition(&definition).unwrap();
    assert!(resolve_publication(unresolved.clone(), &EmptyResolver).is_err());
    let mut schemas = BTreeMap::new();
    schemas.insert(digest, schema);
    assert!(resolve_publication(unresolved, &FakeResolver { schemas }).is_ok());
}

#[test]
fn canonical_hash_ignores_authoring_noise_but_not_array_order() {
    let known_rfc8785_value: serde_json::Value =
        serde_json::from_str(r#"{"nested":{"z":false,"a":null},"b":1,"a":"€"}"#).unwrap();
    assert_eq!(
        serde_jcs::to_vec(&known_rfc8785_value).unwrap(),
        "{\"a\":\"€\",\"b\":1,\"nested\":{\"a\":null,\"z\":false}}".as_bytes()
    );
    let json = valid();
    let a = parse_json_definition(&json).unwrap();
    let yaml = format!(
        "# comment\n{}",
        json.replace('{', "{\n").replace(',', ",\n")
    );
    let b = parse_yaml_definition(&yaml).unwrap();
    assert_eq!(
        revision_hash(&canonical_definition_json(&a).unwrap()),
        revision_hash(&canonical_definition_json(&b).unwrap())
    );

    let mut with_two_targets: serde_json::Value = serde_json::from_str(&json).unwrap();
    with_two_targets["nodes"][0]["next"] = serde_json::json!(["done", "failed"]);
    with_two_targets["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "failed", "kind": "Fail", "code": "test.failed", "message": "failed"
        }));
    let forward = parse_json_definition(&with_two_targets.to_string()).unwrap();
    with_two_targets["nodes"][0]["next"] = serde_json::json!(["failed", "done"]);
    let reverse = parse_json_definition(&with_two_targets.to_string()).unwrap();
    assert_ne!(
        revision_hash(&canonical_definition_json(&forward).unwrap()),
        revision_hash(&canonical_definition_json(&reverse).unwrap())
    );
}

#[test]
fn yaml_aliases_and_json_key_reordering_normalize_to_identical_canonical_bytes() {
    let yaml = format!(
        r#"
definition_format_version: "0.1"
definition_id: example
name: Example
run_input_schema_digest: &digest {DIGEST}
run_output_schema_digest: *digest
entry_node_id: work
nodes:
  - kind: Action
    id: work
    action:
      name: example.action
      contract_version: v1
      input_schema_digest: *digest
      output_schema_digest: *digest
      compatible_implementation_requirement: *digest
    bindings: []
    retry: {{ max_attempts: 1, backoff: {{ kind: fixed, delay_ms: 0 }} }}
    timeout: {{ timeout_ms: 1 }}
    declared_max_cost_units: "0"
    next: [done]
  - output: {{ kind: constant, value: null }}
    id: done
    kind: Succeed
"#
    );
    let json = parse_json_definition(&valid()).unwrap();
    let aliased = parse_yaml_definition(&yaml).unwrap();
    assert_eq!(
        canonical_definition_json(&json).unwrap(),
        canonical_definition_json(&aliased).unwrap()
    );
}

#[test]
fn invalid_definition_table_is_actionable() {
    let duplicate = valid().replace("\"id\":\"done\"", "\"id\":\"work\"");
    assert_eq!(
        invalid_kind(&duplicate),
        ValidationErrorKind::DuplicateNodeId
    );
    let missing = valid().replace("\"next\":[\"done\"]", "\"next\":[\"missing\"]");
    assert_eq!(invalid_kind(&missing), ValidationErrorKind::MissingNode);
    let cycle = valid().replace("\"output\":{\"kind\":\"constant\",\"value\":null}", "\"action\":{\"name\":\"x\",\"contract_version\":\"v1\",\"input_schema_digest\":\"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"output_schema_digest\":\"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"compatible_implementation_requirement\":\"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"},\"bindings\":[],\"retry\":{\"max_attempts\":1,\"backoff\":{\"kind\":\"fixed\",\"delay_ms\":0}},\"timeout\":{\"timeout_ms\":1},\"declared_max_cost_units\":\"0\",\"next\":[\"work\"]").replace("\"kind\":\"Succeed\"", "\"kind\":\"Action\"");
    assert_eq!(invalid_kind(&cycle), ValidationErrorKind::Cycle);
    let map = valid().replace("\"kind\":\"Action\"", "\"kind\":\"Map\"");
    assert!(
        parse_json_definition(&map).is_err(),
        "a Map cannot omit mandatory bounds and child action fields"
    );
}

#[test]
fn friction_1_reconverged_node_output_is_rejected() {
    // 1: a post-Choice reconvergence consumer cannot use a branch-only output.
    let reconvergence = format!(
        r#"{{"definition_format_version":"0.1","definition_id":"x","name":"x","run_input_schema_digest":"{DIGEST}","run_output_schema_digest":"{DIGEST}","entry_node_id":"choose","nodes":[{{"id":"choose","kind":"Choice","input":{{"kind":"constant","value":true}},"selector":"","cases":[{{"equals":true,"next":"branch"}}],"default":"join"}},{{"id":"branch","kind":"Action","action":{{"name":"x","contract_version":"v1","input_schema_digest":"{DIGEST}","output_schema_digest":"{DIGEST}","compatible_implementation_requirement":"{DIGEST}"}},"bindings":[],"retry":{{"max_attempts":1,"backoff":{{"kind":"fixed","delay_ms":0}}}},"timeout":{{"timeout_ms":1}},"declared_max_cost_units":"0","next":["join"]}},{{"id":"join","kind":"Succeed","output":{{"kind":"node_output","node_id":"branch","pointer":""}}}}]}}"#
    );
    assert_eq!(
        invalid_kind(&reconvergence),
        ValidationErrorKind::BindingSourceInvalid
    );
}

fn rejected_extension(document: String, field: &str) {
    let error = parse_json_definition(&document)
        .expect_err("closed format must reject this authoring construct");
    assert_eq!(error.kind, ValidationErrorKind::UnknownField, "{field}");
}

#[test]
fn friction_2_virtual_start_is_not_a_definition_node() {
    rejected_extension(
        valid().replace(
            "\"entry_node_id\"",
            "\"start\":{\"kind\":\"Start\"},\"entry_node_id\"",
        ),
        "start",
    );
}
#[test]
fn friction_3_composite_value_source_is_not_bindings() {
    rejected_extension(
        valid().replace(
            "\"bindings\":[]",
            "\"inputs\":[{\"target\":\"/x\"}],\"bindings\":[]",
        ),
        "inputs",
    );
}
#[test]
fn friction_4_map_cannot_embed_a_subworkflow() {
    rejected_extension(valid().replace("\"kind\":\"Action\"", "\"kind\":\"Map\",\"items\":{\"kind\":\"constant\",\"value\":[]},\"max_items\":1,\"max_concurrency\":1,\"subworkflow\":{}"), "subworkflow");
}
#[test]
fn friction_6_schedule_trigger_is_host_owned() {
    rejected_extension(
        valid().replace(
            "\"entry_node_id\"",
            "\"trigger\":{\"cron\":\"* * * * *\"},\"entry_node_id\"",
        ),
        "trigger",
    );
}
#[test]
fn friction_7_approval_output_cannot_be_author_defined() {
    rejected_extension(
        valid().replace(
            "\"bindings\":[]",
            "\"approval_output\":{\"report\":true},\"bindings\":[]",
        ),
        "approval_output",
    );
}
#[test]
fn friction_8_approval_rejection_has_no_graph_edge() {
    rejected_extension(
        valid().replace(
            "\"bindings\":[]",
            "\"on_reject\":[\"failed\"],\"bindings\":[]",
        ),
        "on_reject",
    );
}
#[test]
fn friction_9_action_catch_edges_are_not_authorable() {
    rejected_extension(
        valid().replace("\"bindings\":[]", "\"catch\":[\"cleanup\"],\"bindings\":[]"),
        "catch",
    );
}
#[test]
fn friction_10_execution_traits_are_registry_metadata() {
    rejected_extension(
        valid().replace("\"bindings\":[]", "\"llm_backed\":true,\"bindings\":[]"),
        "llm_backed",
    );
}

#[test]
fn json_duplicate_keys_are_rejected() {
    let duplicate = valid().replace(
        "\"name\":\"Example\"",
        "\"name\":\"Example\",\"name\":\"Other\"",
    );
    assert_eq!(
        parse_json_definition(&duplicate).unwrap_err().kind,
        ValidationErrorKind::InvalidField
    );
}
