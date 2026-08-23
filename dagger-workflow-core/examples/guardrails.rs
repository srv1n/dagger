//! What the engine refuses to do, printed as a narrated transcript.
//!
//! Run with:
//!
//! ```text
//! cargo run -p dagger-workflow-core --example guardrails
//! ```
//!
//! The sibling examples `yaml_pipeline` and `durable_demo` both show the happy
//! path. This one shows the other half of the contract: the transitions that
//! refuse to commit. Each scenario below publishes a small graph (one Action
//! node named `work` feeding one Succeed node named `done`, plus a Map node in
//! the last scenario) against a different action implementation, retry policy,
//! run limit, or pinned root output schema, then drives it to a terminal state
//! and prints the durable result.
//!
//! Six guardrails, in order.
//!
//! 1. A retryable action failure and the retry that follows it. Contract
//!    section 3.2 N19/N04: a Retryable outcome schedules a persisted delay and
//!    the node comes back Ready when the database clock passes it.
//! 2. The per-node retry ceiling. Contract section 3.2 N24: the last permitted
//!    attempt fails retryably and the node and run land RetriesExhausted.
//! 3. The `max_total_attempts` run ceiling. Contract section 1.4: a claim that
//!    would exceed it is refused, and both node and run land ContractFailed
//!    with `RunAttemptLimitExceeded`.
//! 4. The `max_inline_json_bytes_per_value` run ceiling, enforced at the
//!    Succeed node. Contract section 5.3: the size check runs before any
//!    mutation, so an over-large root output leaves no artifact behind.
//! 5. The pinned root output schema. Contract section 1.4
//!    `RunOutputSchemaMismatch`: an action output that the action's own pin
//!    accepts but the revision's root output schema rejects is refused at the
//!    Succeed node rather than committed as the run output.
//! 6. The `max_dynamic_node_instances` run ceiling. Contract section 5.3: an
//!    over-large Map expansion is refused all-or-nothing, so no child node is
//!    created and the dynamic-node counter is never charged, and the run lands
//!    ContractFailed with `RunDynamicNodeLimitExceeded`.
//!
//! Store choice: the in-memory control plane and object store, same as
//! `yaml_pipeline`. Guardrails are store-independent, so the example keeps the
//! setup to two constructors.
//!
//! Why the YAML carries placeholder digests: an action pin is a content
//! address of the action's JSON Schema documents, and those digests are
//! computed from the registered implementations. The template below is
//! authored with `sha256:000...` placeholders and repinned after parsing and
//! before validation, exactly as the two sibling examples do.

use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, ActionRegistry, InMemoryActionRegistry,
    WorkflowAction,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::{ObjectStore, VerifiedObjectRef};
