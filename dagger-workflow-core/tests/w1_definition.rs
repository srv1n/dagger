use dagger_workflow_core::definition::{
    canonical_definition_json, parse_json_definition, parse_yaml_definition, revision_hash,
    validate_definition, ValidationErrorKind,
};

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
        validate_definition(&definition).unwrap();
    }
}

#[test]
fn canonical_hash_ignores_authoring_noise_but_not_array_order() {
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
fn friction_attempts_are_rejected_by_the_closed_format_or_semantics() {
    // 1: a post-Choice reconvergence consumer cannot use a branch-only output.
    let reconvergence = format!(
        r#"{{"definition_format_version":"0.1","definition_id":"x","name":"x","run_input_schema_digest":"{DIGEST}","run_output_schema_digest":"{DIGEST}","entry_node_id":"choose","nodes":[{{"id":"choose","kind":"Choice","input":{{"kind":"constant","value":true}},"selector":"","cases":[{{"equals":true,"next":"branch"}}],"default":"join"}},{{"id":"branch","kind":"Action","action":{{"name":"x","contract_version":"v1","input_schema_digest":"{DIGEST}","output_schema_digest":"{DIGEST}","compatible_implementation_requirement":"{DIGEST}"}},"bindings":[],"retry":{{"max_attempts":1,"backoff":{{"kind":"fixed","delay_ms":0}}}},"timeout":{{"timeout_ms":1}},"declared_max_cost_units":"0","next":["join"]}},{{"id":"join","kind":"Succeed","output":{{"kind":"node_output","node_id":"branch","pointer":""}}}}]}}"#
    );
    assert_eq!(
        invalid_kind(&reconvergence),
        ValidationErrorKind::BindingSourceInvalid
    );

    // 2–10: an implicit Start, composite value inputs, Map child subworkflow,
    // schema-ref shortcut, trigger, approval-output extension, rejection edge,
    // catch, and action-trait fields are rejected rather than silently accepted.
    for forbidden in [
        "start",
        "inputs",
        "subworkflow",
        "schema_ref",
        "trigger",
        "approval_output",
        "on_reject",
        "catch",
        "llm_backed",
    ] {
        let attempt = valid().replace(
            "\"bindings\":[]",
            &format!("\"{forbidden}\":true,\"bindings\":[]"),
        );
        let error = parse_json_definition(&attempt).expect_err("extension must be rejected");
        assert_eq!(error.kind, ValidationErrorKind::UnknownField, "{forbidden}");
        assert!(!error.valid_alternatives.is_empty());
    }
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
