//! Durable run, node, attempt, edge, state, and operational-view types.

use crate::artifact::{ArtifactRef, FailedReadClass, JsonRef};
use crate::ids::{CostUnits, Digest, Id, NodeInstanceId, Timestamp, TopologicalRank, Version};
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};

/// The closed durable run state vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RunState {
    /// Awaiting compatibility acceptance.
    Pending,
    /// Scheduling and clock transitions are allowed.
    Running,
    /// Recoverably suspended for incompatible pins.
    BlockedIncompatible,
    /// Successful terminal state.
    Succeeded,
    /// Permanent domain failure.
    Failed,
    /// Runtime contract failure.
    ContractFailed,
    /// Retry ceiling exhaustion.
    RetriesExhausted,
    /// Permanently infeasible budget.
    BudgetExhausted,
    /// Cancellation terminal state.
    Cancelled,
    /// Absorbing committed-object corruption.
    CorruptStorage,
}

/// The closed durable node state vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NodeState {
    /// Incoming facts are incomplete.
    Pending,
    /// Node is eligible for execution.
    Ready,
    /// An Action attempt is active.
    Running,
    /// Persisted retry delay is pending.
    RetryWaiting,
    /// Reservation pressure temporarily blocks admission.
    BudgetWaiting,
    /// A durable approval gate is pending.
    WaitingApproval,
    /// A Map awaits children.
    WaitingChildren,
    /// The node is recoverably incompatible.
    BlockedIncompatible,
    /// Successful terminal state.
    Succeeded,
    /// Permanent node failure.
    Failed,
    /// Runtime contract failure.
    ContractFailed,
    /// Retry ceiling exhaustion.
    RetriesExhausted,
    /// Permanently infeasible budget.
    BudgetExhausted,
    /// Cancellation terminal state.
    Cancelled,
    /// Absorbing committed-object corruption.
    CorruptStorage,
    /// No active incoming path reaches this node.
    Skipped,
}

/// Node states that may be saved during compatibility suspension.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BlockedFromState {
    /// Previously Pending.
    Pending,
    /// Previously Ready.
    Ready,
    /// Previously RetryWaiting.
    RetryWaiting,
    /// Previously BudgetWaiting.
    BudgetWaiting,
}

/// The closed immutable attempt state vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AttemptState {
    /// Live reserved attempt.
    Started,
    /// Accepted successful result.
    Succeeded,
    /// Structured retryable failure.
    RetryableFailed,
    /// Structured permanent failure.
    PermanentFailed,
    /// Pinned contract violation.
    ContractFailed,
    /// Database deadline elapsed.
    TimedOut,
    /// Dead-generation recovery outcome.
    UnknownOutcome,
    /// Run-terminalization cancellation.
    Cancelled,
    /// Live completion lost the active-attempt fence.
    Stale,
}

/// The closed approval gate state vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum GateState {
    /// Awaiting first valid resolution.
    Pending,
    /// Human-approved terminal state.
    Approved,
    /// Human-rejected terminal state.
    Rejected,
    /// Expiry-approved terminal state.
    ExpiredApproved,
    /// Expiry-rejected terminal state.
    ExpiredRejected,
    /// Cancellation terminal state.
    Cancelled,
}

/// The closed edge-fact state vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EdgeState {
    /// Unresolved edge fact.
    Dormant,
    /// Active successful path.
    Satisfied,
    /// Inactive path.
    Skipped,
}

/// The closed node kind vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    /// Executable Action.
    Action,
    /// Bounded Map.
    Map,
    /// Deterministic Choice.
    Choice,
    /// Durable Approval.
    Approval,
    /// Successful terminal.
    Succeed,
    /// Explicit failure terminal.
    Fail,
}

