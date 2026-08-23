#![cfg(feature = "sqlite")]
//! W10 acceptance specs: RunLimits ceilings, event correlation, and the
//! temporary-versus-permanent budget distinction.
//!
//! The implemented contract is in `src/run.rs`, `src/budget.rs`,
//! `src/event.rs`, and the `WorkflowStore` command boundary. The host rules
//! are in `docs/system/operations-and-limits.md`.
//!
//! Two things this file deliberately does that a looser suite would not.
//! First, every ceiling is asserted from both sides at its exact arithmetic
//! boundary rather than "somewhere above": a fixture that only proves a huge
//! value is rejected survives an off-by-one and survives a check moved to the
//! wrong command. Second, each RunLimits and event fixture runs against both
//! store implementations through the `WorkflowStore` trait, because the two
//! reducers are separate code paths that have drifted before.

use dagger_workflow_core::action::{
    ActionOutcome, ArtifactOutput, CompatibilityReport, CompletionCredential,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::{ObjectStore, VerifiedObjectRef};
use dagger_workflow_core::definition::{
    canonical_topological_ranks, ActionReference, BackoffPolicy, Binding, BindingSource,
    MapBinding, MapBindingSource, NodeDefinition, PublishableDefinition, RetryPolicy,
    TimeoutPolicy, WorkflowDefinition,
};
use dagger_workflow_core::engine::TestClock;
use dagger_workflow_core::event::WorkflowEvent;
use dagger_workflow_core::ids::{
    map_child_id, map_expansion_digest, CostUnits, Digest, Id, MapChildIdentity, NodeInstanceId,
    Timestamp, Version,
};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::run::{AttemptState, NodeFailureKind, NodeState, RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::sqlite::SqliteWorkflowStore;
use dagger_workflow_core::store::{
    AcquiredEngineClaim, ClaimNodeAttempt, ClaimNodeAttemptResult, CompleteAttempt,
    CompleteAttemptResult, CompletionObjects, CreateDefinition, CreateRun, EventPageRequest,
    ExpandMap, ExpireRunLifetime, OrderedMapItem, PublishRevision, ReleaseRetry,
    ResolveTerminalNode, ResolvedActionSchemas, StartRun, StoreError, WorkflowStore,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tempfile::TempDir;

const SCHEMA: &[u8] = br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#;
const RUN_INPUT: &[u8] = br#"{"value":1}"#;
const CLOCK_ORIGIN: Timestamp = Timestamp(1_000_000);

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}

fn scope(tenant: &str) -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new(tenant).unwrap(),
        namespace: ScopeAtom::new("w10").unwrap(),
    }
}

/// Deliberately generous defaults. Each ceiling fixture narrows exactly one
/// field so a failure can only come from the ceiling under test.
fn generous_limits() -> RunLimits {
    RunLimits {
        max_dynamic_node_instances: 16,
        max_total_attempts: 16,
        max_total_events: 1_000,
        max_inline_json_bytes_per_value: 100_000,
        max_artifacts_per_attempt: 8,
        max_aggregate_object_bytes_per_run: 1_000_000,
        max_run_lifetime_ms: 100_000,
    }
}

#[derive(Clone)]
struct Fixture {
    scope: ExecutionScope,
    principal: AuthenticatedPrincipal,
    revision_hash: Digest,
    schema_digest: Digest,
    input: VerifiedObjectRef,
}

/// Runs one fixture body against both store implementations.
///
/// The two reducers are independent code paths behind one trait; a ceiling
/// enforced in only one of them is exactly the partial wiring this file exists
/// to catch, so no W10 property is accepted from a single store.
macro_rules! both_stores {
    ($body:ident) => {{
        let clock = Arc::new(TestClock::new(CLOCK_ORIGIN));
        let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let store = InMemoryStore::new(clock.clone());
        $body(&store, &objects, &clock, "memory").await;

        let clock = Arc::new(TestClock::new(CLOCK_ORIGIN));
        let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let store =
            SqliteWorkflowStore::open_url("sqlite::memory:", clock.clone(), objects.clone())
                .await
                .unwrap();
        $body(&store, &objects, &clock, "sqlite").await;
    }};
}

