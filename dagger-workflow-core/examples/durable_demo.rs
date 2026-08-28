use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, ActionRegistry, InMemoryActionRegistry,
    WorkflowAction,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::{ObjectStore, VerifiedObjectRef};
use dagger_workflow_core::definition::{
    parse_yaml_definition, resolve_publication, validate_definition, ExtractedActionPin,
    NodeDefinition, PublicationResolver, PublicationSchemaDocument, WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::event::{EventType, WorkflowEvent};
use dagger_workflow_core::fs_object_store::FsObjectStore;
use dagger_workflow_core::ids::{CostUnits, Digest, Id, Timestamp, Version};
use dagger_workflow_core::run::{RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::sqlite::SqliteWorkflowStore;
use dagger_workflow_core::store::{
    CreateDefinition, CreateRun, EventPageRequest, PublishRevision, ResolvedActionSchemas,
    WorkflowStore,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

const PLACEHOLDER_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

const WORKFLOW_YAML: &str = r#"
definition_format_version: "0.1"
definition_id: durable_relevance_demo
name: Durable relevance report
description: Deterministic keyword extraction, scoring, branching, and report composition.
run_input_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
run_output_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
nodes:
  - id: extract_keywords
    kind: Action
    action:
      name: demo.extract_keywords
      contract_version: placeholder
      input_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      output_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      compatible_implementation_requirement: sha256:0000000000000000000000000000000000000000000000000000000000000000
    bindings:
      - target: /text
        source:
          kind: run_input
          pointer: /text
    retry:
      max_attempts: 1
      backoff:
        kind: fixed
        delay_ms: 0
    timeout:
      timeout_ms: 10000
    declared_max_cost_units: "1"
    next: [score_relevance]

  - id: score_relevance
    kind: Action
    action:
      name: demo.score_relevance
      contract_version: placeholder
      input_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      output_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      compatible_implementation_requirement: sha256:0000000000000000000000000000000000000000000000000000000000000000
    bindings:
      - target: /keywords
        source:
          kind: node_output
          node_id: extract_keywords
          pointer: /keywords
      - target: /threshold
        source:
          kind: constant
          value: 60
    retry:
      max_attempts: 1
      backoff:
        kind: fixed
        delay_ms: 0
    timeout:
      timeout_ms: 10000
    declared_max_cost_units: "1"
    next: [choose_relevance]

  - id: choose_relevance
    kind: Choice
    input:
      kind: node_output
      node_id: score_relevance
      pointer: ""
    selector: /is_high_relevance
    cases:
      - equals: true
        next: high_relevance_route
    default: standard_relevance_route

  - id: high_relevance_route
    kind: Choice
    input:
      kind: node_output
      node_id: score_relevance
      pointer: ""
    selector: /is_high_relevance
    cases:
      - equals: true
        next: compose_report
    default: route_invariant_failed

  - id: standard_relevance_route
    kind: Choice
    input:
      kind: node_output
      node_id: score_relevance
      pointer: ""
    selector: /is_high_relevance
    cases:
      - equals: false
        next: compose_report
    default: route_invariant_failed

  - id: compose_report
    kind: Action
    action:
      name: demo.compose_report
      contract_version: placeholder
      input_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      output_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      compatible_implementation_requirement: sha256:0000000000000000000000000000000000000000000000000000000000000000
    bindings:
      - target: /keywords
        source:
          kind: node_output
          node_id: score_relevance
          pointer: /keywords
      - target: /score
        source:
          kind: node_output
          node_id: score_relevance
          pointer: /score
      - target: /template
        source:
          kind: node_output
          node_id: score_relevance
          pointer: /template
    retry:
      max_attempts: 1
      backoff:
        kind: fixed
        delay_ms: 0
    timeout:
      timeout_ms: 10000
    declared_max_cost_units: "1"
    next: [done]

  - id: route_invariant_failed
    kind: Fail
    code: demo.route_invariant
    message: Scoring and route guards disagreed.

  - id: done
    kind: Succeed
    output:
      kind: node_output
      node_id: compose_report
      pointer: ""
"#;

type DemoResult<T> = Result<T, Box<dyn Error>>;
type DurableObjects = FsObjectStore<TestClock>;
type DurableStore = SqliteWorkflowStore<TestClock, DurableObjects>;

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

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn keyword_array_schema() -> Value {
    json!({
        "type": "array",
        "items": {"type": "string", "minLength": 1, "maxLength": 64},
        "minItems": 0,
        "maxItems": 32,
        "uniqueItems": true
    })
}

fn run_input_schema() -> Value {
    object_schema(
        json!({"text": {"type": "string", "minLength": 1, "maxLength": 4000}}),
        &["text"],
    )
}

fn extract_input_schema() -> Value {
    run_input_schema()
}

fn extract_output_schema() -> Value {
    object_schema(json!({"keywords": keyword_array_schema()}), &["keywords"])
}

fn score_input_schema() -> Value {
    object_schema(
        json!({
            "keywords": keyword_array_schema(),
            "threshold": {"type": "integer", "minimum": 0, "maximum": 100}
        }),
        &["keywords", "threshold"],
    )
}

fn score_output_schema() -> Value {
    object_schema(
        json!({
            "keywords": keyword_array_schema(),
            "score": {"type": "integer", "minimum": 0, "maximum": 100},
            "threshold": {"type": "integer", "minimum": 0, "maximum": 100},
            "is_high_relevance": {"type": "boolean"},
            "template": {"type": "string", "enum": ["executive", "brief"]}
        }),
        &[
            "keywords",
            "score",
            "threshold",
            "is_high_relevance",
            "template",
        ],
    )
}

fn compose_input_schema() -> Value {
    object_schema(
        json!({
            "keywords": keyword_array_schema(),
            "score": {"type": "integer", "minimum": 0, "maximum": 100},
            "template": {"type": "string", "enum": ["executive", "brief"]}
        }),
        &["keywords", "score", "template"],
    )
}

fn report_schema() -> Value {
    object_schema(
        json!({
            "title": {"type": "string", "minLength": 1, "maxLength": 120},
            "relevance": {"type": "string", "enum": ["high", "standard"]},
            "score": {"type": "integer", "minimum": 0, "maximum": 100},
            "keyword_count": {"type": "integer", "minimum": 0, "maximum": 32},
            "keywords": keyword_array_schema(),
            "narrative": {"type": "string", "minLength": 1, "maxLength": 2000}
        }),
        &[
            "title",
            "relevance",
            "score",
            "keyword_count",
            "keywords",
            "narrative",
        ],
    )
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

struct ExtractKeywords {
    descriptor: ActionDescriptor,
}

impl WorkflowAction for ExtractKeywords {
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
                Err(_) => return invalid_input("extract input is not JSON"),
            };
            let Some(text) = input.get("text").and_then(Value::as_str) else {
                return invalid_input("extract input requires a text string");
            };
            let keywords = text
                .split(|character: char| !character.is_alphanumeric())
                .filter(|word| word.chars().count() >= 4)
                .map(str::to_lowercase)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .take(32)
                .collect::<Vec<_>>();
            success(json!({"keywords": keywords}))
        })
    }
}

