//! Immutable event envelope and closed catalogue from contract sections 1.12 and 15.

use crate::ids::{Id, NodeInstanceId, Timestamp};
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The closed event actor vocabulary. Contract section 1.12.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EventActorKind {
    /// Scheduler engine actor. Contract section 1.12.
    Engine,
    /// Credential-authenticated result intake. Contract section 1.12.
    ActionCompletion,
    /// Authenticated host actor. Contract section 1.12.
    Host,
    /// Engine recovery actor. Contract section 1.12.
    Recovery,
    /// Database-clock actor. Contract section 1.12.
    Clock,
}

/// The complete closed v0.1 event type set. Contract section 15.2.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum EventType {
    /// R01 run creation. Contract section 15.2.
    RunCreated,
    /// N01 pending-node creation. Contract section 15.2.
    NodeCreatedPending,
    /// N02 ready-node creation. Contract section 15.2.
    NodeCreatedReady,
    /// R02 run start. Contract section 15.2.
    RunStarted,
    /// R03/R04 incompatibility suspension. Contract section 15.2.
    RunBlockedIncompatible,
    /// N29-N31/N61 node suspension. Contract section 15.2.
    NodeBlockedIncompatible,
    /// R05 compatible resume. Contract section 15.2.
    RunResumedCompatible,
    /// N32-N34/N62 node resume. Contract section 15.2.
    NodeResumedCompatible,
    /// R06 run success. Contract section 15.2.
    RunSucceeded,
    /// R07 run failure. Contract section 15.2.
    RunFailed,
    /// R08 run contract failure. Contract section 15.2.
    RunContractFailed,
    /// R09 run retry exhaustion. Contract section 15.2.
    RunRetriesExhausted,
    /// R10 run budget exhaustion. Contract section 15.2.
    RunBudgetExhausted,
    /// R11-R13 cancellation. Contract section 15.2.
    RunCancelled,
    /// R14-R22 storage corruption. Contract section 15.2.
    RunCorruptStorage,
    /// N03 frontier readiness. Contract section 15.2.
    NodeBecameReady,
    /// N04 retry release. Contract section 15.2.
    NodeRetryEligible,
    /// N05/N58 claim. Contract section 15.2.
    NodeAttemptClaimed,
    /// A01 attempt start. Contract section 15.2.
    AttemptStarted,
    /// A01 budget reservation. Contract section 15.2.
    BudgetReserved,
    /// N02M Map child creation. Contract section 15.2.
    MapChildCreated,
    /// N06 Map expansion. Contract section 15.2.
    MapExpanded,
    /// N07 zero-item Map success. Contract section 15.2.
    MapZeroItemsSucceeded,
    /// N08 Map aggregation success. Contract section 15.2.
    MapSucceeded,
    /// N09 Choice decision. Contract section 15.2.
    ChoiceSelected,
    /// N11 approval request. Contract section 15.2.
    ApprovalRequested,
    /// N12 approval node success. Contract section 15.2.
    ApprovalApproved,
    /// N13 approval node rejection. Contract section 15.2.
    ApprovalRejected,
    /// N14 approval expiry success. Contract section 15.2.
    ApprovalExpiredApproved,
    /// N15 approval expiry rejection. Contract section 15.2.
    ApprovalExpiredRejected,
    /// N16 Succeed terminal. Contract section 15.2.
    SucceedNodeReached,
    /// N17 Fail terminal. Contract section 15.2.
    FailNodeReached,
    /// N18 action-node success. Contract section 15.2.
    NodeSucceeded,
    /// N19/N22/N23 retry scheduling. Contract section 15.2.
    NodeRetryScheduled,
    /// N20 permanent node failure. Contract section 15.2.
    NodeFailed,
    /// N21/N46/N64/N67 node contract failure. Contract section 15.2.
    NodeContractFailed,
    /// N24-N26 node retry exhaustion. Contract section 15.2.
    NodeRetriesExhausted,
    /// N59 temporary budget wait. Contract section 15.2.
    NodeBudgetWaiting,
    /// N27/N60 permanent node budget exhaustion. Contract section 15.2.
    NodeBudgetExhausted,
    /// N28 inactive node. Contract section 15.2.
    NodeSkipped,
    /// N35-N41/N63/N66 node cancellation. Contract section 15.2.
    NodeCancelled,
    /// N42 child permanent failure. Contract section 15.2.
    MapFailedFast,
    /// N43/N65 Map contract failure. Contract section 15.2.
    MapContractFailed,
    /// N44 Map retry exhaustion. Contract section 15.2.
    MapRetriesExhausted,
    /// N45 Map budget exhaustion. Contract section 15.2.
    MapBudgetExhausted,
    /// N47-N57 node object corruption. Contract section 15.2.
    NodeCorruptStorage,
    /// A02 attempt success. Contract section 15.2.
    AttemptSucceeded,
    /// A03 retryable attempt failure. Contract section 15.2.
    AttemptRetryableFailed,
    /// A04 permanent attempt failure. Contract section 15.2.
    AttemptPermanentFailed,
    /// A05 attempt contract failure. Contract section 15.2.
    AttemptContractFailed,
    /// A06 timeout. Contract section 15.2.
    AttemptTimedOut,
    /// A07 crash-unknown recovery. Contract section 15.2.
    AttemptOutcomeUnknown,
    /// A08 attempt cancellation. Contract section 15.2.
    AttemptCancelled,
    /// A09 live stale completion. Contract section 15.2.
    AttemptMarkedStale,
    /// A10-A17 immutable late observation. Contract section 15.2.
    StaleCompletionObserved,
    /// A02-A09 settlement. Contract section 15.2.
    BudgetSettled,
    /// N27/N60 reservation refusal. Contract section 15.2.
    BudgetReservationRefused,
    /// G01 gate creation. Contract section 15.2.
    ApprovalGateCreated,
    /// G02 gate approval. Contract section 15.2.
    ApprovalGateApproved,
    /// G03 gate rejection. Contract section 15.2.
    ApprovalGateRejected,
    /// G04 approve-on-expiry. Contract section 15.2.
    ApprovalGateExpiredApproved,
    /// G05 reject-on-expiry. Contract section 15.2.
    ApprovalGateExpiredRejected,
    /// G06 gate cancellation. Contract section 15.2.
    ApprovalGateCancelled,
    /// E01 satisfied edge. Contract section 15.2.
    EdgeSatisfied,
    /// E02 skipped edge. Contract section 15.2.
    EdgeSkipped,
}

