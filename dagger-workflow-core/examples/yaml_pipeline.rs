//! YAML-defined arithmetic pipeline: define, load, publish, execute, verify.
//!
//! Run with:
//!
//! ```text
//! cargo run -p dagger-workflow-core --example yaml_pipeline
//! ```
//!
//! What this shows, in order:
//!
//! 1. A workflow graph written in `examples/pipeline.yaml` and parsed at
//!    runtime by `parse_yaml_definition`. Nothing about the graph is built in
//!    Rust; the Rust side only supplies the action implementations.
//! 2. Real dependency structure: one fan-out node with two independent
//!    branches and a join node that waits for both.
//! 3. A Map node: bounded fan-out over an array, one child action instance per
//!    item, aggregated back into an ordered array.
//! 4. Content addressing: every node result and the run's final output are
//!    stored by SHA-256 digest, and the final digest is recomputed here and
//!    compared.
//! 5. The durable event stream, printed in commit order.
//!
//! Store choice: the in-memory store and in-memory object store. This example
//! is about the definition-to-execution path, so it keeps the setup to two
//! constructors and no filesystem state. The sibling example `durable_demo`
//! covers the SQLite store, the filesystem object store, and crash recovery.
//!
//! Why the YAML carries placeholder digests: an action pin is a content
//! address of the action's JSON Schema documents, and those digests are
//! computed from the registered implementations. So the YAML is authored with
//! `sha256:000...` placeholders and this example rewrites the pins after
//! parsing and before validation. A real deployment would generate the YAML
//! with the pins already filled in.

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
use dagger_workflow_core::run::{NodeRun, RunLimits, RunState};
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

const WORKFLOW_YAML: &str = include_str!("pipeline.yaml");

type DemoResult<T> = Result<T, Box<dyn Error>>;
type Store = InMemoryStore<TestClock>;
type Objects = InMemoryObjectStore<TestClock>;

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

fn short(value: &Digest) -> String {
    format!("{}...", &value.as_str()[..23])
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

fn number_array() -> Value {
    json!({
        "type": "array",
        "items": {"type": "integer", "minimum": -1000, "maximum": 1000},
        "minItems": 1,
        "maxItems": 16
    })
}

fn integer() -> Value {
    json!({"type": "integer", "minimum": -1000000, "maximum": 1000000})
}

fn run_input_schema() -> Value {
    object_schema(
        json!({"numbers": number_array(), "bonus": integer()}),
        &["numbers", "bonus"],
    )
}

fn load_output_schema() -> Value {
    object_schema(
        json!({"numbers": number_array(), "count": integer()}),
        &["numbers", "count"],
    )
}

/// Shared by `math.load_batch`, `math.sum`, and `math.range`: all take the batch.
fn numbers_input_schema() -> Value {
    object_schema(json!({"numbers": number_array()}), &["numbers"])
}

fn sum_output_schema() -> Value {
    object_schema(
        json!({"sum": integer(), "count": integer()}),
        &["sum", "count"],
    )
}

fn range_output_schema() -> Value {
    object_schema(json!({"min": integer(), "max": integer()}), &["min", "max"])
}

fn combine_input_schema() -> Value {
    object_schema(
        json!({
            "numbers": number_array(),
            "sum": integer(),
            "min": integer(),
            "max": integer(),
            "bonus": integer()
        }),
        &["numbers", "sum", "min", "max", "bonus"],
    )
}

fn combine_output_schema() -> Value {
    object_schema(
        json!({
            "numbers": number_array(),
            "count": integer(),
            "sum": integer(),
            "min": integer(),
            "max": integer(),
            "bonus": integer(),
            "total": integer()
        }),
        &["numbers", "count", "sum", "min", "max", "bonus", "total"],
    )
}

fn score_input_schema() -> Value {
    object_schema(
        json!({
            "index": integer(),
            "value": integer(),
            "sum": integer(),
            "bonus": integer()
        }),
        &["index", "value", "sum", "bonus"],
    )
}

fn score_output_schema() -> Value {
    object_schema(
        json!({
            "index": integer(),
            "value": integer(),
            "square": integer(),
            "adjusted": integer(),
            "percent_of_sum": integer()
        }),
        &["index", "value", "square", "adjusted", "percent_of_sum"],
    )
}

/// The run's output is the Map aggregate: one child output per input number.
fn report_schema() -> Value {
    json!({
        "type": "array",
        "items": score_output_schema(),
        "minItems": 1,
        "maxItems": 16
    })
}

// --- actions -----------------------------------------------------------------

/// One deterministic arithmetic action. The engine hands the action its
/// canonical bound input bytes and takes back a validated outcome.
struct MathAction {
    descriptor: ActionDescriptor,
    compute: fn(&Value) -> Result<Value, String>,
}

impl WorkflowAction for MathAction {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    fn invoke<'a>(
        &'a self,
        _context: ActionContext,
        canonical_bound_input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            let input: Value = match serde_json::from_slice(canonical_bound_input) {
                Ok(value) => value,
                Err(_) => return failure("bound input is not JSON"),
            };
            match (self.compute)(&input) {
                Ok(output) => ActionOutcome::success(output, Vec::new(), CostUnits(1), None)
                    .expect("demo success outcomes are persistence-safe"),
                Err(message) => failure(&message),
            }
        })
    }
}