struct ScoreRelevance {
    descriptor: ActionDescriptor,
}

impl WorkflowAction for ScoreRelevance {
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
                Err(_) => return invalid_input("score input is not JSON"),
            };
            let Some(keywords) = input.get("keywords").and_then(Value::as_array) else {
                return invalid_input("score input requires keywords");
            };
            let Some(threshold) = input.get("threshold").and_then(Value::as_u64) else {
                return invalid_input("score input requires an integer threshold");
            };
            let score = (keywords.len() as u64 * 20).min(100);
            let is_high_relevance = score >= threshold;
            success(json!({
                "keywords": keywords,
                "score": score,
                "threshold": threshold,
                "is_high_relevance": is_high_relevance,
                "template": if is_high_relevance { "executive" } else { "brief" }
            }))
        })
    }
}

struct ComposeReport {
    descriptor: ActionDescriptor,
}

impl WorkflowAction for ComposeReport {
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
                Err(_) => return invalid_input("compose input is not JSON"),
            };
            let Some(keywords) = input.get("keywords").and_then(Value::as_array) else {
                return invalid_input("compose input requires keywords");
            };
            let Some(score) = input.get("score").and_then(Value::as_u64) else {
                return invalid_input("compose input requires a score");
            };
            let Some(template) = input.get("template").and_then(Value::as_str) else {
                return invalid_input("compose input requires a template");
            };
            let joined = keywords
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            success(json!({
                "title": if template == "executive" {
                    "Executive relevance report"
                } else {
                    "Relevance brief"
                },
                "relevance": if template == "executive" { "high" } else { "standard" },
                "score": score,
                "keyword_count": keywords.len(),
                "keywords": keywords,
                "narrative": format!(
                    "Deterministic {template} composition from {} keyword(s): {joined}.",
                    keywords.len()
                )
            }))
        })
    }
}

