//! Immutable event envelope and closed catalogue.

use crate::ids::{Id, NodeInstanceId, Timestamp};
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The closed event actor vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EventActorKind {
    /// Scheduler engine actor.
    Engine,
    /// Credential-authenticated result intake.
    ActionCompletion,
    /// Authenticated host actor.
    Host,
    /// Engine recovery actor.
    Recovery,
    /// Database-clock actor.
    Clock,
}

/// The complete closed event type set.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum EventType {
    /// R01 run creation.
    RunCreated,
    /// N01 pending-node creation.
    NodeCreatedPending,
    /// N02 ready-node creation.
    NodeCreatedReady,
    /// R02 run start.
    RunStarted,
    /// R03/R04 incompatibility suspension.
    RunBlockedIncompatible,
    /// N29-N31/N61 node suspension.
    NodeBlockedIncompatible,
    /// R05 compatible resume.
    RunResumedCompatible,
    /// N32-N34/N62 node resume.
    NodeResumedCompatible,
    /// R06 run success.
    RunSucceeded,
    /// R07 run failure.
    RunFailed,
    /// R08 run contract failure.
    RunContractFailed,
    /// R09 run retry exhaustion.
    RunRetriesExhausted,
    /// R10 run budget exhaustion.
    RunBudgetExhausted,
    /// R11-R13 cancellation.
    RunCancelled,
    /// R14-R22 storage corruption.
    RunCorruptStorage,
    /// N03 frontier readiness.
    NodeBecameReady,
    /// N04 retry release.
    NodeRetryEligible,
    /// N05/N58 claim.
    NodeAttemptClaimed,
    /// A01 attempt start.
    AttemptStarted,
    /// A01 budget reservation.
    BudgetReserved,
    /// N02M Map child creation.
    MapChildCreated,
    /// N06 Map expansion.
    MapExpanded,
    /// N07 zero-item Map success.
    MapZeroItemsSucceeded,
    /// N08 Map aggregation success.
    MapSucceeded,
    /// N09 Choice decision.
    ChoiceSelected,
    /// N11 approval request.
    ApprovalRequested,
    /// N12 approval node success.
    ApprovalApproved,
    /// N13 approval node rejection.
    ApprovalRejected,
    /// N14 approval expiry success.
    ApprovalExpiredApproved,
    /// N15 approval expiry rejection.
    ApprovalExpiredRejected,
    /// N16 Succeed terminal.
    SucceedNodeReached,
    /// N17 Fail terminal.
    FailNodeReached,
    /// N18 action-node success.
    NodeSucceeded,
    /// N19/N22/N23 retry scheduling.
    NodeRetryScheduled,
    /// N20 permanent node failure.
    NodeFailed,
    /// N21/N46/N64/N67 node contract failure.
    NodeContractFailed,
    /// N24-N26 node retry exhaustion.
    NodeRetriesExhausted,
    /// N59 temporary budget wait.
    NodeBudgetWaiting,
    /// N27/N60 permanent node budget exhaustion.
    NodeBudgetExhausted,
    /// N28 inactive node.
    NodeSkipped,
    /// N35-N41/N63/N66 node cancellation.
    NodeCancelled,
    /// N42 child permanent failure.
    MapFailedFast,
    /// N43/N65 Map contract failure.
    MapContractFailed,
    /// N44 Map retry exhaustion.
    MapRetriesExhausted,
    /// N45 Map budget exhaustion.
    MapBudgetExhausted,
    /// N47-N57 node object corruption.
    NodeCorruptStorage,
    /// A02 attempt success.
    AttemptSucceeded,
    /// A03 retryable attempt failure.
    AttemptRetryableFailed,
    /// A04 permanent attempt failure.
    AttemptPermanentFailed,
    /// A05 attempt contract failure.
    AttemptContractFailed,
    /// A06 timeout.
    AttemptTimedOut,
    /// A07 crash-unknown recovery.
    AttemptOutcomeUnknown,
    /// A08 attempt cancellation.
    AttemptCancelled,
    /// A09 live stale completion.
    AttemptMarkedStale,
    /// A10-A17 immutable late observation.
    StaleCompletionObserved,
    /// A02-A09 settlement.
    BudgetSettled,
    /// N27/N60 reservation refusal.
    BudgetReservationRefused,
    /// G01 gate creation.
    ApprovalGateCreated,
    /// G02 gate approval.
    ApprovalGateApproved,
    /// G03 gate rejection.
    ApprovalGateRejected,
    /// G04 approve-on-expiry.
    ApprovalGateExpiredApproved,
    /// G05 reject-on-expiry.
    ApprovalGateExpiredRejected,
    /// G06 gate cancellation.
    ApprovalGateCancelled,
    /// E01 satisfied edge.
    EdgeSatisfied,
    /// E02 skipped edge.
    EdgeSkipped,
}

/// One immutable event in a run-lifetime-unique atomic batch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowEvent {
    /// Event scope.
    pub scope: ExecutionScope,
    /// Owning run ID.
    pub run_id: Id,
    /// Strictly increasing per-run sequence.
    pub event_seq: u64,
    /// Closed event type.
    pub event_type: EventType,
    /// State-transition row ID.
    pub transition_id: String,
    /// Run-lifetime-unique transaction batch ID.
    pub batch_id: Id,
    /// Zero-based position in the batch.
    pub batch_index: u32,
    /// Identical complete batch count.
    pub batch_count: u32,
    /// Database occurrence timestamp.
    pub occurred_at: Timestamp,
    /// Closed actor kind.
    pub actor_kind: EventActorKind,
    /// Persistence-safe actor identifier.
    pub actor_id: String,
    /// Optional node correlation.
    pub node_instance_id: Option<NodeInstanceId>,
    /// Optional attempt correlation.
    pub attempt_id: Option<Id>,
    /// Optional gate correlation.
    pub gate_id: Option<Id>,
    /// Strict event-specific canonical payload.
    pub payload: Value,
}