fn failure(message: &str) -> ActionOutcome {
    ActionOutcome::permanent(
        "demo.invalid_input".to_owned(),
        message.to_owned(),
        None,
        CostUnits(0),
    )
    .expect("demo errors are persistence-safe")
}

fn integers(input: &Value, field: &str) -> Result<Vec<i64>, String> {
    input
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("`{field}` must be an array"))?
        .iter()
        .map(|value| {
            value
                .as_i64()
                .ok_or_else(|| "items must be integers".into())
        })
        .collect()
}

fn field(input: &Value, name: &str) -> Result<i64, String> {
    input
        .get(name)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("`{name}` must be an integer"))
}

fn load_batch(input: &Value) -> Result<Value, String> {
    let numbers = integers(input, "numbers")?;
    Ok(json!({"numbers": numbers, "count": numbers.len()}))
}

fn sum(input: &Value) -> Result<Value, String> {
    let numbers = integers(input, "numbers")?;
    Ok(json!({"sum": numbers.iter().sum::<i64>(), "count": numbers.len()}))
}

fn range(input: &Value) -> Result<Value, String> {
    let numbers = integers(input, "numbers")?;
    let min = numbers.iter().min().ok_or("`numbers` is empty")?;
    let max = numbers.iter().max().ok_or("`numbers` is empty")?;
    Ok(json!({"min": min, "max": max}))
}

/// The join: reads both branch results, their shared ancestor, and run input.
fn combine(input: &Value) -> Result<Value, String> {
    let numbers = integers(input, "numbers")?;
    let sum = field(input, "sum")?;
    let bonus = field(input, "bonus")?;
    Ok(json!({
        "numbers": numbers,
        "count": numbers.len(),
        "sum": sum,
        "min": field(input, "min")?,
        "max": field(input, "max")?,
        "bonus": bonus,
        "total": sum + bonus
    }))
}

