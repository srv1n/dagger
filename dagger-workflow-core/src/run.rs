//! Durable run, node, attempt, edge, state, and operational-view types from contract sections 1 and 2.

use crate::artifact::{ArtifactRef, FailedReadClass, JsonRef};
use crate::ids::{CostUnits, Digest, Id, NodeInstanceId, Timestamp, TopologicalRank, Version};
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};

/// The closed durable run state vocabulary. Contract section 2.1.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RunState {
    /// Awaiting compatibility acceptance. Contract section 2.1.
    Pending,
    /// Scheduling and clock transitions are allowed. Contract section 2.1.
    Running,
    /// Recoverably suspended for incompatible pins. Contract section 2.1.
    BlockedIncompatible,
    /// Successful terminal state. Contract section 2.1.
    Succeeded,
    /// Permanent domain failure. Contract section 2.1.
    Failed,
    /// Runtime contract failure. Contract section 2.1.
    ContractFailed,
    /// Retry ceiling exhaustion. Contract section 2.1.
    RetriesExhausted,
    /// Permanently infeasible budget. Contract section 2.1.
    BudgetExhausted,
    /// Cancellation terminal state. Contract section 2.1.
    Cancelled,
    /// Absorbing committed-object corruption. Contract section 2.1.
    CorruptStorage,
}

/// The closed durable node state vocabulary. Contract section 2.2.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NodeState {
    /// Incoming facts are incomplete. Contract section 2.2.
    Pending,
    /// Node is eligible for execution. Contract section 2.2.
    Ready,
    /// An Action attempt is active. Contract section 2.2.
    Running,
    /// Persisted retry delay is pending. Contract section 2.2.
    RetryWaiting,
    /// Reservation pressure temporarily blocks admission. Contract section 2.2.
    BudgetWaiting,
    /// A durable approval gate is pending. Contract section 2.2.
    WaitingApproval,
    /// A Map awaits children. Contract section 2.2.
    WaitingChildren,
    /// The node is recoverably incompatible. Contract section 2.2.
    BlockedIncompatible,
    /// Successful terminal state. Contract section 2.2.
    Succeeded,
    /// Permanent node failure. Contract section 2.2.
    Failed,
    /// Runtime contract failure. Contract section 2.2.
    ContractFailed,
    /// Retry ceiling exhaustion. Contract section 2.2.
    RetriesExhausted,
    /// Permanently infeasible budget. Contract section 2.2.
    BudgetExhausted,
    /// Cancellation terminal state. Contract section 2.2.
    Cancelled,
    /// Absorbing committed-object corruption. Contract section 2.2.
    CorruptStorage,
    /// No active incoming path reaches this node. Contract section 2.2.
    Skipped,
}

/// Node states that may be saved during compatibility suspension. Contract section 1.5.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BlockedFromState {
    /// Previously Pending. Contract section 1.5.
    Pending,
    /// Previously Ready. Contract section 1.5.
    Ready,
    /// Previously RetryWaiting. Contract section 1.5.
    RetryWaiting,
    /// Previously BudgetWaiting. Contract section 1.5.
    BudgetWaiting,
}

/// The closed immutable attempt state vocabulary. Contract section 2.3.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AttemptState {
    /// Live reserved attempt. Contract section 2.3.
    Started,
    /// Accepted successful result. Contract section 2.3.
    Succeeded,
    /// Structured retryable failure. Contract section 2.3.
    RetryableFailed,
    /// Structured permanent failure. Contract section 2.3.
    PermanentFailed,
    /// Pinned contract violation. Contract section 2.3.
    ContractFailed,
    /// Database deadline elapsed. Contract section 2.3.
    TimedOut,
    /// Dead-generation recovery outcome. Contract section 2.3.
    UnknownOutcome,
    /// Run-terminalization cancellation. Contract section 2.3.
    Cancelled,
    /// Live completion lost the active-attempt fence. Contract section 2.3.
    Stale,
}

