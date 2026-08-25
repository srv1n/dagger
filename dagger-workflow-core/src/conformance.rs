//! Adapter-neutral black-box checks shared by volatile and durable stores.

use crate::action::{ActionOutcome, ArtifactOutput, CompatibilityReport, CompletionCredential};
use crate::approval::{
    canonical_human_approval_result, ApprovalDecision, ApprovalExpiryPolicy,
    AuthenticatedPrincipal, DecisionAuthorizationPolicy,
};
use crate::artifact::{
    ArtifactRef, FailedReadClass, FailedReadProof, ObjectRecord, ObjectStore, ObjectStoreError,
    VerifiedObjectRef,
};
use crate::definition::{
    ActionReference, ApprovalGateConfig, BackoffPolicy, Binding, BindingSource, NodeDefinition,
    PublishableDefinition, RetryPolicy, TimeoutPolicy, ValidationErrorKind, WorkflowDefinition,
};
use crate::ids::{
    map_child_id, map_expansion_digest, CostUnits, Digest, Id, MapChildIdentity, Timestamp,
    TopologicalRank, Version,
};
use crate::run::{
    AttemptState, GateState, NodeFailureKind, NodeRun, NodeState, RunFailureKind, RunLimits,
    RunState, WorkflowRun,
};
use crate::scope::ExecutionScope;
use crate::store::{
    ClaimNodeAttempt, ClaimNodeAttemptResult, CompleteAttempt, CompleteAttemptResult, CompleteMap,
    CompletionObjects, CreateDefinition, CreateRun, DecideApproval, EnginePermit, EventPageRequest,
    ExpandMap, MarkCorruptStorage, OrderedMapItem, PageRequest, PublishRevision, ReleaseRetry,
    RequestApproval, ResolveTerminalNode, ResolvedActionSchemas, StartRun, StoreError,
    SuspendIncompatible, WorkflowStore,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Number of independent adapter-neutral cases.
pub const CASE_COUNT: usize = 68;

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
    "corrupt_unreachable_ref_rejected",
    "corrupt_run_produced_ref_accepted",
    "corrupt_pinned_revision_ref_accepted",
    "pinned_root_schema_subset_enforced",
    "succeed_output_schema_enforced",
    "succeed_output_inline_limit_enforced",
    "succeed_output_inline_limit_boundary_accepted",
    "succeed_output_aggregate_limit_enforced",
    "succeed_output_aggregate_limit_boundary_accepted",
    "map_aggregate_inline_limit_enforced",
    "map_aggregate_inline_limit_boundary_accepted",
    "map_aggregate_object_limit_enforced",
    "map_aggregate_object_limit_boundary_accepted",
    "expand_map_input_invalid_applied",
    "expand_map_dynamic_node_limit_applied",
    "claim_inline_json_limit_applied",
    "decide_approval_payload_invalid_applied",
    "revision_non_canonical_bytes_rejected",
    "revision_typed_definition_mismatch_rejected",
    "revision_definition_id_mismatch_rejected",
    "revision_topological_ranks_mismatch_rejected",
    "action_output_inline_limit_enforced",
    "action_output_inline_limit_boundary_accepted",
    "action_artifacts_per_attempt_limit_enforced",
    "action_artifacts_per_attempt_limit_boundary_accepted",
    "action_output_aggregate_limit_enforced",
    "action_output_aggregate_limit_boundary_accepted",
    "action_output_schema_enforced",
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
    /// Returns exact workflow-store object metadata for no-write assertions.
    fn object_records(&self, scope: &ExecutionScope) -> Vec<ObjectRecord>;
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
        .put(
            scope_a,
            br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#,
            "application/json",
        )
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
                    target: "/value".to_owned(),
                    source: BindingSource::RunInput {
                        pointer: "/value".to_owned(),
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
        .put(
            scope,
            br#"{"additionalProperties":false,"properties":{"request":{"type":"boolean"}},"required":["request"],"type":"object"}"#,
            "application/json",
        )
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
                    target: "/request".to_owned(),
                    source: BindingSource::RunInput {
                        pointer: "/request".to_owned(),
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
    prepare_map_conformance_with_limits(
        adapter,
        scope,
        case,
        item_count,
        max_concurrency,
        retry,
        RunLimits {
            max_dynamic_node_instances: 100,
            max_total_attempts: 200,
            max_total_events: 2_000,
            max_inline_json_bytes_per_value: 10_000,
            max_artifacts_per_attempt: 10,
            max_aggregate_object_bytes_per_run: 1_000_000,
            max_run_lifetime_ms: 31_536_000_000,
        },
    )
    .await
}

/// Same fixture, with the section 1.4 ceilings chosen by the caller.
#[allow(clippy::too_many_arguments)]
async fn prepare_map_conformance_with_limits<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
    item_count: u32,
    max_concurrency: u32,
    retry: RetryPolicy,
    limits: RunLimits,
) -> Result<MapConformanceFixture, ConformanceFailure> {
    let schema = adapter
        .objects()
        .put(
            scope,
            br#"{"additionalProperties":false,"properties":{"item":{"type":"integer"}},"type":"object"}"#,
            "application/json",
        )
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
                    target: "/item".to_owned(),
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
                limits,
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
    let mut wrong_order = fixture.ordered_items.clone();
    wrong_order.swap(0, 1);
    let mut dropped = fixture.ordered_items.clone();
    dropped.pop();
    let variants = [
        (
            fixture.ordered_items.clone(),
            Digest::new(format!("sha256:{}", "00".repeat(32)))
                .map_err(|_| failure(38, "forged digest construction failed"))?,
            "forged Map expansion digest was accepted",
        ),
        (
            wrong_order,
            fixture.expansion_digest.clone(),
            "forged Map item order was accepted",
        ),
        (
            dropped,
            fixture.expansion_digest.clone(),
            "forged dropped Map item was accepted",
        ),
    ];
    for (ordered_items, expansion_digest, detail) in variants {
        if !matches!(
            adapter
                .store()
                .expand_map(
                    scope,
                    ExpandMap {
                        permit: fixture.permit.clone(),
                        run_id: id("map-run-38"),
                        map_node_id: id("map"),
                        expected_node_version: Version(1),
                        input: fixture.input.clone(),
                        ordered_items,
                        expansion_digest,
                    },
                )
                .await,
            Err(StoreError::IdempotencyConflict)
        ) {
            return Err(failure(38, detail));
        }
    }
    Ok(())
}

async fn map_concurrency_admission<A: ConformanceAdapter>(
    adapter_factory: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    for max_concurrency in [1, 2] {
        let adapter = adapter_factory.fresh();
        let fixture = prepare_map_conformance(
            &adapter,
            scope,
            39,
            3,
            max_concurrency,
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
        for (index, child) in children.iter().take(max_concurrency as usize).enumerate() {
            if !matches!(
                adapter
                    .store()
                    .claim_node_attempt(
                        scope,
                        ClaimNodeAttempt {
                            permit: fixture.permit.clone(),
                            run_id: id("map-run-39"),
                            node_id: child.node_instance_id.clone(),
                            expected_node_version: child.version,
                            attempt_id: id(&format!("map-attempt-admitted-{index}")),
                            worker_id: id("worker"),
                            bound_input: fixture.input.clone(),
                            binding_derivation_digest: fixture.input.digest().clone(),
                        },
                    )
                    .await,
                Ok(ClaimNodeAttemptResult::Claimed { .. })
            ) {
                return Err(failure(39, "Map child below concurrency cap was refused"));
            }
        }
        let refused_input = adapter
            .objects()
            .put(
                scope,
                &serde_jcs::to_vec(&json!({"refused_at_cap": max_concurrency}))
                    .map_err(|_| failure(39, "refused input encoding failed"))?,
                "application/json",
            )
            .await
            .map_err(|_| failure(39, "refused input publication failed"))?;
        let events_before = adapter
            .store()
            .list_events_after(
                scope,
                &id("map-run-39"),
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1_000,
                    hard_response_byte_limit: 1_000_000,
                },
            )
            .await
            .map_err(|_| failure(39, "pre-refusal events read failed"))?;
        let run_version_before = adapter
            .store()
            .get_run(scope, &id("map-run-39"))
            .await
            .map_err(|_| failure(39, "pre-refusal run read failed"))?
            .run
            .version;
        let node_versions_before = adapter
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
            .map_err(|_| failure(39, "pre-refusal nodes read failed"))?
            .items
            .into_iter()
            .map(|node| (node.node_instance_id, node.version))
            .collect::<Vec<_>>();
        let records_before = adapter.object_records(scope);
        let refused = &children[max_concurrency as usize];
        if !matches!(
            adapter
                .store()
                .claim_node_attempt(
                    scope,
                    ClaimNodeAttempt {
                        permit: fixture.permit,
                        run_id: id("map-run-39"),
                        node_id: refused.node_instance_id.clone(),
                        expected_node_version: refused.version,
                        attempt_id: id("map-attempt-refused"),
                        worker_id: id("worker"),
                        binding_derivation_digest: refused_input.digest().clone(),
                        bound_input: refused_input,
                    },
                )
                .await,
            Ok(ClaimNodeAttemptResult::MapConcurrencyLimited)
        ) {
            return Err(failure(39, "Map concurrency boundary was not enforced"));
        }
        let events_after = adapter
            .store()
            .list_events_after(
                scope,
                &id("map-run-39"),
                EventPageRequest {
                    after_event_seq: 0,
                    page_size: 1_000,
                    hard_response_byte_limit: 1_000_000,
                },
            )
            .await
            .map_err(|_| failure(39, "post-refusal events read failed"))?;
        let run_version_after = adapter
            .store()
            .get_run(scope, &id("map-run-39"))
            .await
            .map_err(|_| failure(39, "post-refusal run read failed"))?
            .run
            .version;
        let node_versions_after = adapter
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
            .map_err(|_| failure(39, "post-refusal nodes read failed"))?
            .items
            .into_iter()
            .map(|node| (node.node_instance_id, node.version))
            .collect::<Vec<_>>();
        if events_after != events_before
            || run_version_after != run_version_before
            || node_versions_after != node_versions_before
            || adapter.object_records(scope) != records_before
        {
            return Err(failure(39, "Map concurrency refusal mutated store state"));
        }
    }
    Ok(())
}

async fn exponential_backoff_cap<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let initial_delay = 1_000_i64;
    let max_delay = 5_000_i64;
    let fixture = prepare_map_conformance(
        adapter,
        scope,
        40,
        1,
        1,
        RetryPolicy {
            max_attempts: 5,
            backoff: BackoffPolicy::Exponential {
                initial_delay_ms: initial_delay as u64,
                multiplier: 2,
                max_delay_ms: max_delay as u64,
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
    let permit = fixture.permit;
    for attempt_number in 1..5 {
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
        let expected_delay = [initial_delay, 2_000, 4_000, max_delay][attempt_number as usize - 1];
        if waiting.next_eligible_at
            != attempt.finished_at.and_then(|finished| {
                finished
                    .0
                    .checked_add(expected_delay)
                    .map(crate::ids::Timestamp)
            })
        {
            return Err(failure(40, "exponential backoff progression was incorrect"));
        }
        if attempt_number < 4 {
            adapter.advance_clock_ms(expected_delay);
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

struct CorruptionConformanceFixture {
    run: WorkflowRun,
    store_instance_nonce: Vec<u8>,
}

/// Prepares one started run plus the object-store binding a proof must carry.
async fn prepare_corruption_conformance<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
) -> Result<CorruptionConformanceFixture, ConformanceFailure> {
    prepare_map_conformance(
        adapter,
        scope,
        case,
        1,
        1,
        RetryPolicy {
            max_attempts: 1,
            backoff: BackoffPolicy::Fixed { delay_ms: 0 },
        },
    )
    .await?;
    let binding = adapter
        .objects()
        .put(scope, b"{}", "application/json")
        .await
        .map_err(|_| failure(case, "corruption nonce publication failed"))?;
    let run = adapter
        .store()
        .get_run(scope, &id(&format!("map-run-{case}")))
        .await
        .map_err(|_| failure(case, "corruption run read failed"))?
        .run;
    Ok(CorruptionConformanceFixture {
        run,
        store_instance_nonce: binding.store_instance_nonce().to_vec(),
    })
}

/// Mints the object-store proof a corruption mark requires.
fn corruption_proof(
    scope: &ExecutionScope,
    bad_ref: &ArtifactRef,
    store_instance_nonce: Vec<u8>,
    case: usize,
) -> FailedReadProof {
    FailedReadProof::mint(
        scope.clone(),
        bad_ref.digest.clone(),
        FailedReadClass::Missing,
        None,
        store_instance_nonce,
        format!("conformance-corruption-{case}").into_bytes(),
        Timestamp(0),
    )
}

async fn corrupt_unreachable_ref_rejected<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let fixture = prepare_corruption_conformance(adapter, scope, 41).await?;
    let unrelated_input = adapter
        .objects()
        .put(scope, br#"{"item":7}"#, "application/json")
        .await
        .map_err(|_| failure(41, "unrelated input publication failed"))?;
    let principal = AuthenticatedPrincipal::mint(
        scope.clone(),
        "corruption-case-41".to_owned(),
        Vec::new(),
        unrelated_input.digest().clone(),
    )
    .map_err(|_| failure(41, "unrelated principal construction failed"))?;
    adapter
        .store()
        .create_run(
            scope,
            CreateRun {
                run_id: id("unrelated-run-41"),
                definition_id: fixture.run.definition_id.clone(),
                revision_hash: fixture.run.revision_hash.clone(),
                input: unrelated_input,
                budget_limit: CostUnits(1_000),
                limits: fixture.run.limits.clone(),
                principal,
                idempotency_token: "corruption-case-41-create".to_owned(),
            },
        )
        .await
        .map_err(|_| failure(41, "unrelated run creation failed"))?;
    let unrelated_ref = adapter
        .store()
        .get_run(scope, &id("unrelated-run-41"))
        .await
        .map_err(|_| failure(41, "unrelated run read failed"))?
        .run
        .input_ref
        .0;
    let proof = corruption_proof(scope, &unrelated_ref, fixture.store_instance_nonce, 41);
    if !matches!(
        adapter
            .store()
            .mark_corrupt_storage(
                scope,
                MarkCorruptStorage {
                    run_id: fixture.run.run_id.clone(),
                    bad_ref: unrelated_ref,
                    proof,
                    owner_node_id: None,
                },
            )
            .await,
        Err(StoreError::InvalidFailedReadProof)
    ) {
        return Err(failure(
            41,
            "unreachable bad ref corrupted an unrelated run",
        ));
    }
    if adapter
        .store()
        .get_run(scope, &fixture.run.run_id)
        .await
        .map_err(|_| failure(41, "target run read failed"))?
        .run
        .status
        == RunState::CorruptStorage
    {
        return Err(failure(41, "refused corruption mark terminalized the run"));
    }
    Ok(())
}

async fn corrupt_run_produced_ref_accepted<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let fixture = prepare_corruption_conformance(adapter, scope, 42).await?;
    let bad_ref = fixture.run.input_ref.0.clone();
    let proof = corruption_proof(scope, &bad_ref, fixture.store_instance_nonce, 42);
    let run = adapter
        .store()
        .mark_corrupt_storage(
            scope,
            MarkCorruptStorage {
                run_id: fixture.run.run_id.clone(),
                bad_ref: bad_ref.clone(),
                proof,
                owner_node_id: None,
            },
        )
        .await
        .map_err(|_| failure(42, "run-produced corruption mark was refused"))?;
    if run.status != RunState::CorruptStorage
        || run.corrupt_bad_artifact_ref_id != Some(bad_ref.artifact_ref_id)
    {
        return Err(failure(42, "run-produced corruption mark was not recorded"));
    }
    Ok(())
}

async fn corrupt_pinned_revision_ref_accepted<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let fixture = prepare_corruption_conformance(adapter, scope, 43).await?;
    let revision = adapter
        .store()
        .get_revision(
            scope,
            &fixture.run.definition_id,
            &fixture.run.revision_hash,
        )
        .await
        .map_err(|_| failure(43, "pinned revision read failed"))?;
    let bad_ref = revision.run_input_schema_ref.0.clone();
    if bad_ref.producer_run_id.is_some() {
        return Err(failure(43, "pinned schema ref was run-attributed"));
    }
    let proof = corruption_proof(scope, &bad_ref, fixture.store_instance_nonce, 43);
    let run = adapter
        .store()
        .mark_corrupt_storage(
            scope,
            MarkCorruptStorage {
                run_id: fixture.run.run_id.clone(),
                bad_ref: bad_ref.clone(),
                proof,
                owner_node_id: None,
            },
        )
        .await
        .map_err(|_| failure(43, "pinned schema corruption mark was refused"))?;
    if run.status != RunState::CorruptStorage
        || run.corrupt_bad_artifact_ref_id != Some(bad_ref.artifact_ref_id)
    {
        return Err(failure(
            43,
            "pinned schema corruption mark was not recorded",
        ));
    }
    Ok(())
}

/// Publishes one revision differing only in its root input schema object.
///
/// Both stacks must reach the schema decision, so everything else the command
/// validates first is held identical to the passing fixtures above.
/// Builds the two-node publication fixture shared by the section 5.2 cases.
fn publishable_definition(
    definition_id: Id,
    action_schema: &VerifiedObjectRef,
    root_input_schema: &VerifiedObjectRef,
) -> WorkflowDefinition {
    WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id,
        name: "conformance".to_owned(),
        description: String::new(),
        run_input_schema_digest: root_input_schema.digest().clone(),
        run_output_schema_digest: action_schema.digest().clone(),
        entry_node_id: id("action"),
        nodes: vec![
            NodeDefinition::Action {
                id: id("action"),
                action: ActionReference {
                    name: "conformance.action".to_owned(),
                    contract_version: "1".to_owned(),
                    input_schema_digest: action_schema.digest().clone(),
                    output_schema_digest: action_schema.digest().clone(),
                    compatible_implementation_requirement: action_schema.digest().clone(),
                },
                bindings: vec![Binding {
                    target: "/value".to_owned(),
                    source: BindingSource::RunInput {
                        pointer: "/value".to_owned(),
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
    }
}

async fn publish_with_root_input_schema<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    principal: &AuthenticatedPrincipal,
    action_schema: &VerifiedObjectRef,
    root_input_schema: &VerifiedObjectRef,
) -> Result<(VerifiedObjectRef, Result<(), StoreError>), ConformanceFailure> {
    let definition = publishable_definition(
        id("conformance-definition"),
        action_schema,
        root_input_schema,
    );
    let canonical = adapter
        .objects()
        .put(
            scope,
            &serde_jcs::to_vec(&definition)
                .map_err(|_| failure(44, "definition encoding failed"))?,
            "application/json",
        )
        .await
        .map_err(|_| failure(44, "definition object publication failed"))?;
    let mut ranks = BTreeMap::new();
    ranks.insert(id("action"), TopologicalRank(0));
    ranks.insert(id("succeed"), TopologicalRank(1));
    let mut schemas = BTreeMap::new();
    schemas.insert(
        "action".to_owned(),
        ResolvedActionSchemas {
            input_schema: action_schema.clone(),
            output_schema: action_schema.clone(),
        },
    );
    let outcome = adapter
        .store()
        .publish_revision(
            scope,
            PublishRevision {
                definition_id: id("conformance-definition"),
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: root_input_schema.clone(),
                run_output_schema: action_schema.clone(),
                resolved_action_schema_objects: schemas,
                parsed_revision: PublishableDefinition {
                    definition,
                    topological_ranks: ranks,
                },
                principal: principal.clone(),
            },
        )
        .await
        .map(|_| ());
    Ok((canonical, outcome))
}

/// Both stacks reject an out-of-subset root schema and a violating run input.
///
///.
/// section 14 requires the same validator at run creation, so a store that pins an
/// out-of-subset schema or admits an input the pinned schema rejects is
/// non-conforming. This is the parity direction that hides worst: an oracle weaker
/// than the adapter it certifies makes every other parity case rest on less than
/// it appears to.
async fn pinned_root_schema_subset_enforced<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let schema = adapter
        .objects()
        .put(
            scope,
            br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#,
            "application/json",
        )
        .await
        .map_err(|_| failure(44, "schema publication failed"))?;
    // `title` is an annotation keyword section 14.3 forbids outright; the bytes
    // are otherwise canonical and a full Draft 2020-12 validator would accept them.
    let annotated_schema = adapter
        .objects()
        .put(
            scope,
            br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"title":"run input","type":"object"}"#,
            "application/json",
        )
        .await
        .map_err(|_| failure(44, "annotated schema publication failed"))?;
    let principal = AuthenticatedPrincipal::mint(
        scope.clone(),
        "conformance".to_owned(),
        Vec::new(),
        schema.digest().clone(),
    )
    .map_err(|_| failure(44, "principal construction failed"))?;
    adapter
        .store()
        .create_definition(
            scope,
            CreateDefinition {
                definition_id: id("conformance-definition"),
                display_name: "conformance".to_owned(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .map_err(|_| failure(44, "definition creation failed"))?;
    let (_, annotated_outcome) =
        publish_with_root_input_schema(adapter, scope, &principal, &schema, &annotated_schema)
            .await?;
    if !matches!(annotated_outcome, Err(StoreError::SchemaSubsetUnsupported)) {
        return Err(failure(44, "out-of-subset root schema was published"));
    }
    let (canonical, outcome) =
        publish_with_root_input_schema(adapter, scope, &principal, &schema, &schema).await?;
    outcome.map_err(|_| failure(44, "in-subset revision publication failed"))?;
    let input = adapter
        .objects()
        .put(scope, br#"{"value":"one"}"#, "application/json")
        .await
        .map_err(|_| failure(44, "violating input publication failed"))?;
    if !matches!(
        adapter
            .store()
            .create_run(
                scope,
                CreateRun {
                    run_id: id("conformance-run"),
                    definition_id: id("conformance-definition"),
                    revision_hash: canonical.digest().clone(),
                    input,
                    budget_limit: CostUnits(10),
                    limits: RunLimits {
                        max_dynamic_node_instances: 10,
                        max_total_attempts: 10,
                        max_total_events: 1_000,
                        max_inline_json_bytes_per_value: 10_000,
                        max_artifacts_per_attempt: 10,
                        max_aggregate_object_bytes_per_run: 100_000,
                        max_run_lifetime_ms: 100_000,
                    },
                    principal,
                    idempotency_token: "conformance-subset-token".to_owned(),
                },
            )
            .await,
        Err(StoreError::ContractValidation {
            kind: ValidationErrorKind::SchemaSubsetUnsupported,
            ..
        })
    ) {
        return Err(failure(
            44,
            "input violating the pinned root schema was accepted",
        ));
    }
    if !matches!(
        adapter.store().get_run(scope, &id("conformance-run")).await,
        Err(StoreError::NotFound)
    ) {
        return Err(failure(44, "rejected run creation left a run row"));
    }
    Ok(())
}

/// Publishes one hand-built canonical-bytes / typed-definition pair.
///
/// Section 5.2 makes the canonical document the publication authority, so the two
/// halves are supplied independently here: every canonical-authority case differs
/// only in how it makes them disagree.
async fn publish_canonical_pair<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    principal: &AuthenticatedPrincipal,
    case: usize,
    canonical_bytes: Vec<u8>,
    parsed_revision: PublishableDefinition,
    schema: &VerifiedObjectRef,
) -> Result<Result<(), StoreError>, ConformanceFailure> {
    let canonical = adapter
        .objects()
        .put(scope, &canonical_bytes, "application/json")
        .await
        .map_err(|_| failure(case, "definition object publication failed"))?;
    // The object store JCS-normalises `application/json`, which silently repairs
    // some malformed inputs. A case whose bytes were repaired on the way in would
    // pass while proving nothing, so refuse to continue on a rewritten object.
    if canonical.verified_bytes() != canonical_bytes.as_slice() {
        return Err(failure(case, "object store rewrote the fixture bytes"));
    }
    let mut schemas = BTreeMap::new();
    schemas.insert(
        "action".to_owned(),
        ResolvedActionSchemas {
            input_schema: schema.clone(),
            output_schema: schema.clone(),
        },
    );
    Ok(adapter
        .store()
        .publish_revision(
            scope,
            PublishRevision {
                definition_id: id("conformance-definition"),
                expected_definition_version: Version(1),
                canonical_definition: canonical,
                run_input_schema: schema.clone(),
                run_output_schema: schema.clone(),
                resolved_action_schema_objects: schemas,
                parsed_revision,
                principal: principal.clone(),
            },
        )
        .await
        .map(|_| ()))
}

/// Prepares the schema object, principal, and empty definition row for a
/// canonical-authority case.
async fn prepare_canonical_authority<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
) -> Result<(VerifiedObjectRef, AuthenticatedPrincipal), ConformanceFailure> {
    let schema = adapter
        .objects()
        .put(
            scope,
            br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#,
            "application/json",
        )
        .await
        .map_err(|_| failure(case, "schema publication failed"))?;
    let principal = AuthenticatedPrincipal::mint(
        scope.clone(),
        "conformance".to_owned(),
        Vec::new(),
        schema.digest().clone(),
    )
    .map_err(|_| failure(case, "principal construction failed"))?;
    adapter
        .store()
        .create_definition(
            scope,
            CreateDefinition {
                definition_id: id("conformance-definition"),
                display_name: "conformance".to_owned(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .map_err(|_| failure(case, "definition creation failed"))?;
    Ok((schema, principal))
}

/// Both stacks reject definition bytes that are not RFC 8785 canonical.
///
/// Section 5.2 derives `revision_hash` from the canonical document, and section
/// 13.1 fixes canonical form. Accepting a differently serialised encoding of the
/// same document would give one definition two revision hashes, so the byte
/// comparison against `canonical_definition_json` is the publication gate.
async fn revision_non_canonical_bytes_rejected<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let (schema, principal) = prepare_canonical_authority(adapter, scope, 58).await?;
    let definition = publishable_definition(id("conformance-definition"), &schema, &schema);
    let canonical = crate::definition::canonical_definition_json(&definition)
        .map_err(|_| failure(58, "canonical encoding failed"))?;
    // Section 14.2 expands schema defaults, so a document that merely omits a
    // defaulted member parses to the same definition but is not its canonical
    // form. Whitespace and key order cannot be used here: the object store
    // already JCS-normalises `application/json` on put, and dropping the root
    // `description` is the omission that survives that pass.
    let mut object = match serde_json::from_slice::<Value>(&canonical) {
        Ok(Value::Object(object)) => object,
        _ => return Err(failure(58, "canonical bytes did not parse")),
    };
    if object.remove("description").is_none() {
        return Err(failure(58, "canonical bytes carried no defaulted member"));
    }
    let elided = serde_jcs::to_vec(&object).map_err(|_| failure(58, "elided encoding failed"))?;
    if elided == canonical {
        return Err(failure(58, "fixture failed to produce non-canonical bytes"));
    }
    let ranks = crate::definition::canonical_topological_ranks(&definition)
        .map_err(|_| failure(58, "rank derivation failed"))?;
    let outcome = publish_canonical_pair(
        adapter,
        scope,
        &principal,
        58,
        elided,
        PublishableDefinition {
            definition,
            topological_ranks: ranks,
        },
        &schema,
    )
    .await?;
    if !matches!(outcome, Err(StoreError::RevisionInvalid { .. })) {
        return Err(failure(58, "non-canonical definition bytes were published"));
    }
    Ok(())
}

/// Both stacks reject a typed definition that does not canonicalize to the bytes.
///
/// Section 5.2 makes the canonical document authoritative, so a store that stored
/// the caller's struct would persist an entry node, node set, and ranks that the
/// `revision_hash` does not cover. Section 1.5 then makes those unverified values
/// the recovery-order key used by sections 3.4 and 4.
async fn revision_typed_definition_mismatch_rejected<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let (schema, principal) = prepare_canonical_authority(adapter, scope, 59).await?;
    let definition = publishable_definition(id("conformance-definition"), &schema, &schema);
    let canonical = crate::definition::canonical_definition_json(&definition)
        .map_err(|_| failure(59, "canonical encoding failed"))?;
    let ranks = crate::definition::canonical_topological_ranks(&definition)
        .map_err(|_| failure(59, "rank derivation failed"))?;
    let mut divergent = definition;
    divergent.name = "divergent".to_owned();
    let outcome = publish_canonical_pair(
        adapter,
        scope,
        &principal,
        59,
        canonical,
        PublishableDefinition {
            definition: divergent,
            topological_ranks: ranks,
        },
        &schema,
    )
    .await?;
    if !matches!(outcome, Err(StoreError::RevisionInvalid { .. })) {
        return Err(failure(
            59,
            "typed definition diverging from canonical bytes was published",
        ));
    }
    Ok(())
}

/// Both stacks reject a canonical document naming a different definition.
///
/// Section 5.2 requires the canonical document's `definition_id` to equal the
/// publication target exactly; otherwise a revision attaches to a definition row
/// its own bytes disclaim.
async fn revision_definition_id_mismatch_rejected<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let (schema, principal) = prepare_canonical_authority(adapter, scope, 60).await?;
    // Bytes and typed struct agree with each other and disagree only with the
    // publication target, which is what isolates this from the canonical checks.
    let definition = publishable_definition(id("other-definition"), &schema, &schema);
    let canonical = crate::definition::canonical_definition_json(&definition)
        .map_err(|_| failure(60, "canonical encoding failed"))?;
    let ranks = crate::definition::canonical_topological_ranks(&definition)
        .map_err(|_| failure(60, "rank derivation failed"))?;
    let outcome = publish_canonical_pair(
        adapter,
        scope,
        &principal,
        60,
        canonical,
        PublishableDefinition {
            definition,
            topological_ranks: ranks,
        },
        &schema,
    )
    .await?;
    if !matches!(outcome, Err(StoreError::RevisionDefinitionIdMismatch)) {
        return Err(failure(
            60,
            "revision naming another definition was published",
        ));
    }
    Ok(())
}

/// Both stacks reject supplied ranks that are not the canonical lexical Kahn ranks.
///
/// Section 1.5 fixes the ranking algorithm and makes the persisted rank the
/// recovery-order key read by sections 3.4 and 4. A host-chosen rank would make
/// deterministic bulk recovery ordering host-dependent, which is precisely the
/// property that the recovery fixtures certify.
async fn revision_topological_ranks_mismatch_rejected<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let (schema, principal) = prepare_canonical_authority(adapter, scope, 61).await?;
    let definition = publishable_definition(id("conformance-definition"), &schema, &schema);
    let canonical = crate::definition::canonical_definition_json(&definition)
        .map_err(|_| failure(61, "canonical encoding failed"))?;
    // The reverse of the canonical ranking: `succeed` cannot precede `action`.
    let mut ranks = BTreeMap::new();
    ranks.insert(id("action"), TopologicalRank(1));
    ranks.insert(id("succeed"), TopologicalRank(0));
    if ranks
        == crate::definition::canonical_topological_ranks(&definition)
            .map_err(|_| failure(61, "rank derivation failed"))?
    {
        return Err(failure(61, "fixture ranks match the canonical ranking"));
    }
    let outcome = publish_canonical_pair(
        adapter,
        scope,
        &principal,
        61,
        canonical,
        PublishableDefinition {
            definition,
            topological_ranks: ranks,
        },
        &schema,
    )
    .await?;
    if !matches!(
        outcome,
        Err(StoreError::RevisionInvalid {
            code: ValidationErrorKind::Cycle,
            ..
        })
    ) {
        return Err(failure(
            61,
            "non-canonical topological ranks were published",
        ));
    }
    Ok(())
}

/// Builds run limits differing only in the two section 1.4 value ceilings.
fn terminal_output_limits(inline_bytes: u64, aggregate_bytes: u64) -> RunLimits {
    RunLimits {
        max_dynamic_node_instances: 10,
        max_total_attempts: 10,
        max_total_events: 1_000,
        max_inline_json_bytes_per_value: inline_bytes,
        max_artifacts_per_attempt: 10,
        max_aggregate_object_bytes_per_run: aggregate_bytes,
        max_run_lifetime_ms: 100_000,
    }
}

/// Drives one run to a `Started` attempt on its entry Action node.
///
/// Everything `complete_attempt` validates ahead of the section 1.4 ceilings
/// (credential, fence, active attempt, run state, diagnostics, media type) is held
/// identical across the action-output fixtures, so the only thing that differs
/// between them is the output object handed to `complete_attempt`.
async fn prepare_action_attempt_conformance<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
    limits: RunLimits,
) -> Result<(EnginePermit, CompletionCredential, WorkflowRun), ConformanceFailure> {
    let claim = adapter
        .store()
        .acquire_engine_claim(scope, id("engine-a"))
        .await
        .map_err(|_| failure(case, "engine claim failed"))?;
    let schema = adapter
        .objects()
        .put(
            scope,
            br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#,
            "application/json",
        )
        .await
        .map_err(|_| failure(case, "schema publication failed"))?;
    let principal = AuthenticatedPrincipal::mint(
        scope.clone(),
        "conformance".to_owned(),
        Vec::new(),
        schema.digest().clone(),
    )
    .map_err(|_| failure(case, "principal construction failed"))?;
    adapter
        .store()
        .create_definition(
            scope,
            CreateDefinition {
                definition_id: id("conformance-definition"),
                display_name: "conformance".to_owned(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .map_err(|_| failure(case, "definition creation failed"))?;
    let (canonical, outcome) =
        publish_with_root_input_schema(adapter, scope, &principal, &schema, &schema).await?;
    outcome.map_err(|_| failure(case, "revision publication failed"))?;
    let input = adapter
        .objects()
        .put(scope, br#"{"value":1}"#, "application/json")
        .await
        .map_err(|_| failure(case, "run input publication failed"))?;
    adapter
        .store()
        .create_run(
            scope,
            CreateRun {
                run_id: id("conformance-run"),
                definition_id: id("conformance-definition"),
                revision_hash: canonical.digest().clone(),
                input: input.clone(),
                budget_limit: CostUnits(10),
                limits,
                principal,
                idempotency_token: "terminal-output-token".to_owned(),
            },
        )
        .await
        .map_err(|_| failure(case, "run creation failed"))?;
    adapter
        .store()
        .start_run(
            scope,
            StartRun {
                permit: claim.permit.clone(),
                run_id: id("conformance-run"),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: schema.digest().clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .map_err(|_| failure(case, "run start failed"))?;
    let node = adapter
        .store()
        .get_node(scope, &id("conformance-run"), &id("action"))
        .await
        .map_err(|_| failure(case, "entry node read failed"))?;
    let claimed = adapter
        .store()
        .claim_node_attempt(
            scope,
            ClaimNodeAttempt {
                permit: claim.permit.clone(),
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
        .map_err(|_| failure(case, "attempt claim failed"))?;
    let credential = match claimed {
        ClaimNodeAttemptResult::Claimed {
            completion_credential,
            ..
        } => completion_credential,
        _ => return Err(failure(case, "claim did not create an attempt")),
    };
    let run = adapter
        .store()
        .get_run(scope, &id("conformance-run"))
        .await
        .map_err(|_| failure(case, "claimed run read failed"))?
        .run;
    Ok((claim.permit, credential, run))
}

/// Offers `output` plus `artifacts` as the prepared Action attempt's committed result.
///
/// `artifacts` must mirror the outcome's own `ArtifactOutput` list: section 7.2 pairs
/// each declared artifact with exactly one verified object, and `complete_attempt`
/// rejects a mismatched pair before it reaches the section 1.4 ceilings.
async fn offer_action_output<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    credential: CompletionCredential,
    outcome: ActionOutcome,
    output: VerifiedObjectRef,
    artifacts: Vec<VerifiedObjectRef>,
) -> Result<CompleteAttemptResult, StoreError> {
    adapter
        .store()
        .complete_attempt(
            scope,
            CompleteAttempt {
                completion_credential: credential,
                run_id: id("conformance-run"),
                node_id: id("action"),
                attempt_id: id("attempt"),
                submitted_outcome: outcome,
                objects: CompletionObjects {
                    output: Some(output),
                    artifacts,
                    diagnostics: None,
                },
            },
        )
        .await
}

/// Drives one run to a `Ready` Succeed node under caller-chosen limits.
///
/// Everything the Succeed resolution validates first is held identical across the
/// terminal-output fixtures, so the only thing that differs between them is the
/// output object handed to `resolve_terminal_node`.
async fn prepare_terminal_output_conformance<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
    limits: RunLimits,
) -> Result<(EnginePermit, Version, WorkflowRun), ConformanceFailure> {
    let (permit, credential, _) =
        prepare_action_attempt_conformance(adapter, scope, case, limits).await?;
    let action_output = adapter
        .objects()
        .put(scope, br#"{"value":1}"#, "application/json")
        .await
        .map_err(|_| failure(case, "action output publication failed"))?;
    offer_action_output(
        adapter,
        scope,
        credential,
        ActionOutcome::success(json!({"value": 1}), Vec::new(), CostUnits(1), None)
            .map_err(|_| failure(case, "completion outcome invalid"))?,
        action_output,
        Vec::new(),
    )
    .await
    .map_err(|_| failure(case, "attempt completion failed"))?;
    let terminal = adapter
        .store()
        .get_node(scope, &id("conformance-run"), &id("succeed"))
        .await
        .map_err(|_| failure(case, "terminal node read failed"))?;
    let run = adapter
        .store()
        .get_run(scope, &id("conformance-run"))
        .await
        .map_err(|_| failure(case, "prepared run read failed"))?
        .run;
    Ok((permit, terminal.version, run))
}

/// Offers `output` to the prepared Succeed node and returns the command outcome.
async fn offer_terminal_output<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    permit: EnginePermit,
    node_version: Version,
    output: VerifiedObjectRef,
) -> Result<WorkflowRun, StoreError> {
    adapter
        .store()
        .resolve_terminal_node(
            scope,
            ResolveTerminalNode {
                permit,
                run_id: id("conformance-run"),
                node_id: id("succeed"),
                expected_node_version: node_version,
                output: Some(output),
            },
        )
        .await
}

/// Asserts the N46 plus R08 pair committed nothing the contract forbids.
///
/// N46's postcondition is "no attempt/reservation" and section 1.4 charges only
/// committed values, so a rejected Succeed output must leave the node
/// non-terminal-successful with no result ref, the run without an `output_ref`,
/// and the aggregate counter exactly where preparation left it.
async fn assert_terminal_contract_failure<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
    expected_kind: NodeFailureKind,
    aggregate_before: u64,
) -> Result<(), ConformanceFailure> {
    let node = adapter
        .store()
        .get_node(scope, &id("conformance-run"), &id("succeed"))
        .await
        .map_err(|_| failure(case, "terminal node read failed"))?;
    if node.status != NodeState::ContractFailed {
        return Err(failure(case, "Succeed node did not reach ContractFailed"));
    }
    if node.failure_kind != Some(expected_kind) {
        return Err(failure(
            case,
            "Succeed node recorded the wrong failure kind",
        ));
    }
    if node.result_ref.is_some() {
        return Err(failure(case, "rejected Succeed output registered a ref"));
    }
    let run = adapter
        .store()
        .get_run(scope, &id("conformance-run"))
        .await
        .map_err(|_| failure(case, "contract-failed run read failed"))?
        .run;
    if run.status != RunState::ContractFailed {
        return Err(failure(case, "run did not reach ContractFailed"));
    }
    if run.output_ref.is_some() {
        return Err(failure(
            case,
            "rejected Succeed output was bound to the run",
        ));
    }
    if run.aggregate_object_bytes != aggregate_before {
        return Err(failure(case, "rejected Succeed output charged run bytes"));
    }
    Ok(())
}

/// A Succeed output violating the pinned root output schema is rejected.
///
///.
/// schema/limit" a precondition and N16 repeats it, so a store that commits the
/// value unvalidated publishes a run output its own revision forbids.
async fn succeed_output_schema_enforced<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let (permit, node_version, run) = prepare_terminal_output_conformance(
        adapter,
        scope,
        45,
        terminal_output_limits(10_000, 100_000),
    )
    .await?;
    let output = adapter
        .objects()
        .put(scope, br#"{"value":"one"}"#, "application/json")
        .await
        .map_err(|_| failure(45, "violating output publication failed"))?;
    let resolved = offer_terminal_output(adapter, scope, permit, node_version, output)
        .await
        .map_err(|_| {
            failure(
                45,
                "terminal resolution errored instead of failing the contract",
            )
        })?;
    if resolved.status != RunState::ContractFailed {
        return Err(failure(45, "off-schema Succeed output was committed"));
    }
    assert_terminal_contract_failure(
        adapter,
        scope,
        45,
        NodeFailureKind::RunOutputSchemaMismatch,
        run.aggregate_object_bytes,
    )
    .await
}

/// A Succeed output over `max_inline_json_bytes_per_value` is rejected.
///
/// Section 1.4 applies the value ceiling "before binding, invocation,
/// event-inline value, or output commit"; the Succeed output is an output commit.
async fn succeed_output_inline_limit_enforced<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let (permit, node_version, run) = prepare_terminal_output_conformance(
        adapter,
        scope,
        46,
        terminal_output_limits(20, 100_000),
    )
    .await?;
    // Twenty-one bytes: schema-valid, exactly one byte over the ceiling.
    let output = adapter
        .objects()
        .put(scope, br#"{"value":12345678901}"#, "application/json")
        .await
        .map_err(|_| failure(46, "oversized output publication failed"))?;
    let resolved = offer_terminal_output(adapter, scope, permit, node_version, output)
        .await
        .map_err(|_| {
            failure(
                46,
                "terminal resolution errored instead of failing the contract",
            )
        })?;
    if resolved.status != RunState::ContractFailed {
        return Err(failure(46, "oversized Succeed output was committed"));
    }
    assert_terminal_contract_failure(
        adapter,
        scope,
        46,
        NodeFailureKind::InlineJsonLimitExceeded,
        run.aggregate_object_bytes,
    )
    .await
}

/// A Succeed output exactly at `max_inline_json_bytes_per_value` is committed.
///
/// Section 1.4 states the ceiling as an inclusive maximum, so pinning the
/// accepting side of the boundary keeps the rejection above from being satisfied
/// by any over-strict check.
async fn succeed_output_inline_limit_boundary_accepted<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let (permit, node_version, run) = prepare_terminal_output_conformance(
        adapter,
        scope,
        47,
        terminal_output_limits(20, 100_000),
    )
    .await?;
    // Exactly twenty bytes.
    let output = adapter
        .objects()
        .put(scope, br#"{"value":1234567890}"#, "application/json")
        .await
        .map_err(|_| failure(47, "boundary output publication failed"))?;
    let size = output.size_bytes();
    offer_terminal_output(adapter, scope, permit, node_version, output)
        .await
        .map_err(|_| failure(47, "boundary Succeed output was rejected"))?;
    let settled = adapter
        .store()
        .get_run(scope, &id("conformance-run"))
        .await
        .map_err(|_| failure(47, "settled run read failed"))?
        .run;
    if settled.status != RunState::Succeeded || settled.output_ref.is_none() {
        return Err(failure(
            47,
            "boundary Succeed output did not finish the run",
        ));
    }
    if settled.aggregate_object_bytes != run.aggregate_object_bytes + size {
        return Err(failure(47, "committed Succeed output was not charged"));
    }
    Ok(())
}

/// A Succeed output past `max_aggregate_object_bytes_per_run` is rejected.
///
/// Section 1.4 lists "Map/Succeed outputs" as charged run data, so the last value
/// a run commits is bound by the same aggregate ceiling as every earlier one. The
/// ceiling is calibrated from a throwaway run rather than hardcoded so the case
/// pins the contract rather than an accounting implementation detail.
async fn succeed_output_aggregate_limit_enforced<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let (_, _, calibration) = prepare_terminal_output_conformance(
        adapter,
        scope,
        48,
        terminal_output_limits(10_000, 100_000),
    )
    .await?;
    // Eleven bytes of Succeed output, with room for exactly ten of them.
    let ceiling = calibration.aggregate_object_bytes + 10;
    let tight = adapter.fresh();
    let (permit, node_version, run) = prepare_terminal_output_conformance(
        &tight,
        scope,
        48,
        terminal_output_limits(10_000, ceiling),
    )
    .await?;
    let output = tight
        .objects()
        .put(scope, br#"{"value":1}"#, "application/json")
        .await
        .map_err(|_| failure(48, "output publication failed"))?;
    let resolved = offer_terminal_output(&tight, scope, permit, node_version, output)
        .await
        .map_err(|_| {
            failure(
                48,
                "terminal resolution errored instead of failing the contract",
            )
        })?;
    if resolved.status != RunState::ContractFailed {
        return Err(failure(48, "over-ceiling Succeed output was committed"));
    }
    assert_terminal_contract_failure(
        &tight,
        scope,
        48,
        NodeFailureKind::AggregateObjectLimitExceeded,
        run.aggregate_object_bytes,
    )
    .await
}

/// A Succeed output landing exactly on the aggregate ceiling is committed.
async fn succeed_output_aggregate_limit_boundary_accepted<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let (_, _, calibration) = prepare_terminal_output_conformance(
        adapter,
        scope,
        49,
        terminal_output_limits(10_000, 100_000),
    )
    .await?;
    let ceiling = calibration.aggregate_object_bytes + 11;
    let exact = adapter.fresh();
    let (permit, node_version, run) = prepare_terminal_output_conformance(
        &exact,
        scope,
        49,
        terminal_output_limits(10_000, ceiling),
    )
    .await?;
    let output = exact
        .objects()
        .put(scope, br#"{"value":1}"#, "application/json")
        .await
        .map_err(|_| failure(49, "output publication failed"))?;
    let size = output.size_bytes();
    offer_terminal_output(&exact, scope, permit, node_version, output)
        .await
        .map_err(|_| failure(49, "boundary Succeed output was rejected"))?;
    let settled = exact
        .store()
        .get_run(scope, &id("conformance-run"))
        .await
        .map_err(|_| failure(49, "settled run read failed"))?
        .run;
    if settled.status != RunState::Succeeded || settled.output_ref.is_none() {
        return Err(failure(
            49,
            "boundary Succeed output did not finish the run",
        ));
    }
    if settled.aggregate_object_bytes != run.aggregate_object_bytes + size
        || settled.aggregate_object_bytes != ceiling
    {
        return Err(failure(49, "committed Succeed output was not charged"));
    }
    Ok(())
}

/// Twenty-one canonical bytes: schema-valid, and the calibration subject of both
/// action-output ceiling cases. Neither case hardcodes that count; each reads the
/// published object's own `size_bytes` and derives its ceiling from it.
const ACTION_OUTPUT_BYTES: &[u8] = br#"{"value":12345678901}"#;

/// Asserts the A05/N21 plus R08 triple an oversized action output must produce.
///
/// N21's postcondition clears the active attempt and section 1.4 charges only
/// committed values, so a rejected output must leave the attempt and node
/// `ContractFailed` with the closed kind, no result ref, no active attempt, the
/// run `ContractFailed`, and the aggregate counter exactly where the claim left it.
async fn assert_action_output_contract_failure<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
    expected_kind: NodeFailureKind,
    expected_run_kind: RunFailureKind,
    aggregate_before: u64,
) -> Result<(), ConformanceFailure> {
    let attempt = adapter
        .store()
        .get_attempt(scope, &id("conformance-run"), &id("attempt"))
        .await
        .map_err(|_| failure(case, "attempt read failed"))?;
    if attempt.status != AttemptState::ContractFailed {
        return Err(failure(case, "attempt did not reach ContractFailed"));
    }
    if attempt.output_ref.is_some() {
        return Err(failure(case, "rejected action output registered a ref"));
    }
    let node = adapter
        .store()
        .get_node(scope, &id("conformance-run"), &id("action"))
        .await
        .map_err(|_| failure(case, "action node read failed"))?;
    if node.status != NodeState::ContractFailed {
        return Err(failure(case, "Action node did not reach ContractFailed"));
    }
    if node.failure_kind != Some(expected_kind) {
        return Err(failure(case, "Action node recorded the wrong failure kind"));
    }
    if node.active_attempt_id.is_some() {
        return Err(failure(
            case,
            "contract-failed node kept its active attempt",
        ));
    }
    if node.result_ref.is_some() {
        return Err(failure(case, "rejected action output bound a result ref"));
    }
    let run = adapter
        .store()
        .get_run(scope, &id("conformance-run"))
        .await
        .map_err(|_| failure(case, "contract-failed run read failed"))?
        .run;
    if run.status != RunState::ContractFailed {
        return Err(failure(case, "run did not reach ContractFailed"));
    }
    if run.failure_kind != Some(expected_run_kind) {
        return Err(failure(case, "run recorded the wrong failure kind"));
    }
    if run.aggregate_object_bytes != aggregate_before {
        return Err(failure(case, "rejected action output charged run bytes"));
    }
    Ok(())
}

/// A successful Action output violating its own pinned schema is rejected.
async fn action_output_schema_enforced<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let case = 68;
    let (_, credential, run) = prepare_action_attempt_conformance(
        adapter,
        scope,
        case,
        terminal_output_limits(100_000, 1_000_000),
    )
    .await?;
    let output = adapter
        .objects()
        .put(scope, br#"{"value":"one"}"#, "application/json")
        .await
        .map_err(|_| failure(case, "off-schema output publication failed"))?;
    let completed = offer_action_output(
        adapter,
        scope,
        credential,
        ActionOutcome::success(json!({"value": "one"}), Vec::new(), CostUnits(1), None)
            .map_err(|_| failure(case, "completion outcome invalid"))?,
        output,
        Vec::new(),
    )
    .await
    .map_err(|_| failure(case, "attempt completion rejected instead of applied"))?;
    if !matches!(completed, CompleteAttemptResult::TerminalRun(_)) {
        return Err(failure(case, "off-schema action output was committed"));
    }
    assert_action_output_contract_failure(
        adapter,
        scope,
        case,
        NodeFailureKind::ActionOutputSchemaMismatch,
        RunFailureKind::ActionOutputSchemaMismatch,
        run.aggregate_object_bytes,
    )
    .await
}

/// An action output over `max_inline_json_bytes_per_value` is rejected.
///
/// Section 1.4 applies the value ceiling "before binding, invocation, event-inline
/// value, or output commit"; an Action's own result is an output commit, so the
/// ceiling holds at `complete_attempt` and not merely at whatever later node
/// happens to bind the value. Escalation E7.
async fn action_output_inline_limit_enforced<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let probe = adapter.fresh();
    let sized = probe
        .objects()
        .put(scope, ACTION_OUTPUT_BYTES, "application/json")
        .await
        .map_err(|_| failure(62, "calibration output publication failed"))?;
    // One byte under the value the action commits, and still above every earlier
    // value this run publishes, so only the output commit can trip the ceiling.
    let ceiling = sized.size_bytes() - 1;
    let (_, credential, run) = prepare_action_attempt_conformance(
        adapter,
        scope,
        62,
        terminal_output_limits(ceiling, 100_000),
    )
    .await?;
    let output = adapter
        .objects()
        .put(scope, ACTION_OUTPUT_BYTES, "application/json")
        .await
        .map_err(|_| failure(62, "oversized output publication failed"))?;
    let completed = offer_action_output(
        adapter,
        scope,
        credential,
        ActionOutcome::success(
            json!({"value": 12345678901u64}),
            Vec::new(),
            CostUnits(1),
            None,
        )
        .map_err(|_| failure(62, "completion outcome invalid"))?,
        output,
        Vec::new(),
    )
    .await
    .map_err(|_| {
        failure(
            62,
            "attempt completion errored instead of failing the contract",
        )
    })?;
    match completed {
        CompleteAttemptResult::TerminalRun(terminal) => {
            if terminal.status != RunState::ContractFailed {
                return Err(failure(62, "oversized action output was committed"));
            }
        }
        _ => return Err(failure(62, "oversized action output was committed")),
    }
    assert_action_output_contract_failure(
        adapter,
        scope,
        62,
        NodeFailureKind::InlineJsonLimitExceeded,
        RunFailureKind::InlineJsonLimitExceeded,
        run.aggregate_object_bytes,
    )
    .await
}

/// An action output exactly at `max_inline_json_bytes_per_value` is committed.
///
/// Section 1.4 states the ceiling as an inclusive maximum. Without this the case
/// above is satisfied by any check strict enough to reject, including one placed
/// on the wrong value or one off by a byte.
async fn action_output_inline_limit_boundary_accepted<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let probe = adapter.fresh();
    let sized = probe
        .objects()
        .put(scope, ACTION_OUTPUT_BYTES, "application/json")
        .await
        .map_err(|_| failure(63, "calibration output publication failed"))?;
    let ceiling = sized.size_bytes();
    let (_, credential, run) = prepare_action_attempt_conformance(
        adapter,
        scope,
        63,
        terminal_output_limits(ceiling, 100_000),
    )
    .await?;
    let output = adapter
        .objects()
        .put(scope, ACTION_OUTPUT_BYTES, "application/json")
        .await
        .map_err(|_| failure(63, "boundary output publication failed"))?;
    let size = output.size_bytes();
    offer_action_output(
        adapter,
        scope,
        credential,
        ActionOutcome::success(
            json!({"value": 12345678901u64}),
            Vec::new(),
            CostUnits(1),
            None,
        )
        .map_err(|_| failure(63, "completion outcome invalid"))?,
        output,
        Vec::new(),
    )
    .await
    .map_err(|_| failure(63, "boundary action output was rejected"))?;
    let node = adapter
        .store()
        .get_node(scope, &id("conformance-run"), &id("action"))
        .await
        .map_err(|_| failure(63, "action node read failed"))?;
    if node.status != NodeState::Succeeded || node.result_ref.is_none() {
        return Err(failure(63, "boundary action output did not succeed"));
    }
    let settled = adapter
        .store()
        .get_run(scope, &id("conformance-run"))
        .await
        .map_err(|_| failure(63, "settled run read failed"))?
        .run;
    if settled.status != RunState::Running {
        return Err(failure(63, "boundary action output terminated the run"));
    }
    if settled.aggregate_object_bytes != run.aggregate_object_bytes + size {
        return Err(failure(63, "committed action output was not charged"));
    }
    Ok(())
}

/// Publishes the two distinct artifact objects the artifact-ceiling cases submit.
///
/// The bytes differ so the pair is two objects rather than one deduplicated digest,
/// which is what makes the section 1.4 per-attempt *count* observable at all. Both
/// are tiny, so the per-value and aggregate ceilings the cases leave wide open
/// cannot be what rejects them.
async fn action_artifact_pair<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
) -> Result<Vec<VerifiedObjectRef>, ConformanceFailure> {
    let mut objects = Vec::new();
    for bytes in [br#"{"artifact":1}"#, br#"{"artifact":2}"#] {
        objects.push(
            adapter
                .objects()
                .put(scope, bytes, "application/json")
                .await
                .map_err(|_| failure(case, "artifact publication failed"))?,
        );
    }
    Ok(objects)
}

/// Builds the success outcome declaring one `ArtifactOutput` per submitted object.
fn action_success_with_artifacts(
    case: usize,
    artifacts: &[VerifiedObjectRef],
) -> Result<ActionOutcome, ConformanceFailure> {
    ActionOutcome::success(
        json!({"value": 1}),
        artifacts
            .iter()
            .map(|object| ArtifactOutput {
                media_type: "application/json".to_owned(),
                object: object.clone(),
            })
            .collect(),
        CostUnits(1),
        None,
    )
    .map_err(|_| failure(case, "completion outcome invalid"))
}

/// An accepted completion over `max_artifacts_per_attempt` is rejected.
///
/// Section 1.4 bounds artifacts per attempt and lists the ceiling among those
/// enforced "before accepted action completion", so `complete_attempt` is the call
/// site that must hold it: no later node ever revisits an attempt's artifact count.
/// The per-value and aggregate ceilings are left wide open here, so the count is the
/// only ceiling in the chain the submission can breach, and the recorded closed kind
/// confirms which of the three fired.
async fn action_artifacts_per_attempt_limit_enforced<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let (_, credential, run) = prepare_action_attempt_conformance(
        adapter,
        scope,
        64,
        RunLimits {
            max_artifacts_per_attempt: 1,
            ..terminal_output_limits(100_000, 1_000_000)
        },
    )
    .await?;
    let artifacts = action_artifact_pair(adapter, scope, 64).await?;
    let output = adapter
        .objects()
        .put(scope, br#"{"value":1}"#, "application/json")
        .await
        .map_err(|_| failure(64, "output publication failed"))?;
    let completed = offer_action_output(
        adapter,
        scope,
        credential,
        action_success_with_artifacts(64, &artifacts)?,
        output,
        artifacts,
    )
    .await
    .map_err(|_| {
        failure(
            64,
            "attempt completion errored instead of failing the contract",
        )
    })?;
    match completed {
        CompleteAttemptResult::TerminalRun(terminal) => {
            if terminal.status != RunState::ContractFailed {
                return Err(failure(64, "over-count artifact set was committed"));
            }
        }
        _ => return Err(failure(64, "over-count artifact set was committed")),
    }
    assert_action_output_contract_failure(
        adapter,
        scope,
        64,
        NodeFailureKind::ArtifactsPerAttemptLimitExceeded,
        RunFailureKind::ArtifactsPerAttemptLimitExceeded,
        run.aggregate_object_bytes,
    )
    .await
}

/// An accepted completion at exactly `max_artifacts_per_attempt` is committed.
///
/// Section 1.4 states the ceiling as an inclusive maximum. Without this the case
/// above is satisfied by any check strict enough to reject, including one off by one
/// or one that rejects every artifact-bearing completion outright.
async fn action_artifacts_per_attempt_limit_boundary_accepted<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let (_, credential, run) = prepare_action_attempt_conformance(
        adapter,
        scope,
        65,
        RunLimits {
            max_artifacts_per_attempt: 2,
            ..terminal_output_limits(100_000, 1_000_000)
        },
    )
    .await?;
    let artifacts = action_artifact_pair(adapter, scope, 65).await?;
    let output = adapter
        .objects()
        .put(scope, br#"{"value":1}"#, "application/json")
        .await
        .map_err(|_| failure(65, "output publication failed"))?;
    // Section 1.4 charges the output and every artifact against the run aggregate.
    let charged = artifacts.iter().fold(output.size_bytes(), |total, object| {
        total + object.size_bytes()
    });
    offer_action_output(
        adapter,
        scope,
        credential,
        action_success_with_artifacts(65, &artifacts)?,
        output,
        artifacts,
    )
    .await
    .map_err(|_| failure(65, "boundary artifact set was rejected"))?;
    let node = adapter
        .store()
        .get_node(scope, &id("conformance-run"), &id("action"))
        .await
        .map_err(|_| failure(65, "action node read failed"))?;
    if node.status != NodeState::Succeeded || node.result_ref.is_none() {
        return Err(failure(65, "boundary artifact set did not succeed"));
    }
    let attempt = adapter
        .store()
        .get_attempt(scope, &id("conformance-run"), &id("attempt"))
        .await
        .map_err(|_| failure(65, "attempt read failed"))?;
    if attempt.artifact_refs.len() != 2 {
        return Err(failure(65, "boundary artifact set was not registered"));
    }
    let settled = adapter
        .store()
        .get_run(scope, &id("conformance-run"))
        .await
        .map_err(|_| failure(65, "settled run read failed"))?
        .run;
    if settled.status != RunState::Running {
        return Err(failure(65, "boundary artifact set terminated the run"));
    }
    if settled.aggregate_object_bytes != run.aggregate_object_bytes + charged {
        return Err(failure(65, "committed artifacts were not charged"));
    }
    Ok(())
}

/// An accepted completion past `max_aggregate_object_bytes_per_run` is rejected.
///
/// Section 1.4 charges committed action outputs against the run aggregate, so the
/// ceiling binds at `complete_attempt` and not merely at the Map or Succeed commits
/// the rest of the suite covers. The ceiling is calibrated from a throwaway run
/// rather than hardcoded, so the case pins the contract and not an accounting
/// detail. The per-value ceiling is left far above the submitted output and no
/// artifacts are submitted, so neither earlier check in the chain can fire; the
/// recorded closed kind confirms which one did.
async fn action_output_aggregate_limit_enforced<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let probe = adapter.fresh();
    let (_, _, calibration) = prepare_action_attempt_conformance(
        &probe,
        scope,
        66,
        terminal_output_limits(100_000, 1_000_000),
    )
    .await?;
    let sized = probe
        .objects()
        .put(scope, ACTION_OUTPUT_BYTES, "application/json")
        .await
        .map_err(|_| failure(66, "calibration output publication failed"))?;
    // Room for every byte the prepared run already charged, and one short of the
    // output it is about to commit.
    let ceiling = calibration.aggregate_object_bytes + sized.size_bytes() - 1;
    let tight = adapter.fresh();
    let (_, credential, run) = prepare_action_attempt_conformance(
        &tight,
        scope,
        66,
        terminal_output_limits(100_000, ceiling),
    )
    .await?;
    let output = tight
        .objects()
        .put(scope, ACTION_OUTPUT_BYTES, "application/json")
        .await
        .map_err(|_| failure(66, "output publication failed"))?;
    let completed = offer_action_output(
        &tight,
        scope,
        credential,
        ActionOutcome::success(
            json!({"value": 12345678901u64}),
            Vec::new(),
            CostUnits(1),
            None,
        )
        .map_err(|_| failure(66, "completion outcome invalid"))?,
        output,
        Vec::new(),
    )
    .await
    .map_err(|_| {
        failure(
            66,
            "attempt completion errored instead of failing the contract",
        )
    })?;
    match completed {
        CompleteAttemptResult::TerminalRun(terminal) => {
            if terminal.status != RunState::ContractFailed {
                return Err(failure(66, "over-ceiling action output was committed"));
            }
        }
        _ => return Err(failure(66, "over-ceiling action output was committed")),
    }
    assert_action_output_contract_failure(
        &tight,
        scope,
        66,
        NodeFailureKind::AggregateObjectLimitExceeded,
        RunFailureKind::AggregateObjectLimitExceeded,
        run.aggregate_object_bytes,
    )
    .await
}

/// An action output landing exactly on the aggregate ceiling is committed.
///
/// Section 1.4 states the ceiling as an inclusive maximum, so pinning the accepting
/// side keeps the rejection above from being satisfied by a check placed a byte
/// early or on the wrong quantity.
async fn action_output_aggregate_limit_boundary_accepted<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let probe = adapter.fresh();
    let (_, _, calibration) = prepare_action_attempt_conformance(
        &probe,
        scope,
        67,
        terminal_output_limits(100_000, 1_000_000),
    )
    .await?;
    let sized = probe
        .objects()
        .put(scope, ACTION_OUTPUT_BYTES, "application/json")
        .await
        .map_err(|_| failure(67, "calibration output publication failed"))?;
    let ceiling = calibration.aggregate_object_bytes + sized.size_bytes();
    let exact = adapter.fresh();
    let (_, credential, run) = prepare_action_attempt_conformance(
        &exact,
        scope,
        67,
        terminal_output_limits(100_000, ceiling),
    )
    .await?;
    let output = exact
        .objects()
        .put(scope, ACTION_OUTPUT_BYTES, "application/json")
        .await
        .map_err(|_| failure(67, "boundary output publication failed"))?;
    let size = output.size_bytes();
    offer_action_output(
        &exact,
        scope,
        credential,
        ActionOutcome::success(
            json!({"value": 12345678901u64}),
            Vec::new(),
            CostUnits(1),
            None,
        )
        .map_err(|_| failure(67, "completion outcome invalid"))?,
        output,
        Vec::new(),
    )
    .await
    .map_err(|_| failure(67, "boundary action output was rejected"))?;
    let node = exact
        .store()
        .get_node(scope, &id("conformance-run"), &id("action"))
        .await
        .map_err(|_| failure(67, "action node read failed"))?;
    if node.status != NodeState::Succeeded || node.result_ref.is_none() {
        return Err(failure(67, "boundary action output did not succeed"));
    }
    let settled = exact
        .store()
        .get_run(scope, &id("conformance-run"))
        .await
        .map_err(|_| failure(67, "settled run read failed"))?
        .run;
    if settled.status != RunState::Running {
        return Err(failure(67, "boundary action output terminated the run"));
    }
    if settled.aggregate_object_bytes != run.aggregate_object_bytes + size
        || settled.aggregate_object_bytes != ceiling
    {
        return Err(failure(67, "committed action output was not charged"));
    }
    Ok(())
}

/// Canonical single-child Map aggregate: the JCS array of the one child output.
const MAP_AGGREGATE_BYTES: &[u8] = br#"[{"item":0}]"#;

/// Drives one single-item Map to `WaitingChildren` with its only child Succeeded.
///
/// Everything `complete_map` validates ahead of the section 1.4 ceilings (fence,
/// CAS, child completeness, identity recomputation, canonical form, per-child
/// digest equality) is held identical across the aggregate fixtures, so the only
/// thing that differs between them is the run's limits.
async fn prepare_map_aggregate_conformance<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
    limits: RunLimits,
) -> Result<(EnginePermit, Version, WorkflowRun), ConformanceFailure> {
    let fixture = prepare_map_conformance_with_limits(
        adapter,
        scope,
        case,
        1,
        1,
        RetryPolicy {
            max_attempts: 1,
            backoff: BackoffPolicy::Fixed { delay_ms: 0 },
        },
        limits,
    )
    .await?;
    let run_id = id(&format!("map-run-{case}"));
    adapter
        .store()
        .expand_map(
            scope,
            ExpandMap {
                permit: fixture.permit.clone(),
                run_id: run_id.clone(),
                map_node_id: id("map"),
                expected_node_version: Version(1),
                input: fixture.input.clone(),
                ordered_items: fixture.ordered_items.clone(),
                expansion_digest: fixture.expansion_digest.clone(),
            },
        )
        .await
        .map_err(|_| failure(case, "aggregate Map expansion failed"))?;
    let child_id = fixture.ordered_items[0].child_id.clone();
    let child = adapter
        .store()
        .get_node(scope, &run_id, &child_id)
        .await
        .map_err(|_| failure(case, "aggregate Map child read failed"))?;
    let claimed = adapter
        .store()
        .claim_node_attempt(
            scope,
            ClaimNodeAttempt {
                permit: fixture.permit.clone(),
                run_id: run_id.clone(),
                node_id: child_id.clone(),
                expected_node_version: child.version,
                attempt_id: id("map-aggregate-attempt"),
                worker_id: id("worker"),
                bound_input: fixture.input.clone(),
                binding_derivation_digest: fixture.input.digest().clone(),
            },
        )
        .await
        .map_err(|_| failure(case, "aggregate Map child claim failed"))?;
    let credential = match claimed {
        ClaimNodeAttemptResult::Claimed {
            completion_credential,
            ..
        } => completion_credential,
        _ => return Err(failure(case, "aggregate Map child was not claimed")),
    };
    let child_output = adapter
        .objects()
        .put(scope, br#"{"item":0}"#, "application/json")
        .await
        .map_err(|_| failure(case, "aggregate Map child output publication failed"))?;
    adapter
        .store()
        .complete_attempt(
            scope,
            CompleteAttempt {
                completion_credential: credential,
                run_id: run_id.clone(),
                node_id: child_id,
                attempt_id: id("map-aggregate-attempt"),
                submitted_outcome: ActionOutcome::success(
                    json!({"item": 0}),
                    Vec::new(),
                    CostUnits(1),
                    None,
                )
                .map_err(|_| failure(case, "aggregate Map child outcome invalid"))?,
                objects: CompletionObjects {
                    output: Some(child_output),
                    artifacts: Vec::new(),
                    diagnostics: None,
                },
            },
        )
        .await
        .map_err(|_| failure(case, "aggregate Map child completion failed"))?;
    let map = adapter
        .store()
        .get_node(scope, &run_id, &id("map"))
        .await
        .map_err(|_| failure(case, "aggregate Map parent read failed"))?;
    if map.status != NodeState::WaitingChildren {
        return Err(failure(case, "aggregate Map parent is not WaitingChildren"));
    }
    let run = adapter
        .store()
        .get_run(scope, &run_id)
        .await
        .map_err(|_| failure(case, "aggregate Map run read failed"))?
        .run;
    Ok((fixture.permit, map.version, run))
}

/// Offers `aggregate` to the prepared Map node and returns the command outcome.
async fn offer_map_aggregate<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
    permit: EnginePermit,
    node_version: Version,
    aggregate: VerifiedObjectRef,
) -> Result<NodeRun, StoreError> {
    adapter
        .store()
        .complete_map(
            scope,
            CompleteMap {
                permit,
                run_id: id(&format!("map-run-{case}")),
                map_node_id: id("map"),
                expected_node_version: node_version,
                aggregate,
            },
        )
        .await
}

/// Asserts the N65 plus R08 pair committed nothing the contract forbids.
///
/// N65's postcondition is "do not register aggregate ref; R08 with
/// `AggregateObjectLimitExceeded` or output contract kind; cancel children", and
/// section 1.4 charges only committed values, so a rejected aggregate must leave
/// the Map node without a result ref, the run `ContractFailed` with the cascade
/// run, and the aggregate counter exactly where preparation left it.
async fn assert_map_contract_failure<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
    expected_kind: NodeFailureKind,
    aggregate_before: u64,
) -> Result<(), ConformanceFailure> {
    let run_id = id(&format!("map-run-{case}"));
    let map = adapter
        .store()
        .get_node(scope, &run_id, &id("map"))
        .await
        .map_err(|_| failure(case, "contract-failed Map node read failed"))?;
    if map.status != NodeState::ContractFailed {
        return Err(failure(case, "Map node did not reach ContractFailed"));
    }
    if map.failure_kind != Some(expected_kind) {
        return Err(failure(case, "Map node recorded the wrong failure kind"));
    }
    if map.result_ref.is_some() {
        return Err(failure(case, "rejected Map aggregate registered a ref"));
    }
    // The cancellation half of N65: the only children a rejected aggregate can
    // have are already Succeeded (complete_map refuses otherwise), so what the
    // cascade must prove here is that it left no node of the run runnable.
    let nodes = adapter
        .store()
        .list_nodes(
            scope,
            &run_id,
            PageRequest {
                cursor: None,
                page_size: 100,
            },
        )
        .await
        .map_err(|_| failure(case, "contract-failed Map nodes read failed"))?
        .items;
    if nodes.iter().any(|node| {
        matches!(
            node.status,
            NodeState::Pending
                | NodeState::Ready
                | NodeState::Running
                | NodeState::RetryWaiting
                | NodeState::BudgetWaiting
                | NodeState::WaitingApproval
                | NodeState::WaitingChildren
                | NodeState::BlockedIncompatible
        )
    }) {
        return Err(failure(case, "rejected Map aggregate left a live node"));
    }
    let run = adapter
        .store()
        .get_run(scope, &run_id)
        .await
        .map_err(|_| failure(case, "contract-failed Map run read failed"))?
        .run;
    if run.status != RunState::ContractFailed {
        return Err(failure(case, "run did not reach ContractFailed"));
    }
    if run.aggregate_object_bytes != aggregate_before {
        return Err(failure(case, "rejected Map aggregate charged run bytes"));
    }
    Ok(())
}

/// A Map aggregate over `max_inline_json_bytes_per_value` is rejected.
///
/// Section 1.4 applies the value ceiling "before binding, invocation,
/// event-inline value, or output commit"; the aggregate is an output commit, and
/// N65 names the output contract kind as one of its two failure kinds.
async fn map_aggregate_inline_limit_enforced<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let aggregate = adapter
        .objects()
        .put(scope, MAP_AGGREGATE_BYTES, "application/json")
        .await
        .map_err(|_| failure(50, "aggregate publication failed"))?;
    // One byte under the aggregate: still above every earlier value this run
    // commits, so only the aggregate can trip the ceiling.
    let inline = aggregate.size_bytes() - 1;
    let (permit, node_version, run) = prepare_map_aggregate_conformance(
        adapter,
        scope,
        50,
        terminal_output_limits(inline, 100_000),
    )
    .await?;
    let node = offer_map_aggregate(adapter, scope, 50, permit, node_version, aggregate)
        .await
        .map_err(|_| failure(50, "Map completion errored instead of failing the contract"))?;
    if node.status != NodeState::ContractFailed {
        return Err(failure(50, "oversized Map aggregate was committed"));
    }
    assert_map_contract_failure(
        adapter,
        scope,
        50,
        NodeFailureKind::InlineJsonLimitExceeded,
        run.aggregate_object_bytes,
    )
    .await
}