macro_rules! failure_kind {
    ($name:ident, $section:literal) => {
        #[doc = "The closed workflow failure vocabulary."]
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub enum $name {
            /// Permanent action error.
            ActionPermanent,
            /// Explicit Fail node.
            ExplicitFailNode,
            /// Failed Map child.
            MapChildFailed,
            /// Human rejection.
            ApprovalRejected,
            /// Reject-on-expiry.
            ApprovalExpiredRejected,
            /// Dynamic-node ceiling.
            RunDynamicNodeLimitExceeded,
            /// Total-attempt ceiling.
            RunAttemptLimitExceeded,
            /// Inline JSON ceiling.
            InlineJsonLimitExceeded,
            /// Per-attempt artifact ceiling.
            ArtifactsPerAttemptLimitExceeded,
            /// Aggregate object-byte ceiling.
            AggregateObjectLimitExceeded,
            /// Root output schema mismatch.
            RunOutputSchemaMismatch,
            /// Unavailable binding source.
            BindingSourceUnavailable,
            /// Missing binding pointer.
            BindingPointerMissing,
            /// Binding type mismatch.
            BindingTypeMismatch,
            /// Action output schema mismatch.
            ActionOutputSchemaMismatch,
            /// Invalid Choice input.
            ChoiceInputInvalid,
            /// Invalid Map input.
            MapInputInvalid,
            /// Map bound exceeded.
            MapBoundExceeded,
            /// Invalid approval payload.
            ApprovalPayloadInvalid,
            /// Action reported cost beyond its declaration.
            ActionCostProtocolViolation,
        }
    };
}

failure_kind!(RunFailureKind, "1.4");
failure_kind!(NodeFailureKind, "1.5");

/// The closed attempt error-class vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AttemptErrorClass {
    /// Retryable action failure.
    Retryable,
    /// Permanent action failure.
    Permanent,
    /// Contract violation.
    Contract,
}

/// Immutable seven-ceiling run limits.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunLimits {
    /// Total synthetic Map child ceiling.
    pub max_dynamic_node_instances: u64,
    /// Total started attempt ceiling.
    pub max_total_attempts: u64,
    /// Total event ceiling.
    pub max_total_events: u64,
    /// Per-value canonical JSON byte ceiling.
    pub max_inline_json_bytes_per_value: u64,
    /// Per-attempt action artifact ceiling.
    pub max_artifacts_per_attempt: u64,
    /// Per-run charged ArtifactRef byte ceiling.
    pub max_aggregate_object_bytes_per_run: u64,
    /// Run lifetime ceiling in milliseconds.
    pub max_run_lifetime_ms: u64,
}

/// One durable workflow run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowRun {
    /// Run scope.
    pub scope: ExecutionScope,
    /// Run ID.
    pub run_id: Id,
    /// Pinned definition ID.
    pub definition_id: Id,
    /// Pinned revision hash.
    pub revision_hash: Digest,
    /// Immutable canonical input ref.
    pub input_ref: JsonRef,
    /// Scope-bound creation fingerprint.
    pub create_request_fingerprint: Digest,
    /// Durable run state.
    pub status: RunState,
    /// Closed terminal failure kind.
    pub failure_kind: Option<RunFailureKind>,
    /// Optional terminal diagnostics.
    pub failure_diagnostics_ref: Option<JsonRef>,
    /// Successful root output.
    pub output_ref: Option<JsonRef>,
    /// Immutable budget limit.
    pub budget_limit: CostUnits,
    /// Monotonic consumed cost.
    pub budget_consumed: CostUnits,
    /// Currently reserved cost.
    pub budget_reserved: CostUnits,
    /// Monotonic dynamic-node count.
    pub dynamic_node_count: u64,
    /// Monotonic attempt count.
    pub total_attempt_count: u64,
    /// Charged ArtifactRef byte count.
    pub aggregate_object_bytes: u64,
    /// Immutable run limits.
    pub limits: RunLimits,
    /// Immutable database-clock lifetime deadline.
    pub lifetime_deadline_at: Timestamp,
    /// Frontier change epoch.
    pub frontier_epoch: u64,
    /// Last allocated event sequence.
    pub last_event_seq: u64,
    /// Creation timestamp.
    pub created_at: Timestamp,
    /// Last mutation timestamp.
    pub updated_at: Timestamp,
    /// First Running timestamp.
    pub started_at: Option<Timestamp>,
    /// Terminal or integrity-override timestamp.
    pub finished_at: Option<Timestamp>,
    /// Incompatibility evidence ref.
    pub blocked_incompatibilities_ref: Option<JsonRef>,
    /// Exact suspension replay fingerprint.
    pub blocked_incompatibility_fingerprint: Option<Digest>,
    /// Bad committed ArtifactRef ID.
    pub corrupt_bad_artifact_ref_id: Option<Id>,
    /// Optional node owning the corrupt ref.
    pub corrupt_owner_node_id: Option<NodeInstanceId>,
    /// Closed corruption class.
    pub corrupt_error_class: Option<FailedReadClass>,
    /// Opaque proof fingerprint.
    pub corrupt_proof_fingerprint: Option<Digest>,
    /// Run CAS version.
    pub version: Version,
}