/// The closed approval gate state vocabulary. Contract section 2.4.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum GateState {
    /// Awaiting first valid resolution. Contract section 2.4.
    Pending,
    /// Human-approved terminal state. Contract section 2.4.
    Approved,
    /// Human-rejected terminal state. Contract section 2.4.
    Rejected,
    /// Expiry-approved terminal state. Contract section 2.4.
    ExpiredApproved,
    /// Expiry-rejected terminal state. Contract section 2.4.
    ExpiredRejected,
    /// Cancellation terminal state. Contract section 2.4.
    Cancelled,
}

/// The closed edge-fact state vocabulary. Contract section 2.4.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EdgeState {
    /// Unresolved edge fact. Contract section 2.4.
    Dormant,
    /// Active successful path. Contract section 2.4.
    Satisfied,
    /// Inactive path. Contract section 2.4.
    Skipped,
}

/// The closed node kind vocabulary. Contract section 1.5.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    /// Executable Action. Contract section 1.5.
    Action,
    /// Bounded Map. Contract section 1.5.
    Map,
    /// Deterministic Choice. Contract section 1.5.
    Choice,
    /// Durable Approval. Contract section 1.5.
    Approval,
    /// Successful terminal. Contract section 1.5.
    Succeed,
    /// Explicit failure terminal. Contract section 1.5.
    Fail,
}

macro_rules! failure_kind {
    ($name:ident, $section:literal) => {
        #[doc = concat!("The closed workflow failure vocabulary. Contract section ", $section, ".")]
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub enum $name {
            /// Permanent action error. Contract section 1.4.
            ActionPermanent,
            /// Explicit Fail node. Contract section 1.4.
            ExplicitFailNode,
            /// Failed Map child. Contract section 1.4.
            MapChildFailed,
            /// Human rejection. Contract section 1.4.
            ApprovalRejected,
            /// Reject-on-expiry. Contract section 1.4.
            ApprovalExpiredRejected,
            /// Dynamic-node ceiling. Contract section 1.4.
            RunDynamicNodeLimitExceeded,
            /// Total-attempt ceiling. Contract section 1.4.
            RunAttemptLimitExceeded,
            /// Inline JSON ceiling. Contract section 1.4.
            InlineJsonLimitExceeded,
            /// Per-attempt artifact ceiling. Contract section 1.4.
            ArtifactsPerAttemptLimitExceeded,
            /// Aggregate object-byte ceiling. Contract section 1.4.
            AggregateObjectLimitExceeded,
            /// Root output schema mismatch. Contract section 1.4.
            RunOutputSchemaMismatch,
            /// Unavailable binding source. Contract section 1.4.
            BindingSourceUnavailable,
            /// Missing binding pointer. Contract section 1.4.
            BindingPointerMissing,
            /// Binding type mismatch. Contract section 1.4.
            BindingTypeMismatch,
            /// Action output schema mismatch. Contract section 1.4.
            ActionOutputSchemaMismatch,
            /// Invalid Choice input. Contract section 1.4.
            ChoiceInputInvalid,
            /// Invalid Map input. Contract section 1.4.
            MapInputInvalid,
            /// Map bound exceeded. Contract section 1.4.
            MapBoundExceeded,
            /// Invalid approval payload. Contract section 1.4.
            ApprovalPayloadInvalid,
            /// Action reported cost beyond its declaration. Contract section 1.4.
            ActionCostProtocolViolation,
        }
    };
}

failure_kind!(RunFailureKind, "1.4");
failure_kind!(NodeFailureKind, "1.5");

/// The closed attempt error-class vocabulary. Contract section 1.7.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AttemptErrorClass {
    /// Retryable action failure. Contract section 1.7.
    Retryable,
    /// Permanent action failure. Contract section 1.7.
    Permanent,
    /// Contract violation. Contract section 1.7.
    Contract,
}

/// Immutable seven-ceiling run limits. Contract section 1.4.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunLimits {
    /// Total synthetic Map child ceiling. Contract section 1.4.
    pub max_dynamic_node_instances: u64,
    /// Total started attempt ceiling. Contract section 1.4.
    pub max_total_attempts: u64,
    /// Total event ceiling. Contract section 1.4.
    pub max_total_events: u64,
    /// Per-value canonical JSON byte ceiling. Contract section 1.4.
    pub max_inline_json_bytes_per_value: u64,
    /// Per-attempt action artifact ceiling. Contract section 1.4.
    pub max_artifacts_per_attempt: u64,
    /// Per-run charged ArtifactRef byte ceiling. Contract section 1.4.
    pub max_aggregate_object_bytes_per_run: u64,
    /// Run lifetime ceiling in milliseconds. Contract section 1.4.
    pub max_run_lifetime_ms: u64,
}