use dagger_workflow_core::definition::{
    extract_action_pins, parse_yaml_definition, resolve_publication, validate_definition,
    ExtractedActionPin, NodeDefinition, PublicationResolver, PublicationSchemaDocument,
    PublishableDefinition, WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::event::WorkflowEvent;
use dagger_workflow_core::ids::{CostUnits, Digest, Id, Timestamp, Version};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::run::{RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::store::{
    CreateDefinition, CreateRun, EventPageRequest, PageRequest, PublishRevision,
    ResolvedActionSchemas, WorkflowStore,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::error::Error;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;

type DemoResult<T> = Result<T, Box<dyn Error>>;
type Store = InMemoryStore<TestClock>;
type Objects = InMemoryObjectStore<TestClock>;
type Engine = WorkflowEngine<Store, Objects, InMemoryActionRegistry>;

/// One Action node feeding the single Succeed terminal. `__ACTION__`,
/// `__DEFINITION_ID__`, and `__MAX_ATTEMPTS__` are substituted per scenario;
/// every `sha256:000...` is a placeholder rewritten by `repin_and_validate`.
const WORKFLOW_TEMPLATE: &str = r#"
definition_format_version: "0.1"
definition_id: __DEFINITION_ID__
name: Guardrail probe
description: One Action node and one Succeed terminal, exercising a single refusal path.
run_input_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
run_output_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
entry_node_id: work
nodes:
  - id: work
    kind: Action
    action:
      name: __ACTION__
      contract_version: placeholder
      input_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      output_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      compatible_implementation_requirement: sha256:0000000000000000000000000000000000000000000000000000000000000000
    bindings:
      - target: /label
        source:
          kind: run_input
          pointer: /label
    retry:
      max_attempts: __MAX_ATTEMPTS__
      backoff:
        kind: fixed
        delay_ms: 1000
    timeout:
      timeout_ms: 60000
    declared_max_cost_units: "1"
    next: [done]

  - id: done
    kind: Succeed
    output:
      kind: node_output
      node_id: work
      pointer: ""
"#;

/// A Map fan-out feeding the single Succeed terminal, used only by the
/// `max_dynamic_node_instances` scenario. Same placeholder-pin convention.
const MAP_TEMPLATE: &str = r#"
definition_format_version: "0.1"
definition_id: __DEFINITION_ID__
name: Guardrail map probe
description: One Action node, one Map fan-out, and one Succeed terminal.
run_input_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
run_output_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
entry_node_id: work
nodes:
  - id: work
    kind: Action
    action:
      name: guard.items
      contract_version: placeholder
      input_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      output_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      compatible_implementation_requirement: sha256:0000000000000000000000000000000000000000000000000000000000000000
    bindings:
      - target: /label
        source:
          kind: run_input
          pointer: /label
    retry:
      max_attempts: __MAX_ATTEMPTS__
      backoff:
        kind: fixed
        delay_ms: 1000
    timeout:
      timeout_ms: 60000
    declared_max_cost_units: "1"
    next: [spread]

  - id: spread
    kind: Map
    items:
      kind: node_output
      node_id: work
      pointer: /items
    max_items: 16
    max_concurrency: 2
    action:
      name: guard.double
      contract_version: placeholder
      input_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      output_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      compatible_implementation_requirement: sha256:0000000000000000000000000000000000000000000000000000000000000000
    bindings:
      - target: /value
        source:
          kind: map_item
          pointer: ""
    retry:
      max_attempts: 1
      backoff:
        kind: fixed
        delay_ms: 1000
    timeout:
      timeout_ms: 60000
    declared_max_cost_units: "1"
    next: [done]

  - id: done
    kind: Succeed
    output:
      kind: node_output
      node_id: spread
      pointer: ""
"#;

/// Persisted retry backoff in the template above. The example advances the
/// deterministic clock by exactly this much between passes so a scheduled retry
/// becomes eligible, rather than letting a zero delay hide the wait state.
const RETRY_DELAY_MS: i64 = 1_000;

fn digest(bytes: &[u8]) -> Digest {
    Digest::new(format!(
        "sha256:{}",
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
    .expect("SHA-256 always produces a valid digest")
}

fn canonical_digest(value: &Value) -> Digest {
    digest(&serde_jcs::to_vec(value).expect("schema JSON is canonicalizable"))
}

fn demo_error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::other(message.into()))
}

// --- schemas -----------------------------------------------------------------

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn label() -> Value {
    json!({"type": "string", "minLength": 1, "maxLength": 64})
}

/// Shared by every action in this example: the bound input is just the label.
fn label_input_schema() -> Value {
    object_schema(json!({"label": label()}), &["label"])
}

fn flaky_output_schema() -> Value {
    object_schema(
        json!({"attempt": {"type": "integer", "minimum": 1, "maximum": 64}, "label": label()}),
        &["attempt", "label"],
    )
}

fn broken_output_schema() -> Value {
    object_schema(json!({"label": label()}), &["label"])
}

fn bloat_output_schema() -> Value {
    object_schema(
        json!({
            "label": label(),
            "payload": {"type": "string", "minLength": 1, "maxLength": 8192}
        }),
        &["label", "payload"],
    )
}

fn echo_output_schema() -> Value {
    object_schema(
        json!({"checked": {"type": "boolean"}, "label": label()}),
        &["checked", "label"],
    )
}

fn integer() -> Value {
    json!({"type": "integer", "minimum": -1000, "maximum": 1000})
}

fn items_output_schema() -> Value {
    object_schema(
        json!({
            "items": {"type": "array", "items": integer(), "minItems": 1, "maxItems": 16},
            "label": label()
        }),
        &["items", "label"],
    )
}

fn double_input_schema() -> Value {
    object_schema(json!({"value": integer()}), &["value"])
}

fn double_output_schema() -> Value {
    object_schema(json!({"doubled": integer()}), &["doubled"])
}

/// A Map node's result is the ordered aggregate of its children (contract
/// section 3.3 N08), so the root output schema of the Map scenario is an array
/// document, not the child object schema.
fn doubled_aggregate_schema() -> Value {
    json!({
        "type": "array",
        "items": double_output_schema(),
        "minItems": 0,
        "maxItems": 16
    })
}

/// Guardrail 5 pins this as the run's root output schema. It requires a
/// `signature` leaf that `guard.echo` never emits, so the action's own output
/// pin accepts the value and the root schema does not.
fn signed_report_schema() -> Value {
    object_schema(
        json!({
            "checked": {"type": "boolean"},
            "label": label(),
            "signature": {"type": "string", "minLength": 1, "maxLength": 128}
        }),
        &["checked", "label", "signature"],
    )
}

// --- actions -----------------------------------------------------------------

/// A deterministic action whose outcome is a pure function of the label and the
/// one-based attempt number the engine hands it in `ActionContext`. Nothing in
/// this example needs interior mutability or wall-clock nondeterminism.
struct ProbeAction {
    descriptor: ActionDescriptor,
    compute: fn(&Value, u32) -> ActionOutcome,
}

impl WorkflowAction for ProbeAction {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        canonical_bound_input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            let input: Value = match serde_json::from_slice(canonical_bound_input) {
                Ok(value) => value,
                Err(_) => return permanent("bound input is not JSON"),
            };
            (self.compute)(&input, context.attempt_number)
        })
    }
}

