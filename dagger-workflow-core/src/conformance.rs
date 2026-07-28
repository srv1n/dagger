//! Adapter-neutral black-box checks shared by volatile and durable stores.

use crate::action::{ActionOutcome, CompatibilityReport};
use crate::approval::AuthenticatedPrincipal;
use crate::artifact::{ObjectStore, ObjectStoreError};
use crate::definition::{
    ActionReference, BackoffPolicy, Binding, BindingSource, NodeDefinition, PublishableDefinition,
    RetryPolicy, TimeoutPolicy, WorkflowDefinition,
};
use crate::ids::{CostUnits, Digest, Id, TopologicalRank, Version};
use crate::run::{AttemptState, NodeState, RunLimits, RunState};
use crate::scope::ExecutionScope;
use crate::store::{
    ClaimNodeAttempt, ClaimNodeAttemptResult, CompleteAttempt, CompleteAttemptResult,
    CompletionObjects, CreateDefinition, CreateRun, EventPageRequest, PublishRevision,
    ResolveTerminalNode, ResolvedActionSchemas, StartRun, StoreError, WorkflowStore,
};
use serde_json::json;
use std::collections::BTreeMap;

/// Number of independent adapter-neutral cases.
pub const CASE_COUNT: usize = 36;

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
pub async fn run_conformance<A: ConformanceAdapter>(
    adapter: &A,
    scope_a: &ExecutionScope,
    scope_b: &ExecutionScope,
) -> Result<usize, ConformanceFailure> {
    let bytes = br#"{"same":true}"#;
    let a = adapter
        .objects()
        .put(scope_a, bytes, "application/json")
        .await
        .map_err(|_| failure(1, "object publication failed"))?;
    let b = adapter
        .objects()
        .put(scope_b, bytes, "application/json")
        .await
        .map_err(|_| failure(2, "second-scope publication failed"))?;
    if a.digest() != b.digest() {
        return Err(failure(3, "equal bytes did not share a digest"));
    }
    if a.scope() == b.scope() {
        return Err(failure(4, "verified refs were not scope-bound"));
    }
    let replay = adapter
        .objects()
        .publish_if_absent(scope_a, bytes, "application/json")
        .await
        .map_err(|_| failure(5, "same-scope replay failed"))?;
    if replay != a {
        return Err(failure(5, "same-scope replay changed the capability"));
    }
    let read_a = adapter
        .objects()
        .get(scope_a, a.digest())
        .await
        .map_err(|_| failure(6, "verified read failed"))?;
    if read_a.bytes != bytes {
        return Err(failure(7, "verified read changed bytes"));
    }
    if adapter.objects().get(scope_b, b.digest()).await.is_err() {
        return Err(failure(8, "scope-b verified read failed"));
    }
    let missing = Digest::new(format!("sha256:{}", "0".repeat(64))).expect("fixture digest");
    if adapter.objects().get(scope_a, &missing).await.is_ok() {
        return Err(failure(9, "missing read did not mint a failure"));
    }
    let first = adapter
        .store()
        .acquire_engine_claim(scope_a, id("engine-a"))
        .await
        .map_err(|_| failure(10, "first engine claim failed"))?;
    if first.claim.control_plane_id != "scheduler" {
        return Err(failure(11, "control-plane ID was not frozen scheduler"));
    }
    if !matches!(
        adapter
            .store()
            .acquire_engine_claim(scope_a, id("engine-b"))
            .await,
        Err(StoreError::EngineAlreadyLive { .. })
    ) {
        return Err(failure(12, "second live engine was accepted"));
    }
    adapter
        .store()
        .acquire_engine_claim(scope_b, id("engine-b"))
        .await
        .map_err(|_| failure(13, "claim leaked across scopes"))?;
    adapter.advance_clock_ms(20_000);
    let takeover = adapter
        .store()
        .acquire_engine_claim(scope_a, id("engine-c"))
        .await
        .map_err(|_| failure(14, "expired takeover failed"))?;
    if takeover.claim.generation != first.claim.generation + 1 {
        return Err(failure(15, "takeover generation did not increment"));
    }
    if !matches!(
        adapter
            .store()
            .heartbeat_engine_claim(scope_a, &first.permit)
            .await,
        Err(StoreError::EngineClaimLost)
    ) {
        return Err(failure(16, "stale permit survived takeover"));
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
    let replay_receipt = adapter
        .store()
        .create_run(scope_a, create(CostUnits(10)))
        .await
        .map_err(|_| failure(20, "create replay failed"))?;
    if replay_receipt != receipt {
        return Err(failure(20, "create replay changed its receipt"));
    }
    if !matches!(
        adapter
            .store()
            .create_run(scope_a, create(CostUnits(11)))
            .await,
        Err(StoreError::IdempotencyConflict)
    ) {
        return Err(failure(21, "conflicting create replay was accepted"));
    }
    if !matches!(
        adapter
            .store()
            .get_run(scope_b, &id("conformance-run"))
            .await,
        Err(StoreError::NotFound)
    ) {
        return Err(failure(22, "run point read crossed scope"));
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
    let node = adapter
        .store()
        .get_node(scope_a, &id("conformance-run"), &id("action"))
        .await
        .map_err(|_| failure(23, "entry node read failed"))?;
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
    if running_node.status != NodeState::Running {
        return Err(failure(25, "claimed node was not Running"));
    }
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
    let reserved = adapter
        .store()
        .get_run(scope_a, &id("conformance-run"))
        .await
        .map_err(|_| failure(27, "reserved run read failed"))?
        .run;
    if reserved.budget_reserved != CostUnits(2) || reserved.budget_consumed != CostUnits(0) {
        return Err(failure(27, "reservation totals were wrong"));
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
    let attempt = adapter
        .store()
        .get_attempt(scope_a, &id("conformance-run"), &id("attempt"))
        .await
        .map_err(|_| failure(29, "terminal attempt read failed"))?;
    if attempt.status != AttemptState::Succeeded {
        return Err(failure(29, "attempt was not terminal Succeeded"));
    }
    let settled = adapter
        .store()
        .get_run(scope_a, &id("conformance-run"))
        .await
        .map_err(|_| failure(30, "settled run read failed"))?
        .run;
    if settled.budget_reserved != CostUnits(0) || settled.budget_consumed != CostUnits(1) {
        return Err(failure(30, "settlement totals were wrong"));
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
    if !events
        .iter()
        .enumerate()
        .all(|(index, event)| event.event_seq == index as u64 + 1)
    {
        return Err(failure(31, "event sequence was not contiguous"));
    }
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
    if events.iter().any(|event| {
        event.event_type == crate::event::EventType::EdgeSatisfied
            && event.payload.get("cause").is_some()
    }) {
        return Err(failure(33, "EdgeSatisfied carried a forbidden cause"));
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
    if !matches!(
        due_result,
        CompleteAttemptResult::TimedOutAndStaleRecorded(_)
    ) {
        return Err(failure(35, "due completion did not time out"));
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
    if due_events
        .get(timeout_index + 1)
        .is_none_or(|event| event.event_type != crate::event::EventType::StaleCompletionObserved)
    {
        return Err(failure(36, "A06 and A14 were not adjacent and ordered"));
    }
    Ok(CASE_COUNT)
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