/// One durable static or synthetic node instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeRun {
    /// Node scope.
    pub scope: ExecutionScope,
    /// Owning run ID.
    pub run_id: Id,
    /// Static or synthetic instance ID.
    pub node_instance_id: NodeInstanceId,
    /// Definition node ID.
    pub definition_node_id: Id,
    /// Immutable node kind.
    pub kind: NodeKind,
    /// Optional parent Map instance.
    pub parent_map_instance_id: Option<NodeInstanceId>,
    /// Optional Map item index.
    pub map_item_index: Option<u32>,
    /// Optional Map item digest.
    pub map_item_digest: Option<Digest>,
    /// Persisted canonical recovery rank.
    pub topological_rank: TopologicalRank,
    /// Durable node state.
    pub status: NodeState,
    /// Saved compatibility-suspension state.
    pub blocked_from_status: Option<BlockedFromState>,
    /// Only attempt permitted to complete.
    pub active_attempt_id: Option<Id>,
    /// Started attempt count.
    pub attempt_count: u32,
    /// Persisted retry eligibility.
    pub next_eligible_at: Option<Timestamp>,
    /// Declared cost while BudgetWaiting.
    pub budget_wait_amount: Option<CostUnits>,
    /// Successful result ref.
    pub result_ref: Option<JsonRef>,
    /// Closed terminal failure kind.
    pub failure_kind: Option<NodeFailureKind>,
    /// Optional terminal diagnostics.
    pub failure_diagnostics_ref: Option<JsonRef>,
    /// Immutable incoming edge count.
    pub incoming_total: u32,
    /// Satisfied incoming edge count.
    pub incoming_satisfied: u32,
    /// Skipped incoming edge count.
    pub incoming_skipped: u32,
    /// Committed Choice input.
    pub choice_input_ref: Option<JsonRef>,
    /// Committed Choice selection.
    pub choice_selected_case: Option<ChoiceSelection>,
    /// Committed Map input.
    pub map_input_ref: Option<JsonRef>,
    /// Map expansion digest.
    pub map_expansion_digest: Option<Digest>,
    /// Persisted Map child count.
    pub map_child_count: Option<u32>,
    /// Durable approval gate ID.
    pub approval_gate_id: Option<Id>,
    /// Creation timestamp.
    pub created_at: Timestamp,
    /// Last mutation timestamp.
    pub updated_at: Timestamp,
    /// Node CAS version.
    pub version: Version,
}

/// The closed committed Choice selection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChoiceSelection {
    /// An ordered matching case.
    Case {
        /// Zero-based case index.
        case_index: u32,
        /// Selected deterministic edge ID.
        edge_id: Id,
    },
    /// The required default branch.
    Default {
        /// Selected deterministic edge ID.
        edge_id: Id,
    },
}

/// One immutable attempt row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeAttempt {
    /// Attempt scope.
    pub scope: ExecutionScope,
    /// Owning run ID.
    pub run_id: Id,
    /// Attempt ID.
    pub attempt_id: Id,
    /// Owning node instance.
    pub node_instance_id: NodeInstanceId,
    /// One-based attempt number.
    pub attempt_number: u32,
    /// Worker label.
    pub worker_id: Id,
    /// Claiming engine label.
    pub engine_instance_id: Id,
    /// Claiming engine generation.
    pub engine_generation: u64,
    /// Persisted completion capability digest.
    pub completion_credential_digest: Digest,
    /// Immutable ActionInvocation ID.
    pub invocation_id: Id,
    /// Retry-stable external idempotency key.
    pub idempotency_key: String,
    /// Durable attempt state.
    pub status: AttemptState,
    /// Declared maximum cost.
    pub declared_max_cost: CostUnits,
    /// Immutable reserved cost.
    pub reserved_cost: CostUnits,
    /// Terminal settled cost.
    pub settled_cost: Option<CostUnits>,
    /// Database-clock deadline.
    pub deadline_at: Timestamp,
    /// Start timestamp.
    pub started_at: Timestamp,
    /// Terminal timestamp.
    pub finished_at: Option<Timestamp>,
    /// Successful output ref.
    pub output_ref: Option<JsonRef>,
    /// Ordered action artifacts.
    pub artifact_refs: Vec<ArtifactRef>,
    /// Closed terminal error class.
    pub error_class: Option<AttemptErrorClass>,
    /// Namespaced action error code.
    pub error_code: Option<String>,
    /// Optional diagnostics ref.
    pub diagnostics_ref: Option<JsonRef>,
}