fn success(output: Value) -> ActionOutcome {
    ActionOutcome::success(output, Vec::new(), CostUnits(1), None)
        .expect("demo success outcomes are persistence-safe")
}

fn retryable(message: &str) -> ActionOutcome {
    ActionOutcome::retryable(
        "guard.transient".to_owned(),
        message.to_owned(),
        None,
        CostUnits(1),
    )
    .expect("demo errors are persistence-safe")
}

fn permanent(message: &str) -> ActionOutcome {
    ActionOutcome::permanent(
        "guard.invalid_input".to_owned(),
        message.to_owned(),
        None,
        CostUnits(0),
    )
    .expect("demo errors are persistence-safe")
}

fn label_of(input: &Value) -> Option<&str> {
    input.get("label").and_then(Value::as_str)
}

/// Fails retryably on the first attempt only, then succeeds.
fn flaky(input: &Value, attempt: u32) -> ActionOutcome {
    let Some(label) = label_of(input) else {
        return permanent("bound input requires a label string");
    };
    if attempt == 1 {
        return retryable("upstream dependency was not reachable on the first attempt");
    }
    success(json!({"attempt": attempt, "label": label}))
}

/// Never succeeds, and always claims the failure is transient.
fn always_broken(_input: &Value, attempt: u32) -> ActionOutcome {
    retryable(&format!(
        "upstream dependency was not reachable on attempt {attempt}"
    ))
}

/// Succeeds with an output far larger than guardrail 4's per-value ceiling.
fn bloat(input: &Value, _attempt: u32) -> ActionOutcome {
    match label_of(input) {
        Some(label) => success(json!({"label": label, "payload": "x".repeat(4096)})),
        None => permanent("bound input requires a label string"),
    }
}

/// Succeeds with a value its own output pin accepts.
fn echo(input: &Value, _attempt: u32) -> ActionOutcome {
    match label_of(input) {
        Some(label) => success(json!({"checked": true, "label": label})),
        None => permanent("bound input requires a label string"),
    }
}

/// Emits the array that the Map scenario fans out over.
fn items(input: &Value, _attempt: u32) -> ActionOutcome {
    match label_of(input) {
        Some(label) => success(json!({"items": [1, 2, 3, 4], "label": label})),
        None => permanent("bound input requires a label string"),
    }
}

