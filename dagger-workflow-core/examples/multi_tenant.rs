//! Two tenants, one store, one definition: nothing crosses the scope boundary.
//!
//! Run with:
//!
//! ```text
//! cargo run -p dagger-workflow-core --example multi_tenant
//! ```
//!
//! This crate is meant to be embedded in a server that holds many tenants in
//! one process and one database, so scope isolation is the first property such
//! a host has to be able to demonstrate. Every durable key in contract section
//! 1.1 is `(ExecutionScope, id)`, never `id` alone, and every store command and
//! query takes the scope explicitly.
//!
//! What this shows, in order:
//!
//! 1. The same YAML definition, the same `definition_id`, and the same logical
//!    `run_id` published and executed under two different `ExecutionScope`s
//!    against one shared `InMemoryStore` and one shared `InMemoryObjectStore`.
//!    The identical run IDs do not collide; they are two different rows.
//! 2. Both runs making progress in the same loop, one scheduler pass per scope
//!    per tick, each engine holding its own scoped singleton claim.
//! 3. Four isolation checks run against the live store afterwards: runs,
//!    events, budget and attempt counters, and content-addressed artifacts are
//!    all disjoint, and a read issued in one scope cannot observe the other's
//!    state even when it supplies the other's exact ID or digest.
//!
//! Store choice: the in-memory control plane and object store, so the example
//! stays a single process with no filesystem state. Scope confinement is a
//! property of the key space, not of the backend; the SQLite store keys the
//! same way. The sibling example `durable_demo` covers the durable backends.
//!
//! Why the YAML carries placeholder digests: an action pin is a content
//! address of the action's JSON Schema documents, and those digests are
//! computed from the registered implementations, so the definition is authored
//! with `sha256:000...` placeholders and repinned after parsing and before
//! validation. Both sibling examples do the same.

use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, ActionRegistry, InMemoryActionRegistry,
    WorkflowAction,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::{ObjectReadError, ObjectStore, VerifiedObjectRef};
use dagger_workflow_core::definition::{
    extract_action_pins, parse_yaml_definition, resolve_publication, validate_definition,
    ExtractedActionPin, NodeDefinition, PublicationResolver, PublicationSchemaDocument,
    PublishableDefinition, WorkflowDefinition,
};
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::ids::{CostUnits, Digest, Id, Timestamp, Version};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::run::{RunLimits, RunState, WorkflowRun};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::store::{
    CreateDefinition, CreateRun, EventPageRequest, PageRequest, PublishRevision,
    ResolvedActionSchemas, StoreError, WorkflowStore,
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

/// The definition both tenants publish, byte for byte. Every `sha256:000...`
/// is a placeholder rewritten by `repin_and_validate` before validation.
const WORKFLOW_YAML: &str = r#"
definition_format_version: "0.1"
definition_id: monthly_rollup
name: Monthly usage rollup
description: Normalize a tenant's metered units, then total them.
run_input_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
run_output_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
entry_node_id: normalize
nodes:
  - id: normalize
    kind: Action
    action:
      name: rollup.normalize
      contract_version: placeholder
      input_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      output_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      compatible_implementation_requirement: sha256:0000000000000000000000000000000000000000000000000000000000000000
    bindings:
      - target: /units
        source:
          kind: run_input
          pointer: /units
    retry:
      max_attempts: 2
      backoff:
        kind: fixed
        delay_ms: 0
    timeout:
      timeout_ms: 10000
    declared_max_cost_units: "1"
    next: [total]

  - id: total
    kind: Action
    action:
      name: rollup.total
      contract_version: placeholder
      input_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      output_schema_digest: sha256:0000000000000000000000000000000000000000000000000000000000000000
      compatible_implementation_requirement: sha256:0000000000000000000000000000000000000000000000000000000000000000
    bindings:
      - target: /units
        source:
          kind: node_output
          node_id: normalize
          pointer: /units
      - target: /count
        source:
          kind: node_output
          node_id: normalize
          pointer: /count
    retry:
      max_attempts: 2
      backoff:
        kind: fixed
        delay_ms: 0
    timeout:
      timeout_ms: 10000
    declared_max_cost_units: "1"
    next: [done]

  - id: done
    kind: Succeed
    output:
      kind: node_output
      node_id: total
      pointer: ""
"#;

/// Both tenants use this identical logical run ID. Contract section 1.1 keys
/// every run by `(scope, run_id)`, so the two are distinct durable rows.
const SHARED_RUN_ID: &str = "rollup-2026-07";

/// A run that exists only in the second tenant's scope, used by the negative
/// read checks at the end.
const GLOBEX_ONLY_RUN_ID: &str = "globex-adhoc-rollup";

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

fn integer() -> Value {
    json!({"type": "integer", "minimum": 0, "maximum": 1000000})
}

fn unit_array() -> Value {
    json!({
        "type": "array",
        "items": integer(),
        "minItems": 1,
        "maxItems": 32
    })
}

/// Also the run input schema: a run is created with the tenant's metered units.
fn normalize_input_schema() -> Value {
    object_schema(json!({"units": unit_array()}), &["units"])
}

fn normalize_output_schema() -> Value {
    object_schema(
        json!({"count": integer(), "units": unit_array()}),
        &["count", "units"],
    )
}

fn total_input_schema() -> Value {
    normalize_output_schema()
}

/// Also the pinned root output schema.
fn total_output_schema() -> Value {
    object_schema(
        json!({"count": integer(), "total": integer()}),
        &["count", "total"],
    )
}

// --- actions -----------------------------------------------------------------

struct RollupAction {
    descriptor: ActionDescriptor,
    compute: fn(&Value) -> Result<Value, String>,
}

impl WorkflowAction for RollupAction {
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
        "rollup.invalid_input".to_owned(),
        message.to_owned(),
        None,
        CostUnits(0),
    )
    .expect("demo errors are persistence-safe")
}