async fn publish<S: WorkflowStore>(
    store: &S,
    objects: &Arc<InMemoryObjectStore<TestClock>>,
    tenant: &str,
    nodes: Vec<NodeDefinition>,
    entry: Id,
    action_schema_keys: &[&str],
) -> Fixture {
    let workflow_scope = scope(tenant);
    let schema = objects
        .put(&workflow_scope, SCHEMA, "application/json")
        .await
        .unwrap();
    let principal = AuthenticatedPrincipal::mint(
        workflow_scope.clone(),
        format!("{tenant}-host"),
        Vec::new(),
        schema.digest().clone(),
    )
    .unwrap();
    let definition_id = id("definition");
    store
        .create_definition(
            &workflow_scope,
            CreateDefinition {
                definition_id: definition_id.clone(),
                display_name: "w10 fixture".to_owned(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: definition_id.clone(),
        name: "w10-fixture".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.digest().clone(),
        run_output_schema_digest: schema.digest().clone(),
        entry_node_id: entry,
        nodes,
    };
    let canonical = objects
        .put(
            &workflow_scope,
            &serde_jcs::to_vec(&definition).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let ranks = canonical_topological_ranks(&definition).unwrap();
    let action_schemas = action_schema_keys
        .iter()
        .map(|key| {
            (
                (*key).to_owned(),
                ResolvedActionSchemas {
                    input_schema: schema.clone(),
                    output_schema: schema.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    store
        .publish_revision(
            &workflow_scope,
            PublishRevision {
                definition_id,
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema.clone(),
                resolved_action_schema_objects: action_schemas,
                parsed_revision: PublishableDefinition {
                    definition,
                    topological_ranks: ranks,
                },
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let input = objects
        .put(&workflow_scope, RUN_INPUT, "application/json")
        .await
        .unwrap();
    Fixture {
        scope: workflow_scope,
        principal,
        revision_hash: canonical.digest().clone(),
        schema_digest: schema.digest().clone(),
        input,
    }
}

/// Two static nodes: `action` then `succeed`. Node count is load-bearing for
/// the `max_total_events` boundary, which is derived from it.
async fn seed_action<S: WorkflowStore>(
    store: &S,
    objects: &Arc<InMemoryObjectStore<TestClock>>,
    tenant: &str,
    max_attempts: u32,
    declared_max_cost_units: CostUnits,
) -> Fixture {
    let schema_digest = {
        let put = objects
            .put(&scope(tenant), SCHEMA, "application/json")
            .await
            .unwrap();
        put.digest().clone()
    };
    let nodes = vec![
        NodeDefinition::Action {
            id: id("action"),
            action: ActionReference {
                name: "w10.action".to_owned(),
                contract_version: "1".to_owned(),
                input_schema_digest: schema_digest.clone(),
                output_schema_digest: schema_digest.clone(),
                compatible_implementation_requirement: schema_digest.clone(),
            },
            bindings: vec![Binding {
                target: "/value".to_owned(),
                source: BindingSource::RunInput {
                    pointer: "/value".to_owned(),
                },
            }],
            retry: RetryPolicy {
                max_attempts,
                backoff: BackoffPolicy::Fixed { delay_ms: 0 },
            },
            timeout: TimeoutPolicy { timeout_ms: 60_000 },
            declared_max_cost_units,
            next: vec![id("succeed")],
        },
        NodeDefinition::Succeed {
            id: id("succeed"),
            output: BindingSource::NodeOutput {
                node_id: id("action"),
                pointer: String::new(),
            },
        },
    ];
    publish(store, objects, tenant, nodes, id("action"), &["action"]).await
}

/// A Map whose children are concurrently claimable. Map children are the only
/// v0.1 way to hold two live reservations in one run, which is what the
/// reservation-pressure branch requires.
async fn seed_map<S: WorkflowStore>(
    store: &S,
    objects: &Arc<InMemoryObjectStore<TestClock>>,
    tenant: &str,
    declared_max_cost_units: CostUnits,
) -> Fixture {
    let schema_digest = {
        let put = objects
            .put(&scope(tenant), SCHEMA, "application/json")
            .await
            .unwrap();
        put.digest().clone()
    };
    let nodes = vec![
        NodeDefinition::Map {
            id: id("map"),
            items: BindingSource::RunInput {
                pointer: String::new(),
            },
            max_items: 8,
            max_concurrency: 4,
            action: ActionReference {
                name: "w10.map.action".to_owned(),
                contract_version: "1".to_owned(),
                input_schema_digest: schema_digest.clone(),
                output_schema_digest: schema_digest.clone(),
                compatible_implementation_requirement: schema_digest.clone(),
            },
            bindings: vec![MapBinding {
                target: "/value".to_owned(),
                source: MapBindingSource::MapItem {
                    pointer: "/value".to_owned(),
                },
            }],
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: BackoffPolicy::Fixed { delay_ms: 0 },
            },
            timeout: TimeoutPolicy { timeout_ms: 60_000 },
            declared_max_cost_units,
            next: vec![id("succeed")],
        },
        NodeDefinition::Succeed {
            id: id("succeed"),
            output: BindingSource::NodeOutput {
                node_id: id("map"),
                pointer: String::new(),
            },
        },
    ];
    publish(
        store,
        objects,
        tenant,
        nodes,
        id("map"),
        &["map/map_action"],
    )
    .await
}

fn create_run_command(
    fixture: &Fixture,
    run_id: &str,
    budget_limit: CostUnits,
    limits: RunLimits,
) -> CreateRun {
    CreateRun {
        run_id: id(run_id),
        definition_id: id("definition"),
        revision_hash: fixture.revision_hash.clone(),
        input: fixture.input.clone(),
        budget_limit,
        limits,
        principal: fixture.principal.clone(),
        idempotency_token: format!("w10-create-{run_id}-token-long-enough"),
    }
}

async fn claim_engine<S: WorkflowStore>(
    store: &S,
    fixture: &Fixture,
    engine: &str,
) -> AcquiredEngineClaim {
    store
        .acquire_engine_claim(&fixture.scope, id(engine))
        .await
        .unwrap()
}

async fn start<S: WorkflowStore>(
    store: &S,
    fixture: &Fixture,
    claim: &AcquiredEngineClaim,
    run_id: &str,
) {
    store
        .start_run(
            &fixture.scope,
            StartRun {
                permit: claim.permit.clone(),
                run_id: id(run_id),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: fixture.schema_digest.clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .unwrap();
}

async fn claim_attempt<S: WorkflowStore>(
    store: &S,
    fixture: &Fixture,
    claim: &AcquiredEngineClaim,
    run_id: &str,
    node: &NodeInstanceId,
    attempt_id: &str,
    bound_input: &VerifiedObjectRef,
) -> Result<ClaimNodeAttemptResult, StoreError> {
    let current = store
        .get_node(&fixture.scope, &id(run_id), node)
        .await
        .unwrap();
    store
        .claim_node_attempt(
            &fixture.scope,
            ClaimNodeAttempt {
                permit: claim.permit.clone(),
                run_id: id(run_id),
                node_id: node.clone(),
                expected_node_version: current.version,
                attempt_id: id(attempt_id),
                worker_id: id("w10-worker"),
                bound_input: bound_input.clone(),
                binding_derivation_digest: fixture.schema_digest.clone(),
            },
        )
        .await
}

/// Closed claim results carry no `Debug`, so failures name the variant here.
fn describe(result: &ClaimNodeAttemptResult) -> &'static str {
    match result {
        ClaimNodeAttemptResult::Claimed { .. } => "Claimed",
        ClaimNodeAttemptResult::BudgetWaitingApplied(_) => "BudgetWaitingApplied",
        ClaimNodeAttemptResult::MapConcurrencyLimited => "MapConcurrencyLimited",
        ClaimNodeAttemptResult::BudgetExhaustedApplied(_) => "BudgetExhaustedApplied",
        ClaimNodeAttemptResult::RunLimitApplied(_) => "RunLimitApplied",
    }
}

fn credential_of(result: ClaimNodeAttemptResult) -> CompletionCredential {
    match result {
        ClaimNodeAttemptResult::Claimed {
            completion_credential,
            ..
        } => completion_credential,
        other => panic!("expected a claimed attempt, observed {}", describe(&other)),
    }
}

async fn events<S: WorkflowStore>(
    store: &S,
    fixture: &Fixture,
    run_id: &str,
) -> Vec<WorkflowEvent> {
    store
        .list_events_after(
            &fixture.scope,
            &id(run_id),
            EventPageRequest {
                after_event_seq: 0,
                page_size: 1_000,
                hard_response_byte_limit: 8_000_000,
            },
        )
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// The seven RunLimits ceilings.
// ---------------------------------------------------------------------------

/// `max_total_events` is refused at run creation by exact arithmetic.
///
/// Section 1.4 enforces the event ceiling "before every event batch", and
/// creation must not produce a run that cannot even reach a terminal event.
/// The store's admission rule is `creation_events + creation_reserve`, which
/// for a two-node definition is `(1 + 2) + (1 + 2 + 2) = 8`. Asserting both
/// sides pins the reserve arithmetic, not just the presence of a check.
#[tokio::test]
async fn max_total_events_ceiling_is_exact_at_create_run() {
    async fn body<S: WorkflowStore>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _clock: &Arc<TestClock>,
        tenant: &str,
    ) {
        let fixture =
            seed_action(store, objects, &format!("events-{tenant}"), 2, CostUnits(1)).await;
        let mut below = generous_limits();
        below.max_total_events = 7;
        assert_eq!(
            store
                .create_run(
                    &fixture.scope,
                    create_run_command(&fixture, "below", CostUnits(10), below)
                )
                .await,
            Err(StoreError::RunLimitsInvalid),
            "{tenant}: a run that cannot afford creation plus its terminal reserve must not exist"
        );
        assert_eq!(
            store.get_run(&fixture.scope, &id("below")).await.err(),
            Some(StoreError::NotFound),
            "{tenant}: the refused creation left a row behind"
        );

        let mut at = generous_limits();
        at.max_total_events = 8;
        store
            .create_run(
                &fixture.scope,
                create_run_command(&fixture, "at", CostUnits(10), at),
            )
            .await
            .unwrap();
        let run = store.get_run(&fixture.scope, &id("at")).await.unwrap().run;
        // R01 plus one creation event per static node: the measured cost the
        // boundary above is derived from.
        assert_eq!(run.last_event_seq, 3, "{tenant}");
    }
    both_stores!(body);
}

/// `max_dynamic_node_instances` is refused at Map expansion by exact count.
///
/// Section 1.4 enforces this "before Map expansion". A two-item expansion must
/// be admitted at a ceiling of 2 and refused at 1, with no partial child set.
#[tokio::test]
async fn max_dynamic_node_instances_ceiling_is_exact_at_expand_map() {
    async fn body<S: WorkflowStore>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _clock: &Arc<TestClock>,
        tenant: &str,
    ) {
        let fixture = seed_map(store, objects, &format!("dynamic-{tenant}"), CostUnits(1)).await;
        let engine = claim_engine(store, &fixture, "dynamic-engine").await;
        let items = objects
            .put(
                &fixture.scope,
                br#"[{"value":1},{"value":2}]"#,
                "application/json",
            )
            .await
            .unwrap();
        let first = objects
            .put(&fixture.scope, br#"{"value":1}"#, "application/json")
            .await
            .unwrap();
        let second = objects
            .put(&fixture.scope, br#"{"value":2}"#, "application/json")
            .await
            .unwrap();

        for (run_id, ceiling, admitted) in [("below", 1_u64, false), ("at", 2_u64, true)] {
            let mut limits = generous_limits();
            limits.max_dynamic_node_instances = ceiling;
            store
                .create_run(
                    &fixture.scope,
                    create_run_command(&fixture, run_id, CostUnits(10), limits),
                )
                .await
                .unwrap();
            start(store, &fixture, &engine, run_id).await;
            let map = store
                .get_node(&fixture.scope, &id(run_id), &id("map"))
                .await
                .unwrap();
            let identities = vec![
                MapChildIdentity {
                    item_index: 0,
                    item_digest: first.digest().clone(),
                    child_id: map_child_id(&id(run_id), &id("map"), 0, first.digest()),
                },
                MapChildIdentity {
                    item_index: 1,
                    item_digest: second.digest().clone(),
                    child_id: map_child_id(&id(run_id), &id("map"), 1, second.digest()),
                },
            ];
            let outcome = store
                .expand_map(
                    &fixture.scope,
                    ExpandMap {
                        permit: engine.permit.clone(),
                        run_id: id(run_id),
                        map_node_id: id("map"),
                        expected_node_version: map.version,
                        input: items.clone(),
                        ordered_items: identities
                            .iter()
                            .map(|identity| OrderedMapItem {
                                index: identity.item_index,
                                item_digest: identity.item_digest.clone(),
                                child_id: identity.child_id.clone(),
                            })
                            .collect(),
                        expansion_digest: map_expansion_digest(&identities),
                    },
                )
                .await;
            if admitted {
                outcome.unwrap();
                assert_eq!(
                    store
                        .get_run(&fixture.scope, &id(run_id))
                        .await
                        .unwrap()
                        .run
                        .dynamic_node_count,
                    2,
                    "{tenant}"
                );
            } else {
                assert_eq!(
                    outcome.err(),
                    Some(StoreError::RunLimitApplied {
                        code: "RunDynamicNodeLimitExceeded".to_owned()
                    }),
                    "{tenant}: a two-child expansion must not fit a one-child ceiling"
                );
                // Refusal is all-or-nothing: no child may survive a rejected
                // expansion, or the Map aggregate would later be short.
                assert_eq!(
                    store
                        .get_node(&fixture.scope, &id(run_id), &identities[0].child_id)
                        .await
                        .err(),
                    Some(StoreError::NotFound),
                    "{tenant}"
                );
            }
        }
    }
    both_stores!(body);
}

/// `max_total_attempts` is refused at the claim that would exceed it.
///
/// Section 1.4 enforces this "before A01". The first attempt of a ceiling-1 run
/// must be admitted and the retry claim refused, and section 3 requires the
/// refusal to be a durably applied contract failure rather than a bare error.
#[tokio::test]
async fn max_total_attempts_ceiling_is_exact_at_claim() {
    async fn body<S: WorkflowStore>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _clock: &Arc<TestClock>,
        tenant: &str,
    ) {
        let fixture = seed_action(
            store,
            objects,
            &format!("attempts-{tenant}"),
            3,
            CostUnits(1),
        )
        .await;
        let engine = claim_engine(store, &fixture, "attempts-engine").await;

        for (run_id, ceiling, second_admitted) in [("below", 1_u64, false), ("at", 2_u64, true)] {
            let mut limits = generous_limits();
            limits.max_total_attempts = ceiling;
            store
                .create_run(
                    &fixture.scope,
                    create_run_command(&fixture, run_id, CostUnits(10), limits),
                )
                .await
                .unwrap();
            start(store, &fixture, &engine, run_id).await;
            let credential = credential_of(
                claim_attempt(
                    store,
                    &fixture,
                    &engine,
                    run_id,
                    &id("action"),
                    "attempt-one",
                    &fixture.input,
                )
                .await
                .unwrap(),
            );
            let retry = store
                .complete_attempt(
                    &fixture.scope,
                    CompleteAttempt {
                        completion_credential: credential,
                        run_id: id(run_id),
                        node_id: id("action"),
                        attempt_id: id("attempt-one"),
                        submitted_outcome: ActionOutcome::Retryable {
                            code: "w10.transient".to_owned(),
                            message: "retry once".to_owned(),
                            diagnostics: None,
                            actual_cost_units: CostUnits(0),
                        },
                        objects: CompletionObjects {
                            output: None,
                            artifacts: Vec::new(),
                            diagnostics: None,
                        },
                    },
                )
                .await
                .unwrap();
            assert!(
                matches!(retry, CompleteAttemptResult::RetryScheduled(_)),
                "{tenant}: retry was not scheduled"
            );
            let waiting = store
                .get_node(&fixture.scope, &id(run_id), &id("action"))
                .await
                .unwrap();
            store
                .release_retry(
                    &fixture.scope,
                    ReleaseRetry {
                        permit: engine.permit.clone(),
                        run_id: id(run_id),
                        node_id: id("action"),
                        expected_node_version: waiting.version,
                    },
                )
                .await
                .unwrap();

            let second = claim_attempt(
                store,
                &fixture,
                &engine,
                run_id,
                &id("action"),
                "attempt-two",
                &fixture.input,
            )
            .await
            .unwrap();
            if second_admitted {
                assert!(
                    matches!(second, ClaimNodeAttemptResult::Claimed { .. }),
                    "{tenant}: a second attempt fits a ceiling of two"
                );
            } else {
                let ClaimNodeAttemptResult::RunLimitApplied(run) = second else {
                    panic!("{tenant}: exceeding the attempt ceiling must apply a run limit");
                };
                assert_eq!(run.status, RunState::ContractFailed, "{tenant}");
                assert_eq!(run.total_attempt_count, 1, "{tenant}");
                assert_eq!(
                    store
                        .get_attempt(&fixture.scope, &id(run_id), &id("attempt-two"))
                        .await
                        .err(),
                    Some(StoreError::NotFound),
                    "{tenant}: the refused claim must not mint an attempt"
                );
            }
        }
    }
    both_stores!(body);
}

/// `max_inline_json_bytes_per_value` is refused at invocation binding.
///
/// Section 1.4 enforces this "before binding, invocation, ...". The bound input
/// is deliberately larger than the run input so the run-creation copy of this
/// check cannot be what fires; the boundary is the bound value's exact size.
///
/// The refusal is N46 (from `Ready`) with R08, not a bare error: section 5.3
/// requires the terminal batch to commit and no attempt to be minted, so the
/// fixture asserts the durable node, run, and attempt state as well as the
/// boundary.
#[tokio::test]
async fn max_inline_json_bytes_per_value_ceiling_is_exact_at_claim() {
    async fn body<S: WorkflowStore>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _clock: &Arc<TestClock>,
        tenant: &str,
    ) {
        let fixture =
            seed_action(store, objects, &format!("inline-{tenant}"), 2, CostUnits(1)).await;
        let engine = claim_engine(store, &fixture, "inline-engine").await;
        let padded = format!(r#"{{"pad":"{}","value":1}}"#, "p".repeat(64));
        let bound = objects
            .put(&fixture.scope, padded.as_bytes(), "application/json")
            .await
            .unwrap();
        let exact = bound.size_bytes();
        assert!(exact > fixture.input.size_bytes(), "{tenant}");

        for (run_id, ceiling, admitted) in [("below", exact - 1, false), ("at", exact, true)] {
            let mut limits = generous_limits();
            limits.max_inline_json_bytes_per_value = ceiling;
            store
                .create_run(
                    &fixture.scope,
                    create_run_command(&fixture, run_id, CostUnits(10), limits),
                )
                .await
                .unwrap();
            start(store, &fixture, &engine, run_id).await;
            let outcome = claim_attempt(
                store,
                &fixture,
                &engine,
                run_id,
                &id("action"),
                &format!("{run_id}-attempt"),
                &bound,
            )
            .await;
            if admitted {
                assert!(
                    matches!(outcome, Ok(ClaimNodeAttemptResult::Claimed { .. })),
                    "{tenant}: a value exactly at the ceiling is admitted"
                );
            } else {
                let Ok(ClaimNodeAttemptResult::RunLimitApplied(run)) = outcome else {
                    panic!("{tenant}: a value one byte over the ceiling must apply a run limit");
                };
                assert_eq!(run.status, RunState::ContractFailed, "{tenant}: R08");
                let node = store
                    .get_node(&fixture.scope, &id(run_id), &id("action"))
                    .await
                    .unwrap();
                assert_eq!(node.status, NodeState::ContractFailed, "{tenant}: N46");
                assert_eq!(
                    node.failure_kind,
                    Some(NodeFailureKind::InlineJsonLimitExceeded),
                    "{tenant}: the applied failure must name the ceiling that fired"
                );
                assert_eq!(
                    store
                        .get_attempt(
                            &fixture.scope,
                            &id(run_id),
                            &id(&format!("{run_id}-attempt"))
                        )
                        .await
                        .err(),
                    Some(StoreError::NotFound),
                    "{tenant}: the refused claim must not mint an attempt"
                );
            }
        }
    }
    both_stores!(body);
}

/// `max_artifacts_per_attempt` is refused at accepted action completion.
///
/// Section 1.4 enforces this "before accepted action completion", so the
/// ceiling is per attempt and counted against the submitted artifact list.
///
/// A breach here is an accepted completion, not a rejected command: transitions
/// A05 and N21 make it a contract failure that is applied, with R08 ending the
/// run. The refused side therefore asserts that terminal state rather than
/// asserting the node stayed put, which would be the pre-repair defect.
#[tokio::test]
async fn max_artifacts_per_attempt_ceiling_is_exact_at_complete_attempt() {
    async fn body<S: WorkflowStore>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _clock: &Arc<TestClock>,
        tenant: &str,
    ) {
        let fixture = seed_action(
            store,
            objects,
            &format!("artifacts-{tenant}"),
            2,
            CostUnits(1),
        )
        .await;
        let engine = claim_engine(store, &fixture, "artifacts-engine").await;
        let artifact = objects
            .put(&fixture.scope, br#"{"value":7}"#, "application/json")
            .await
            .unwrap();

        for (run_id, ceiling, admitted) in [("below", 0_u64, false), ("at", 1_u64, true)] {
            let mut limits = generous_limits();
            limits.max_artifacts_per_attempt = ceiling;
            store
                .create_run(
                    &fixture.scope,
                    create_run_command(&fixture, run_id, CostUnits(10), limits),
                )
                .await
                .unwrap();
            start(store, &fixture, &engine, run_id).await;
            let credential = credential_of(
                claim_attempt(
                    store,
                    &fixture,
                    &engine,
                    run_id,
                    &id("action"),
                    &format!("{run_id}-attempt"),
                    &fixture.input,
                )
                .await
                .unwrap(),
            );
            let outcome = store
                .complete_attempt(
                    &fixture.scope,
                    CompleteAttempt {
                        completion_credential: credential,
                        run_id: id(run_id),
                        node_id: id("action"),
                        attempt_id: id(&format!("{run_id}-attempt")),
                        submitted_outcome: ActionOutcome::Success {
                            output: serde_json::json!({"value": 1}),
                            artifacts: vec![ArtifactOutput {
                                media_type: artifact.media_type().to_owned(),
                                object: artifact.clone(),
                            }],
                            actual_cost_units: CostUnits(1),
                            diagnostics: None,
                        },
                        objects: CompletionObjects {
                            output: Some(fixture.input.clone()),
                            artifacts: vec![artifact.clone()],
                            diagnostics: None,
                        },
                    },
                )
                .await;
            if admitted {
                assert!(
                    matches!(outcome, Ok(CompleteAttemptResult::Applied(_))),
                    "{tenant}: one artifact fits a ceiling of one"
                );
            } else {
                let Ok(CompleteAttemptResult::TerminalRun(run)) = outcome else {
                    panic!("{tenant}: one artifact over the ceiling must terminalize the run");
                };
                assert_eq!(run.status, RunState::ContractFailed, "{tenant}: R08");
                let node = store
                    .get_node(&fixture.scope, &id(run_id), &id("action"))
                    .await
                    .unwrap();
                assert_eq!(node.status, NodeState::ContractFailed, "{tenant}: N21");
                assert_eq!(
                    node.failure_kind,
                    Some(NodeFailureKind::ArtifactsPerAttemptLimitExceeded),
                    "{tenant}: the applied failure must name the ceiling that fired"
                );
                assert_eq!(
                    node.active_attempt_id, None,
                    "{tenant}: N21 clears the active attempt"
                );
                assert_eq!(
                    store
                        .get_attempt(
                            &fixture.scope,
                            &id(run_id),
                            &id(&format!("{run_id}-attempt"))
                        )
                        .await
                        .unwrap()
                        .status,
                    AttemptState::ContractFailed,
                    "{tenant}: A05"
                );
            }
        }
    }
    both_stores!(body);
}

/// `max_aggregate_object_bytes_per_run` is charged cumulatively across commands.
///
/// Section 1.4 enforces this "before every charged run-data ArtifactRef
/// registration" and charges run input and invocation input alike. The exact
/// boundary is therefore the sum, which is what distinguishes a real running
/// total from a per-value check wearing the same name.
#[tokio::test]
async fn max_aggregate_object_bytes_per_run_ceiling_is_exact_at_claim() {
    async fn body<S: WorkflowStore>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _clock: &Arc<TestClock>,
        tenant: &str,
    ) {
        let fixture = seed_action(
            store,
            objects,
            &format!("aggregate-{tenant}"),
            2,
            CostUnits(1),
        )
        .await;
        let engine = claim_engine(store, &fixture, "aggregate-engine").await;
        let bound = objects
            .put(&fixture.scope, br#"{"value":22}"#, "application/json")
            .await
            .unwrap();
        let total = fixture.input.size_bytes() + bound.size_bytes();

        for (run_id, ceiling, admitted) in [("below", total - 1, false), ("at", total, true)] {
            let mut limits = generous_limits();
            limits.max_aggregate_object_bytes_per_run = ceiling;
            store
                .create_run(
                    &fixture.scope,
                    create_run_command(&fixture, run_id, CostUnits(10), limits),
                )
                .await
                .unwrap();
            assert_eq!(
                store
                    .get_run(&fixture.scope, &id(run_id))
                    .await
                    .unwrap()
                    .run
                    .aggregate_object_bytes,
                fixture.input.size_bytes(),
                "{tenant}: run input is charged at creation"
            );
            start(store, &fixture, &engine, run_id).await;
            let outcome = claim_attempt(
                store,
                &fixture,
                &engine,
                run_id,
                &id("action"),
                &format!("{run_id}-attempt"),
                &bound,
            )
            .await;
            if admitted {
                assert!(
                    matches!(outcome, Ok(ClaimNodeAttemptResult::Claimed { .. })),
                    "{tenant}"
                );
                assert_eq!(
                    store
                        .get_run(&fixture.scope, &id(run_id))
                        .await
                        .unwrap()
                        .run
                        .aggregate_object_bytes,
                    total,
                    "{tenant}"
                );
            } else {
                let Ok(ClaimNodeAttemptResult::RunLimitApplied(run)) = outcome else {
                    panic!(
                        "{tenant}: the invocation input must be charged on top of the run input \
                         and applied as a contract failure"
                    );
                };
                assert_eq!(run.status, RunState::ContractFailed, "{tenant}: R08");
                // The refusal is a rejection of the charge, so the running total
                // must still read the creation-time watermark: a ceiling that
                // fired after charging would leave the ledger overstated.
                assert_eq!(
                    run.aggregate_object_bytes,
                    fixture.input.size_bytes(),
                    "{tenant}: the refused invocation input must not be charged"
                );
                let node = store
                    .get_node(&fixture.scope, &id(run_id), &id("action"))
                    .await
                    .unwrap();
                assert_eq!(node.status, NodeState::ContractFailed, "{tenant}: N46");
                assert_eq!(
                    node.failure_kind,
                    Some(NodeFailureKind::AggregateObjectLimitExceeded),
                    "{tenant}: the applied failure must name the ceiling that fired"
                );
                assert_eq!(
                    store
                        .get_attempt(
                            &fixture.scope,
                            &id(run_id),
                            &id(&format!("{run_id}-attempt"))
                        )
                        .await
                        .err(),
                    Some(StoreError::NotFound),
                    "{tenant}: the refused claim must not mint an attempt"
                );
            }
        }
    }
    both_stores!(body);
}

/// `max_run_lifetime_ms` becomes an exact database-clock deadline.
///
/// Section 1.4 stores `created_at + max_run_lifetime_ms` with checked
/// arithmetic and cancels on it. The exactness of the ceiling lives in that
/// stored arithmetic, which is asserted to the millisecond; the deadline itself
/// is then proved to gate the command in both directions. This fixture cannot
/// use the shared macro: the two stores advance time by different mechanisms
/// (the SQLite reducer reads a database clock, not the injected one), and a
/// deadline fixture that quietly ran against wall time would prove nothing.
async fn lifetime_body<S, A, F>(
    store: &S,
    objects: &Arc<InMemoryObjectStore<TestClock>>,
    tenant: &str,
    advance: A,
) where
    S: WorkflowStore,
    A: Fn(i64) -> F,
    F: std::future::Future<Output = ()>,
{
    let fixture = seed_action(
        store,
        objects,
        &format!("lifetime-{tenant}"),
        2,
        CostUnits(1),
    )
    .await;
    let engine = claim_engine(store, &fixture, "lifetime-engine").await;
    let mut limits = generous_limits();
    limits.max_run_lifetime_ms = 5_000;
    store
        .create_run(
            &fixture.scope,
            create_run_command(&fixture, "run", CostUnits(10), limits),
        )
        .await
        .unwrap();
    let run = store.get_run(&fixture.scope, &id("run")).await.unwrap().run;
    assert_eq!(
        run.lifetime_deadline_at.0,
        run.created_at.0 + 5_000,
        "{tenant}: the deadline is exactly created_at plus the ceiling"
    );

    advance(4_000).await;
    let early = store
        .expire_run_lifetime(
            &fixture.scope,
            ExpireRunLifetime {
                permit: engine.permit.clone(),
                run_id: id("run"),
            },
        )
        .await;
    let Err(StoreError::LifetimeNotDue {
        database_now,
        lifetime_deadline_at,
    }) = early
    else {
        panic!("{tenant}: a run before its deadline must not be cancellable");
    };
    assert_eq!(lifetime_deadline_at, run.lifetime_deadline_at, "{tenant}");
    assert!(database_now < lifetime_deadline_at, "{tenant}");
    assert_eq!(
        store
            .get_run(&fixture.scope, &id("run"))
            .await
            .unwrap()
            .run
            .status,
        RunState::Pending,
        "{tenant}: the refused expiry must not transition the run"
    );

    advance(1_000).await;
    let cancelled = store
        .expire_run_lifetime(
            &fixture.scope,
            ExpireRunLifetime {
                permit: engine.permit.clone(),
                run_id: id("run"),
            },
        )
        .await
        .unwrap();
    assert_eq!(cancelled.status, RunState::Cancelled, "{tenant}");
}

#[tokio::test]
async fn max_run_lifetime_ms_ceiling_is_exact_in_memory() {
    let clock = Arc::new(TestClock::new(CLOCK_ORIGIN));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = InMemoryStore::new(clock.clone());
    let ticker = clock.clone();
    lifetime_body(&store, &objects, "memory", move |ms| {
        let ticker = ticker.clone();
        async move {
            ticker.advance_ms(ms).unwrap();
        }
    })
    .await;
}

#[tokio::test]
async fn max_run_lifetime_ms_ceiling_is_exact_in_sqlite() {
    let clock = Arc::new(TestClock::new(CLOCK_ORIGIN));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = SqliteWorkflowStore::open_url("sqlite::memory:", clock.clone(), objects.clone())
        .await
        .unwrap();
    let ticker = &store;
    lifetime_body(&store, &objects, "sqlite", move |ms| async move {
        ticker.advance_database_clock_ms(ms).await.unwrap();
    })
    .await;
}

// ---------------------------------------------------------------------------
// Event ordering, batch identity, correlation.
// ---------------------------------------------------------------------------

/// Drives a full success lifecycle and returns the run's complete event stream.
async fn drive_success<S: WorkflowStore>(
    store: &S,
    fixture: &Fixture,
    engine: &AcquiredEngineClaim,
    run_id: &str,
    attempt_id: &str,
) {
    start(store, fixture, engine, run_id).await;
    let credential = credential_of(
        claim_attempt(
            store,
            fixture,
            engine,
            run_id,
            &id("action"),
            attempt_id,
            &fixture.input,
        )
        .await
        .unwrap(),
    );
    store
        .complete_attempt(
            &fixture.scope,
            CompleteAttempt {
                completion_credential: credential,
                run_id: id(run_id),
                node_id: id("action"),
                attempt_id: id(attempt_id),
                submitted_outcome: ActionOutcome::Success {
                    output: serde_json::json!({"value": 1}),
                    artifacts: Vec::new(),
                    actual_cost_units: CostUnits(1),
                    diagnostics: None,
                },
                objects: CompletionObjects {
                    output: Some(fixture.input.clone()),
                    artifacts: Vec::new(),
                    diagnostics: None,
                },
            },
        )
        .await
        .unwrap();
    let terminal = store
        .get_node(&fixture.scope, &id(run_id), &id("succeed"))
        .await
        .unwrap();
    store
        .resolve_terminal_node(
            &fixture.scope,
            ResolveTerminalNode {
                permit: engine.permit.clone(),
                run_id: id(run_id),
                node_id: id("succeed"),
                expected_node_version: terminal.version,
                output: Some(fixture.input.clone()),
            },
        )
        .await
        .unwrap();
}

/// Asserts the run-local total order and its agreement with the run row.
///
/// Section 1.12 allocates `event_seq` as `last_event_seq + 1` inside the same
/// transaction, so the stream must be dense from 1 with no gap, no repeat, and
/// no event whose scope or run correlation drifts.
fn assert_dense_total_order(stream: &[WorkflowEvent], run_id: &Id, scope: &ExecutionScope) {
    assert!(!stream.is_empty());
    for (index, event) in stream.iter().enumerate() {
        assert_eq!(
            event.event_seq,
            index as u64 + 1,
            "event stream is not dense"
        );
        assert_eq!(&event.run_id, run_id);
        assert_eq!(&event.scope, scope);
    }
}

/// The event stream is a dense total order per run and matches `last_event_seq`.
#[tokio::test]
async fn event_stream_is_a_dense_total_order_per_run() {
    async fn body<S: WorkflowStore>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _clock: &Arc<TestClock>,
        tenant: &str,
    ) {
        let fixture =
            seed_action(store, objects, &format!("order-{tenant}"), 2, CostUnits(1)).await;
        let engine = claim_engine(store, &fixture, "order-engine").await;
        store
            .create_run(
                &fixture.scope,
                create_run_command(&fixture, "run", CostUnits(10), generous_limits()),
            )
            .await
            .unwrap();
        drive_success(store, &fixture, &engine, "run", "attempt-one").await;

        let stream = events(store, &fixture, "run").await;
        assert_dense_total_order(&stream, &id("run"), &fixture.scope);
        let run = store.get_run(&fixture.scope, &id("run")).await.unwrap().run;
        assert_eq!(
            run.last_event_seq,
            stream.len() as u64,
            "{tenant}: the run row must name the last allocated sequence"
        );
        // Ordering is a property of the stream, not of the reader: a second
        // read must return the identical sequence.
        assert_eq!(events(store, &fixture, "run").await, stream, "{tenant}");
    }
    both_stores!(body);
}

/// Batches are atomic, contiguous, self-describing, and lifetime-unique.
///
/// Section 15.1 makes `batch_id` unique for the complete scoped run lifetime,
/// `batch_index` zero-based within the batch, and `batch_count` identical
/// across it. A batch's sequences must also be contiguous, or "atomic batch"
/// would not survive paging.
#[tokio::test]
async fn event_batches_are_contiguous_and_lifetime_unique() {
    async fn body<S: WorkflowStore>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _clock: &Arc<TestClock>,
        tenant: &str,
    ) {
        let fixture =
            seed_action(store, objects, &format!("batch-{tenant}"), 2, CostUnits(1)).await;
        let engine = claim_engine(store, &fixture, "batch-engine").await;
        for run_id in ["run-one", "run-two"] {
            store
                .create_run(
                    &fixture.scope,
                    create_run_command(&fixture, run_id, CostUnits(10), generous_limits()),
                )
                .await
                .unwrap();
            drive_success(
                store,
                &fixture,
                &engine,
                run_id,
                &format!("{run_id}-attempt"),
            )
            .await;
        }

        let mut seen_scope_wide = BTreeSet::new();
        for run_id in ["run-one", "run-two"] {
            let stream = events(store, &fixture, run_id).await;
            let mut grouped: Vec<(Id, Vec<WorkflowEvent>)> = Vec::new();
            for event in stream {
                match grouped.last_mut() {
                    Some((batch, members)) if *batch == event.batch_id => members.push(event),
                    _ => grouped.push((event.batch_id.clone(), vec![event])),
                }
            }
            assert!(grouped.len() > 1, "{tenant}: expected several batches");
            for (batch_id, members) in &grouped {
                assert!(
                    seen_scope_wide.insert(batch_id.clone()),
                    "{tenant}: batch {batch_id:?} was reused within the scope"
                );
                let count = members[0].batch_count;
                assert_eq!(
                    count as usize,
                    members.len(),
                    "{tenant}: batch_count disagrees with the batch that was stored"
                );
                for (index, event) in members.iter().enumerate() {
                    assert_eq!(event.batch_index, index as u32, "{tenant}");
                    assert_eq!(event.batch_count, count, "{tenant}");
                    assert_eq!(
                        event.event_seq,
                        members[0].event_seq + index as u64,
                        "{tenant}: a batch must occupy contiguous sequences"
                    );
                }
            }
        }
    }
    both_stores!(body);
}

/// A requested page boundary inside a batch extends to the batch end.
///
/// Section 5.4 requires the adapter to extend a page rather than emit a partial
/// batch. Requesting one event at a time is the sharpest form of that: every
/// page must still be a whole number of batches, and the concatenation of all
/// pages must reproduce the stream exactly.
#[tokio::test]
async fn event_pages_never_split_an_atomic_batch() {
    async fn body<S: WorkflowStore>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _clock: &Arc<TestClock>,
        tenant: &str,
    ) {
        let fixture =
            seed_action(store, objects, &format!("paging-{tenant}"), 2, CostUnits(1)).await;
        let engine = claim_engine(store, &fixture, "paging-engine").await;
        store
            .create_run(
                &fixture.scope,
                create_run_command(&fixture, "run", CostUnits(10), generous_limits()),
            )
            .await
            .unwrap();
        drive_success(store, &fixture, &engine, "run", "attempt-one").await;
        let whole = events(store, &fixture, "run").await;
        assert!(
            whole.iter().any(|event| event.batch_count > 1),
            "{tenant}: this fixture is vacuous without a multi-event batch"
        );

        let mut cursor = 0_u64;
        let mut paged = Vec::new();
        loop {
            let page = store
                .list_events_after(
                    &fixture.scope,
                    &id("run"),
                    EventPageRequest {
                        after_event_seq: cursor,
                        page_size: 1,
                        hard_response_byte_limit: 8_000_000,
                    },
                )
                .await
                .unwrap();
            if page.is_empty() {
                break;
            }
            let first = &page[0];
            assert_eq!(
                first.batch_index, 0,
                "{tenant}: a page must begin at a batch boundary"
            );
            assert_eq!(
                page.len(),
                first.batch_count as usize,
                "{tenant}: the page cut a batch in half"
            );
            assert!(
                page.iter().all(|event| event.batch_id == first.batch_id),
                "{tenant}"
            );
            cursor = page[page.len() - 1].event_seq;
            paged.extend(page);
        }
        assert_eq!(paged, whole, "{tenant}: paging lost or duplicated events");
    }
    both_stores!(body);
}

/// Run, node, and attempt correlation survives a retry.
///
/// Section 15.1 makes node and attempt correlation part of the envelope. Across
/// two attempts of one node the run and node correlation must be identical and
/// the attempt correlation must partition the attempt-scoped events exactly,
/// otherwise a consumer cannot attribute a failure to the attempt that caused
/// it.
#[tokio::test]
async fn correlation_is_stable_across_a_retry() {
    async fn body<S: WorkflowStore>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _clock: &Arc<TestClock>,
        tenant: &str,
    ) {
        let fixture =
            seed_action(store, objects, &format!("retry-{tenant}"), 3, CostUnits(1)).await;
        let engine = claim_engine(store, &fixture, "retry-engine").await;
        store
            .create_run(
                &fixture.scope,
                create_run_command(&fixture, "run", CostUnits(10), generous_limits()),
            )
            .await
            .unwrap();
        start(store, &fixture, &engine, "run").await;
        let credential = credential_of(
            claim_attempt(
                store,
                &fixture,
                &engine,
                "run",
                &id("action"),
                "attempt-one",
                &fixture.input,
            )
            .await
            .unwrap(),
        );
        store
            .complete_attempt(
                &fixture.scope,
                CompleteAttempt {
                    completion_credential: credential,
                    run_id: id("run"),
                    node_id: id("action"),
                    attempt_id: id("attempt-one"),
                    submitted_outcome: ActionOutcome::Retryable {
                        code: "w10.transient".to_owned(),
                        message: "retry once".to_owned(),
                        diagnostics: None,
                        actual_cost_units: CostUnits(0),
                    },
                    objects: CompletionObjects {
                        output: None,
                        artifacts: Vec::new(),
                        diagnostics: None,
                    },
                },
            )
            .await
            .unwrap();
        let waiting = store
            .get_node(&fixture.scope, &id("run"), &id("action"))
            .await
            .unwrap();
        store
            .release_retry(
                &fixture.scope,
                ReleaseRetry {
                    permit: engine.permit.clone(),
                    run_id: id("run"),
                    node_id: id("action"),
                    expected_node_version: waiting.version,
                },
            )
            .await
            .unwrap();
        let credential = credential_of(
            claim_attempt(
                store,
                &fixture,
                &engine,
                "run",
                &id("action"),
                "attempt-two",
                &fixture.input,
            )
            .await
            .unwrap(),
        );
        store
            .complete_attempt(
                &fixture.scope,
                CompleteAttempt {
                    completion_credential: credential,
                    run_id: id("run"),
                    node_id: id("action"),
                    attempt_id: id("attempt-two"),
                    submitted_outcome: ActionOutcome::Success {
                        output: serde_json::json!({"value": 1}),
                        artifacts: Vec::new(),
                        actual_cost_units: CostUnits(1),
                        diagnostics: None,
                    },
                    objects: CompletionObjects {
                        output: Some(fixture.input.clone()),
                        artifacts: Vec::new(),
                        diagnostics: None,
                    },
                },
            )
            .await
            .unwrap();

        let stream = events(store, &fixture, "run").await;
        assert_dense_total_order(&stream, &id("run"), &fixture.scope);
        let mut per_attempt: BTreeMap<Id, usize> = BTreeMap::new();
        for event in &stream {
            if let Some(attempt) = &event.attempt_id {
                assert_eq!(
                    event.node_instance_id,
                    Some(id("action")),
                    "{tenant}: an attempt-correlated event must name its node"
                );
                *per_attempt.entry(attempt.clone()).or_default() += 1;
            }
        }
        assert!(
            per_attempt.contains_key(&id("attempt-one"))
                && per_attempt.contains_key(&id("attempt-two")),
            "{tenant}: both attempts must appear in the stream, observed {per_attempt:?}"
        );
        assert_eq!(
            per_attempt.len(),
            2,
            "{tenant}: no third attempt identity may appear"
        );
        // The durable rows agree with the correlation the stream published.
        let first = store
            .get_attempt(&fixture.scope, &id("run"), &id("attempt-one"))
            .await
            .unwrap();
        let second = store
            .get_attempt(&fixture.scope, &id("run"), &id("attempt-two"))
            .await
            .unwrap();
        assert_eq!(first.node_instance_id, second.node_instance_id, "{tenant}");
        assert_eq!(
            (first.attempt_number, second.attempt_number),
            (1, 2),
            "{tenant}"
        );
    }
    both_stores!(body);
}

/// Correlation and batch identity survive a full store restart.
///
/// A fresh `SqliteWorkflowStore` over the same file is the closest available
/// analogue of process restart. Section 15.1's "unique for the complete scoped
/// run lifetime" is only meaningful if the batch counter is itself durable, so
/// the post-restart batches must not collide with the pre-restart ones and the
/// pre-restart events must come back byte-identical.
#[tokio::test]
async fn restart_preserves_correlation_and_batch_identity() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("w10-restart.sqlite");
    let clock = Arc::new(TestClock::new(CLOCK_ORIGIN));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));

    // The engine permit and the completion credential are capabilities held by
    // the caller, not store state, so they deliberately outlive the store
    // instance; everything they name has to be re-derived from SQL after the
    // restart.
    let (fixture, engine, credential, before) = {
        let store = SqliteWorkflowStore::open(&path, clock.clone(), objects.clone())
            .await
            .unwrap();
        let fixture = seed_action(&store, &objects, "restart", 2, CostUnits(1)).await;
        let engine = claim_engine(&store, &fixture, "restart-engine").await;
        store
            .create_run(
                &fixture.scope,
                create_run_command(&fixture, "run", CostUnits(10), generous_limits()),
            )
            .await
            .unwrap();
        start(&store, &fixture, &engine, "run").await;
        let credential = credential_of(
            claim_attempt(
                &store,
                &fixture,
                &engine,
                "run",
                &id("action"),
                "attempt-one",
                &fixture.input,
            )
            .await
            .unwrap(),
        );
        let before = events(&store, &fixture, "run").await;
        (fixture, engine, credential, before)
    };

    let store = SqliteWorkflowStore::open(&path, clock.clone(), objects.clone())
        .await
        .unwrap();
    let replayed = events(&store, &fixture, "run").await;
    assert_eq!(
        replayed, before,
        "restart must reproduce the committed stream exactly"
    );
    let attempt = store
        .get_attempt(&fixture.scope, &id("run"), &id("attempt-one"))
        .await
        .unwrap();
    assert_eq!(attempt.node_instance_id, id("action"));

    // Complete the pre-restart attempt against the restarted store. The
    // credential authenticates against durable state only, so acceptance here
    // is itself proof that attempt identity survived the restart.
    store
        .complete_attempt(
            &fixture.scope,
            CompleteAttempt {
                completion_credential: credential,
                run_id: id("run"),
                node_id: id("action"),
                attempt_id: id("attempt-one"),
                submitted_outcome: ActionOutcome::Success {
                    output: serde_json::json!({"value": 1}),
                    artifacts: Vec::new(),
                    actual_cost_units: CostUnits(1),
                    diagnostics: None,
                },
                objects: CompletionObjects {
                    output: Some(fixture.input.clone()),
                    artifacts: Vec::new(),
                    diagnostics: None,
                },
            },
        )
        .await
        .unwrap();
    let terminal = store
        .get_node(&fixture.scope, &id("run"), &id("succeed"))
        .await
        .unwrap();
    store
        .resolve_terminal_node(
            &fixture.scope,
            ResolveTerminalNode {
                permit: engine.permit.clone(),
                run_id: id("run"),
                node_id: id("succeed"),
                expected_node_version: terminal.version,
                output: Some(fixture.input.clone()),
            },
        )
        .await
        .unwrap();
    let after = events(&store, &fixture, "run").await;
    assert!(
        after.len() > before.len(),
        "the restarted store must be able to append"
    );
    assert_dense_total_order(&after, &id("run"), &fixture.scope);
    assert_eq!(&after[..before.len()], &before[..], "history was rewritten");

    let old: BTreeSet<Id> = before.iter().map(|event| event.batch_id.clone()).collect();
    for event in &after[before.len()..] {
        assert!(
            !old.contains(&event.batch_id),
            "a post-restart batch reused the pre-restart batch id {:?}",
            event.batch_id
        );
        assert_eq!(event.run_id, id("run"));
    }
    // The post-restart attempt correlation still names the pre-restart attempt.
    assert!(
        after[before.len()..]
            .iter()
            .any(|event| event.attempt_id == Some(id("attempt-one"))
                && event.node_instance_id == Some(id("action"))),
        "the restarted store lost the attempt correlation"
    );
}

// ---------------------------------------------------------------------------
// Budget: temporary pressure versus permanent infeasibility. Sections 11.1, 3.x.
// ---------------------------------------------------------------------------

/// Expands a two-child Map and returns the two child node ids.
async fn expand_two<S: WorkflowStore>(
    store: &S,
    objects: &Arc<InMemoryObjectStore<TestClock>>,
    fixture: &Fixture,
    engine: &AcquiredEngineClaim,
    run_id: &str,
) -> (NodeInstanceId, NodeInstanceId) {
    let items = objects
        .put(
            &fixture.scope,
            br#"[{"value":1},{"value":2}]"#,
            "application/json",
        )
        .await
        .unwrap();
    let first = objects
        .put(&fixture.scope, br#"{"value":1}"#, "application/json")
        .await
        .unwrap();
    let second = objects
        .put(&fixture.scope, br#"{"value":2}"#, "application/json")
        .await
        .unwrap();
    let identities = vec![
        MapChildIdentity {
            item_index: 0,
            item_digest: first.digest().clone(),
            child_id: map_child_id(&id(run_id), &id("map"), 0, first.digest()),
        },
        MapChildIdentity {
            item_index: 1,
            item_digest: second.digest().clone(),
            child_id: map_child_id(&id(run_id), &id("map"), 1, second.digest()),
        },
    ];
    let map = store
        .get_node(&fixture.scope, &id(run_id), &id("map"))
        .await
        .unwrap();
    store
        .expand_map(
            &fixture.scope,
            ExpandMap {
                permit: engine.permit.clone(),
                run_id: id(run_id),
                map_node_id: id("map"),
                expected_node_version: map.version,
                input: items,
                ordered_items: identities
                    .iter()
                    .map(|identity| OrderedMapItem {
                        index: identity.item_index,
                        item_digest: identity.item_digest.clone(),
                        child_id: identity.child_id.clone(),
                    })
                    .collect(),
                expansion_digest: map_expansion_digest(&identities),
            },
        )
        .await
        .unwrap();
    (
        identities[0].child_id.clone(),
        identities[1].child_id.clone(),
    )
}

/// Reservation-only shortage waits, and settlement revives the waiter.
///
/// and transition N59: when the declared maximum still
/// fits `limit - consumed` but not `limit - consumed - reserved`, the node is
/// suspended by accounting and the run stays Running. Once the live reservation
/// settles below its reservation, the same node must claim normally. Proving
/// the resume is what separates a temporary wait from a disguised terminal.
#[tokio::test]
async fn reservation_pressure_waits_and_resumes_after_settlement() {
    async fn body<S: WorkflowStore>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _clock: &Arc<TestClock>,
        tenant: &str,
    ) {
        let fixture = seed_map(store, objects, &format!("waiting-{tenant}"), CostUnits(1)).await;
        let engine = claim_engine(store, &fixture, "waiting-engine").await;
        store
            .create_run(
                &fixture.scope,
                // A budget of exactly one unit: two concurrent children of cost
                // one can never both be reserved, but either alone always fits.
                create_run_command(&fixture, "run", CostUnits(1), generous_limits()),
            )
            .await
            .unwrap();
        start(store, &fixture, &engine, "run").await;
        let (left, right) = expand_two(store, objects, &fixture, &engine, "run").await;

        let credential = credential_of(
            claim_attempt(
                store,
                &fixture,
                &engine,
                "run",
                &left,
                "left-attempt",
                &fixture.input,
            )
            .await
            .unwrap(),
        );
        let run = store.get_run(&fixture.scope, &id("run")).await.unwrap().run;
        assert_eq!(run.budget_reserved, CostUnits(1), "{tenant}");
        assert_eq!(run.budget_consumed, CostUnits(0), "{tenant}");

        let blocked = claim_attempt(
            store,
            &fixture,
            &engine,
            "run",
            &right,
            "right-attempt",
            &fixture.input,
        )
        .await
        .unwrap();
        let ClaimNodeAttemptResult::BudgetWaitingApplied(node) = blocked else {
            panic!(
                "{tenant}: reservation-only shortage must persist BudgetWaiting, got {}",
                describe(&blocked)
            );
        };
        assert_eq!(node.status, NodeState::BudgetWaiting, "{tenant}");
        assert_eq!(node.budget_wait_amount, Some(CostUnits(1)), "{tenant}");
        assert_eq!(
            store
                .get_run(&fixture.scope, &id("run"))
                .await
                .unwrap()
                .run
                .status,
            RunState::Running,
            "{tenant}: a waiting node must not disturb the run"
        );
        assert_eq!(
            store
                .get_attempt(&fixture.scope, &id("run"), &id("right-attempt"))
                .await
                .err(),
            Some(StoreError::NotFound),
            "{tenant}: a refused reservation must not mint an attempt"
        );

        // Settle the live reservation at zero cost: the reservation is released
        // and nothing is consumed, so the waiter becomes feasible again.
        store
            .complete_attempt(
                &fixture.scope,
                CompleteAttempt {
                    completion_credential: credential,
                    run_id: id("run"),
                    node_id: left.clone(),
                    attempt_id: id("left-attempt"),
                    submitted_outcome: ActionOutcome::Success {
                        output: serde_json::json!({"value": 1}),
                        artifacts: Vec::new(),
                        actual_cost_units: CostUnits(0),
                        diagnostics: None,
                    },
                    objects: CompletionObjects {
                        output: Some(fixture.input.clone()),
                        artifacts: Vec::new(),
                        diagnostics: None,
                    },
                },
            )
            .await
            .unwrap();
        let settled = store.get_run(&fixture.scope, &id("run")).await.unwrap().run;
        assert_eq!(settled.budget_reserved, CostUnits(0), "{tenant}");
        assert_eq!(settled.budget_consumed, CostUnits(0), "{tenant}");

        let resumed = claim_attempt(
            store,
            &fixture,
            &engine,
            "run",
            &right,
            "right-attempt",
            &fixture.input,
        )
        .await
        .unwrap();
        assert!(
            matches!(resumed, ClaimNodeAttemptResult::Claimed { .. }),
            "{tenant}: a temporary budget wait must resume after settlement, got {}",
            describe(&resumed)
        );
        assert_eq!(
            store
                .get_node(&fixture.scope, &id("run"), &right)
                .await
                .unwrap()
                .budget_wait_amount,
            None,
            "{tenant}: the wait amount must clear on claim"
        );
    }
    both_stores!(body);
}

/// Permanent infeasibility terminalizes and cannot be revived.
///
/// `BudgetExhausted` is terminal subject only to
/// integrity override. The distinguishing input is consumption, not
/// reservation: once `limit - consumed` is smaller than the declared maximum no
/// settlement can ever make the node feasible, so the same claim that produced
/// a temporary wait above must instead end the run here.
#[tokio::test]
async fn permanent_infeasibility_exhausts_and_cannot_revive() {
    async fn body<S: WorkflowStore>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _clock: &Arc<TestClock>,
        tenant: &str,
    ) {
        let fixture = seed_map(store, objects, &format!("exhausted-{tenant}"), CostUnits(1)).await;
        let engine = claim_engine(store, &fixture, "exhausted-engine").await;
        store
            .create_run(
                &fixture.scope,
                create_run_command(&fixture, "run", CostUnits(1), generous_limits()),
            )
            .await
            .unwrap();
        start(store, &fixture, &engine, "run").await;
        let (left, right) = expand_two(store, objects, &fixture, &engine, "run").await;

        let credential = credential_of(
            claim_attempt(
                store,
                &fixture,
                &engine,
                "run",
                &left,
                "left-attempt",
                &fixture.input,
            )
            .await
            .unwrap(),
        );
        // Consume the whole budget rather than releasing it.
        store
            .complete_attempt(
                &fixture.scope,
                CompleteAttempt {
                    completion_credential: credential,
                    run_id: id("run"),
                    node_id: left.clone(),
                    attempt_id: id("left-attempt"),
                    submitted_outcome: ActionOutcome::Success {
                        output: serde_json::json!({"value": 1}),
                        artifacts: Vec::new(),
                        actual_cost_units: CostUnits(1),
                        diagnostics: None,
                    },
                    objects: CompletionObjects {
                        output: Some(fixture.input.clone()),
                        artifacts: Vec::new(),
                        diagnostics: None,
                    },
                },
            )
            .await
            .unwrap();
        let spent = store.get_run(&fixture.scope, &id("run")).await.unwrap().run;
        assert_eq!(spent.budget_consumed, CostUnits(1), "{tenant}");
        assert_eq!(spent.budget_reserved, CostUnits(0), "{tenant}");

        let refused = claim_attempt(
            store,
            &fixture,
            &engine,
            "run",
            &right,
            "right-attempt",
            &fixture.input,
        )
        .await
        .unwrap();
        let ClaimNodeAttemptResult::BudgetExhaustedApplied(run) = refused else {
            panic!(
                "{tenant}: consumed-budget infeasibility must exhaust, got {}",
                describe(&refused)
            );
        };
        assert_eq!(run.status, RunState::BudgetExhausted, "{tenant}");
        assert_eq!(
            store
                .get_node(&fixture.scope, &id("run"), &right)
                .await
                .unwrap()
                .status,
            NodeState::BudgetExhausted,
            "{tenant}"
        );

        // No later command may resurrect the run: this is the property that
        // distinguishes N27/N60 from the N59 wait proved above.
        let revived = claim_attempt(
            store,
            &fixture,
            &engine,
            "run",
            &right,
            "revival-attempt",
            &fixture.input,
        )
        .await;
        assert!(
            revived.is_err(),
            "{tenant}: a BudgetExhausted run must not admit another claim, got {}",
            revived.map(|value| describe(&value)).unwrap_or("error")
        );
        assert_eq!(
            store
                .get_run(&fixture.scope, &id("run"))
                .await
                .unwrap()
                .run
                .status,
            RunState::BudgetExhausted,
            "{tenant}"
        );
    }
    both_stores!(body);
}

// ---------------------------------------------------------------------------
// Budget ledger durability.
// ---------------------------------------------------------------------------

/// The in-memory store's ledger records the reservation and its settlement.
///
/// Section 1.15 makes each entry carry its own attempt and node correlation, so
/// a ledger that only tracked run totals would be indistinguishable from the
/// run row and could not attribute spend.
#[tokio::test]
async fn memory_budget_ledger_correlates_reservation_and_settlement() {
    let clock = Arc::new(TestClock::new(CLOCK_ORIGIN));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = InMemoryStore::new(clock.clone());
    let fixture = seed_action(&store, &objects, "ledger", 2, CostUnits(3)).await;
    let engine = claim_engine(&store, &fixture, "ledger-engine").await;
    store
        .create_run(
            &fixture.scope,
            create_run_command(&fixture, "run", CostUnits(10), generous_limits()),
        )
        .await
        .unwrap();
    drive_success(&store, &fixture, &engine, "run", "attempt-one").await;

    let ledger = store.budget_ledger(&fixture.scope, &id("run"));
    assert_eq!(ledger.len(), 2, "one reservation and one settlement");
    for (index, entry) in ledger.iter().enumerate() {
        assert_eq!(entry.ledger_seq, index as u64 + 1);
        assert_eq!(entry.run_id, id("run"));
        assert_eq!(entry.attempt_id, id("attempt-one"));
        assert_eq!(entry.node_instance_id, id("action"));
        assert_eq!(entry.reservation_amount, CostUnits(3));
    }
    assert_eq!(ledger[0].reserved_delta, 3);
    assert_eq!(ledger[0].consumed_delta, CostUnits(0));
    assert_eq!(ledger[1].reserved_delta, -3);
    assert_eq!(ledger[1].consumed_delta, CostUnits(1));
    let run = store.get_run(&fixture.scope, &id("run")).await.unwrap().run;
    assert_eq!(run.budget_consumed, CostUnits(1));
    assert_eq!(run.budget_reserved, CostUnits(0));
}

/// The SQLite store persists the same ledger in its own table.
///
/// There is no ledger read method on `WorkflowStore`, so durability is asserted
/// against SQL directly. Without this, "the ledger exists in both stores" would
/// rest entirely on the in-memory implementation.
#[tokio::test]
async fn sqlite_budget_ledger_is_durable_and_correlated() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("w10-ledger.sqlite");
    let clock = Arc::new(TestClock::new(CLOCK_ORIGIN));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let workflow_scope = {
        let store = SqliteWorkflowStore::open(&path, clock.clone(), objects.clone())
            .await
            .unwrap();
        let fixture = seed_action(&store, &objects, "sql-ledger", 2, CostUnits(3)).await;
        let engine = claim_engine(&store, &fixture, "sql-ledger-engine").await;
        store
            .create_run(
                &fixture.scope,
                create_run_command(&fixture, "run", CostUnits(10), generous_limits()),
            )
            .await
            .unwrap();
        drive_success(&store, &fixture, &engine, "run", "attempt-one").await;
        fixture.scope.clone()
    };

    let store = SqliteWorkflowStore::open(&path, clock.clone(), objects.clone())
        .await
        .unwrap();
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT ledger_seq, attempt_id, node_id FROM dagger_workflow_budget_ledger
         WHERE tenant_id = ? AND namespace = ? AND run_id = ?
         ORDER BY ledger_seq",
    )
    .bind(workflow_scope.tenant_id.as_str())
    .bind(workflow_scope.namespace.as_str())
    .bind("run")
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert_eq!(
        rows.len(),
        2,
        "reservation and settlement must both survive"
    );
    assert_eq!(rows[0].0, 1);
    assert_eq!(rows[1].0, 2);
    for row in &rows {
        assert_eq!(row.1, "attempt-one");
        assert_eq!(row.2, "action");
    }
    let run = store
        .get_run(&workflow_scope, &id("run"))
        .await
        .unwrap()
        .run;
    assert_eq!(run.budget_consumed, CostUnits(1));
    assert_eq!(run.budget_reserved, CostUnits(0));
}