/// A Map aggregate exactly at `max_inline_json_bytes_per_value` is committed.
///
/// Section 1.4 states the ceiling as an inclusive maximum, so pinning the
/// accepting side keeps the rejection above from being satisfied by an
/// over-strict check.
async fn map_aggregate_inline_limit_boundary_accepted<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let aggregate = adapter
        .objects()
        .put(scope, MAP_AGGREGATE_BYTES, "application/json")
        .await
        .map_err(|_| failure(51, "aggregate publication failed"))?;
    let size = aggregate.size_bytes();
    let (permit, node_version, run) = prepare_map_aggregate_conformance(
        adapter,
        scope,
        51,
        terminal_output_limits(size, 100_000),
    )
    .await?;
    let node = offer_map_aggregate(adapter, scope, 51, permit, node_version, aggregate)
        .await
        .map_err(|_| failure(51, "boundary Map aggregate was rejected"))?;
    if node.status != NodeState::Succeeded || node.result_ref.is_none() {
        return Err(failure(51, "boundary Map aggregate did not succeed"));
    }
    let settled = adapter
        .store()
        .get_run(scope, &id("map-run-51"))
        .await
        .map_err(|_| failure(51, "settled run read failed"))?
        .run;
    if settled.aggregate_object_bytes != run.aggregate_object_bytes + size {
        return Err(failure(51, "committed Map aggregate was not charged"));
    }
    Ok(())
}