fn units(input: &Value) -> Result<Vec<i64>, String> {
    input
        .get("units")
        .and_then(Value::as_array)
        .ok_or("`units` must be an array")?
        .iter()
        .map(|value| {
            value
                .as_i64()
                .ok_or_else(|| "units must be integers".into())
        })
        .collect()
}

fn normalize(input: &Value) -> Result<Value, String> {
    let mut units = units(input)?;
    units.sort_unstable();
    Ok(json!({"count": units.len(), "units": units}))
}

fn total(input: &Value) -> Result<Value, String> {
    let units = units(input)?;
    Ok(json!({"count": units.len(), "total": units.iter().sum::<i64>()}))
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

/// The action registry and the schema catalogue are process-wide, not
/// per-tenant: implementations and schema documents are code, and the scope
/// boundary is over durable state. Contract section 13.2.
fn build_registry_and_schemas() -> DemoResult<(Arc<InMemoryActionRegistry>, BTreeMap<Digest, Value>)>
{
    let registry = Arc::new(InMemoryActionRegistry::new());
    let mut schemas = BTreeMap::new();
    for (name, input, output, compute) in [
        (
            "rollup.normalize",
            normalize_input_schema(),
            normalize_output_schema(),
            normalize as fn(&Value) -> Result<Value, String>,
        ),
        (
            "rollup.total",
            total_input_schema(),
            total_output_schema(),
            total as fn(&Value) -> Result<Value, String>,
        ),
    ] {
        schemas.insert(canonical_digest(&input), input.clone());
        schemas.insert(canonical_digest(&output), output.clone());
        registry.register(Arc::new(RollupAction {
            descriptor: descriptor(name, &input, &output),
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

fn repin_and_validate(
    definition: &mut WorkflowDefinition,
    registry: &Arc<InMemoryActionRegistry>,
    schemas: &BTreeMap<Digest, Value>,
) -> DemoResult<PublishableDefinition> {
    definition.run_input_schema_digest = canonical_digest(&normalize_input_schema());
    definition.run_output_schema_digest = canonical_digest(&total_output_schema());
    for node in &mut definition.nodes {
        let NodeDefinition::Action { action, .. } = node else {
            continue;
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

/// Publishes the definition and every schema object into one scope. Called
/// once per tenant with the identical `PublishableDefinition`, which is what
/// makes the two revisions byte-identical and their storage still disjoint.
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
    let publisher = principal(scope, "rollup-publisher")?;
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

#[allow(clippy::too_many_arguments)]
async fn create_run(
    store: &Store,
    objects: &Objects,
    scope: &ExecutionScope,
    definition: &WorkflowDefinition,
    revision_hash: &Digest,
    run_id: &str,
    input: &Value,
    budget_limit: CostUnits,
    max_total_attempts: u64,
) -> DemoResult<Id> {
    let run_id = Id::new(run_id)?;
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
                budget_limit,
                limits: RunLimits {
                    max_dynamic_node_instances: 32,
                    max_total_attempts,
                    max_total_events: 1000,
                    max_inline_json_bytes_per_value: 100_000,
                    max_artifacts_per_attempt: 10,
                    max_aggregate_object_bytes_per_run: 1_000_000,
                    max_run_lifetime_ms: 120_000,
                },
                // The principal is minted against the scope, so a token issued
                // for one tenant cannot address another. Contract section 16.1.
                principal: principal(scope, "rollup-runner")?,
                // Idempotency tokens are scope-confined too: the same token
                // string in two scopes is two independent creations.
                idempotency_token: format!("rollup-create-{}", run_id.as_str()),
            },
        )
        .await?;
    Ok(run_id)
}

// --- one tenant --------------------------------------------------------------

/// Everything the transcript needs about one tenant, gathered in one place so
/// the two can be printed side by side.
struct Tenant {
    name: &'static str,
    scope: ExecutionScope,
    engine: Engine,
    shared_run: Id,
    input: Value,
}

async fn event_count(store: &Store, scope: &ExecutionScope, run_id: &Id) -> DemoResult<usize> {
    Ok(store
        .list_events_after(
            scope,
            run_id,
            EventPageRequest {
                after_event_seq: 0,
                page_size: 1000,
                hard_response_byte_limit: 4_000_000,
            },
        )
        .await?
        .len())
}

async fn all_runs(store: &Store, scope: &ExecutionScope) -> DemoResult<Vec<WorkflowRun>> {
    Ok(store
        .list_runs(
            scope,
            PageRequest {
                cursor: None,
                page_size: 100,
            },
        )
        .await?
        .items)
}

fn scope_label(scope: &ExecutionScope) -> String {
    format!("{}/{}", scope.tenant_id.as_str(), scope.namespace.as_str())
}

// --- main --------------------------------------------------------------------

#[tokio::main]
async fn main() -> DemoResult<()> {
    println!("Multi-tenant demo: one store, one definition, two execution scopes");
    println!("  store: one in-memory control plane and one in-memory object store, shared");
    println!("  definition: monthly_rollup, published byte-identically into both scopes");
    println!("  graph: normalize [Action] -> total [Action] -> done [Succeed]");

    let clock = Arc::new(TestClock::new(Timestamp(1_000_000)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = Arc::new(InMemoryStore::new(clock.clone()));
    let (registry, schemas) = build_registry_and_schemas()?;

    let mut definition = parse_yaml_definition(WORKFLOW_YAML)?;
    let publishable = repin_and_validate(&mut definition, &registry, &schemas)?;

    println!("\nStep 1: the two scopes");
    println!(
        "  {:<10} {:<24} {:<16} {:<8} run input",
        "tenant", "scope (tenant/namespace)", "run id", "budget"
    );
    let mut tenants = Vec::new();
    for (name, tenant_id, namespace, budget, attempts, input) in [
        (
            "acme",
            "acme",
            "billing",
            CostUnits(20),
            8,
            json!({"units": [7, 3, 11]}),
        ),
        (
            "globex",
            "globex",
            "billing",
            CostUnits(40),
            16,
            json!({"units": [100, 250, 25, 125]}),
        ),
    ] {
        let scope = ExecutionScope {
            tenant_id: ScopeAtom::new(tenant_id)?,
            namespace: ScopeAtom::new(namespace)?,
        };
        let (definition, revision_hash) =
            publish(&store, &objects, &scope, publishable.clone(), &schemas).await?;
        let shared_run = create_run(
            &store,
            &objects,
            &scope,
            &definition,
            &revision_hash,
            SHARED_RUN_ID,
            &input,
            budget,
            attempts,
        )
        .await?;
        println!(
            "  {name:<10} {:<24} {:<16} {:<8} {}",
            scope_label(&scope),
            shared_run.as_str(),
            budget.0,
            serde_json::to_string(&input)?
        );
        let engine = WorkflowEngine::new(
            store.clone(),
            objects.clone(),
            registry.clone(),
            EngineConfig {
                // One scheduler generation is held per scope, so each tenant is
                // served by its own claim. Contract section 6.
                instance_id: Id::new(format!("rollup-engine-{name}"))?,
                max_concurrency: 2,
            },
        )?;
        engine.acquire_scope(&scope).await?;
        tenants.push(Tenant {
            name,
            scope,
            engine,
            shared_run,
            input,
        });

        // The second tenant also owns a run that has no counterpart in the
        // first, used by the negative read checks in step 3.
        if name == "globex" {
            let tenant = tenants.last().expect("just pushed");
            create_run(
                &store,
                &objects,
                &tenant.scope,
                &definition,
                &revision_hash,
                GLOBEX_ONLY_RUN_ID,
                &json!({"units": [1]}),
                CostUnits(10),
                4,
            )
            .await?;
        }
    }
    println!("  note: both runs carry the identical run id `{SHARED_RUN_ID}`");
    println!("  note: globex additionally owns `{GLOBEX_ONLY_RUN_ID}`, which acme has never seen");

    println!("\nStep 2: execute both, one scheduler pass per scope per tick");
    for tenant in &tenants {
        tenant
            .engine
            .start(&tenant.scope, &tenant.shared_run)
            .await?;
    }
    for tick in 1..=20 {
        let mut line = format!("  tick {tick}:");
        let mut terminal = 0;
        for tenant in &tenants {
            let changes = tenant.engine.tick(&tenant.scope).await?;
            let status = store
                .get_run(&tenant.scope, &tenant.shared_run)
                .await?
                .run
                .status;
            line.push_str(&format!(
                " {}={changes} change(s) [{status:?}]",
                tenant.name
            ));
            if status.is_terminal() {
                terminal += 1;
            }
        }
        println!("{line}");
        if terminal == tenants.len() {
            break;
        }
    }

    println!("\nStep 3: the two scopes side by side");
    let mut summaries = Vec::new();
    for tenant in &tenants {
        let run = store.get_run(&tenant.scope, &tenant.shared_run).await?.run;
        if run.status != RunState::Succeeded {
            return Err(demo_error(format!(
                "{} ended in unexpected state {:?}",
                tenant.name, run.status
            )));
        }
        let output_ref = run
            .output_ref
            .clone()
            .ok_or_else(|| demo_error("succeeded run has no output artifact"))?;
        let verified = objects.get(&tenant.scope, &output_ref.0.digest).await?;
        let output: Value = serde_json::from_slice(&verified.bytes)?;
        summaries.push((
            tenant,
            run,
            output_ref.0.digest.clone(),
            output,
            event_count(&store, &tenant.scope, &tenant.shared_run).await?,
            all_runs(&store, &tenant.scope).await?.len(),
        ));
    }
    let field = |label: &str, values: Vec<String>| {
        println!("  {label:<24} {:<40} {}", values[0], values[1]);
    };
    println!(
        "  {:<24} {:<40} {}",
        "", summaries[0].0.name, summaries[1].0.name
    );
    field(
        "scope",
        summaries
            .iter()
            .map(|entry| scope_label(&entry.0.scope))
            .collect(),
    );
    field(
        "run id",
        summaries
            .iter()
            .map(|entry| entry.1.run_id.as_str().to_owned())
            .collect(),
    );
    field(
        "run input",
        summaries
            .iter()
            .map(|entry| serde_json::to_string(&entry.0.input).expect("input is JSON"))
            .collect(),
    );
    field(
        "status",
        summaries
            .iter()
            .map(|entry| format!("{:?}", entry.1.status))
            .collect(),
    );
    field(
        "attempts",
        summaries
            .iter()
            .map(|entry| entry.1.total_attempt_count.to_string())
            .collect(),
    );
    field(
        "budget limit/consumed",
        summaries
            .iter()
            .map(|entry| format!("{}/{}", entry.1.budget_limit.0, entry.1.budget_consumed.0))
            .collect(),
    );
    field(
        "max_total_attempts",
        summaries
            .iter()
            .map(|entry| entry.1.limits.max_total_attempts.to_string())
            .collect(),
    );
    field(
        "events for this run",
        summaries.iter().map(|entry| entry.4.to_string()).collect(),
    );
    field(
        "runs visible in scope",
        summaries.iter().map(|entry| entry.5.to_string()).collect(),
    );
    field(
        "output digest",
        summaries.iter().map(|entry| short(&entry.2)).collect(),
    );
    field(
        "output JSON",
        summaries
            .iter()
            .map(|entry| serde_json::to_string(&entry.3).expect("output is JSON"))
            .collect(),
    );

    println!("\nStep 4: isolation checks against the live store");
    let (acme, globex) = (&summaries[0], &summaries[1]);

    // 1. Identical logical run IDs are two rows, not one.
    if acme.1.run_id != globex.1.run_id {
        return Err(demo_error("the two runs were expected to share a run id"));
    }
    if acme.2 == globex.2 {
        return Err(demo_error(
            "the two tenants produced the same output digest",
        ));
    }
    println!(
        "  same run id in both scopes  -> two distinct runs, distinct outputs ({} vs {})",
        short(&acme.2),
        short(&globex.2)
    );

    // 2. A run read in the wrong scope is absent, not merely unreadable.
    let leaked = store
        .get_run(&acme.0.scope, &Id::new(GLOBEX_ONLY_RUN_ID)?)
        .await;
    match leaked {
        Err(StoreError::NotFound) => println!(
            "  get_run(acme, `{GLOBEX_ONLY_RUN_ID}`) -> StoreError::NotFound, exact ID supplied"
        ),
        other => {
            return Err(demo_error(format!(
                "a globex run was observable from acme's scope: {other:?}"
            )))
        }
    }

    // 3. Content addressing does not bypass the boundary: the digest is
    //    correct and the object is still not in acme's scope. Contract
    //    section 12.3 answers with a failed-read proof, never with bytes.
    match objects.get(&acme.0.scope, &globex.2).await {
        Err(ObjectReadError::Corrupt(proof)) => println!(
            "  objects.get(acme, globex digest) -> failed-read proof, class {:?}",
            proof.error_class()
        ),
        Ok(_) => {
            return Err(demo_error(
                "globex's output artifact was readable from acme's scope",
            ))
        }
        Err(other) => {
            return Err(demo_error(format!(
                "unexpected object read error: {other:?}"
            )))
        }
    }

    // 4. Every event returned for a scoped run carries that scope, so an event
    //    stream cannot be a leak channel either.
    for (tenant, run, _, _, _, _) in &summaries {
        let events = store
            .list_events_after(
                &tenant.scope,
                &run.run_id,
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1000,
                    hard_response_byte_limit: 4_000_000,
                },
            )
            .await?;
        if events.iter().any(|event| event.scope != tenant.scope) {
            return Err(demo_error(format!(
                "{}'s event stream contained a foreign scope",
                tenant.name
            )));
        }
        println!(
            "  events for {:<7} -> {} events, all scoped to {}",
            tenant.name,
            events.len(),
            scope_label(&tenant.scope)
        );
    }

    println!("\nBoth tenants ran the same definition to Succeeded under the same run id.");
    println!("Runs, events, budgets, attempt counters, and artifacts are disjoint, and no");
    println!("read in one scope could observe the other's state.");
    Ok(())
}