/// One Map child: scores a single item against the joined batch totals.
fn score(input: &Value) -> Result<Value, String> {
    let value = field(input, "value")?;
    let sum = field(input, "sum")?;
    let bonus = field(input, "bonus")?;
    if sum == 0 {
        return Err("batch sum is zero".to_owned());
    }
    Ok(json!({
        "index": field(input, "index")?,
        "value": value,
        "square": value * value,
        "adjusted": value * value + bonus,
        "percent_of_sum": value * 100 / sum
    }))
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

/// Registers every action implementation and returns the schema catalogue that
/// publication resolves pins against.
fn build_registry_and_schemas() -> DemoResult<(
    Arc<InMemoryActionRegistry>,
    BTreeMap<Digest, Value>,
    Digest,
    Digest,
)> {
    let registry = Arc::new(InMemoryActionRegistry::new());
    let mut schemas = BTreeMap::new();
    let remember = |schema: Value, schemas: &mut BTreeMap<Digest, Value>| -> Digest {
        let key = canonical_digest(&schema);
        schemas.insert(key.clone(), schema);
        key
    };

    let run_input = remember(run_input_schema(), &mut schemas);
    let report = remember(report_schema(), &mut schemas);
    for (name, input, output, compute) in [
        (
            "math.load_batch",
            numbers_input_schema(),
            load_output_schema(),
            load_batch as fn(&Value) -> Result<Value, String>,
        ),
        (
            "math.sum",
            numbers_input_schema(),
            sum_output_schema(),
            sum as fn(&Value) -> Result<Value, String>,
        ),
        (
            "math.range",
            numbers_input_schema(),
            range_output_schema(),
            range as fn(&Value) -> Result<Value, String>,
        ),
        (
            "math.combine",
            combine_input_schema(),
            combine_output_schema(),
            combine as fn(&Value) -> Result<Value, String>,
        ),
        (
            "math.score",
            score_input_schema(),
            score_output_schema(),
            score as fn(&Value) -> Result<Value, String>,
        ),
    ] {
        remember(input.clone(), &mut schemas);
        remember(output.clone(), &mut schemas);
        registry.register(Arc::new(MathAction {
            descriptor: descriptor(name, &input, &output),
            compute,
        }))?;
    }
    Ok((registry, schemas, run_input, report))
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

/// Rewrites the YAML placeholder pins from the registry, then validates and
/// resolves the definition into a publishable revision.
fn repin_and_validate(
    definition: &mut WorkflowDefinition,
    registry: &Arc<InMemoryActionRegistry>,
    schemas: &BTreeMap<Digest, Value>,
    run_input_digest: &Digest,
    report_digest: &Digest,
) -> DemoResult<PublishableDefinition> {
    definition.run_input_schema_digest = run_input_digest.clone();
    definition.run_output_schema_digest = report_digest.clone();
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

/// Publishes every schema document plus the canonical definition, then records
/// the immutable revision.
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
    let publisher = principal(scope, "yaml-pipeline-publisher")?;
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

    // Publication keys resolved schemas by reference location: a node ID for an
    // Action node and `<node_id>/map_action` for a Map node's child action.
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

async fn create_run(
    store: &Store,
    objects: &Objects,
    scope: &ExecutionScope,
    definition: &WorkflowDefinition,
    revision_hash: &Digest,
    run_id: &Id,
    input: &Value,
) -> DemoResult<()> {
    let input = objects
        .put(scope, &serde_jcs::to_vec(input)?, "application/json")
        .await?;
    store
        .create_run(
            scope,
            CreateRun {
                run_id: run_id.clone(),
                definition_id: definition.definition_id.clone(),
                revision_hash: revision_hash.clone(),
                input,
                budget_limit: CostUnits(50),
                limits: RunLimits {
                    max_dynamic_node_instances: 32,
                    max_total_attempts: 32,
                    max_total_events: 1000,
                    max_inline_json_bytes_per_value: 100_000,
                    max_artifacts_per_attempt: 10,
                    max_aggregate_object_bytes_per_run: 1_000_000,
                    max_run_lifetime_ms: 120_000,
                },
                principal: principal(scope, "yaml-pipeline-runner")?,
                idempotency_token: "yaml-pipeline-create-run-token-0001".to_owned(),
            },
        )
        .await?;
    Ok(())
}

// --- reporting ---------------------------------------------------------------

fn describe_nodes(definition: &WorkflowDefinition) {
    for node in &definition.nodes {
        match node {
            NodeDefinition::Action {
                id, action, next, ..
            } => println!(
                "    {:<12} Action   action={:<16} next={}",
                id.as_str(),
                action.name,
                next.iter().map(Id::as_str).collect::<Vec<_>>().join(",")
            ),
            NodeDefinition::Map {
                id,
                action,
                max_items,
                max_concurrency,
                next,
                ..
            } => println!(
                "    {:<12} Map      action={:<16} next={} max_items={max_items} max_concurrency={max_concurrency}",
                id.as_str(),
                action.name,
                next.iter().map(Id::as_str).collect::<Vec<_>>().join(",")
            ),
            NodeDefinition::Succeed { id, .. } => {
                println!("    {:<12} Succeed  terminal", id.as_str())
            }
            NodeDefinition::Choice { id, .. } => println!("    {:<12} Choice", id.as_str()),
            NodeDefinition::Approval { id, .. } => println!("    {:<12} Approval", id.as_str()),
            NodeDefinition::Fail { id, .. } => println!("    {:<12} Fail", id.as_str()),
        }
    }
}

async fn print_new_events(
    store: &Store,
    scope: &ExecutionScope,
    run_id: &Id,
    after: &mut u64,
    labels: &BTreeMap<String, String>,
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
        print_event(&event, labels);
        *after = event.event_seq;
    }
    Ok(())
}

fn print_event(event: &WorkflowEvent, labels: &BTreeMap<String, String>) {
    println!(
        "    event #{:02} {:<22} node={}",
        event.event_seq,
        format!("{:?}", event.event_type),
        event
            .node_instance_id
            .as_ref()
            .map(|id| label(labels, id.as_str()))
            .unwrap_or_else(|| "-".to_owned())
    );
}

/// Map children get synthetic instance IDs derived from the item digest. This
/// prints them as `<map node>[<item index>]` instead.
fn label(labels: &BTreeMap<String, String>, instance_id: &str) -> String {
    labels
        .get(instance_id)
        .cloned()
        .unwrap_or_else(|| instance_id.to_owned())
}

fn labels_for(nodes: &[NodeRun]) -> BTreeMap<String, String> {
    nodes
        .iter()
        .map(|node| {
            let label = match node.map_item_index {
                Some(index) => format!("{}[{index}]", node.definition_node_id.as_str()),
                None => node.definition_node_id.as_str().to_owned(),
            };
            (node.node_instance_id.as_str().to_owned(), label)
        })
        .collect()
}

async fn all_nodes(store: &Store, scope: &ExecutionScope, run_id: &Id) -> DemoResult<Vec<NodeRun>> {
    let mut nodes = Vec::new();
    let mut cursor = None;
    loop {
        let page = store
            .list_nodes(
                scope,
                run_id,
                PageRequest {
                    cursor: cursor.clone(),
                    page_size: 100,
                },
            )
            .await?;
        nodes.extend(page.items);
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    nodes.sort_by(|left, right| {
        (left.topological_rank.0, left.map_item_index)
            .cmp(&(right.topological_rank.0, right.map_item_index))
    });
    Ok(nodes)
}

// --- main --------------------------------------------------------------------

#[tokio::main]
async fn main() -> DemoResult<()> {
    let scope = ExecutionScope {
        tenant_id: ScopeAtom::new("demo-tenant")?,
        namespace: ScopeAtom::new("arithmetic")?,
    };
    let run_id = Id::new("yaml-pipeline-run")?;
    let run_input = json!({"numbers": [3, 7, 11, 4], "bonus": 5});

    println!("YAML pipeline demo");
    println!("  definition: dagger-workflow-core/examples/pipeline.yaml");
    println!("  store: in-memory control plane and in-memory object store");
    println!(
        "  scope: tenant={} namespace={}",
        scope.tenant_id.as_str(),
        scope.namespace.as_str()
    );

    println!("\nStep 1: parse the YAML definition");
    let mut definition = parse_yaml_definition(WORKFLOW_YAML)?;
    println!("  definition_id: {}", definition.definition_id.as_str());
    println!("  name: {}", definition.name);
    println!("  entry node: {}", definition.entry_node_id.as_str());
    println!("  nodes: {}", definition.nodes.len());
    describe_nodes(&definition);
    println!("  graph: load_batch fans out to sum_batch and range_batch; combine joins");
    println!("         both branches; score_each maps over the batch; done is the sole");
    println!("         Succeed node and publishes the Map aggregate as the run output.");

    println!("\nStep 2: pin actions, validate, resolve publication");
    let (registry, schemas, run_input_digest, report_digest) = build_registry_and_schemas()?;
    println!(
        "  registered actions: math.load_batch, math.sum, math.range, math.combine, math.score"
    );
    let publishable = repin_and_validate(
        &mut definition,
        &registry,
        &schemas,
        &run_input_digest,
        &report_digest,
    )?;
    println!("  validation: passed (acyclic, reachable, terminated, bindings assignable)");
    println!("  action pins resolved:");
    for pin in extract_action_pins(&publishable.definition) {
        println!(
            "    {:<24} -> {:<16} in={} out={}",
            pin.reference_location,
            pin.name,
            short(&pin.input_schema_digest),
            short(&pin.output_schema_digest)
        );
    }

    println!("\nStep 3: publish the revision and create a run");
    let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let (definition, revision_hash) =
        publish(&store, &objects, &scope, publishable, &schemas).await?;
    println!("  published {} schema objects", schemas.len());
    println!("  revision hash: {}", revision_hash.as_str());
    create_run(
        &store,
        &objects,
        &scope,
        &definition,
        &revision_hash,
        &run_id,
        &run_input,
    )
    .await?;
    println!("  run id: {}", run_id.as_str());
    println!("  run input: {}", serde_json::to_string(&run_input)?);

    println!("\nStep 4: execute, one scheduler pass per tick");
    let engine = WorkflowEngine::new(
        store.clone(),
        objects.clone(),
        registry,
        EngineConfig {
            instance_id: Id::new("yaml-pipeline-engine")?,
            max_concurrency: 4,
        },
    )?;
    engine.acquire_scope(&scope).await?;
    engine.start(&scope, &run_id).await?;
    let mut last_event = 0;
    let mut nodes = all_nodes(&store, &scope, &run_id).await?;
    print_new_events(
        &store,
        &scope,
        &run_id,
        &mut last_event,
        &labels_for(&nodes),
    )
    .await?;
    for tick in 1..=20 {
        let changes = engine.tick(&scope).await?;
        println!("  tick {tick}: {changes} durable change(s)");
        nodes = all_nodes(&store, &scope, &run_id).await?;
        print_new_events(
            &store,
            &scope,
            &run_id,
            &mut last_event,
            &labels_for(&nodes),
        )
        .await?;
        if store
            .get_run(&scope, &run_id)
            .await?
            .run
            .status
            .is_terminal()
        {
            break;
        }
        if changes == 0 {
            return Err(demo_error("engine became idle before the run was terminal"));
        }
    }

    println!("\nStep 5: node results, each stored by content digest");
    let nodes = all_nodes(&store, &scope, &run_id).await?;
    let labels = labels_for(&nodes);
    println!(
        "    {:<16} {:<8} {:<10} result digest",
        "node", "kind", "status"
    );
    for node in &nodes {
        println!(
            "    {:<16} {:<8} {:<10} {}",
            label(&labels, node.node_instance_id.as_str()),
            format!("{:?}", node.kind),
            format!("{:?}", node.status),
            node.result_ref
                .as_ref()
                .map(|reference| short(&reference.0.digest))
                .unwrap_or_else(|| "-".to_owned())
        );
    }
    println!("  score_each and done share a digest: identical bytes are one stored object.");

    let joined = nodes
        .iter()
        .find(|node| node.definition_node_id.as_str() == "combine")
        .and_then(|node| node.result_ref.clone())
        .ok_or_else(|| demo_error("join node produced no result"))?;
    let joined: Value =
        serde_json::from_slice(&objects.get(&scope, &joined.0.digest).await?.bytes)?;
    println!(
        "  join result (combine): {}",
        serde_json::to_string(&joined)?
    );

    println!("\nStep 6: verify and print the final artifact");
    let run = store.get_run(&scope, &run_id).await?.run;
    if run.status != RunState::Succeeded {
        return Err(demo_error(format!(
            "workflow ended in unexpected state {:?}",
            run.status
        )));
    }
    let output_ref = run
        .output_ref
        .as_ref()
        .ok_or_else(|| demo_error("succeeded run has no output artifact"))?;
    let verified = objects.get(&scope, &output_ref.0.digest).await?;
    if digest(&verified.bytes) != output_ref.0.digest {
        return Err(demo_error("final output digest verification failed"));
    }
    let report: Value = serde_json::from_slice(&verified.bytes)?;

    println!("  final status: {:?}", run.status);
    println!("  output digest: {}", output_ref.0.digest.as_str());
    println!("  digest recomputed from bytes: matches");
    println!("  output JSON:\n{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