/// A Map aggregate past `max_aggregate_object_bytes_per_run` is rejected.
///
/// Section 1.4 lists Map outputs as charged run data and N65 names
/// `AggregateObjectLimitExceeded` explicitly. The ceiling is calibrated from a
/// throwaway run rather than hardcoded so the case pins the contract rather than
/// an accounting implementation detail.
async fn map_aggregate_object_limit_enforced<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let (_, _, calibration) = prepare_map_aggregate_conformance(
        adapter,
        scope,
        52,
        terminal_output_limits(10_000, 1_000_000),
    )
    .await?;
    let tight = adapter.fresh();
    let aggregate = tight
        .objects()
        .put(scope, MAP_AGGREGATE_BYTES, "application/json")
        .await
        .map_err(|_| failure(52, "aggregate publication failed"))?;
    // Room for everything the identical run already charged, minus one byte of
    // the aggregate itself.
    let ceiling = calibration.aggregate_object_bytes + aggregate.size_bytes() - 1;
    let (permit, node_version, run) = prepare_map_aggregate_conformance(
        &tight,
        scope,
        52,
        terminal_output_limits(10_000, ceiling),
    )
    .await?;
    let node = offer_map_aggregate(&tight, scope, 52, permit, node_version, aggregate)
        .await
        .map_err(|_| failure(52, "Map completion errored instead of failing the contract"))?;
    if node.status != NodeState::ContractFailed {
        return Err(failure(52, "over-ceiling Map aggregate was committed"));
    }
    assert_map_contract_failure(
        &tight,
        scope,
        52,
        NodeFailureKind::AggregateObjectLimitExceeded,
        run.aggregate_object_bytes,
    )
    .await
}