fn success(output: Value) -> ActionOutcome {
    ActionOutcome::success(output, Vec::new(), CostUnits(1), None)
        .expect("demo success outcomes are persistence-safe")
}

fn invalid_input(message: &str) -> ActionOutcome {
    ActionOutcome::permanent(
        "demo.invalid_input".to_owned(),
        message.to_owned(),
        None,
        CostUnits(0),
    )
    .expect("demo errors are persistence-safe")
}

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

fn demo_error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::other(message.into()))
}

fn build_registry_and_schemas() -> DemoResult<(
    Arc<InMemoryActionRegistry>,
    BTreeMap<Digest, Value>,
    Digest,
    Digest,
)> {
    let extract_input = extract_input_schema();
    let extract_output = extract_output_schema();
    let score_input = score_input_schema();
    let score_output = score_output_schema();
    let compose_input = compose_input_schema();
    let report = report_schema();
    let registry = Arc::new(InMemoryActionRegistry::new());

    let extract_descriptor = descriptor("demo.extract_keywords", &extract_input, &extract_output);
    let score_descriptor = descriptor("demo.score_relevance", &score_input, &score_output);
    let compose_descriptor = descriptor("demo.compose_report", &compose_input, &report);
    registry.register(Arc::new(ExtractKeywords {
        descriptor: extract_descriptor,
    }))?;
    registry.register(Arc::new(ScoreRelevance {
        descriptor: score_descriptor,
    }))?;
    registry.register(Arc::new(ComposeReport {
        descriptor: compose_descriptor,
    }))?;

    let run_input = run_input_schema();
    let run_input_digest = canonical_digest(&run_input);
    let report_digest = canonical_digest(&report);
    let schemas = [
        run_input,
        extract_input,
        extract_output,
        score_input,
        score_output,
        compose_input,
        report,
    ]
    .into_iter()
    .map(|schema| (canonical_digest(&schema), schema))
    .collect();
    Ok((registry, schemas, run_input_digest, report_digest))
}