/// One Map child: doubles a single item.
fn double(input: &Value, _attempt: u32) -> ActionOutcome {
    match input.get("value").and_then(Value::as_i64) {
        Some(value) => success(json!({"doubled": value * 2})),
        None => permanent("bound input requires an integer value"),
    }
}

fn descriptor(name: &str, input: &Value, output: &Value) -> ActionDescriptor {
    ActionDescriptor {
        name: name.to_owned(),
        contract_version: "demo-1".to_owned(),
        input_schema_digest: canonical_digest(input),
        output_schema_digest: canonical_digest(output),
        implementation_compatibility_digest: digest(
            format!("{name}:deterministic-implementation-v1").as_bytes(),
        ),
    }
}

/// Registers every probe action and returns the schema catalogue that
/// publication resolves pins against.
fn build_registry_and_schemas() -> DemoResult<(Arc<InMemoryActionRegistry>, BTreeMap<Digest, Value>)>
{
    let registry = Arc::new(InMemoryActionRegistry::new());
    let mut schemas = BTreeMap::new();
    let input = label_input_schema();
    // The run input schema and every action input schema are the same document,
    // so the catalogue stores it once under its one content address.
    schemas.insert(canonical_digest(&input), input.clone());
    // Guardrail 5's root output schema is never an action pin, but publication
    // still resolves it by digest, so it belongs in the catalogue.
    for schema in [signed_report_schema(), doubled_aggregate_schema()] {
        schemas.insert(canonical_digest(&schema), schema);
    }

    for (name, action_input, output, compute) in [
        (
            "guard.flaky",
            input.clone(),
            flaky_output_schema(),
            flaky as fn(&Value, u32) -> ActionOutcome,
        ),
        (
            "guard.always_broken",
            input.clone(),
            broken_output_schema(),
            always_broken as fn(&Value, u32) -> ActionOutcome,
        ),
        (
            "guard.bloat",
            input.clone(),
            bloat_output_schema(),
            bloat as fn(&Value, u32) -> ActionOutcome,
        ),
        (
            "guard.echo",
            input.clone(),
            echo_output_schema(),
            echo as fn(&Value, u32) -> ActionOutcome,
        ),
        (
            "guard.items",
            input.clone(),
            items_output_schema(),
            items as fn(&Value, u32) -> ActionOutcome,
        ),
        (
            "guard.double",
            double_input_schema(),
            double_output_schema(),
            double as fn(&Value, u32) -> ActionOutcome,
        ),
    ] {
        schemas.insert(canonical_digest(&action_input), action_input.clone());
        schemas.insert(canonical_digest(&output), output.clone());
        registry.register(Arc::new(ProbeAction {
            descriptor: descriptor(name, &action_input, &output),
            compute,
        }))?;
    }
    Ok((registry, schemas))
}

// --- publication -------------------------------------------------------------

struct DemoResolver {
    schemas: BTreeMap<Digest, Value>,
    registry: Arc<InMemoryActionRegistry>,
}

impl PublicationResolver for DemoResolver {
    fn schema_document(&self, digest: &Digest) -> Option<PublicationSchemaDocument> {
        self.schemas
            .get(digest)
            .cloned()
            .map(|value| PublicationSchemaDocument {
                digest: digest.clone(),
                value,
            })
    }

    fn artifact_exists(&self, _artifact_ref_id: &Id, _digest: &Digest, _media_type: &str) -> bool {
        true
    }

    fn action_pin_available(&self, pin: &ExtractedActionPin) -> bool {
        self.registry.resolve(&pin.name).is_some_and(|action| {
            let descriptor = action.descriptor();
            descriptor.contract_version == pin.contract_version
                && descriptor.input_schema_digest == pin.input_schema_digest
                && descriptor.output_schema_digest == pin.output_schema_digest
                && descriptor.implementation_compatibility_digest
                    == pin.compatible_implementation_requirement
        })
    }
}