/// A Map aggregate landing exactly on the aggregate ceiling is committed.
async fn map_aggregate_object_limit_boundary_accepted<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let (_, _, calibration) = prepare_map_aggregate_conformance(
        adapter,
        scope,
        53,
        terminal_output_limits(10_000, 1_000_000),
    )
    .await?;
    let exact = adapter.fresh();
    let aggregate = exact
        .objects()
        .put(scope, MAP_AGGREGATE_BYTES, "application/json")
        .await
        .map_err(|_| failure(53, "aggregate publication failed"))?;
    let size = aggregate.size_bytes();
    let ceiling = calibration.aggregate_object_bytes + size;
    let (permit, node_version, run) = prepare_map_aggregate_conformance(
        &exact,
        scope,
        53,
        terminal_output_limits(10_000, ceiling),
    )
    .await?;
    let node = offer_map_aggregate(&exact, scope, 53, permit, node_version, aggregate)
        .await
        .map_err(|_| failure(53, "boundary Map aggregate was rejected"))?;
    if node.status != NodeState::Succeeded || node.result_ref.is_none() {
        return Err(failure(53, "boundary Map aggregate did not succeed"));
    }
    let settled = exact
        .store()
        .get_run(scope, &id("map-run-53"))
        .await
        .map_err(|_| failure(53, "settled run read failed"))?
        .run;
    if settled.aggregate_object_bytes != run.aggregate_object_bytes + size
        || settled.aggregate_object_bytes != ceiling
    {
        return Err(failure(53, "committed Map aggregate was not charged"));
    }
    Ok(())
}