fn repin_definition(
    registry: &Arc<InMemoryActionRegistry>,
    schemas: &BTreeMap<Digest, Value>,
    run_input_digest: &Digest,
    report_digest: &Digest,
) -> DemoResult<dagger_workflow_core::definition::PublishableDefinition> {
    let mut definition = parse_yaml_definition(WORKFLOW_YAML)?;
    debug_assert_eq!(
        definition.run_input_schema_digest,
        Digest::new(PLACEHOLDER_DIGEST)?
    );
    definition.run_input_schema_digest = run_input_digest.clone();
    definition.run_output_schema_digest = report_digest.clone();
    for node in &mut definition.nodes {
        if let NodeDefinition::Action { action, .. } = node {
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
    }
    let unresolved = validate_definition(&definition)
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
    store: &DurableStore,
    objects: &DurableObjects,
    scope: &ExecutionScope,
    publishable: dagger_workflow_core::definition::PublishableDefinition,
    schemas: &BTreeMap<Digest, Value>,
) -> DemoResult<(WorkflowDefinition, Digest)> {
    let definition = publishable.definition.clone();
    let mut schema_objects = BTreeMap::<Digest, VerifiedObjectRef>::new();
    for (schema_digest, schema) in schemas {
        let object = objects
            .put(scope, &serde_jcs::to_vec(schema)?, "application/json")
            .await?;
        if object.digest() != schema_digest {
            return Err(demo_error(
                "schema digest changed during durable publication",
            ));
        }
        schema_objects.insert(schema_digest.clone(), object);
    }
    println!(
        "  published {} durable schema objects",
        schema_objects.len()
    );
    let canonical_definition = objects
        .put(scope, &serde_jcs::to_vec(&definition)?, "application/json")
        .await?;
    let publishing_principal = principal(scope, "durable-demo-publisher")?;
    store
        .create_definition(
            scope,
            CreateDefinition {
                definition_id: definition.definition_id.clone(),
                display_name: definition.name.clone(),
                description: definition.description.clone(),
                principal: publishing_principal.clone(),
            },
        )
        .await?;
    println!("  created definition metadata");

    let resolved_action_schema_objects = definition
        .nodes
        .iter()
        .filter_map(|node| match node {
            NodeDefinition::Action { id, action, .. } => Some((id.as_str().to_owned(), action)),
            _ => None,
        })
        .map(|(location, action)| {
            let input_schema = schema_objects
                .get(&action.input_schema_digest)
                .expect("every action input schema was published")
                .clone();
            let output_schema = schema_objects
                .get(&action.output_schema_digest)
                .expect("every action output schema was published")
                .clone();
            (
                location,
                ResolvedActionSchemas {
                    input_schema,
                    output_schema,
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
                principal: publishing_principal,
            },
        )
        .await?;
    println!("  published immutable workflow revision");
    Ok((definition, canonical_definition.digest().clone()))
}

fn engine(
    store: Arc<DurableStore>,
    objects: Arc<DurableObjects>,
    registry: Arc<InMemoryActionRegistry>,
    instance_id: &str,
) -> DemoResult<WorkflowEngine<DurableStore, DurableObjects, InMemoryActionRegistry>> {
    Ok(WorkflowEngine::new(
        store,
        objects,
        registry,
        EngineConfig {
            instance_id: Id::new(instance_id)?,
            max_concurrency: 2,
            cancellation_grace: std::time::Duration::from_secs(1),
        },
    )?)
}

async fn print_new_events(
    store: &DurableStore,
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
        print_event(&event);
        *after = event.event_seq;
    }
    Ok(())
}

fn print_event(event: &WorkflowEvent) {
    let node = event
        .node_instance_id
        .as_ref()
        .map(|id| id.as_str())
        .unwrap_or("-");
    match event.event_type {
        EventType::RunCreated
        | EventType::RunStarted
        | EventType::NodeAttemptClaimed
        | EventType::AttemptSucceeded
        | EventType::NodeSucceeded
        | EventType::NodeSkipped
        | EventType::SucceedNodeReached
        | EventType::RunSucceeded => {
            println!(
                "  event #{:02} {:<22} node={node}",
                event.event_seq,
                format!("{:?}", event.event_type)
            );
        }
        EventType::ChoiceSelected => {
            let edge = event
                .payload
                .get("edge_id")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let selection = event
                .payload
                .get("selection_kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            println!(
                "  event #{:02} ChoiceSelected         node={node} branch={selection} edge={edge}",
                event.event_seq
            );
        }
        _ => {}
    }
}

async fn create_run(
    store: &DurableStore,
    objects: &DurableObjects,
    scope: &ExecutionScope,
    definition: &WorkflowDefinition,
    revision_hash: &Digest,
    run_id: &Id,
) -> DemoResult<()> {
    let input = json!({
        "text": "Durable Rust workflows preserve verified artifacts across scheduler recovery."
    });
    let input = objects
        .put(scope, &serde_jcs::to_vec(&input)?, "application/json")
        .await?;
    store
        .create_run(
            scope,
            CreateRun {
                run_id: run_id.clone(),
                definition_id: definition.definition_id.clone(),
                revision_hash: revision_hash.clone(),
                input,
                budget_limit: CostUnits(20),
                limits: RunLimits {
                    max_dynamic_node_instances: 20,
                    max_total_attempts: 20,
                    max_total_events: 1000,
                    max_inline_json_bytes_per_value: 100_000,
                    max_artifacts_per_attempt: 10,
                    max_aggregate_object_bytes_per_run: 1_000_000,
                    max_run_lifetime_ms: 120_000,
                },
                principal: principal(scope, "durable-demo-runner")?,
                idempotency_token: "durable-demo-create-run-token-0001".to_owned(),
            },
        )
        .await?;
    Ok(())
}

#[tokio::main]
async fn main() -> DemoResult<()> {
    let scratch = tempfile::tempdir()?;
    let database_path = scratch.path().join("workflow.sqlite");
    let object_root = scratch.path().join("objects");
    println!("Durable workflow demo");
    println!("  SQLite: {}", database_path.display());
    println!("  objects: {}", object_root.display());

    let scope = ExecutionScope {
        tenant_id: ScopeAtom::new("demo-tenant")?,
        namespace: ScopeAtom::new("relevance")?,
    };
    let run_id = Id::new("durable-demo-run")?;
    let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
    let (registry, schemas, run_input_digest, report_digest) = build_registry_and_schemas()?;
    let publishable = repin_definition(&registry, &schemas, &run_input_digest, &report_digest)?;

    let objects = Arc::new(FsObjectStore::open(&object_root, clock.clone())?);
    let store =
        Arc::new(SqliteWorkflowStore::open(&database_path, clock.clone(), objects.clone()).await?);
    println!("publishing workflow...");
    let (definition, revision_hash) =
        publish(&store, &objects, &scope, publishable, &schemas).await?;
    println!("creating run...");
    create_run(
        &store,
        &objects,
        &scope,
        &definition,
        &revision_hash,
        &run_id,
    )
    .await?;
    println!("run created; starting scheduler...");

    let first_engine = engine(
        store.clone(),
        objects.clone(),
        registry.clone(),
        "demo-engine-before-crash",
    )?;
    println!("acquiring initial scheduler claim...");
    let first_claim = first_engine.acquire_scope(&scope).await?;
    println!("starting run...");
    first_engine.start(&scope, &run_id).await?;
    let mut last_event = 0;
    print_new_events(&store, &scope, &run_id, &mut last_event).await?;

    for tick in 1..=2 {
        let changes = first_engine.tick(&scope).await?;
        println!("tick {tick}: {changes} durable change(s)");
        print_new_events(&store, &scope, &run_id, &mut last_event).await?;
    }
    let extraction_attempts = store
        .get_node(&scope, &run_id, &Id::new("extract_keywords")?)
        .await?
        .attempt_count;
    let score_attempts = store
        .get_node(&scope, &run_id, &Id::new("score_relevance")?)
        .await?
        .attempt_count;
    println!(
        "\n*** simulated kill after generation {}: run is still {:?}; completed attempts extract={extraction_attempts}, score={score_attempts} ***",
        first_claim.generation,
        store.get_run(&scope, &run_id).await?.run.status
    );

    drop(first_engine);
    drop(store);
    drop(objects);
    println!("waiting 20.1s for the SQLite-authoritative crashed-engine lease to expire...");
    std::thread::sleep(Duration::from_millis(20_100));

    let recovered_objects = Arc::new(FsObjectStore::open(&object_root, clock.clone())?);
    let recovered_store = Arc::new(
        SqliteWorkflowStore::open(&database_path, clock.clone(), recovered_objects.clone()).await?,
    );
    let recovered_engine = engine(
        recovered_store.clone(),
        recovered_objects.clone(),
        registry,
        "demo-engine-after-recovery",
    )?;
    let recovered_claim = recovered_engine.acquire_scope(&scope).await?;
    println!(
        "*** recovered SQLite + filesystem stores; claim generation {} -> {} ***\n",
        first_claim.generation, recovered_claim.generation
    );

    for tick in 3..=20 {
        let changes = recovered_engine.tick(&scope).await?;
        println!("tick {tick}: {changes} durable change(s)");
        print_new_events(&recovered_store, &scope, &run_id, &mut last_event).await?;
        if recovered_store
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

    let run = recovered_store.get_run(&scope, &run_id).await?.run;
    if run.status != RunState::Succeeded {
        return Err(demo_error(format!(
            "workflow ended in unexpected state {:?}",
            run.status
        )));
    }
    let extract_after_recovery = recovered_store
        .get_node(&scope, &run_id, &Id::new("extract_keywords")?)
        .await?
        .attempt_count;
    let score_after_recovery = recovered_store
        .get_node(&scope, &run_id, &Id::new("score_relevance")?)
        .await?
        .attempt_count;
    if (extract_after_recovery, score_after_recovery) != (extraction_attempts, score_attempts) {
        return Err(demo_error("recovery replayed an already-completed action"));
    }

    let output_ref = run
        .output_ref
        .as_ref()
        .ok_or_else(|| demo_error("succeeded run has no output artifact"))?;
    let verified_report = recovered_objects.get(&scope, &output_ref.0.digest).await?;
    let recomputed = digest(&verified_report.bytes);
    if recomputed != output_ref.0.digest {
        return Err(demo_error("final report digest verification failed"));
    }
    let report: Value = serde_json::from_slice(&verified_report.bytes)?;

    println!("\nSUCCESS");
    println!("  run id: {}", run.run_id.as_str());
    println!("  final status: {:?}", run.status);
    println!("  report digest: {}", output_ref.0.digest.as_str());
    println!("  digest verified: yes");
    println!("  completed work replayed after recovery: no");
    println!("  report JSON:\n{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