/// One immutable control-edge fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EdgeFact {
    /// Edge scope.
    pub scope: ExecutionScope,
    /// Owning run ID.
    pub run_id: Id,
    /// Deterministic edge ID.
    pub edge_id: Id,
    /// Source node instance.
    pub from_node_id: NodeInstanceId,
    /// Target node instance.
    pub to_node_id: NodeInstanceId,
    /// Optional Choice case index.
    pub choice_case_index: Option<u32>,
    /// Durable edge state.
    pub state: EdgeState,
    /// Terminal resolution timestamp.
    pub resolved_at: Option<Timestamp>,
    /// Edge CAS version.
    pub version: Version,
}

/// The non-durable operational phase vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OperationalPhase {
    /// Ready or active attempt work exists.
    Executing,
    /// Only or partly budget-waiting work exists.
    AwaitingBudget,
    /// Only or partly pending approvals exist.
    AwaitingApproval,
    /// Only or partly persisted retry delays exist.
    RetryDelay,
    /// Only or partly Maps waiting for children exist.
    WaitingChildren,
    /// Two or more operational categories are active.
    Mixed,
}

/// Snapshot counts used to derive operational phase.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOperationalCounts {
    /// Ready node count.
    pub ready: u64,
    /// Started active-attempt count.
    pub running_attempts: u64,
    /// BudgetWaiting node count.
    pub budget_waiting: u64,
    /// Pending approval count.
    pub pending_approvals: u64,
    /// RetryWaiting node count.
    pub retry_waiting: u64,
    /// WaitingChildren Map count.
    pub maps_waiting_children: u64,
}

/// One computed run operational snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOperationalView {
    /// Derived phase.
    pub phase: OperationalPhase,
    /// Category counts.
    pub counts: RunOperationalCounts,
    /// Earliest durable deadline.
    pub next_due_at: Option<Timestamp>,
}

/// A run projection with its non-durable view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowRunView {
    /// Durable run entity.
    pub run: WorkflowRun,
    /// Present only for a conforming Running run.
    pub operational: Option<RunOperationalView>,
}

impl RunState {
    /// Returns whether no ordinary workflow transition can leave this state.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::ContractFailed
                | Self::RetriesExhausted
                | Self::BudgetExhausted
                | Self::Cancelled
                | Self::CorruptStorage
        )
    }
}

impl NodeState {
    /// Returns whether the node has reached an ordinary terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::ContractFailed
                | Self::RetriesExhausted
                | Self::BudgetExhausted
                | Self::Cancelled
                | Self::CorruptStorage
                | Self::Skipped
        )
    }
}

impl RunOperationalCounts {
    /// Derives the closed operational phase from one consistent snapshot.
    pub fn phase(&self) -> Option<OperationalPhase> {
        let categories = [
            self.ready > 0 || self.running_attempts > 0,
            self.budget_waiting > 0,
            self.pending_approvals > 0,
            self.retry_waiting > 0,
            self.maps_waiting_children > 0,
        ];
        let active = categories.iter().filter(|present| **present).count();
        if active == 0 {
            None
        } else if active > 1 {
            Some(OperationalPhase::Mixed)
        } else if categories[0] {
            Some(OperationalPhase::Executing)
        } else if categories[1] {
            Some(OperationalPhase::AwaitingBudget)
        } else if categories[2] {
            Some(OperationalPhase::AwaitingApproval)
        } else if categories[3] {
            Some(OperationalPhase::RetryDelay)
        } else {
            Some(OperationalPhase::WaitingChildren)
        }
    }
}