/// Durable watermark taken immediately before an applied-failure command.
///
/// Section 5.5's `ContractValidationApplied` and `RunLimitApplied` claim the
/// transition is already durable, so proving them needs a before/after comparison
/// on committed rows rather than on the returned value. Versions alone are not
/// enough either: a store that silently bumped a version while discarding the
/// transition would pass a version-only check, so every assertion below pairs the
/// watermark with the exact closed status and failure kind.
struct AppliedFailureWatermark {
    node_version: Version,
    run_version: Version,
    last_event_seq: u64,
    aggregate_object_bytes: u64,
    dynamic_node_count: u64,
    total_attempt_count: u64,
}

async fn applied_failure_watermark<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
    run_id: &Id,
    node_id: &Id,
) -> Result<AppliedFailureWatermark, ConformanceFailure> {
    let node = adapter
        .store()
        .get_node(scope, run_id, node_id)
        .await
        .map_err(|_| failure(case, "watermark node read failed"))?;
    let run = adapter
        .store()
        .get_run(scope, run_id)
        .await
        .map_err(|_| failure(case, "watermark run read failed"))?
        .run;
    Ok(AppliedFailureWatermark {
        node_version: node.version,
        run_version: run.version,
        last_event_seq: run.last_event_seq,
        aggregate_object_bytes: run.aggregate_object_bytes,
        dynamic_node_count: run.dynamic_node_count,
        total_attempt_count: run.total_attempt_count,
    })
}