/// Rewrites the template's placeholder pins from the registry, pins the two
/// root schemas, then validates and resolves the definition.
fn repin_and_validate(
    definition: &mut WorkflowDefinition,
    registry: &Arc<InMemoryActionRegistry>,
    schemas: &BTreeMap<Digest, Value>,
    run_output_schema: &Value,
) -> DemoResult<PublishableDefinition> {
    definition.run_input_schema_digest = canonical_digest(&label_input_schema());
    definition.run_output_schema_digest = canonical_digest(run_output_schema);
    for node in &mut definition.nodes {
        let action = match node {
            NodeDefinition::Action { action, .. } | NodeDefinition::Map { action, .. } => action,
            _ => continue,
        };
        let implementation = registry
            .resolve(&action.name)
            .ok_or_else(|| demo_error(format!("action {} is not registered", action.name)))?;
        let descriptor = implementation.descriptor();
        action.contract_version = descriptor.contract_version.clone();
        action.input_schema_digest = descriptor.input_schema_digest.clone();
        action.output_schema_digest = descriptor.output_schema_digest.clone();
        action.compatible_implementation_requirement =
            descriptor.implementation_compatibility_digest.clone();
    }
    let unresolved = validate_definition(definition)
        .map_err(|errors| demo_error(format!("definition validation failed: {errors:#?}")))?;
    resolve_publication(
        unresolved,
        &DemoResolver {
            schemas: schemas.clone(),
            registry: registry.clone(),
        },
    )
    .map_err(|errors| demo_error(format!("publication resolution failed: {errors:#?}")))
}

fn principal(scope: &ExecutionScope, id: &str) -> DemoResult<AuthenticatedPrincipal> {
    Ok(AuthenticatedPrincipal::mint(
        scope.clone(),
        id.to_owned(),
        Vec::new(),
        digest(id.as_bytes()),
    )?)
}

async fn publish(
    store: &Store,
    objects: &Objects,
    scope: &ExecutionScope,
    publishable: PublishableDefinition,
    schemas: &BTreeMap<Digest, Value>,
) -> DemoResult<(WorkflowDefinition, Digest)> {
    let definition = publishable.definition.clone();
    let mut schema_objects = BTreeMap::<Digest, VerifiedObjectRef>::new();
    for (schema_digest, schema) in schemas {
        let object = objects
            .put(scope, &serde_jcs::to_vec(schema)?, "application/json")
            .await?;
        schema_objects.insert(schema_digest.clone(), object);
    }
    let canonical_definition = objects
        .put(scope, &serde_jcs::to_vec(&definition)?, "application/json")
        .await?;
    let publisher = principal(scope, "guardrails-publisher")?;
    store
        .create_definition(
            scope,
            CreateDefinition {
                definition_id: definition.definition_id.clone(),
                display_name: definition.name.clone(),
                description: definition.description.clone(),
                principal: publisher.clone(),
            },
        )
        .await?;
    let resolved_action_schema_objects = extract_action_pins(&definition)
        .into_iter()
        .map(|pin| {
            (
                pin.reference_location,
                ResolvedActionSchemas {
                    input_schema: schema_objects[&pin.input_schema_digest].clone(),
                    output_schema: schema_objects[&pin.output_schema_digest].clone(),
                },
            )
        })
        .collect();
    store
        .publish_revision(
            scope,
            PublishRevision {
                definition_id: definition.definition_id.clone(),
                expected_definition_version: Version(1),
                canonical_definition: canonical_definition.clone(),
                run_input_schema: schema_objects[&definition.run_input_schema_digest].clone(),
                run_output_schema: schema_objects[&definition.run_output_schema_digest].clone(),
                resolved_action_schema_objects,
                parsed_revision: publishable,
                principal: publisher,
            },
        )
        .await?;
    Ok((definition, canonical_definition.digest().clone()))
}

// --- scenarios ---------------------------------------------------------------

/// The seven-ceiling defaults. Individual scenarios lower exactly one of them
/// so the transcript attributes each refusal to a single named limit.
fn generous_limits() -> RunLimits {
    RunLimits {
        max_dynamic_node_instances: 32,
        max_total_attempts: 32,
        max_total_events: 1000,
        max_inline_json_bytes_per_value: 100_000,
        max_artifacts_per_attempt: 10,
        max_aggregate_object_bytes_per_run: 1_000_000,
        max_run_lifetime_ms: 600_000,
    }
}