/// One durable workflow run. Contract section 1.4.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowRun {
    /// Run scope. Contract section 1.4.
    pub scope: ExecutionScope,
    /// Run ID. Contract section 1.4.
    pub run_id: Id,
    /// Pinned definition ID. Contract section 1.4.
    pub definition_id: Id,
    /// Pinned revision hash. Contract section 1.4.
    pub revision_hash: Digest,
    /// Immutable canonical input ref. Contract section 1.4.
    pub input_ref: JsonRef,
    /// Scope-bound creation fingerprint. Contract section 1.4.
    pub create_request_fingerprint: Digest,
    /// Durable run state. Contract section 2.1.
    pub status: RunState,
    /// Closed terminal failure kind. Contract section 1.4.
    pub failure_kind: Option<RunFailureKind>,
    /// Optional terminal diagnostics. Contract section 1.4.
    pub failure_diagnostics_ref: Option<JsonRef>,
    /// Successful root output. Contract section 1.4.
    pub output_ref: Option<JsonRef>,
    /// Immutable budget limit. Contract section 1.4.
    pub budget_limit: CostUnits,
    /// Monotonic consumed cost. Contract section 1.4.
    pub budget_consumed: CostUnits,
    /// Currently reserved cost. Contract section 1.4.
    pub budget_reserved: CostUnits,
    /// Monotonic dynamic-node count. Contract section 1.4.
    pub dynamic_node_count: u64,
    /// Monotonic attempt count. Contract section 1.4.
    pub total_attempt_count: u64,
    /// Charged ArtifactRef byte count. Contract section 1.4.
    pub aggregate_object_bytes: u64,
    /// Immutable run limits. Contract section 1.4.
    pub limits: RunLimits,
    /// Immutable database-clock lifetime deadline. Contract section 1.4.
    pub lifetime_deadline_at: Timestamp,
    /// Frontier change epoch. Contract section 1.4.
    pub frontier_epoch: u64,
    /// Last allocated event sequence. Contract section 1.4.
    pub last_event_seq: u64,
    /// Creation timestamp. Contract section 1.4.
    pub created_at: Timestamp,
    /// Last mutation timestamp. Contract section 1.4.
    pub updated_at: Timestamp,
    /// First Running timestamp. Contract section 1.4.
    pub started_at: Option<Timestamp>,
    /// Terminal or integrity-override timestamp. Contract section 1.4.
    pub finished_at: Option<Timestamp>,
    /// Incompatibility evidence ref. Contract section 1.4.
    pub blocked_incompatibilities_ref: Option<JsonRef>,
    /// Exact suspension replay fingerprint. Contract section 1.4.
    pub blocked_incompatibility_fingerprint: Option<Digest>,
    /// Bad committed ArtifactRef ID. Contract section 1.4.
    pub corrupt_bad_artifact_ref_id: Option<Id>,
    /// Optional node owning the corrupt ref. Contract section 1.4.
    pub corrupt_owner_node_id: Option<NodeInstanceId>,
    /// Closed corruption class. Contract section 1.4.
    pub corrupt_error_class: Option<FailedReadClass>,
    /// Opaque proof fingerprint. Contract section 1.4.
    pub corrupt_proof_fingerprint: Option<Digest>,
    /// Run CAS version. Contract section 1.4.
    pub version: Version,
}