/// Asserts the node/run halves of an applied N46/N64/N67 plus R08.
async fn assert_applied_contract_failure<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
    run_id: &Id,
    node_id: &Id,
    node_kind: NodeFailureKind,
    run_kind: RunFailureKind,
    before: &AppliedFailureWatermark,
) -> Result<(NodeRun, WorkflowRun), ConformanceFailure> {
    let node = adapter
        .store()
        .get_node(scope, run_id, node_id)
        .await
        .map_err(|_| failure(case, "applied-failure node read failed"))?;
    let run = adapter
        .store()
        .get_run(scope, run_id)
        .await
        .map_err(|_| failure(case, "applied-failure run read failed"))?
        .run;
    if node.status != NodeState::ContractFailed {
        return Err(failure(case, "node did not durably reach ContractFailed"));
    }
    if node.failure_kind != Some(node_kind) {
        return Err(failure(case, "node failure kind was not the closed kind"));
    }
    if run.status != RunState::ContractFailed {
        return Err(failure(case, "run did not durably reach ContractFailed"));
    }
    if run.failure_kind != Some(run_kind) {
        return Err(failure(case, "run failure kind was not the closed kind"));
    }
    if node.version <= before.node_version || run.version <= before.run_version {
        return Err(failure(
            case,
            "applied failure did not advance row versions",
        ));
    }
    if run.last_event_seq <= before.last_event_seq {
        return Err(failure(case, "applied failure appended no event batch"));
    }
    if run.finished_at.is_none() {
        return Err(failure(case, "terminalized run kept a null finish time"));
    }
    Ok((node, run))
}