/// One immutable event in a run-lifetime-unique atomic batch. Contract section 1.12.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowEvent {
    /// Event scope. Contract section 1.12.
    pub scope: ExecutionScope,
    /// Owning run ID. Contract section 1.12.
    pub run_id: Id,
    /// Strictly increasing per-run sequence. Contract section 1.12.
    pub event_seq: u64,
    /// Closed event type. Contract section 15.2.
    pub event_type: EventType,
    /// State-transition row ID. Contract section 1.12.
    pub transition_id: String,
    /// Run-lifetime-unique transaction batch ID. Contract section 15.1.
    pub batch_id: Id,
    /// Zero-based position in the batch. Contract section 15.1.
    pub batch_index: u32,
    /// Identical complete batch count. Contract section 15.1.
    pub batch_count: u32,
    /// Database occurrence timestamp. Contract section 1.12.
    pub occurred_at: Timestamp,
    /// Closed actor kind. Contract section 1.12.
    pub actor_kind: EventActorKind,
    /// Persistence-safe actor identifier. Contract section 1.12.
    pub actor_id: String,
    /// Optional node correlation. Contract section 15.1.
    pub node_instance_id: Option<NodeInstanceId>,
    /// Optional attempt correlation. Contract section 15.1.
    pub attempt_id: Option<Id>,
    /// Optional gate correlation. Contract section 15.1.
    pub gate_id: Option<Id>,
    /// Strict event-specific canonical payload. Contract section 15.2.
    pub payload: Value,
}