/// One durable static or synthetic node instance. Contract section 1.5.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeRun {
    /// Node scope. Contract section 1.5.
    pub scope: ExecutionScope,
    /// Owning run ID. Contract section 1.5.
    pub run_id: Id,
    /// Static or synthetic instance ID. Contract section 1.5.
    pub node_instance_id: NodeInstanceId,
    /// Definition node ID. Contract section 1.5.
    pub definition_node_id: Id,
    /// Immutable node kind. Contract section 1.5.
    pub kind: NodeKind,
    /// Optional parent Map instance. Contract section 1.5.
    pub parent_map_instance_id: Option<NodeInstanceId>,
    /// Optional Map item index. Contract section 1.5.
    pub map_item_index: Option<u32>,
    /// Optional Map item digest. Contract section 1.5.
    pub map_item_digest: Option<Digest>,
    /// Persisted canonical recovery rank. Contract section 1.5.
    pub topological_rank: TopologicalRank,
    /// Durable node state. Contract section 2.2.
    pub status: NodeState,
    /// Saved compatibility-suspension state. Contract section 1.5.
    pub blocked_from_status: Option<BlockedFromState>,
    /// Only attempt permitted to complete. Contract section 1.5.
    pub active_attempt_id: Option<Id>,
    /// Started attempt count. Contract section 1.5.
    pub attempt_count: u32,
    /// Persisted retry eligibility. Contract section 1.5.
    pub next_eligible_at: Option<Timestamp>,
    /// Declared cost while BudgetWaiting. Contract section 1.5.
    pub budget_wait_amount: Option<CostUnits>,
    /// Successful result ref. Contract section 1.5.
    pub result_ref: Option<JsonRef>,
    /// Closed terminal failure kind. Contract section 1.5.
    pub failure_kind: Option<NodeFailureKind>,
    /// Optional terminal diagnostics. Contract section 1.5.
    pub failure_diagnostics_ref: Option<JsonRef>,
    /// Immutable incoming edge count. Contract section 1.5.
    pub incoming_total: u32,
    /// Satisfied incoming edge count. Contract section 1.5.
    pub incoming_satisfied: u32,
    /// Skipped incoming edge count. Contract section 1.5.
    pub incoming_skipped: u32,
    /// Committed Choice input. Contract section 1.5.
    pub choice_input_ref: Option<JsonRef>,
    /// Committed Choice selection. Contract section 1.5.
    pub choice_selected_case: Option<ChoiceSelection>,
    /// Committed Map input. Contract section 1.5.
    pub map_input_ref: Option<JsonRef>,
    /// Map expansion digest. Contract section 1.5.
    pub map_expansion_digest: Option<Digest>,
    /// Persisted Map child count. Contract section 1.5.
    pub map_child_count: Option<u32>,
    /// Durable approval gate ID. Contract section 1.5.
    pub approval_gate_id: Option<Id>,
    /// Creation timestamp. Contract section 1.5.
    pub created_at: Timestamp,
    /// Last mutation timestamp. Contract section 1.5.
    pub updated_at: Timestamp,
    /// Node CAS version. Contract section 1.5.
    pub version: Version,
}

/// The closed committed Choice selection. Contract section 1.5.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChoiceSelection {
    /// An ordered matching case. Contract section 1.5.
    Case {
        /// Zero-based case index. Contract section 1.5.
        case_index: u32,
        /// Selected deterministic edge ID. Contract section 1.5.
        edge_id: Id,
    },
    /// The required default branch. Contract section 1.5.
    Default {
        /// Selected deterministic edge ID. Contract section 1.5.
        edge_id: Id,
    },
}