/// Section 5.3 `expand_map`: a Map input that is not a canonical JSON array applies
/// N46/R08 with `MapInputInvalid` and reports it as `ContractValidationApplied`.
async fn expand_map_input_invalid_applied<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let case = 54;
    let fixture = prepare_map_conformance(
        adapter,
        scope,
        case,
        2,
        1,
        RetryPolicy {
            max_attempts: 1,
            backoff: BackoffPolicy::Fixed { delay_ms: 0 },
        },
    )
    .await?;
    let run_id = id(&format!("map-run-{case}"));
    let before = applied_failure_watermark(adapter, scope, case, &run_id, &id("map")).await?;
    let object_input = adapter
        .objects()
        .put(scope, b"{}", "application/json")
        .await
        .map_err(|_| failure(case, "non-array Map input publication failed"))?;
    match adapter
        .store()
        .expand_map(
            scope,
            ExpandMap {
                permit: fixture.permit.clone(),
                run_id: run_id.clone(),
                map_node_id: id("map"),
                expected_node_version: before.node_version,
                input: object_input,
                ordered_items: fixture.ordered_items.clone(),
                expansion_digest: fixture.expansion_digest.clone(),
            },
        )
        .await
    {
        Err(StoreError::ContractValidationApplied { code }) if code == "MapInputInvalid" => {}
        _ => return Err(failure(case, "non-array Map input was not rejected")),
    }
    let (node, run) = assert_applied_contract_failure(
        adapter,
        scope,
        case,
        &run_id,
        &id("map"),
        NodeFailureKind::MapInputInvalid,
        RunFailureKind::MapInputInvalid,
        &before,
    )
    .await?;
    if node.map_input_ref.is_some()
        || node.map_expansion_digest.is_some()
        || node.map_child_count.is_some()
    {
        return Err(failure(case, "rejected expansion left Map state behind"));
    }
    if run.aggregate_object_bytes != before.aggregate_object_bytes
        || run.dynamic_node_count != before.dynamic_node_count
    {
        return Err(failure(case, "rejected expansion charged run counters"));
    }
    Ok(())
}

/// Section 5.3 `expand_map`: exceeding `max_dynamic_node_instances` applies N46/R08
/// and reports `RunLimitApplied`, registering no child and no Map input ref.
async fn expand_map_dynamic_node_limit_applied<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let case = 55;
    let fixture = prepare_map_conformance_with_limits(
        adapter,
        scope,
        case,
        2,
        1,
        RetryPolicy {
            max_attempts: 1,
            backoff: BackoffPolicy::Fixed { delay_ms: 0 },
        },
        RunLimits {
            max_dynamic_node_instances: 1,
            max_total_attempts: 200,
            max_total_events: 2_000,
            max_inline_json_bytes_per_value: 10_000,
            max_artifacts_per_attempt: 10,
            max_aggregate_object_bytes_per_run: 1_000_000,
            max_run_lifetime_ms: 31_536_000_000,
        },
    )
    .await?;
    let run_id = id(&format!("map-run-{case}"));
    let before = applied_failure_watermark(adapter, scope, case, &run_id, &id("map")).await?;
    match adapter
        .store()
        .expand_map(
            scope,
            ExpandMap {
                permit: fixture.permit.clone(),
                run_id: run_id.clone(),
                map_node_id: id("map"),
                expected_node_version: before.node_version,
                input: fixture.input.clone(),
                ordered_items: fixture.ordered_items.clone(),
                expansion_digest: fixture.expansion_digest.clone(),
            },
        )
        .await
    {
        Err(StoreError::RunLimitApplied { code }) if code == "RunDynamicNodeLimitExceeded" => {}
        _ => return Err(failure(case, "dynamic-node limit was not enforced")),
    }
    let (node, run) = assert_applied_contract_failure(
        adapter,
        scope,
        case,
        &run_id,
        &id("map"),
        NodeFailureKind::RunDynamicNodeLimitExceeded,
        RunFailureKind::RunDynamicNodeLimitExceeded,
        &before,
    )
    .await?;
    if node.map_expansion_digest.is_some() || run.dynamic_node_count != before.dynamic_node_count {
        return Err(failure(case, "over-limit expansion partially applied"));
    }
    for item in &fixture.ordered_items {
        if adapter
            .store()
            .get_node(scope, &run_id, &item.child_id)
            .await
            .is_ok()
        {
            return Err(failure(case, "over-limit expansion created a child"));
        }
    }
    Ok(())
}

/// Section 5.3 `claim_node_attempt`: a bound input over
/// `max_inline_json_bytes_per_value` commits the terminal batch and creates no
/// attempt, reporting the `RunLimitApplied` result rather than an error.
async fn claim_inline_json_limit_applied<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let case = 56;
    let fixture = prepare_map_conformance_with_limits(
        adapter,
        scope,
        case,
        1,
        1,
        RetryPolicy {
            max_attempts: 1,
            backoff: BackoffPolicy::Fixed { delay_ms: 0 },
        },
        RunLimits {
            max_dynamic_node_instances: 100,
            max_total_attempts: 200,
            max_total_events: 2_000,
            max_inline_json_bytes_per_value: 256,
            max_artifacts_per_attempt: 10,
            max_aggregate_object_bytes_per_run: 1_000_000,
            max_run_lifetime_ms: 31_536_000_000,
        },
    )
    .await?;
    let run_id = id(&format!("map-run-{case}"));
    adapter
        .store()
        .expand_map(
            scope,
            ExpandMap {
                permit: fixture.permit.clone(),
                run_id: run_id.clone(),
                map_node_id: id("map"),
                expected_node_version: Version(1),
                input: fixture.input.clone(),
                ordered_items: fixture.ordered_items.clone(),
                expansion_digest: fixture.expansion_digest.clone(),
            },
        )
        .await
        .map_err(|_| failure(case, "Map expansion failed"))?;
    let child_id = fixture.ordered_items[0].child_id.clone();
    let before = applied_failure_watermark(adapter, scope, case, &run_id, &child_id).await?;
    let oversized = serde_jcs::to_vec(&json!({ "item": "x".repeat(512) }))
        .map_err(|_| failure(case, "oversized bound input encoding failed"))?;
    let bound_input = adapter
        .objects()
        .put(scope, &oversized, "application/json")
        .await
        .map_err(|_| failure(case, "oversized bound input publication failed"))?;
    match adapter
        .store()
        .claim_node_attempt(
            scope,
            ClaimNodeAttempt {
                permit: fixture.permit.clone(),
                run_id: run_id.clone(),
                node_id: child_id.clone(),
                expected_node_version: before.node_version,
                attempt_id: id("oversized-attempt"),
                worker_id: id("worker"),
                bound_input,
                binding_derivation_digest: fixture.expansion_digest.clone(),
            },
        )
        .await
    {
        Ok(ClaimNodeAttemptResult::RunLimitApplied(_)) => {}
        _ => return Err(failure(case, "inline JSON limit was not applied")),
    }
    let (node, run) = assert_applied_contract_failure(
        adapter,
        scope,
        case,
        &run_id,
        &child_id,
        NodeFailureKind::InlineJsonLimitExceeded,
        RunFailureKind::InlineJsonLimitExceeded,
        &before,
    )
    .await?;
    // N46's postcondition is "no attempt/reservation".
    if node.active_attempt_id.is_some() || node.attempt_count != 0 {
        return Err(failure(case, "rejected claim left an attempt on the node"));
    }
    if run.total_attempt_count != before.total_attempt_count || run.budget_reserved != CostUnits(0)
    {
        return Err(failure(case, "rejected claim charged the run"));
    }
    if adapter
        .store()
        .get_attempt(scope, &run_id, &id("oversized-attempt"))
        .await
        .is_ok()
    {
        return Err(failure(case, "rejected claim persisted an attempt row"));
    }
    Ok(())
}