struct Scenario {
    title: &'static str,
    definition_id: &'static str,
    template: &'static str,
    action: &'static str,
    max_attempts: u32,
    /// What the reviewer should watch for, printed before the run starts.
    setup: &'static [&'static str],
    limits: RunLimits,
    run_output_schema: Value,
    expected_state: RunState,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            title: "Guardrail 1: a retryable failure is retried, not dropped",
            definition_id: "guardrail_retry_recovers",
            template: WORKFLOW_TEMPLATE,
            action: "guard.flaky",
            max_attempts: 3,
            setup: &[
                "action: guard.flaky returns a Retryable outcome on attempt 1 and succeeds after",
                "node retry policy: max_attempts=3, fixed backoff 1000ms",
                "run limits: all seven ceilings generous",
            ],
            limits: generous_limits(),
            run_output_schema: flaky_output_schema(),
            expected_state: RunState::Succeeded,
        },
        Scenario {
            title: "Guardrail 2: the per-node retry ceiling stops the retry loop",
            definition_id: "guardrail_retries_exhausted",
            template: WORKFLOW_TEMPLATE,
            action: "guard.always_broken",
            max_attempts: 2,
            setup: &[
                "action: guard.always_broken returns Retryable on every attempt",
                "node retry policy: max_attempts=2, fixed backoff 1000ms",
                "run limits: all seven ceilings generous, so only the node policy can stop it",
            ],
            limits: generous_limits(),
            run_output_schema: broken_output_schema(),
            expected_state: RunState::RetriesExhausted,
        },
        Scenario {
            title: "Guardrail 3: the max_total_attempts run ceiling refuses the next claim",
            definition_id: "guardrail_attempt_ceiling",
            template: WORKFLOW_TEMPLATE,
            action: "guard.always_broken",
            max_attempts: 8,
            setup: &[
                "action: guard.always_broken returns Retryable on every attempt",
                "node retry policy: max_attempts=8, which would allow eight attempts",
                "run limits: max_total_attempts=2, which is the binding constraint",
            ],
            limits: RunLimits {
                max_total_attempts: 2,
                ..generous_limits()
            },
            run_output_schema: broken_output_schema(),
            expected_state: RunState::ContractFailed,
        },
        Scenario {
            title:
                "Guardrail 4: the max_inline_json_bytes_per_value ceiling refuses the producing node's output",
            definition_id: "guardrail_inline_json_ceiling",
            template: WORKFLOW_TEMPLATE,
            action: "guard.bloat",
            max_attempts: 1,
            setup: &[
                "action: guard.bloat succeeds with a 4096-character payload string",
                "node retry policy: max_attempts=1",
                "run limits: max_inline_json_bytes_per_value=2048, below the value the action commits",
                "the ceiling applies at output commit (contract 1.4), so the producing",
                "node fails and the failure is attributed to it, not to a downstream consumer",
            ],
            limits: RunLimits {
                max_inline_json_bytes_per_value: 2_048,
                ..generous_limits()
            },
            run_output_schema: bloat_output_schema(),
            expected_state: RunState::ContractFailed,
        },
        Scenario {
            title: "Guardrail 5: the pinned root output schema refuses the run output",
            definition_id: "guardrail_output_schema",
            template: WORKFLOW_TEMPLATE,
            action: "guard.echo",
            max_attempts: 1,
            setup: &[
                "action: guard.echo succeeds with {\"checked\": true, \"label\": ...}",
                "action output pin: accepts that value, so the attempt commits normally",
                "pinned root output schema: additionally requires a `signature` leaf",
            ],
            limits: generous_limits(),
            run_output_schema: signed_report_schema(),
            expected_state: RunState::ContractFailed,
        },
        Scenario {
            title: "Guardrail 6: the max_dynamic_node_instances ceiling refuses the Map expansion",
            definition_id: "guardrail_dynamic_node_ceiling",
            template: MAP_TEMPLATE,
            action: "guard.items",
            max_attempts: 1,
            setup: &[
                "graph: work [Action] -> spread [Map] -> done [Succeed]",
                "action: guard.items emits a four-item array for the Map to fan out over",
                "run limits: max_dynamic_node_instances=2, below the four children required",
            ],
            limits: RunLimits {
                max_dynamic_node_instances: 2,
                ..generous_limits()
            },
            run_output_schema: doubled_aggregate_schema(),
            expected_state: RunState::ContractFailed,
        },
    ]
}

