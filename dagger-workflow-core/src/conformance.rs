//! Adapter-neutral black-box checks shared by volatile and durable stores.

use crate::action::{ActionOutcome, CompatibilityReport};
use crate::approval::{
    canonical_human_approval_result, ApprovalDecision, ApprovalExpiryPolicy,
    AuthenticatedPrincipal, DecisionAuthorizationPolicy,
};
use crate::artifact::{ObjectStore, ObjectStoreError, VerifiedObjectRef};
use crate::definition::{
    ActionReference, ApprovalGateConfig, BackoffPolicy, Binding, BindingSource, NodeDefinition,
    PublishableDefinition, RetryPolicy, TimeoutPolicy, WorkflowDefinition,
};
use crate::ids::{
    map_child_id, map_expansion_digest, CostUnits, Digest, Id, MapChildIdentity, TopologicalRank,
    Version,
};
use crate::run::{AttemptState, NodeState, RunLimits, RunState};
use crate::scope::ExecutionScope;
use crate::store::{
    ClaimNodeAttempt, ClaimNodeAttemptResult, CompleteAttempt, CompleteAttemptResult,
    CompletionObjects, CreateDefinition, CreateRun, DecideApproval, EnginePermit, EventPageRequest,
    ExpandMap, OrderedMapItem, PageRequest, PublishRevision, ReleaseRetry, RequestApproval,
    ResolveTerminalNode, ResolvedActionSchemas, StartRun, StoreError, SuspendIncompatible,
    WorkflowStore,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Number of independent adapter-neutral cases.
pub const CASE_COUNT: usize = 40;

/// Stable case names reported by every adapter.
pub const CASE_NAMES: [&str; CASE_COUNT] = [
    "publish_scope_a",
    "publish_scope_b",
    "equal_bytes_equal_digest",
    "verified_ref_scope_binding",
    "same_scope_publish_replay",
    "verified_read_scope_a",
    "verified_read_exact_bytes",
    "verified_read_scope_b",
    "missing_read_proof",
    "first_engine_claim",
    "frozen_control_plane_id",
    "live_peer_rejected",
    "claim_scope_locality",
    "expired_takeover",
    "takeover_generation_checked_increment",
    "stale_permit_rejected",
    "create_definition",
    "publish_revision",
    "create_run_receipt",
    "create_receipt_replay",
    "create_receipt_conflict",
    "point_read_scope_isolation",
    "start_run",
    "node_version_cas_rejection",
    "claim_attempt",
    "active_attempt_fence",
    "budget_reservation",
    "complete_attempt",
    "attempt_terminal_state",
    "budget_settlement",
    "event_sequence_contiguity",
    "batch_metadata_contiguity",
    "closed_edge_payload",
    "terminal_resolution",
    "due_completion_times_out",
    "timeout_observation_order",
    "blocked_run_host_command_fence",
    "expand_map_recomputes_identity",
    "map_concurrency_admission",
    "exponential_backoff_cap",
];

/// Supplies one isolated store pair and control over its database clock.
pub trait ConformanceAdapter {
    /// Workflow store implementation under test.
    type Store: WorkflowStore;
    /// Object store implementation under test.
    type Objects: ObjectStore;

    /// Returns the workflow store.
    fn store(&self) -> &Self::Store;
    /// Returns the object store.
    fn objects(&self) -> &Self::Objects;
    /// Advances the adapter's database-equivalent clock.
    fn advance_clock_ms(&self, milliseconds: i64);
    /// Constructs a completely fresh store/object-store pair.
    fn fresh(&self) -> Self
    where
        Self: Sized;
}

/// A conformance-suite failure naming the first violated case.
#[derive(Debug, thiserror::Error)]
#[error("conformance case {case} failed: {detail}")]
pub struct ConformanceFailure {
    /// Stable one-based case number.
    pub case: usize,
    /// Compact failure detail.
    pub detail: &'static str,
}

/// Runs the adapter-neutral object and singleton-claim cases.
///
/// Runtime command cases are intentionally exercised by adapter-specific
/// fixtures built through the same public `WorkflowStore` trait; this entry
/// point covers the prerequisites that require no published workflow fixture.
async fn run_original_case<A: ConformanceAdapter>(
    adapter: &A,
    scope_a: &ExecutionScope,
    scope_b: &ExecutionScope,
    case: usize,
) -> Result<(), ConformanceFailure> {
    let bytes = br#"{"same":true}"#;
    let a = adapter
        .objects()
        .put(scope_a, bytes, "application/json")
        .await
        .map_err(|_| failure(1, "object publication failed"))?;
    if case == 1 {
        return Ok(());
    }
    let b = adapter
        .objects()
        .put(scope_b, bytes, "application/json")
        .await
        .map_err(|_| failure(2, "second-scope publication failed"))?;
    if case == 2 {
        return Ok(());
    }
    if case == 3 {
        if a.digest() != b.digest() {
            return Err(failure(3, "equal bytes did not share a digest"));
        }
        return Ok(());
    }
    if case == 4 {
        if a.scope() == b.scope() {
            return Err(failure(4, "verified refs were not scope-bound"));
        }
        return Ok(());
    }
    let replay = adapter
        .objects()
        .publish_if_absent(scope_a, bytes, "application/json")
        .await
        .map_err(|_| failure(5, "same-scope replay failed"))?;
    if case == 5 {
        if replay != a {
            return Err(failure(5, "same-scope replay changed the capability"));
        }
        return Ok(());
    }
    let read_a = adapter
        .objects()
        .get(scope_a, a.digest())
        .await
        .map_err(|_| failure(6, "verified read failed"))?;
    if case == 6 {
        return Ok(());
    }
    if case == 7 {
        if read_a.bytes != bytes {
            return Err(failure(7, "verified read changed bytes"));
        }
        return Ok(());
    }
    if case == 8 {
        if adapter.objects().get(scope_b, b.digest()).await.is_err() {
            return Err(failure(8, "scope-b verified read failed"));
        }
        return Ok(());
    }
    let missing = Digest::new(format!("sha256:{}", "0".repeat(64))).expect("fixture digest");
    if case == 9 {
        if adapter.objects().get(scope_a, &missing).await.is_ok() {
            return Err(failure(9, "missing read did not mint a failure"));
        }
        return Ok(());
    }
    let first = adapter
        .store()
        .acquire_engine_claim(scope_a, id("engine-a"))
        .await
        .map_err(|_| failure(10, "first engine claim failed"))?;
    if case == 10 {
        return Ok(());
    }
    if case == 11 {
        if first.claim.control_plane_id != "scheduler" {
            return Err(failure(11, "control-plane ID was not frozen scheduler"));
        }
        return Ok(());
    }
    if case == 12 {
        if !matches!(
            adapter
                .store()
                .acquire_engine_claim(scope_a, id("engine-b"))
                .await,
            Err(StoreError::EngineAlreadyLive { .. })
        ) {
            return Err(failure(12, "second live engine was accepted"));
        }
        return Ok(());
    }
    adapter
        .store()
        .acquire_engine_claim(scope_b, id("engine-b"))
        .await
        .map_err(|_| failure(13, "claim leaked across scopes"))?;
    if case == 13 {
        return Ok(());
    }
    adapter.advance_clock_ms(20_000);
    let takeover = adapter
        .store()
        .acquire_engine_claim(scope_a, id("engine-c"))
        .await
        .map_err(|_| failure(14, "expired takeover failed"))?;
    if case == 14 {
        return Ok(());
    }
    if case == 15 {
        if takeover.claim.generation != first.claim.generation + 1 {
            return Err(failure(15, "takeover generation did not increment"));
        }
        return Ok(());
    }
    if case == 16 {
        if !matches!(
            adapter
                .store()
                .heartbeat_engine_claim(scope_a, &first.permit)
                .await,
            Err(StoreError::EngineClaimLost)
        ) {
            return Err(failure(16, "stale permit survived takeover"));
        }
        return Ok(());
    }

    let schema = adapter
        .objects()
        .put(scope_a, b"{}", "application/json")
        .await
        .map_err(|_| failure(17, "schema publication failed"))?;
    let principal = AuthenticatedPrincipal::mint(
        scope_a.clone(),
        "conformance".to_owned(),
        Vec::new(),
        schema.digest().clone(),
    )
    .map_err(|_| failure(17, "principal construction failed"))?;
    adapter
        .store()
        .create_definition(
            scope_a,
            CreateDefinition {
                definition_id: id("conformance-definition"),
                display_name: "conformance".to_owned(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .map_err(|_| failure(17, "definition creation failed"))?;
    if case == 17 {
        return Ok(());
    }
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: id("conformance-definition"),
        name: "conformance".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.digest().clone(),
        run_output_schema_digest: schema.digest().clone(),
        entry_node_id: id("action"),
        nodes: vec![
            NodeDefinition::Action {
                id: id("action"),
                action: ActionReference {
                    name: "conformance.action".to_owned(),
                    contract_version: "1".to_owned(),
                    input_schema_digest: schema.digest().clone(),
                    output_schema_digest: schema.digest().clone(),
                    compatible_implementation_requirement: schema.digest().clone(),
                },
                bindings: vec![Binding {
                    target: String::new(),
                    source: BindingSource::RunInput {
                        pointer: String::new(),
                    },
                }],
                retry: RetryPolicy {
                    max_attempts: 1,
                    backoff: BackoffPolicy::Fixed { delay_ms: 0 },
                },
                timeout: TimeoutPolicy { timeout_ms: 1_000 },
                declared_max_cost_units: CostUnits(2),
                next: vec![id("succeed")],
            },
            NodeDefinition::Succeed {
                id: id("succeed"),
                output: BindingSource::NodeOutput {
                    node_id: id("action"),
                    pointer: String::new(),
                },
            },
        ],
    };
    let canonical = adapter
        .objects()
        .put(
            scope_a,
            &serde_jcs::to_vec(&definition)
                .map_err(|_| failure(18, "definition encoding failed"))?,
            "application/json",
        )
        .await
        .map_err(|_| failure(18, "definition object publication failed"))?;
    let mut ranks = BTreeMap::new();
    ranks.insert(id("action"), TopologicalRank(0));
    ranks.insert(id("succeed"), TopologicalRank(1));
    let mut schemas = BTreeMap::new();
    schemas.insert(
        "action".to_owned(),
        ResolvedActionSchemas {
            input_schema: schema.clone(),
            output_schema: schema.clone(),
        },
    );
    adapter
        .store()
        .publish_revision(
            scope_a,
            PublishRevision {
                definition_id: id("conformance-definition"),
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema.clone(),
                resolved_action_schema_objects: schemas,
                parsed_revision: PublishableDefinition {
                    definition,
                    topological_ranks: ranks,
                },
                principal: principal.clone(),
            },
        )
        .await
        .map_err(|_| failure(18, "revision publication failed"))?;
    if case == 18 {
        return Ok(());
    }
    let input = adapter
        .objects()
        .put(scope_a, br#"{"value":1}"#, "application/json")
        .await
        .map_err(|_| failure(19, "run input publication failed"))?;
    let limits = RunLimits {
        max_dynamic_node_instances: 10,
        max_total_attempts: 10,
        max_total_events: 1_000,
        max_inline_json_bytes_per_value: 10_000,
        max_artifacts_per_attempt: 10,
        max_aggregate_object_bytes_per_run: 100_000,
        max_run_lifetime_ms: 100_000,
    };
    let create = |budget_limit| CreateRun {
        run_id: id("conformance-run"),
        definition_id: id("conformance-definition"),
        revision_hash: canonical.digest().clone(),
        input: input.clone(),
        budget_limit,
        limits: limits.clone(),
        principal: principal.clone(),
        idempotency_token: "conformance-create-token".to_owned(),
    };
    let receipt = adapter
        .store()
        .create_run(scope_a, create(CostUnits(10)))
        .await
        .map_err(|_| failure(19, "run creation failed"))?;
    if case == 19 {
        return Ok(());
    }
    let replay_receipt = adapter
        .store()
        .create_run(scope_a, create(CostUnits(10)))
        .await
        .map_err(|_| failure(20, "create replay failed"))?;
    if case == 20 {
        if replay_receipt != receipt {
            return Err(failure(20, "create replay changed its receipt"));
        }
        return Ok(());
    }
    if case == 21 {
        if !matches!(
            adapter
                .store()
                .create_run(scope_a, create(CostUnits(11)))
                .await,
            Err(StoreError::IdempotencyConflict)
        ) {
            return Err(failure(21, "conflicting create replay was accepted"));
        }
        return Ok(());
    }
    if case == 22 {
        if !matches!(
            adapter
                .store()
                .get_run(scope_b, &id("conformance-run"))
                .await,
            Err(StoreError::NotFound)
        ) {
            return Err(failure(22, "run point read crossed scope"));
        }
        return Ok(());
    }
    adapter
        .store()
        .start_run(
            scope_a,
            StartRun {
                permit: takeover.permit.clone(),
                run_id: id("conformance-run"),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: schema.digest().clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .map_err(|_| failure(23, "run start failed"))?;
    if case == 23 {
        return Ok(());
    }
    let node = adapter
        .store()
        .get_node(scope_a, &id("conformance-run"), &id("action"))
        .await
        .map_err(|_| failure(23, "entry node read failed"))?;
    if case == 24 {
        if !matches!(
            adapter
                .store()
                .claim_node_attempt(
                    scope_a,
                    ClaimNodeAttempt {
                        permit: takeover.permit.clone(),
                        run_id: id("conformance-run"),
                        node_id: id("action"),
                        expected_node_version: Version(u64::MAX),
                        attempt_id: id("cas-rejected"),
                        worker_id: id("worker"),
                        bound_input: input.clone(),
                        binding_derivation_digest: schema.digest().clone(),
                    },
                )
                .await,
            Err(StoreError::CasConflict)
        ) {
            return Err(failure(24, "stale node version was accepted"));
        }
        return Ok(());
    }
    let claimed = adapter
        .store()
        .claim_node_attempt(
            scope_a,
            ClaimNodeAttempt {
                permit: takeover.permit.clone(),
                run_id: id("conformance-run"),
                node_id: id("action"),
                expected_node_version: node.version,
                attempt_id: id("attempt"),
                worker_id: id("worker"),
                bound_input: input.clone(),
                binding_derivation_digest: schema.digest().clone(),
            },
        )
        .await
        .map_err(|_| failure(25, "attempt claim failed"))?;
    let (credential, _invocation) = match claimed {
        ClaimNodeAttemptResult::Claimed {
            completion_credential,
            invocation,
        } => (completion_credential, invocation),
        _ => return Err(failure(25, "claim did not create an attempt")),
    };
    let running_node = adapter
        .store()
        .get_node(scope_a, &id("conformance-run"), &id("action"))
        .await
        .map_err(|_| failure(25, "claimed node read failed"))?;
    if case == 25 {
        if running_node.status != NodeState::Running {
            return Err(failure(25, "claimed node was not Running"));
        }
        return Ok(());
    }
    if case == 26 {
        if !matches!(
            adapter
                .store()
                .claim_node_attempt(
                    scope_a,
                    ClaimNodeAttempt {
                        permit: takeover.permit.clone(),
                        run_id: id("conformance-run"),
                        node_id: id("action"),
                        expected_node_version: running_node.version,
                        attempt_id: id("active-fenced"),
                        worker_id: id("worker"),
                        bound_input: input.clone(),
                        binding_derivation_digest: schema.digest().clone(),
                    },
                )
                .await,
            Err(StoreError::CasConflict)
        ) {
            return Err(failure(26, "active attempt fence was bypassed"));
        }
        return Ok(());
    }
    let reserved = adapter
        .store()
        .get_run(scope_a, &id("conformance-run"))
        .await
        .map_err(|_| failure(27, "reserved run read failed"))?
        .run;
    if case == 27 {
        if reserved.budget_reserved != CostUnits(2) || reserved.budget_consumed != CostUnits(0) {
            return Err(failure(27, "reservation totals were wrong"));
        }
        return Ok(());
    }
    let output = adapter
        .objects()
        .put(scope_a, br#"{"value":1}"#, "application/json")
        .await
        .map_err(|_| failure(28, "completion output publication failed"))?;
    adapter
        .store()
        .complete_attempt(
            scope_a,
            CompleteAttempt {
                completion_credential: credential,
                run_id: id("conformance-run"),
                node_id: id("action"),
                attempt_id: id("attempt"),
                submitted_outcome: ActionOutcome::success(
                    json!({"value": 1}),
                    Vec::new(),
                    CostUnits(1),
                    None,
                )
                .map_err(|_| failure(28, "completion outcome invalid"))?,
                objects: CompletionObjects {
                    output: Some(output.clone()),
                    artifacts: Vec::new(),
                    diagnostics: None,
                },
            },
        )
        .await
        .map_err(|_| failure(28, "attempt completion failed"))?;
    if case == 28 {
        return Ok(());
    }
    let attempt = adapter
        .store()
        .get_attempt(scope_a, &id("conformance-run"), &id("attempt"))
        .await
        .map_err(|_| failure(29, "terminal attempt read failed"))?;
    if case == 29 {
        if attempt.status != AttemptState::Succeeded {
            return Err(failure(29, "attempt was not terminal Succeeded"));
        }
        return Ok(());
    }
    let settled = adapter
        .store()
        .get_run(scope_a, &id("conformance-run"))
        .await
        .map_err(|_| failure(30, "settled run read failed"))?
        .run;
    if case == 30 {
        if settled.budget_reserved != CostUnits(0) || settled.budget_consumed != CostUnits(1) {
            return Err(failure(30, "settlement totals were wrong"));
        }
        return Ok(());
    }
    let events = adapter
        .store()
        .list_events_after(
            scope_a,
            &id("conformance-run"),
            EventPageRequest {
                after_event_seq: 0,
                page_size: 1_000,
                hard_response_byte_limit: 1_000_000,
            },
        )
        .await
        .map_err(|_| failure(31, "event read failed"))?;
    if case == 31 {
        if !events
            .iter()
            .enumerate()
            .all(|(index, event)| event.event_seq == index as u64 + 1)
        {
            return Err(failure(31, "event sequence was not contiguous"));
        }
        return Ok(());
    }
    if case == 32 {
        if events.iter().any(|event| {
            event.batch_index >= event.batch_count
                || events
                    .iter()
                    .filter(|candidate| candidate.batch_id == event.batch_id)
                    .count()
                    != event.batch_count as usize
        }) {
            return Err(failure(32, "batch metadata was not contiguous"));
        }
        return Ok(());
    }
    if case == 33 {
        if events.iter().any(|event| {
            event.event_type == crate::event::EventType::EdgeSatisfied
                && event.payload.get("cause").is_some()
        }) {
            return Err(failure(33, "EdgeSatisfied carried a forbidden cause"));
        }
        return Ok(());
    }
    let terminal = adapter
        .store()
        .get_node(scope_a, &id("conformance-run"), &id("succeed"))
        .await
        .map_err(|_| failure(34, "terminal node read failed"))?;
    adapter
        .store()
        .resolve_terminal_node(
            scope_a,
            ResolveTerminalNode {
                permit: takeover.permit.clone(),
                run_id: id("conformance-run"),
                node_id: id("succeed"),
                expected_node_version: terminal.version,
                output: Some(output),
            },
        )
        .await
        .map_err(|_| failure(34, "terminal resolution failed"))?;
    if case == 34 {
        if adapter
            .store()
            .get_run(scope_a, &id("conformance-run"))
            .await
            .map_err(|_| failure(34, "terminal run read failed"))?
            .run
            .status
            != RunState::Succeeded
        {
            return Err(failure(34, "run did not become Succeeded"));
        }
        return Ok(());
    }
    adapter
        .store()
        .create_run(
            scope_a,
            CreateRun {
                run_id: id("due-run"),
                definition_id: id("conformance-definition"),
                revision_hash: canonical.digest().clone(),
                input: input.clone(),
                budget_limit: CostUnits(10),
                limits,
                principal,
                idempotency_token: "due-create-token".to_owned(),
            },
        )
        .await
        .map_err(|_| failure(35, "due run creation failed"))?;
    adapter
        .store()
        .start_run(
            scope_a,
            StartRun {
                permit: takeover.permit.clone(),
                run_id: id("due-run"),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: schema.digest().clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .map_err(|_| failure(35, "due run start failed"))?;
    let due_node = adapter
        .store()
        .get_node(scope_a, &id("due-run"), &id("action"))
        .await
        .map_err(|_| failure(35, "due node read failed"))?;
    let due_claim = adapter
        .store()
        .claim_node_attempt(
            scope_a,
            ClaimNodeAttempt {
                permit: takeover.permit,
                run_id: id("due-run"),
                node_id: id("action"),
                expected_node_version: due_node.version,
                attempt_id: id("due-attempt"),
                worker_id: id("worker"),
                bound_input: input,
                binding_derivation_digest: schema.digest().clone(),
            },
        )
        .await
        .map_err(|_| failure(35, "due attempt claim failed"))?;
    let due_credential = match due_claim {
        ClaimNodeAttemptResult::Claimed {
            completion_credential,
            ..
        } => completion_credential,
        _ => return Err(failure(35, "due attempt was not claimed")),
    };
    adapter.advance_clock_ms(1_000);
    let due_output = adapter
        .objects()
        .put(scope_a, br#"{"value":1}"#, "application/json")
        .await
        .map_err(|_| failure(35, "due output publication failed"))?;
    let due_result = adapter
        .store()
        .complete_attempt(
            scope_a,
            CompleteAttempt {
                completion_credential: due_credential,
                run_id: id("due-run"),
                node_id: id("action"),
                attempt_id: id("due-attempt"),
                submitted_outcome: ActionOutcome::success(
                    json!({"value": 1}),
                    Vec::new(),
                    CostUnits(1),
                    None,
                )
                .map_err(|_| failure(35, "due outcome invalid"))?,
                objects: CompletionObjects {
                    output: Some(due_output),
                    artifacts: Vec::new(),
                    diagnostics: None,
                },
            },
        )
        .await
        .map_err(|_| failure(35, "due completion failed"))?;
    if case == 35 {
        if !matches!(
            due_result,
            CompleteAttemptResult::TimedOutAndStaleRecorded(_)
        ) {
            return Err(failure(35, "due completion did not time out"));
        }
        return Ok(());
    }
    let due_events = adapter
        .store()
        .list_events_after(
            scope_a,
            &id("due-run"),
            EventPageRequest {
                after_event_seq: 0,
                page_size: 1_000,
                hard_response_byte_limit: 1_000_000,
            },
        )
        .await
        .map_err(|_| failure(36, "due event read failed"))?;
    let timeout_index = due_events
        .iter()
        .position(|event| event.event_type == crate::event::EventType::AttemptTimedOut)
        .ok_or_else(|| failure(36, "A06 event missing"))?;
    if case == 36 {
        if due_events.get(timeout_index + 1).is_none_or(|event| {
            event.event_type != crate::event::EventType::StaleCompletionObserved
        }) {
            return Err(failure(36, "A06 and A14 were not adjacent and ordered"));
        }
        return Ok(());
    }
    Ok(())
}

macro_rules! original_fixtures {
    ($( $function:ident => $case:literal ),+ $(,)?) => {
        $(
            async fn $function<A: ConformanceAdapter>(
                adapter: &A,
                scope_a: &ExecutionScope,
                scope_b: &ExecutionScope,
            ) -> Result<(), ConformanceFailure> {
                run_original_case(adapter, scope_a, scope_b, $case).await
            }
        )+
    };
}

original_fixtures! {
    publish_scope_a => 1,
    publish_scope_b => 2,
    equal_bytes_equal_digest => 3,
    verified_ref_scope_binding => 4,
    same_scope_publish_replay => 5,
    verified_read_scope_a => 6,
    verified_read_exact_bytes => 7,
    verified_read_scope_b => 8,
    missing_read_proof => 9,
    first_engine_claim => 10,
    frozen_control_plane_id => 11,
    live_peer_rejected => 12,
    claim_scope_locality => 13,
    expired_takeover => 14,
    takeover_generation_checked_increment => 15,
    stale_permit_rejected => 16,
    create_definition => 17,
    publish_revision => 18,
    create_run_receipt => 19,
    create_receipt_replay => 20,
    create_receipt_conflict => 21,
    point_read_scope_isolation => 22,
    start_run => 23,
    node_version_cas_rejection => 24,
    claim_attempt => 25,
    active_attempt_fence => 26,
    budget_reservation => 27,
    complete_attempt => 28,
    attempt_terminal_state => 29,
    budget_settlement => 30,
    event_sequence_contiguity => 31,
    batch_metadata_contiguity => 32,
    closed_edge_payload => 33,
    terminal_resolution => 34,
    due_completion_times_out => 35,
    timeout_observation_order => 36,
}

async fn blocked_run_host_command_fence<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let schema = adapter
        .objects()
        .put(scope, b"{}", "application/json")
        .await
        .map_err(|_| failure(37, "schema publication failed"))?;
    let creator = AuthenticatedPrincipal::mint(
        scope.clone(),
        "creator".to_owned(),
        Vec::new(),
        schema.digest().clone(),
    )
    .map_err(|_| failure(37, "creator capability failed"))?;
    adapter
        .store()
        .create_definition(
            scope,
            CreateDefinition {
                definition_id: id("approval-definition"),
                display_name: "approval".to_owned(),
                description: String::new(),
                principal: creator.clone(),
            },
        )
        .await
        .map_err(|_| failure(37, "definition creation failed"))?;
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: id("approval-definition"),
        name: "approval".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.digest().clone(),
        run_output_schema_digest: schema.digest().clone(),
        entry_node_id: id("approval"),
        nodes: vec![
            NodeDefinition::Approval {
                id: id("approval"),
                request: BindingSource::RunInput {
                    pointer: String::new(),
                },
                gate: ApprovalGateConfig {
                    expires_after_ms: 10_000,
                    on_expiry: ApprovalExpiryPolicy::Reject,
                    authorization: DecisionAuthorizationPolicy {
                        allowed_principal_ids: vec!["approver".to_owned()],
                        allowed_role_ids: Vec::new(),
                    },
                },
                next: vec![id("action")],
            },
            NodeDefinition::Action {
                id: id("action"),
                action: ActionReference {
                    name: "approval.action".to_owned(),
                    contract_version: "1".to_owned(),
                    input_schema_digest: schema.digest().clone(),
                    output_schema_digest: schema.digest().clone(),
                    compatible_implementation_requirement: schema.digest().clone(),
                },
                bindings: vec![Binding {
                    target: String::new(),
                    source: BindingSource::RunInput {
                        pointer: String::new(),
                    },
                }],
                retry: RetryPolicy {
                    max_attempts: 1,
                    backoff: BackoffPolicy::Fixed { delay_ms: 0 },
                },
                timeout: TimeoutPolicy { timeout_ms: 1_000 },
                declared_max_cost_units: CostUnits(1),
                next: vec![id("succeed")],
            },
            NodeDefinition::Succeed {
                id: id("succeed"),
                output: BindingSource::Constant {
                    value: serde_json::Value::Null,
                },
            },
        ],
    };
    let canonical = adapter
        .objects()
        .put(
            scope,
            &serde_jcs::to_vec(&definition)
                .map_err(|_| failure(37, "definition encoding failed"))?,
            "application/json",
        )
        .await
        .map_err(|_| failure(37, "definition publication failed"))?;
    adapter
        .store()
        .publish_revision(
            scope,
            PublishRevision {
                definition_id: id("approval-definition"),
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema.clone(),
                resolved_action_schema_objects: BTreeMap::from([(
                    "action".to_owned(),
                    ResolvedActionSchemas {
                        input_schema: schema.clone(),
                        output_schema: schema.clone(),
                    },
                )]),
                parsed_revision: PublishableDefinition {
                    definition,
                    topological_ranks: BTreeMap::from([
                        (id("approval"), TopologicalRank(0)),
                        (id("action"), TopologicalRank(1)),
                        (id("succeed"), TopologicalRank(2)),
                    ]),
                },
                principal: creator.clone(),
            },
        )
        .await
        .map_err(|_| failure(37, "revision publication failed"))?;
    let input = adapter
        .objects()
        .put(scope, br#"{"request":true}"#, "application/json")
        .await
        .map_err(|_| failure(37, "input publication failed"))?;
    adapter
        .store()
        .create_run(
            scope,
            CreateRun {
                run_id: id("approval-run"),
                definition_id: id("approval-definition"),
                revision_hash: canonical.digest().clone(),
                input,
                budget_limit: CostUnits(1),
                limits: RunLimits {
                    max_dynamic_node_instances: 1,
                    max_total_attempts: 1,
                    max_total_events: 100,
                    max_inline_json_bytes_per_value: 10_000,
                    max_artifacts_per_attempt: 1,
                    max_aggregate_object_bytes_per_run: 100_000,
                    max_run_lifetime_ms: 100_000,
                },
                principal: creator,
                idempotency_token: "approval-create-token".to_owned(),
            },
        )
        .await
        .map_err(|_| failure(37, "run creation failed"))?;
    let claim = adapter
        .store()
        .acquire_engine_claim(scope, id("approval-engine"))
        .await
        .map_err(|_| failure(37, "engine claim failed"))?;
    adapter
        .store()
        .start_run(
            scope,
            StartRun {
                permit: claim.permit.clone(),
                run_id: id("approval-run"),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: schema.digest().clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .map_err(|_| failure(37, "run start failed"))?;
    let node = adapter
        .store()
        .get_node(scope, &id("approval-run"), &id("approval"))
        .await
        .map_err(|_| failure(37, "approval node read failed"))?;
    let request = adapter
        .objects()
        .put(scope, br#"{"request":true}"#, "application/json")
        .await
        .map_err(|_| failure(37, "approval request publication failed"))?;
    let gate = adapter
        .store()
        .request_approval(
            scope,
            RequestApproval {
                permit: claim.permit.clone(),
                run_id: id("approval-run"),
                node_id: id("approval"),
                expected_node_version: node.version,
                gate_id: id("approval-gate"),
                request,
            },
        )
        .await
        .map_err(|_| failure(37, "approval request failed"))?;
    let approver = AuthenticatedPrincipal::mint(
        scope.clone(),
        "approver".to_owned(),
        Vec::new(),
        schema.digest().clone(),
    )
    .map_err(|_| failure(37, "approver capability failed"))?;
    let output = adapter
        .objects()
        .put(
            scope,
            &canonical_human_approval_result(None, &approver),
            "application/json",
        )
        .await
        .map_err(|_| failure(37, "approval output publication failed"))?;
    let run = adapter
        .store()
        .get_run(scope, &id("approval-run"))
        .await
        .map_err(|_| failure(37, "run read failed"))?
        .run;
    let decide = |output, approver| DecideApproval {
        run_id: id("approval-run"),
        gate_id: gate.gate_id.clone(),
        expected_run_version: run.version,
        expected_gate_version: gate.version,
        decision: ApprovalDecision::Approve,
        decision_payload: None,
        approval_output: Some(output),
        principal: approver,
    };
    let incompatibilities = adapter
        .objects()
        .put(scope, br#"{"missing":["synthetic"]}"#, "application/json")
        .await
        .map_err(|_| failure(37, "incompatibility publication failed"))?;
    adapter
        .store()
        .suspend_incompatible(
            scope,
            SuspendIncompatible {
                permit: claim.permit,
                run_id: id("approval-run"),
                incompatibilities,
                evidence: CompatibilityReport {
                    evidence_digest: schema.digest().clone(),
                    incompatible_reference_locations: vec!["action".to_owned()],
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .map_err(|_| failure(37, "run suspension failed"))?;
    if !matches!(
        adapter
            .store()
            .decide_approval(scope, decide(output, approver))
            .await,
        Err(StoreError::RunBlockedIncompatible)
    ) {
        return Err(failure(37, "blocked approval replay bypassed the fence"));
    }
    Ok(())
}

struct MapConformanceFixture {
    permit: EnginePermit,
    input: VerifiedObjectRef,
    ordered_items: Vec<OrderedMapItem>,
    expansion_digest: Digest,
}

async fn prepare_map_conformance<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
    item_count: u32,
    max_concurrency: u32,
    retry: RetryPolicy,
) -> Result<MapConformanceFixture, ConformanceFailure> {
    let schema = adapter
        .objects()
        .put(scope, b"{}", "application/json")
        .await
        .map_err(|_| failure(case, "Map schema publication failed"))?;
    let principal = AuthenticatedPrincipal::mint(
        scope.clone(),
        format!("map-case-{case}"),
        Vec::new(),
        schema.digest().clone(),
    )
    .map_err(|_| failure(case, "Map principal construction failed"))?;
    let definition_id = id(&format!("map-definition-{case}"));
    let run_id = id(&format!("map-run-{case}"));
    adapter
        .store()
        .create_definition(
            scope,
            CreateDefinition {
                definition_id: definition_id.clone(),
                display_name: format!("map-case-{case}"),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .map_err(|_| failure(case, "Map definition creation failed"))?;
    let values = (0..item_count).map(Value::from).collect::<Vec<_>>();
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: definition_id.clone(),
        name: format!("map-case-{case}"),
        description: String::new(),
        run_input_schema_digest: schema.digest().clone(),
        run_output_schema_digest: schema.digest().clone(),
        entry_node_id: id("map"),
        nodes: vec![
            NodeDefinition::Map {
                id: id("map"),
                items: BindingSource::Constant {
                    value: Value::Array(values.clone()),
                },
                max_items: item_count.max(1),
                max_concurrency,
                action: ActionReference {
                    name: format!("map.action.{case}"),
                    contract_version: "1".to_owned(),
                    input_schema_digest: schema.digest().clone(),
                    output_schema_digest: schema.digest().clone(),
                    compatible_implementation_requirement: schema.digest().clone(),
                },
                bindings: vec![crate::definition::MapBinding {
                    target: String::new(),
                    source: crate::definition::MapBindingSource::MapItem {
                        pointer: String::new(),
                    },
                }],
                retry,
                timeout: TimeoutPolicy { timeout_ms: 1_000 },
                declared_max_cost_units: CostUnits(1),
                next: vec![id("succeed")],
            },
            NodeDefinition::Succeed {
                id: id("succeed"),
                output: BindingSource::NodeOutput {
                    node_id: id("map"),
                    pointer: String::new(),
                },
            },
        ],
    };
    let canonical = adapter
        .objects()
        .put(
            scope,
            &serde_jcs::to_vec(&definition)
                .map_err(|_| failure(case, "Map definition encoding failed"))?,
            "application/json",
        )
        .await
        .map_err(|_| failure(case, "Map definition publication failed"))?;
    adapter
        .store()
        .publish_revision(
            scope,
            PublishRevision {
                definition_id: definition_id.clone(),
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema.clone(),
                resolved_action_schema_objects: BTreeMap::from([(
                    "map/map_action".to_owned(),
                    ResolvedActionSchemas {
                        input_schema: schema.clone(),
                        output_schema: schema.clone(),
                    },
                )]),
                parsed_revision: PublishableDefinition {
                    definition,
                    topological_ranks: BTreeMap::from([
                        (id("map"), TopologicalRank(0)),
                        (id("succeed"), TopologicalRank(1)),
                    ]),
                },
                principal: principal.clone(),
            },
        )
        .await
        .map_err(|_| failure(case, "Map revision publication failed"))?;
    let run_input = adapter
        .objects()
        .put(scope, b"{}", "application/json")
        .await
        .map_err(|_| failure(case, "Map run input publication failed"))?;
    adapter
        .store()
        .create_run(
            scope,
            CreateRun {
                run_id: run_id.clone(),
                definition_id,
                revision_hash: canonical.digest().clone(),
                input: run_input,
                budget_limit: CostUnits(1_000),
                limits: RunLimits {
                    max_dynamic_node_instances: 100,
                    max_total_attempts: 200,
                    max_total_events: 2_000,
                    max_inline_json_bytes_per_value: 10_000,
                    max_artifacts_per_attempt: 10,
                    max_aggregate_object_bytes_per_run: 1_000_000,
                    max_run_lifetime_ms: 31_536_000_000,
                },
                principal,
                idempotency_token: format!("map-case-{case}-create"),
            },
        )
        .await
        .map_err(|_| failure(case, "Map run creation failed"))?;
    let claim = adapter
        .store()
        .acquire_engine_claim(scope, id(&format!("map-engine-{case}")))
        .await
        .map_err(|_| failure(case, "Map engine claim failed"))?;
    adapter
        .store()
        .start_run(
            scope,
            StartRun {
                permit: claim.permit.clone(),
                run_id: run_id.clone(),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: schema.digest().clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .map_err(|_| failure(case, "Map run start failed"))?;
    let input_bytes = serde_jcs::to_vec(&values)
        .map_err(|_| failure(case, "Map expansion input encoding failed"))?;
    let input = adapter
        .objects()
        .put(scope, &input_bytes, "application/json")
        .await
        .map_err(|_| failure(case, "Map expansion input publication failed"))?;
    let mut identities = Vec::new();
    let mut ordered_items = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let index = index as u32;
        let item = adapter
            .objects()
            .put(
                scope,
                &serde_jcs::to_vec(value).map_err(|_| failure(case, "Map item encoding failed"))?,
                "application/json",
            )
            .await
            .map_err(|_| failure(case, "Map item digesting failed"))?;
        let child_id = map_child_id(&run_id, &id("map"), index, item.digest());
        identities.push(MapChildIdentity {
            item_index: index,
            item_digest: item.digest().clone(),
            child_id: child_id.clone(),
        });
        ordered_items.push(OrderedMapItem {
            index,
            item_digest: item.digest().clone(),
            child_id,
        });
    }
    Ok(MapConformanceFixture {
        permit: claim.permit,
        input,
        ordered_items,
        expansion_digest: map_expansion_digest(&identities),
    })
}

async fn expand_map_recomputes_identity<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let fixture = prepare_map_conformance(
        adapter,
        scope,
        38,
        2,
        1,
        RetryPolicy {
            max_attempts: 1,
            backoff: BackoffPolicy::Fixed { delay_ms: 0 },
        },
    )
    .await?;
    if !matches!(
        adapter
            .store()
            .expand_map(
                scope,
                ExpandMap {
                    permit: fixture.permit,
                    run_id: id("map-run-38"),
                    map_node_id: id("map"),
                    expected_node_version: Version(1),
                    input: fixture.input,
                    ordered_items: Vec::new(),
                    expansion_digest: map_expansion_digest(&[]),
                },
            )
            .await,
        Err(StoreError::IdempotencyConflict)
    ) {
        return Err(failure(38, "forged empty Map expansion was accepted"));
    }
    Ok(())
}

async fn map_concurrency_admission<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let fixture = prepare_map_conformance(
        adapter,
        scope,
        39,
        2,
        1,
        RetryPolicy {
            max_attempts: 1,
            backoff: BackoffPolicy::Fixed { delay_ms: 0 },
        },
    )
    .await?;
    adapter
        .store()
        .expand_map(
            scope,
            ExpandMap {
                permit: fixture.permit.clone(),
                run_id: id("map-run-39"),
                map_node_id: id("map"),
                expected_node_version: Version(1),
                input: fixture.input.clone(),
                ordered_items: fixture.ordered_items,
                expansion_digest: fixture.expansion_digest,
            },
        )
        .await
        .map_err(|_| failure(39, "valid Map expansion failed"))?;
    let children = adapter
        .store()
        .list_nodes(
            scope,
            &id("map-run-39"),
            PageRequest {
                cursor: None,
                page_size: 100,
            },
        )
        .await
        .map_err(|_| failure(39, "Map children read failed"))?
        .items
        .into_iter()
        .filter(|node| node.parent_map_instance_id.is_some())
        .collect::<Vec<_>>();
    let first = &children[0];
    let second = &children[1];
    if !matches!(
        adapter
            .store()
            .claim_node_attempt(
                scope,
                ClaimNodeAttempt {
                    permit: fixture.permit.clone(),
                    run_id: id("map-run-39"),
                    node_id: first.node_instance_id.clone(),
                    expected_node_version: first.version,
                    attempt_id: id("map-attempt-first"),
                    worker_id: id("worker"),
                    bound_input: fixture.input.clone(),
                    binding_derivation_digest: fixture.input.digest().clone(),
                },
            )
            .await,
        Ok(ClaimNodeAttemptResult::Claimed { .. })
    ) {
        return Err(failure(39, "first Map child was not admitted"));
    }
    if !matches!(
        adapter
            .store()
            .claim_node_attempt(
                scope,
                ClaimNodeAttempt {
                    permit: fixture.permit,
                    run_id: id("map-run-39"),
                    node_id: second.node_instance_id.clone(),
                    expected_node_version: second.version,
                    attempt_id: id("map-attempt-second"),
                    worker_id: id("worker"),
                    bound_input: fixture.input.clone(),
                    binding_derivation_digest: fixture.input.digest().clone(),
                },
            )
            .await,
        Ok(ClaimNodeAttemptResult::MapConcurrencyLimited)
    ) {
        return Err(failure(39, "Map concurrency cap admitted a sibling"));
    }
    Ok(())
}

async fn exponential_backoff_cap<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let delay = 86_400_000_i64;
    let fixture = prepare_map_conformance(
        adapter,
        scope,
        40,
        1,
        1,
        RetryPolicy {
            max_attempts: 100,
            backoff: BackoffPolicy::Exponential {
                initial_delay_ms: delay as u64,
                multiplier: 16,
                max_delay_ms: delay as u64,
            },
        },
    )
    .await?;
    adapter
        .store()
        .expand_map(
            scope,
            ExpandMap {
                permit: fixture.permit.clone(),
                run_id: id("map-run-40"),
                map_node_id: id("map"),
                expected_node_version: Version(1),
                input: fixture.input.clone(),
                ordered_items: fixture.ordered_items,
                expansion_digest: fixture.expansion_digest,
            },
        )
        .await
        .map_err(|_| failure(40, "backoff Map expansion failed"))?;
    let child_id = adapter
        .store()
        .list_nodes(
            scope,
            &id("map-run-40"),
            PageRequest {
                cursor: None,
                page_size: 100,
            },
        )
        .await
        .map_err(|_| failure(40, "backoff child read failed"))?
        .items
        .into_iter()
        .find(|node| node.parent_map_instance_id.is_some())
        .ok_or_else(|| failure(40, "backoff child missing"))?
        .node_instance_id;
    let mut permit = fixture.permit;
    for attempt_number in 1..100 {
        let node = adapter
            .store()
            .get_node(scope, &id("map-run-40"), &child_id)
            .await
            .map_err(|_| failure(40, "backoff node read failed"))?;
        let attempt_id = id(&format!("backoff-attempt-{attempt_number}"));
        let claimed = adapter
            .store()
            .claim_node_attempt(
                scope,
                ClaimNodeAttempt {
                    permit: permit.clone(),
                    run_id: id("map-run-40"),
                    node_id: child_id.clone(),
                    expected_node_version: node.version,
                    attempt_id: attempt_id.clone(),
                    worker_id: id("worker"),
                    bound_input: fixture.input.clone(),
                    binding_derivation_digest: fixture.input.digest().clone(),
                },
            )
            .await
            .map_err(|_| failure(40, "backoff attempt claim failed"))?;
        let credential = match claimed {
            ClaimNodeAttemptResult::Claimed {
                completion_credential,
                ..
            } => completion_credential,
            _ => return Err(failure(40, "backoff attempt was not claimed")),
        };
        adapter
            .store()
            .complete_attempt(
                scope,
                CompleteAttempt {
                    completion_credential: credential,
                    run_id: id("map-run-40"),
                    node_id: child_id.clone(),
                    attempt_id,
                    submitted_outcome: ActionOutcome::retryable(
                        "conformance.retry".to_owned(),
                        "retry".to_owned(),
                        None,
                        CostUnits(0),
                    )
                    .map_err(|_| failure(40, "backoff outcome invalid"))?,
                    objects: CompletionObjects {
                        output: None,
                        artifacts: Vec::new(),
                        diagnostics: None,
                    },
                },
            )
            .await
            .map_err(|_| failure(40, "backoff completion failed"))?;
        let waiting = adapter
            .store()
            .get_node(scope, &id("map-run-40"), &child_id)
            .await
            .map_err(|_| failure(40, "backoff wait read failed"))?;
        let attempt = adapter
            .store()
            .get_attempt(
                scope,
                &id("map-run-40"),
                &id(&format!("backoff-attempt-{attempt_number}")),
            )
            .await
            .map_err(|_| failure(40, "backoff attempt read failed"))?;
        if waiting.next_eligible_at
            != attempt
                .finished_at
                .and_then(|finished| finished.0.checked_add(delay).map(crate::ids::Timestamp))
        {
            return Err(failure(40, "exponential backoff did not cap"));
        }
        if attempt_number < 99 {
            adapter.advance_clock_ms(delay);
            permit = adapter
                .store()
                .acquire_engine_claim(scope, id("map-engine-40"))
                .await
                .map_err(|_| failure(40, "backoff engine takeover failed"))?
                .permit;
            adapter
                .store()
                .release_retry(
                    scope,
                    ReleaseRetry {
                        permit: permit.clone(),
                        run_id: id("map-run-40"),
                        node_id: child_id.clone(),
                        expected_node_version: waiting.version,
                    },
                )
                .await
                .map_err(|_| failure(40, "backoff retry release failed"))?;
        }
    }
    Ok(())
}

/// One independently executed conformance result.
#[derive(Debug)]
pub struct ConformanceCaseResult {
    /// Stable fixture name.
    pub name: &'static str,
    /// Per-case failure, or none when the case passed.
    pub failure: Option<ConformanceFailure>,
}

impl ConformanceCaseResult {
    /// Whether this independently isolated fixture passed.
    pub fn passed(&self) -> bool {
        self.failure.is_none()
    }
}

/// Runs every fixture against a newly constructed adapter pair.
pub async fn run_conformance<A: ConformanceAdapter>(
    adapter_factory: &A,
    scope_a: &ExecutionScope,
    scope_b: &ExecutionScope,
) -> Vec<ConformanceCaseResult> {
    let mut results = Vec::with_capacity(CASE_COUNT);
    macro_rules! run_fixture {
        ($name:literal, $fixture:ident) => {{
            let adapter = adapter_factory.fresh();
            results.push(ConformanceCaseResult {
                name: $name,
                failure: $fixture(&adapter, scope_a, scope_b).await.err(),
            });
        }};
    }
    run_fixture!("publish_scope_a", publish_scope_a);
    run_fixture!("publish_scope_b", publish_scope_b);
    run_fixture!("equal_bytes_equal_digest", equal_bytes_equal_digest);
    run_fixture!("verified_ref_scope_binding", verified_ref_scope_binding);
    run_fixture!("same_scope_publish_replay", same_scope_publish_replay);
    run_fixture!("verified_read_scope_a", verified_read_scope_a);
    run_fixture!("verified_read_exact_bytes", verified_read_exact_bytes);
    run_fixture!("verified_read_scope_b", verified_read_scope_b);
    run_fixture!("missing_read_proof", missing_read_proof);
    run_fixture!("first_engine_claim", first_engine_claim);
    run_fixture!("frozen_control_plane_id", frozen_control_plane_id);
    run_fixture!("live_peer_rejected", live_peer_rejected);
    run_fixture!("claim_scope_locality", claim_scope_locality);
    run_fixture!("expired_takeover", expired_takeover);
    run_fixture!(
        "takeover_generation_checked_increment",
        takeover_generation_checked_increment
    );
    run_fixture!("stale_permit_rejected", stale_permit_rejected);
    run_fixture!("create_definition", create_definition);
    run_fixture!("publish_revision", publish_revision);
    run_fixture!("create_run_receipt", create_run_receipt);
    run_fixture!("create_receipt_replay", create_receipt_replay);
    run_fixture!("create_receipt_conflict", create_receipt_conflict);
    run_fixture!("point_read_scope_isolation", point_read_scope_isolation);
    run_fixture!("start_run", start_run);
    run_fixture!("node_version_cas_rejection", node_version_cas_rejection);
    run_fixture!("claim_attempt", claim_attempt);
    run_fixture!("active_attempt_fence", active_attempt_fence);
    run_fixture!("budget_reservation", budget_reservation);
    run_fixture!("complete_attempt", complete_attempt);
    run_fixture!("attempt_terminal_state", attempt_terminal_state);
    run_fixture!("budget_settlement", budget_settlement);
    run_fixture!("event_sequence_contiguity", event_sequence_contiguity);
    run_fixture!("batch_metadata_contiguity", batch_metadata_contiguity);
    run_fixture!("closed_edge_payload", closed_edge_payload);
    run_fixture!("terminal_resolution", terminal_resolution);
    run_fixture!("due_completion_times_out", due_completion_times_out);
    run_fixture!("timeout_observation_order", timeout_observation_order);
    run_fixture!(
        "blocked_run_host_command_fence",
        blocked_run_host_command_fence
    );
    run_fixture!(
        "expand_map_recomputes_identity",
        expand_map_recomputes_identity
    );
    run_fixture!("map_concurrency_admission", map_concurrency_admission);
    run_fixture!("exponential_backoff_cap", exponential_backoff_cap);
    results
}

fn id(value: &str) -> Id {
    Id::new(value).expect("conformance IDs are valid")
}

fn failure(case: usize, detail: &'static str) -> ConformanceFailure {
    ConformanceFailure { case, detail }
}

#[allow(dead_code)]
fn _closed_object_error(error: ObjectStoreError) -> ObjectStoreError {
    error
}