/// Section 5.3 `decide_approval` and N67: an approval output that is not the exact
/// canonical `ApprovalResult` applies N67/G06/R08 with `ApprovalPayloadInvalid`,
/// registering no decision-payload ref.
async fn decide_approval_payload_invalid_applied<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    _scope_b: &ExecutionScope,
) -> Result<(), ConformanceFailure> {
    let case = 57;
    let fixture = prepare_approval_conformance(adapter, scope, case).await?;
    let run_id = id(&format!("approval-run-{case}"));
    let before = applied_failure_watermark(adapter, scope, case, &run_id, &id("approval")).await?;
    // Well-formed JSON, but not the engine-constructed canonical envelope.
    let forged = adapter
        .objects()
        .put(scope, br#"{"decision":"approve"}"#, "application/json")
        .await
        .map_err(|_| failure(case, "forged approval output publication failed"))?;
    match adapter
        .store()
        .decide_approval(
            scope,
            DecideApproval {
                run_id: run_id.clone(),
                gate_id: id("approval-gate"),
                expected_run_version: before.run_version,
                expected_gate_version: fixture.gate_version,
                decision: ApprovalDecision::Approve,
                decision_payload: None,
                approval_output: Some(forged),
                principal: fixture.approver.clone(),
            },
        )
        .await
    {
        Err(StoreError::ContractValidationApplied { code }) if code == "ApprovalPayloadInvalid" => {
        }
        _ => return Err(failure(case, "forged approval envelope was accepted")),
    }
    let (node, _run) = assert_applied_contract_failure(
        adapter,
        scope,
        case,
        &run_id,
        &id("approval"),
        NodeFailureKind::ApprovalPayloadInvalid,
        RunFailureKind::ApprovalPayloadInvalid,
        &before,
    )
    .await?;
    if node.result_ref.is_some() {
        return Err(failure(case, "rejected approval stored a node output"));
    }
    // N67 requires the still-Pending gate cancelled by G06 in the same transaction.
    let gate = adapter
        .store()
        .get_gate(scope, &run_id, &id("approval-gate"))
        .await
        .map_err(|_| failure(case, "gate read failed"))?;
    if gate.status != GateState::Cancelled {
        return Err(failure(case, "G06 did not cancel the Pending gate"));
    }
    if gate.decision_payload_ref.is_some() || gate.deciding_principal.is_some() {
        return Err(failure(case, "rejected approval recorded a decision"));
    }
    if gate.version <= fixture.gate_version {
        return Err(failure(
            case,
            "gate cancellation did not advance its version",
        ));
    }
    Ok(())
}

struct ApprovalConformanceFixture {
    approver: AuthenticatedPrincipal,
    gate_version: Version,
}

/// Publishes a one-Approval definition, starts its run, and opens a Pending gate.
async fn prepare_approval_conformance<A: ConformanceAdapter>(
    adapter: &A,
    scope: &ExecutionScope,
    case: usize,
) -> Result<ApprovalConformanceFixture, ConformanceFailure> {
    let schema = adapter
        .objects()
        .put(
            scope,
            br#"{"additionalProperties":false,"properties":{"request":{"type":"boolean"}},"required":["request"],"type":"object"}"#,
            "application/json",
        )
        .await
        .map_err(|_| failure(case, "approval schema publication failed"))?;
    let creator = AuthenticatedPrincipal::mint(
        scope.clone(),
        format!("approval-creator-{case}"),
        Vec::new(),
        schema.digest().clone(),
    )
    .map_err(|_| failure(case, "approval creator capability failed"))?;
    let definition_id = id(&format!("approval-definition-{case}"));
    let run_id = id(&format!("approval-run-{case}"));
    adapter
        .store()
        .create_definition(
            scope,
            CreateDefinition {
                definition_id: definition_id.clone(),
                display_name: format!("approval-{case}"),
                description: String::new(),
                principal: creator.clone(),
            },
        )
        .await
        .map_err(|_| failure(case, "approval definition creation failed"))?;
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: definition_id.clone(),
        name: format!("approval-{case}"),
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
                        allowed_principal_ids: vec![format!("approval-approver-{case}")],
                        allowed_role_ids: Vec::new(),
                    },
                },
                next: vec![id("succeed")],
            },
            NodeDefinition::Succeed {
                id: id("succeed"),
                output: BindingSource::Constant { value: Value::Null },
            },
        ],
    };
    let canonical = adapter
        .objects()
        .put(
            scope,
            &serde_jcs::to_vec(&definition)
                .map_err(|_| failure(case, "approval definition encoding failed"))?,
            "application/json",
        )
        .await
        .map_err(|_| failure(case, "approval definition publication failed"))?;
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
                resolved_action_schema_objects: BTreeMap::new(),
                parsed_revision: PublishableDefinition {
                    definition,
                    topological_ranks: BTreeMap::from([
                        (id("approval"), TopologicalRank(0)),
                        (id("succeed"), TopologicalRank(1)),
                    ]),
                },
                principal: creator.clone(),
            },
        )
        .await
        .map_err(|_| failure(case, "approval revision publication failed"))?;
    let input = adapter
        .objects()
        .put(scope, br#"{"request":true}"#, "application/json")
        .await
        .map_err(|_| failure(case, "approval run input publication failed"))?;
    adapter
        .store()
        .create_run(
            scope,
            CreateRun {
                run_id: run_id.clone(),
                definition_id,
                revision_hash: canonical.digest().clone(),
                input,
                budget_limit: CostUnits(10),
                limits: RunLimits {
                    max_dynamic_node_instances: 10,
                    max_total_attempts: 10,
                    max_total_events: 500,
                    max_inline_json_bytes_per_value: 10_000,
                    max_artifacts_per_attempt: 10,
                    max_aggregate_object_bytes_per_run: 1_000_000,
                    max_run_lifetime_ms: 100_000,
                },
                principal: creator,
                idempotency_token: format!("approval-create-{case}"),
            },
        )
        .await
        .map_err(|_| failure(case, "approval run creation failed"))?;
    let claim = adapter
        .store()
        .acquire_engine_claim(scope, id(&format!("approval-engine-{case}")))
        .await
        .map_err(|_| failure(case, "approval engine claim failed"))?;
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
        .map_err(|_| failure(case, "approval run start failed"))?;
    let node = adapter
        .store()
        .get_node(scope, &run_id, &id("approval"))
        .await
        .map_err(|_| failure(case, "approval node read failed"))?;
    let request = adapter
        .objects()
        .put(scope, br#"{"request":true}"#, "application/json")
        .await
        .map_err(|_| failure(case, "approval request publication failed"))?;
    let gate = adapter
        .store()
        .request_approval(
            scope,
            RequestApproval {
                permit: claim.permit,
                run_id,
                node_id: id("approval"),
                expected_node_version: node.version,
                gate_id: id("approval-gate"),
                request,
            },
        )
        .await
        .map_err(|_| failure(case, "approval request failed"))?;
    let approver = AuthenticatedPrincipal::mint(
        scope.clone(),
        format!("approval-approver-{case}"),
        Vec::new(),
        schema.digest().clone(),
    )
    .map_err(|_| failure(case, "approver capability failed"))?;
    Ok(ApprovalConformanceFixture {
        approver,
        gate_version: gate.version,
    })
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
    // Each fixture future is boxed rather than inlined. Awaiting every case inline
    // makes this one state machine the sum of all of them, which overflows an
    // ordinary 2 MiB test-thread stack once the suite is large enough; the box keeps
    // the runner's frame flat as cases are added.
    macro_rules! run_fixture {
        ($name:literal, $fixture:ident) => {{
            let adapter = adapter_factory.fresh();
            results.push(ConformanceCaseResult {
                name: $name,
                failure: Box::pin($fixture(&adapter, scope_a, scope_b)).await.err(),
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
    run_fixture!(
        "corrupt_unreachable_ref_rejected",
        corrupt_unreachable_ref_rejected
    );
    run_fixture!(
        "corrupt_run_produced_ref_accepted",
        corrupt_run_produced_ref_accepted
    );
    run_fixture!(
        "corrupt_pinned_revision_ref_accepted",
        corrupt_pinned_revision_ref_accepted
    );
    run_fixture!(
        "pinned_root_schema_subset_enforced",
        pinned_root_schema_subset_enforced
    );
    run_fixture!(
        "succeed_output_schema_enforced",
        succeed_output_schema_enforced
    );
    run_fixture!(
        "succeed_output_inline_limit_enforced",
        succeed_output_inline_limit_enforced
    );
    run_fixture!(
        "succeed_output_inline_limit_boundary_accepted",
        succeed_output_inline_limit_boundary_accepted
    );
    run_fixture!(
        "succeed_output_aggregate_limit_enforced",
        succeed_output_aggregate_limit_enforced
    );
    run_fixture!(
        "succeed_output_aggregate_limit_boundary_accepted",
        succeed_output_aggregate_limit_boundary_accepted
    );
    run_fixture!(
        "map_aggregate_inline_limit_enforced",
        map_aggregate_inline_limit_enforced
    );
    run_fixture!(
        "map_aggregate_inline_limit_boundary_accepted",
        map_aggregate_inline_limit_boundary_accepted
    );
    run_fixture!(
        "map_aggregate_object_limit_enforced",
        map_aggregate_object_limit_enforced
    );
    run_fixture!(
        "map_aggregate_object_limit_boundary_accepted",
        map_aggregate_object_limit_boundary_accepted
    );
    run_fixture!(
        "expand_map_input_invalid_applied",
        expand_map_input_invalid_applied
    );
    run_fixture!(
        "expand_map_dynamic_node_limit_applied",
        expand_map_dynamic_node_limit_applied
    );
    run_fixture!(
        "claim_inline_json_limit_applied",
        claim_inline_json_limit_applied
    );
    run_fixture!(
        "decide_approval_payload_invalid_applied",
        decide_approval_payload_invalid_applied
    );
    run_fixture!(
        "revision_non_canonical_bytes_rejected",
        revision_non_canonical_bytes_rejected
    );
    run_fixture!(
        "revision_typed_definition_mismatch_rejected",
        revision_typed_definition_mismatch_rejected
    );
    run_fixture!(
        "revision_definition_id_mismatch_rejected",
        revision_definition_id_mismatch_rejected
    );
    run_fixture!(
        "revision_topological_ranks_mismatch_rejected",
        revision_topological_ranks_mismatch_rejected
    );
    run_fixture!(
        "action_output_inline_limit_enforced",
        action_output_inline_limit_enforced
    );
    run_fixture!(
        "action_output_inline_limit_boundary_accepted",
        action_output_inline_limit_boundary_accepted
    );
    run_fixture!(
        "action_artifacts_per_attempt_limit_enforced",
        action_artifacts_per_attempt_limit_enforced
    );
    run_fixture!(
        "action_artifacts_per_attempt_limit_boundary_accepted",
        action_artifacts_per_attempt_limit_boundary_accepted
    );
    run_fixture!(
        "action_output_aggregate_limit_enforced",
        action_output_aggregate_limit_enforced
    );
    run_fixture!(
        "action_output_aggregate_limit_boundary_accepted",
        action_output_aggregate_limit_boundary_accepted
    );
    run_fixture!(
        "action_output_schema_enforced",
        action_output_schema_enforced
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::TestClock;
    use crate::memory::{InMemoryObjectStore, InMemoryStore};
    use crate::scope::ScopeAtom;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    struct ThreadWake(std::thread::Thread);

    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    fn block_on<T>(future: impl Future<Output = T>) -> T {
        let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match Pin::new(&mut future).poll(&mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::park(),
            }
        }
    }

    struct Adapter {
        clock: Arc<TestClock>,
        store: InMemoryStore<TestClock>,
        objects: InMemoryObjectStore<TestClock>,
    }

    impl ConformanceAdapter for Adapter {
        type Store = InMemoryStore<TestClock>;
        type Objects = InMemoryObjectStore<TestClock>;

        fn store(&self) -> &Self::Store {
            &self.store
        }

        fn objects(&self) -> &Self::Objects {
            &self.objects
        }

        fn advance_clock_ms(&self, milliseconds: i64) {
            self.clock.advance_ms(milliseconds).expect("clock advances");
        }

        fn object_records(&self, scope: &ExecutionScope) -> Vec<ObjectRecord> {
            self.store.object_records(scope)
        }

        fn fresh(&self) -> Self {
            let clock = Arc::new(TestClock::new(Timestamp(0)));
            Self {
                store: InMemoryStore::new(clock.clone()),
                objects: InMemoryObjectStore::new(clock.clone()),
                clock,
            }
        }
    }

    fn scope(tenant: &str) -> ExecutionScope {
        ExecutionScope {
            tenant_id: ScopeAtom::new(tenant).expect("scope atom"),
            namespace: ScopeAtom::new("workflow").expect("scope atom"),
        }
    }

    /// Keeps the in-crate suite runnable without the adapter-specific test targets,
    /// so section 5.5's applied-failure fixtures stay checkable under `--lib`.
    #[test]
    fn conformance_suite_passes_against_the_volatile_store() {
        block_on(async {
            let clock = Arc::new(TestClock::new(Timestamp(0)));
            let adapter = Adapter {
                store: InMemoryStore::new(clock.clone()),
                objects: InMemoryObjectStore::new(clock.clone()),
                clock,
            };
            let results = run_conformance(&adapter, &scope("tenant-a"), &scope("tenant-b")).await;
            assert_eq!(results.len(), CASE_COUNT);
            let failed = results
                .iter()
                .filter(|result| !result.passed())
                .collect::<Vec<_>>();
            assert!(failed.is_empty(), "{failed:?}");
        });
    }
}