/// One immutable attempt row. Contract section 1.7.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeAttempt {
    /// Attempt scope. Contract section 1.7.
    pub scope: ExecutionScope,
    /// Owning run ID. Contract section 1.7.
    pub run_id: Id,
    /// Attempt ID. Contract section 1.7.
    pub attempt_id: Id,
    /// Owning node instance. Contract section 1.7.
    pub node_instance_id: NodeInstanceId,
    /// One-based attempt number. Contract section 1.7.
    pub attempt_number: u32,
    /// Worker label. Contract section 1.7.
    pub worker_id: Id,
    /// Claiming engine label. Contract section 1.7.
    pub engine_instance_id: Id,
    /// Claiming engine generation. Contract section 1.7.
    pub engine_generation: u64,
    /// Persisted completion capability digest. Contract section 1.7.
    pub completion_credential_digest: Digest,
    /// Immutable ActionInvocation ID. Contract section 1.7.
    pub invocation_id: Id,
    /// Retry-stable external idempotency key. Contract section 1.7.
    pub idempotency_key: String,
    /// Durable attempt state. Contract section 2.3.
    pub status: AttemptState,
    /// Declared maximum cost. Contract section 1.7.
    pub declared_max_cost: CostUnits,
    /// Immutable reserved cost. Contract section 1.7.
    pub reserved_cost: CostUnits,
    /// Terminal settled cost. Contract section 1.7.
    pub settled_cost: Option<CostUnits>,
    /// Database-clock deadline. Contract section 1.7.
    pub deadline_at: Timestamp,
    /// Start timestamp. Contract section 1.7.
    pub started_at: Timestamp,
    /// Terminal timestamp. Contract section 1.7.
    pub finished_at: Option<Timestamp>,
    /// Successful output ref. Contract section 1.7.
    pub output_ref: Option<JsonRef>,
    /// Ordered action artifacts. Contract section 1.7.
    pub artifact_refs: Vec<ArtifactRef>,
    /// Closed terminal error class. Contract section 1.7.
    pub error_class: Option<AttemptErrorClass>,
    /// Namespaced action error code. Contract section 1.7.
    pub error_code: Option<String>,
    /// Optional diagnostics ref. Contract section 1.7.
    pub diagnostics_ref: Option<JsonRef>,
}

/// One immutable control-edge fact. Contract section 1.6.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EdgeFact {
    /// Edge scope. Contract section 1.6.
    pub scope: ExecutionScope,
    /// Owning run ID. Contract section 1.6.
    pub run_id: Id,
    /// Deterministic edge ID. Contract section 1.6.
    pub edge_id: Id,
    /// Source node instance. Contract section 1.6.
    pub from_node_id: NodeInstanceId,
    /// Target node instance. Contract section 1.6.
    pub to_node_id: NodeInstanceId,
    /// Optional Choice case index. Contract section 1.6.
    pub choice_case_index: Option<u32>,
    /// Durable edge state. Contract section 2.4.
    pub state: EdgeState,
    /// Terminal resolution timestamp. Contract section 1.6.
    pub resolved_at: Option<Timestamp>,
    /// Edge CAS version. Contract section 1.6.
    pub version: Version,
}

/// The non-durable operational phase vocabulary. Contract section 5.4.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OperationalPhase {
    /// Ready or active attempt work exists. Contract section 5.4.
    Executing,
    /// Only or partly budget-waiting work exists. Contract section 5.4.
    AwaitingBudget,
    /// Only or partly pending approvals exist. Contract section 5.4.
    AwaitingApproval,
    /// Only or partly persisted retry delays exist. Contract section 5.4.
    RetryDelay,
    /// Only or partly Maps waiting for children exist. Contract section 5.4.
    WaitingChildren,
    /// Two or more operational categories are active. Contract section 5.4.
    Mixed,
}

/// Snapshot counts used to derive operational phase. Contract section 5.4.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOperationalCounts {
    /// Ready node count. Contract section 5.4.
    pub ready: u64,
    /// Started active-attempt count. Contract section 5.4.
    pub running_attempts: u64,
    /// BudgetWaiting node count. Contract section 5.4.
    pub budget_waiting: u64,
    /// Pending approval count. Contract section 5.4.
    pub pending_approvals: u64,
    /// RetryWaiting node count. Contract section 5.4.
    pub retry_waiting: u64,
    /// WaitingChildren Map count. Contract section 5.4.
    pub maps_waiting_children: u64,
}

/// One computed run operational snapshot. Contract section 5.4.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOperationalView {
    /// Derived phase. Contract section 5.4.
    pub phase: OperationalPhase,
    /// Category counts. Contract section 5.4.
    pub counts: RunOperationalCounts,
    /// Earliest durable deadline. Contract section 5.4.
    pub next_due_at: Option<Timestamp>,
}

/// A run projection with its non-durable view. Contract section 5.4.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowRunView {
    /// Durable run entity. Contract section 5.4.
    pub run: WorkflowRun,
    /// Present only for a conforming Running run. Contract section 5.4.
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