// --- reporting ---------------------------------------------------------------

/// Renders the payload leaves that explain a refusal. The full payload is
/// canonical JSON; the transcript only needs the closed vocabulary fields.
fn payload_detail(event: &WorkflowEvent) -> String {
    let interesting = [
        "failure_kind",
        "error_code",
        "cause",
        "attempt_number",
        "max_attempts",
        "next_eligible_at",
        "actual_cost_units",
    ];
    let rendered = interesting
        .iter()
        .filter_map(|key| {
            event
                .payload
                .get(*key)
                .filter(|value| !value.is_null())
                .map(|value| format!("{key}={}", compact(value)))
        })
        .collect::<Vec<_>>();
    if rendered.is_empty() {
        String::new()
    } else {
        format!("  {}", rendered.join(" "))
    }
}

fn compact(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

async fn print_new_events(
    store: &Store,
    scope: &ExecutionScope,
    run_id: &Id,
    after: &mut u64,
) -> DemoResult<()> {
    let events = store
        .list_events_after(
            scope,
            run_id,
            EventPageRequest {
                after_event_seq: *after,
                page_size: 1000,
                hard_response_byte_limit: 4_000_000,
            },
        )
        .await?;
    for event in events {
        // The transition ID is the state-transition row that produced the
        // event, so a reviewer can look each refusal up directly.
        let line = format!(
            "      #{:02} {:<6} {:<22}{}",
            event.event_seq,
            event.transition_id,
            format!("{:?}", event.event_type),
            payload_detail(&event)
        );
        println!("{}", line.trim_end());
        *after = event.event_seq;
    }
    Ok(())
}

async fn run_scenario(
    store: &Arc<Store>,
    objects: &Arc<Objects>,
    engine: &Engine,
    clock: &TestClock,
    registry: &Arc<InMemoryActionRegistry>,
    schemas: &BTreeMap<Digest, Value>,
    scope: &ExecutionScope,
    scenario: &Scenario,
) -> DemoResult<()> {
    println!("\n{}", scenario.title);
    println!("  setup");
    for line in scenario.setup {
        println!("    {line}");
    }

    let mut definition = parse_yaml_definition(
        &scenario
            .template
            .replace("__DEFINITION_ID__", scenario.definition_id)
            .replace("__ACTION__", scenario.action)
            .replace("__MAX_ATTEMPTS__", &scenario.max_attempts.to_string()),
    )?;
    let publishable = repin_and_validate(
        &mut definition,
        registry,
        schemas,
        &scenario.run_output_schema,
    )?;
    let (definition, revision_hash) = publish(store, objects, scope, publishable, schemas).await?;

    let run_id = Id::new(scenario.definition_id)?;
    let run_input = json!({"label": scenario.definition_id});
    let input = objects
        .put(scope, &serde_jcs::to_vec(&run_input)?, "application/json")
        .await?;
    store
        .create_run(
            scope,
            CreateRun {
                run_id: run_id.clone(),
                definition_id: definition.definition_id.clone(),
                revision_hash,
                input,
                budget_limit: CostUnits(50),
                limits: scenario.limits.clone(),
                principal: principal(scope, "guardrails-runner")?,
                idempotency_token: format!("guardrails-create-{}", scenario.definition_id),
            },
        )
        .await?;
    println!("  attempted");
    println!("    run id: {}", run_id.as_str());
    println!("    run input: {}", serde_json::to_string(&run_input)?);

    println!("  engine");
    engine.start(scope, &run_id).await?;
    let mut last_event = 0;
    print_new_events(store, scope, &run_id, &mut last_event).await?;
    for tick in 1..=12 {
        let changes = engine.tick(scope).await?;
        println!("    tick {tick}: {changes} durable change(s)");
        print_new_events(store, scope, &run_id, &mut last_event).await?;
        if store
            .get_run(scope, &run_id)
            .await?
            .run
            .status
            .is_terminal()
        {
            break;
        }
        // A persisted retry delay is only released once the database clock
        // reaches next_eligible_at, so a scheduler that never advances would
        // legitimately do nothing. Contract section 3.2 N04.
        clock.advance_ms(RETRY_DELAY_MS)?;
        println!("    clock advanced {RETRY_DELAY_MS}ms");
    }

    let run = store.get_run(scope, &run_id).await?.run;
    println!("  durable state");
    println!(
        "    run:  status={:?} failure_kind={} attempts={} budget_consumed={} output_ref={}",
        run.status,
        run.failure_kind
            .map(|kind| format!("{kind:?}"))
            .unwrap_or_else(|| "none".to_owned()),
        run.total_attempt_count,
        run.budget_consumed.0,
        run.output_ref
            .as_ref()
            .map(|reference| reference.0.digest.as_str().to_owned())
            .unwrap_or_else(|| "none".to_owned())
    );
    let mut nodes = store
        .list_nodes(
            scope,
            &run_id,
            PageRequest {
                cursor: None,
                page_size: 100,
            },
        )
        .await?
        .items;
    nodes.sort_by_key(|node| (node.topological_rank.0, node.map_item_index));
    for node in &nodes {
        println!(
            "    node {:<7} status={:?} attempts={} failure_kind={} result_ref={}",
            node.definition_node_id.as_str(),
            node.status,
            node.attempt_count,
            node.failure_kind
                .map(|kind| format!("{kind:?}"))
                .unwrap_or_else(|| "none".to_owned()),
            node.result_ref
                .as_ref()
                .map(|reference| reference.0.digest.as_str().to_owned())
                .unwrap_or_else(|| "none".to_owned())
        );
    }

    if run.status != scenario.expected_state {
        return Err(demo_error(format!(
            "expected {:?} but the run landed in {:?}",
            scenario.expected_state, run.status
        )));
    }
    match &run.output_ref {
        Some(reference) => {
            let verified = objects.get(scope, &reference.0.digest).await?;
            println!(
                "    committed run output: {}",
                serde_json::to_string(&serde_json::from_slice::<Value>(&verified.bytes)?)?
            );
        }
        // Every refusal in this example is checked before any mutation, so the
        // absence of a root output ref is itself the durable evidence that the
        // rejected value never became the run's result. Contract section 5.3.
        None => println!("    committed run output: none"),
    }
    Ok(())
}

// --- main --------------------------------------------------------------------

#[tokio::main]
async fn main() -> DemoResult<()> {
    let scope = ExecutionScope {
        tenant_id: ScopeAtom::new("demo-tenant")?,
        namespace: ScopeAtom::new("guardrails")?,
    };
    println!("Guardrail demo: what the engine refuses to commit");
    println!("  store: in-memory control plane and in-memory object store");
    println!(
        "  scope: tenant={} namespace={}",
        scope.tenant_id.as_str(),
        scope.namespace.as_str()
    );
    println!("  graph: work [Action] -> done [Succeed], plus a Map node in guardrail 6");
    println!("  clock: deterministic TestClock, advanced explicitly between passes");

    let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let (registry, schemas) = build_registry_and_schemas()?;
    let engine = WorkflowEngine::new(
        store.clone(),
        objects.clone(),
        registry.clone(),
        EngineConfig {
            instance_id: Id::new("guardrails-engine")?,
            max_concurrency: 2,
        },
    )?;
    engine.acquire_scope(&scope).await?;

    for scenario in scenarios() {
        run_scenario(
            &store, &objects, &engine, &clock, &registry, &schemas, &scope, &scenario,
        )
        .await?;
    }

    println!("\nAll six guardrails held. Every refusal above is a durable terminal state,");
    println!("not a log line: the run row carries the closed failure kind and no run output.");
    Ok(())
}
