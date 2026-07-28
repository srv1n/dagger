//! Atomic domain-command and scoped read boundary from contract section 5.

use crate::action::{ActionInvocation, ActionOutcome, CompatibilityReport, CompletionCredential};
use crate::approval::{ApprovalDecision, ApprovalGate, AuthenticatedPrincipal};
use crate::artifact::{ArtifactRef, FailedReadProof, VerifiedObjectRef};
use crate::definition::{PublishableDefinition, ValidationErrorKind};
use crate::event::WorkflowEvent;
use crate::ids::{CostUnits, Digest, Id, NodeInstanceId, Timestamp, Version};
use crate::revision::WorkflowRevision;
use crate::run::{
    ChoiceSelection, NodeAttempt, NodeFailureKind, NodeRun, RunLimits, RunState, WorkflowRun,
    WorkflowRunView,
};
use crate::scope::ExecutionScope;
use std::collections::BTreeMap;

/// Mutable definition metadata entity. Contract section 1.2.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionRecord {
    /// Definition scope. Contract section 1.2.
    pub scope: ExecutionScope,
    /// Definition ID. Contract section 1.2.
    pub definition_id: Id,
    /// Mutable display name. Contract section 1.2.
    pub display_name: String,
    /// Mutable description. Contract section 1.2.
    pub description: String,
    /// Creation timestamp. Contract section 1.2.
    pub created_at: Timestamp,
    /// Creating principal ID. Contract section 1.2.
    pub created_by: String,
    /// Latest immutable revision digest. Contract section 1.2.
    pub latest_revision_hash: Option<Digest>,
    /// Definition CAS version. Contract section 1.2.
    pub version: Version,
}

/// Opaque scheduler authorization capability. Contract sections 1.1 and 6.
#[derive(Clone, Eq, PartialEq)]
pub struct EnginePermit {
    instance_id: Id,
    generation: u64,
    session_token: Vec<u8>,
}

impl std::fmt::Debug for EnginePermit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnginePermit")
            .field("instance_id", &self.instance_id)
            .field("generation", &self.generation)
            .field("session_token", &"<redacted>")
            .finish()
    }
}

impl EnginePermit {
    /// Returns the non-secret engine label. Contract section 6.
    pub fn instance_id(&self) -> &Id {
        &self.instance_id
    }

    /// Returns the fenced engine generation. Contract section 6.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Mints a store-owned permit from exactly 256 bits of entropy.
    pub(crate) fn mint(instance_id: Id, generation: u64, token: [u8; 32]) -> Self {
        Self {
            instance_id,
            generation,
            session_token: token.to_vec(),
        }
    }

    /// Returns the raw capability only to store implementations.
    pub(crate) fn session_token(&self) -> &[u8] {
        &self.session_token
    }
}

/// Durable engine-claim projection without the raw token. Contract section 1.14.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineClaim {
    /// Claim scope. Contract section 1.14.
    pub scope: ExecutionScope,
    /// Fixed v0.1 control-plane ID. Contract section 1.14.
    pub control_plane_id: String,
    /// Current engine label. Contract section 1.14.
    pub instance_id: Id,
    /// Current generation. Contract section 1.14.
    pub generation: u64,
    /// Acquisition timestamp. Contract section 1.14.
    pub claimed_at: Timestamp,
    /// Last heartbeat timestamp. Contract section 1.14.
    pub heartbeat_at: Timestamp,
    /// Database-clock expiry. Contract section 1.14.
    pub expires_at: Timestamp,
    /// Claim CAS version. Contract section 1.14.
    pub version: Version,
}

/// A successfully acquired claim and one-time raw permit. Contract section 6.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquiredEngineClaim {
    /// Durable claim projection. Contract section 6.
    pub claim: EngineClaim,
    /// Opaque one-time raw session capability. Contract section 6.
    pub permit: EnginePermit,
}

/// Closed receipt command names. Contract section 1.11.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandKind {
    /// Create-run receipt. Contract section 1.11.
    CreateRun,
    /// Cancel-run receipt. Contract section 1.11.
    CancelRun,
}

/// Closed immutable receipt outcome vocabulary. Contract section 1.11.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandReceiptOutcome {
    /// A create-run transaction committed. Contract section 1.11.
    CreateRunCommitted {
        /// Created run ID. Contract section 1.11.
        run_id: Id,
        /// Always Pending. Contract section 1.11.
        status: RunState,
        /// Committed run version. Contract section 1.11.
        run_version: Version,
        /// Committed event batch ID. Contract section 1.11.
        batch_id: Id,
        /// Inclusive first event sequence. Contract section 1.11.
        first_event_seq: u64,
        /// Inclusive last event sequence. Contract section 1.11.
        last_event_seq: u64,
    },
    /// A cancel-run transaction committed. Contract section 1.11.
    CancelRunCommitted {
        /// Cancelled run ID. Contract section 1.11.
        run_id: Id,
        /// Nonterminal prior status. Contract section 1.11.
        prior_status: RunState,
        /// Always Cancelled. Contract section 1.11.
        status: RunState,
        /// Committed run version. Contract section 1.11.
        run_version: Version,
        /// Committed event batch ID. Contract section 1.11.
        batch_id: Id,
        /// Inclusive first event sequence. Contract section 1.11.
        first_event_seq: u64,
        /// Inclusive last event sequence. Contract section 1.11.
        last_event_seq: u64,
    },
}

/// One immutable idempotent command receipt. Contract section 1.11.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandReceipt {
    /// Receipt scope. Contract section 1.11.
    pub scope: ExecutionScope,
    /// Closed command kind. Contract section 1.11.
    pub command_kind: CommandKind,
    /// Opaque host token. Contract section 1.11.
    pub idempotency_token: String,
    /// Scope-bound request fingerprint. Contract section 1.11.
    pub request_fingerprint: Digest,
    /// Correlated run ID. Contract section 1.11.
    pub run_id: Id,
    /// Closed committed outcome. Contract section 1.11.
    pub outcome: CommandReceiptOutcome,
    /// Committed batch ID. Contract section 1.11.
    pub batch_id: Id,
    /// Database commit timestamp. Contract section 1.11.
    pub committed_at: Timestamp,
}

/// The closed section 5.5 domain error taxonomy. Contract section 5.5.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StoreError {
    /// No entity exists in the supplied scope. Contract section 5.5.
    #[error("not found")]
    NotFound,
    /// An immutable scoped ID already exists. Contract section 5.5.
    #[error("already exists")]
    AlreadyExists,
    /// An idempotency identity was reused with different inputs. Contract section 5.5.
    #[error("idempotency conflict")]
    IdempotencyConflict,
    /// A mutable row version or status changed. Contract section 5.5.
    #[error("compare-and-swap conflict")]
    CasConflict,
    /// The requested transition is absent from section 3. Contract section 5.5.
    #[error("illegal transition")]
    IllegalTransition,
    /// A parameter is malformed before domain evaluation. Contract section 5.5.
    #[error("invalid field")]
    InvalidField,
    /// Publication validation failed before a revision existed. Contract section 5.5.
    #[error("revision invalid at {path}: {message}")]
    RevisionInvalid {
        /// Closed validation category. Contract section 5.5.
        code: ValidationErrorKind,
        /// Bounded document path. Contract section 5.5.
        path: String,
        /// Bounded corrective message. Contract section 5.5.
        message: String,
        /// Bounded alternatives. Contract section 5.5.
        valid_alternatives: Vec<String>,
    },
    /// Creation validation failed before a run existed. Contract section 5.5.
    #[error("contract validation failed at {path}: {message}")]
    ContractValidation {
        /// Closed validation category.
        kind: ValidationErrorKind,
        /// Bounded document path.
        path: String,
        /// Bounded corrective message.
        message: String,
        /// Bounded alternatives.
        valid_alternatives: Vec<String>,
    },
    /// Runtime validation atomically produced ContractFailed. Contract section 5.5.
    #[error("contract validation applied")]
    ContractValidationApplied {
        /// Closed runtime validation failure that was durably applied.
        code: String,
    },
    /// The blocked-run command fence rejected the command. Contract section 5.5.
    #[error("run is blocked incompatible")]
    RunBlockedIncompatible,
    /// Creation limits are inconsistent. Contract section 5.5.
    #[error("run limits invalid")]
    RunLimitsInvalid,
    /// A durable run limit terminalized the run. Contract section 5.5.
    #[error("run limit applied")]
    RunLimitApplied {
        /// Closed run-limit failure that was durably applied.
        code: String,
    },
    /// A non-expired engine claim exists. Contract section 5.5.
    #[error("engine already live")]
    EngineAlreadyLive {
        /// Current scoped engine owner label.
        owner: Id,
        /// Database-clock expiry of the current live claim.
        expires_at: Timestamp,
    },
    /// The permit no longer owns the generation. Contract section 5.5.
    #[error("engine claim lost")]
    EngineClaimLost,
    /// The permit's claim expired. Contract section 5.5.
    #[error("engine claim expired")]
    EngineClaimExpired,
    /// Exact action pins are unavailable. Contract section 5.5.
    #[error("incompatible action pins")]
    IncompatiblePins,
    /// Resume evidence remains incompatible. Contract section 5.5.
    #[error("still incompatible")]
    StillIncompatible {
        /// Exact pin locations that remain unavailable.
        pins: Vec<String>,
    },
    /// Compatibility evidence is malformed or oversized. Contract section 5.5.
    #[error("compatibility evidence invalid")]
    EvidenceInvalid,
    /// A semantic digest substitution was proposed. Contract section 5.5.
    #[error("compatibility override forbidden")]
    CompatibilityOverrideForbidden,
    /// Result intake lacked the attempt capability. Contract section 5.5.
    #[error("invalid completion credential")]
    InvalidCompletionCredential,
    /// Diagnostics violated the closed envelope. Contract section 5.5.
    #[error("diagnostics invalid at {path}: {code}")]
    DiagnosticsInvalid {
        /// Failing diagnostics path. Contract section 5.5.
        path: String,
        /// Closed validation code. Contract section 5.5.
        code: String,
    },
    /// Diagnostics exceeded 65,536 canonical bytes. Contract section 5.5.
    #[error("diagnostics too large: {observed_bytes} > {limit_bytes}")]
    DiagnosticsTooLarge {
        /// Mandatory byte limit. Contract section 5.5.
        limit_bytes: u64,
        /// Observed canonical byte length. Contract section 5.5.
        observed_bytes: u64,
    },
    /// Attempt ID conflicts with immutable inputs. Contract section 5.5.
    #[error("attempt id conflict")]
    AttemptIdConflict,
    /// Attempt or active-node fencing failed. Contract section 5.5.
    #[error("attempt fenced")]
    AttemptFenced,
    /// Recovery encountered a current-generation attempt. Contract section 5.5.
    #[error("current generation attempt present")]
    CurrentGenerationAttemptPresent,
    /// The attempt deadline is not due. Contract section 5.5.
    #[error("deadline not due")]
    DeadlineNotDue {
        /// Database time observed inside the rejected transaction.
        database_now: Timestamp,
        /// Persisted attempt deadline.
        deadline: Timestamp,
    },
    /// The retry timestamp is not due. Contract section 5.5.
    #[error("retry not due")]
    RetryNotDue {
        /// Database time observed inside the rejected transaction.
        database_now: Timestamp,
        /// Persisted next retry eligibility time.
        next_eligible_at: Timestamp,
    },
    /// The gate expiry is not due. Contract section 5.5.
    #[error("expiry not due")]
    ExpiryNotDue {
        /// Database time observed inside the rejected transaction.
        database_now: Timestamp,
        /// Persisted approval expiry.
        expires_at: Timestamp,
    },
    /// The run lifetime is not due. Contract section 5.5.
    #[error("lifetime not due")]
    LifetimeNotDue {
        /// Database time observed inside the rejected transaction.
        database_now: Timestamp,
        /// Persisted run lifetime deadline.
        lifetime_deadline_at: Timestamp,
    },
    /// The authenticated principal is unauthorized. Contract section 5.5.
    #[error("approval unauthorized")]
    ApprovalUnauthorized,
    /// A different decision already resolved the gate. Contract section 5.5.
    #[error("approval already resolved")]
    ApprovalAlreadyResolved,
    /// Expiry or cancellation lost the gate CAS. Contract section 5.5.
    #[error("approval race lost")]
    ApprovalRaceLost,
    /// Host cancellation lost an observed-version race. Contract section 5.5.
    #[error("cancellation race lost")]
    CancellationRaceLost,
    /// The run is already terminal. Contract section 5.5.
    #[error("run already terminal")]
    RunAlreadyTerminal,
    /// A Map still has incomplete children. Contract section 5.5.
    #[error("children incomplete")]
    ChildrenIncomplete,
    /// A Map aggregate does not match its child set. Contract section 5.5.
    #[error("aggregate mismatch")]
    AggregateMismatch,
    /// A proposed object lacks verified capability proof. Contract section 5.5.
    #[error("object not verified")]
    ObjectNotVerified,
    /// Proposed bytes or metadata do not match a digest. Contract section 5.5.
    #[error("digest mismatch")]
    DigestMismatch,
    /// Same-digest artifact metadata conflicts. Contract section 5.5.
    #[error("artifact metadata conflict")]
    ArtifactMetadataConflict,
    /// A corruption proof is invalid. Contract section 5.5.
    #[error("invalid failed read proof")]
    InvalidFailedReadProof,
    /// Publication target and canonical definition IDs differ. Contract section 5.5.
    #[error("revision definition id mismatch")]
    RevisionDefinitionIdMismatch,
    /// A schema document uses unsupported features. Contract section 5.5.
    #[error("schema subset unsupported")]
    SchemaSubsetUnsupported,
    /// A complete event batch exceeds response capacity. Contract section 5.5.
    #[error("event batch too large")]
    BatchTooLarge,
    /// Database time regressed relative to claim state. Contract section 5.5.
    #[error("database clock is non-monotonic")]
    ClockNonMonotonic,
    /// A derived view found impossible durable state. Contract section 5.5.
    #[error("corrupt control plane")]
    CorruptControlPlane,
    /// Checked arithmetic overflowed or found a broken invariant. Contract section 5.5.
    #[error("arithmetic overflow")]
    ArithmeticOverflow,
    /// Durable storage is unavailable. Contract section 5.5.
    #[error("storage unavailable")]
    StorageUnavailable,
    /// The control-plane transaction failed. Contract section 5.5.
    #[error("transaction failed")]
    TransactionFailed,
}

/// Parameters for definition creation. Contract section 5.2.
pub struct CreateDefinition {
    /// Caller-chosen scoped definition ID. Contract section 5.2.
    pub definition_id: Id,
    /// Display name. Contract section 5.2.
    pub display_name: String,
    /// Description. Contract section 5.2.
    pub description: String,
    /// Authenticated creating principal. Contract section 5.2.
    pub principal: AuthenticatedPrincipal,
}

/// Parameters for metadata update. Contract section 5.2.
pub struct UpdateDefinitionMetadata {
    /// Definition ID. Contract section 5.2.
    pub definition_id: Id,
    /// Expected definition version. Contract section 5.2.
    pub expected_version: Version,
    /// Replacement display name. Contract section 5.2.
    pub display_name: String,
    /// Replacement description. Contract section 5.2.
    pub description: String,
}

/// Resolved schema objects for one action pin. Contract section 5.2.
pub struct ResolvedActionSchemas {
    /// Verified input schema object. Contract section 5.2.
    pub input_schema: VerifiedObjectRef,
    /// Verified output schema object. Contract section 5.2.
    pub output_schema: VerifiedObjectRef,
}

/// Parameters for immutable revision publication. Contract section 5.2.
pub struct PublishRevision {
    /// Target definition ID. Contract section 5.2.
    pub definition_id: Id,
    /// Expected mutable definition version. Contract section 5.2.
    pub expected_definition_version: Version,
    /// Verified canonical definition object. Contract section 5.2.
    pub canonical_definition: VerifiedObjectRef,
    /// Verified root input schema. Contract section 5.2.
    pub run_input_schema: VerifiedObjectRef,
    /// Verified root output schema. Contract section 5.2.
    pub run_output_schema: VerifiedObjectRef,
    /// Schema objects keyed by action reference location. Contract section 5.2.
    pub resolved_action_schema_objects: BTreeMap<String, ResolvedActionSchemas>,
    /// Fully validated parsed revision. Contract section 5.2.
    pub parsed_revision: PublishableDefinition,
    /// Authenticated publishing principal. Contract section 5.2.
    pub principal: AuthenticatedPrincipal,
}

/// Parameters for idempotent run creation. Contract section 5.3.
pub struct CreateRun {
    /// Caller-chosen run ID. Contract section 5.3.
    pub run_id: Id,
    /// Definition ID. Contract section 5.3.
    pub definition_id: Id,
    /// Immutable revision hash. Contract section 5.3.
    pub revision_hash: Digest,
    /// Verified canonical run input. Contract section 5.3.
    pub input: VerifiedObjectRef,
    /// Immutable budget limit. Contract section 5.3.
    pub budget_limit: CostUnits,
    /// Fully resolved seven run limits. Contract section 5.3.
    pub limits: RunLimits,
    /// Authenticated creating principal. Contract section 5.3.
    pub principal: AuthenticatedPrincipal,
    /// Opaque 128-bit-or-stronger token. Contract section 1.11.
    pub idempotency_token: String,
}

/// Parameters for starting a compatible run. Contract section 5.3.
pub struct StartRun {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Fresh complete compatibility report. Contract section 13.3.
    pub compatibility_evidence: CompatibilityReport,
}

/// Parameters for compatibility suspension. Contract section 5.3.
pub struct SuspendIncompatible {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Verified bounded incompatibility evidence object. Contract section 5.3.
    pub incompatibilities: VerifiedObjectRef,
    /// Fresh exact registry evidence. Contract section 5.3.
    pub evidence: CompatibilityReport,
}

/// Parameters for compatible resume. Contract section 5.3.
pub struct ResumeCompatible {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Fresh complete availability evidence. Contract section 5.3.
    pub availability_evidence: CompatibilityReport,
}

/// Parameters for an atomic action attempt claim. Contract section 5.3.
pub struct ClaimNodeAttempt {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Action node instance ID. Contract section 5.3.
    pub node_id: NodeInstanceId,
    /// Expected node version. Contract section 5.3.
    pub expected_node_version: Version,
    /// Caller-chosen attempt ID. Contract section 5.3.
    pub attempt_id: Id,
    /// Worker label. Contract section 5.3.
    pub worker_id: Id,
    /// Verified exact canonical bound input. Contract section 5.3.
    pub bound_input: VerifiedObjectRef,
    /// Ordered binding derivation digest. Contract section 8.2.
    pub binding_derivation_digest: Digest,
}

/// Closed atomic claim results. Contract section 5.3.
pub enum ClaimNodeAttemptResult {
    /// Attempt and invocation committed. Contract section 5.3.
    Claimed {
        /// Immutable invocation. Contract section 5.3.
        invocation: ActionInvocation,
        /// One-time raw completion capability. Contract section 5.3.
        completion_credential: CompletionCredential,
    },
    /// Reservation-only shortage persisted BudgetWaiting. Contract section 5.3.
    BudgetWaitingApplied(NodeRun),
    /// A Map concurrency slot is temporarily unavailable; no state changed. Contract section 5.3.
    MapConcurrencyLimited,
    /// Permanent budget exhaustion terminalized the run. Contract section 5.3.
    BudgetExhaustedApplied(WorkflowRun),
    /// A run-limit outcome was atomically applied. Contract section 5.3.
    RunLimitApplied(WorkflowRun),
}

/// Verified refs supplied alongside a submitted action outcome. Contract section 5.3.
pub struct CompletionObjects {
    /// Verified success output when submitted. Contract section 5.3.
    pub output: Option<VerifiedObjectRef>,
    /// Ordered verified action artifacts. Contract section 5.3.
    pub artifacts: Vec<VerifiedObjectRef>,
    /// Verified closed diagnostics when submitted. Contract section 5.3.
    pub diagnostics: Option<VerifiedObjectRef>,
}

/// Parameters for credential-authenticated result intake. Contract section 5.3.
pub struct CompleteAttempt {
    /// Per-attempt completion capability. Contract section 5.3.
    pub completion_credential: CompletionCredential,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Node instance ID. Contract section 5.3.
    pub node_id: NodeInstanceId,
    /// Attempt ID. Contract section 5.3.
    pub attempt_id: Id,
    /// Closed submitted action outcome. Contract section 7.2.
    pub submitted_outcome: ActionOutcome,
    /// Verified objects corresponding to the outcome. Contract section 5.3.
    pub objects: CompletionObjects,
}

/// Closed complete-attempt command outcomes. Contract section 5.3.
pub enum CompleteAttemptResult {
    /// The active attempt and node outcome applied. Contract section 5.3.
    Applied(NodeAttempt),
    /// A persisted retry was scheduled. Contract section 5.3.
    RetryScheduled(NodeRun),
    /// The accepted outcome terminalized the run. Contract section 5.3.
    TerminalRun(WorkflowRun),
    /// A due result timed out and was observed stale. Contract section 5.3.
    TimedOutAndStaleRecorded(NodeAttempt),
    /// A stale completion observation committed. Contract section 5.3.
    StaleRecorded(NodeAttempt),
    /// The terminal attempt was already observed. Contract section 5.3.
    AlreadyObserved(NodeAttempt),
}

/// Parameters for database-clock attempt timeout. Contract section 5.3.
pub struct TimeoutAttempt {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Node instance ID. Contract section 5.3.
    pub node_id: NodeInstanceId,
    /// Attempt ID. Contract section 5.3.
    pub attempt_id: Id,
}

/// Parameters for one-run abandoned-attempt recovery. Contract section 5.3.
pub struct RecoverAbandonedAttemptsForRun {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
}

/// Parameters for retry eligibility release. Contract section 5.3.
pub struct ReleaseRetry {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Node instance ID. Contract section 5.3.
    pub node_id: NodeInstanceId,
    /// Expected node version. Contract section 5.3.
    pub expected_node_version: Version,
}

/// Parameters for one committed Choice decision. Contract section 5.3.
pub struct RecordChoice {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Choice node instance ID. Contract section 5.3.
    pub node_id: NodeInstanceId,
    /// Expected node version. Contract section 5.3.
    pub expected_node_version: Version,
    /// Verified canonical Choice input. Contract section 5.3.
    pub input: VerifiedObjectRef,
    /// Digest of the resolved selector scalar. Contract section 9.
    pub evaluated_selector_digest: Digest,
    /// Deterministic first-match or default selection. Contract section 5.3.
    pub selection: ChoiceSelection,
}

/// One validated ordered Map expansion item. Contract section 5.3.
pub struct OrderedMapItem {
    /// Zero-based item index. Contract section 10.1.
    pub index: u32,
    /// Canonical item digest. Contract section 10.1.
    pub item_digest: Digest,
    /// Deterministic child ID. Contract section 10.1.
    pub child_id: NodeInstanceId,
}

/// Parameters for atomic all-child Map expansion. Contract section 5.3.
pub struct ExpandMap {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Map node instance ID. Contract section 5.3.
    pub map_node_id: NodeInstanceId,
    /// Expected parent version. Contract section 5.3.
    pub expected_node_version: Version,
    /// Verified canonical Map input. Contract section 5.3.
    pub input: VerifiedObjectRef,
    /// Complete ordered item identity set. Contract section 5.3.
    pub ordered_items: Vec<OrderedMapItem>,
    /// Ordered expansion digest. Contract section 10.1.
    pub expansion_digest: Digest,
}

/// Parameters for ordered Map aggregation. Contract section 5.3.
pub struct CompleteMap {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Map node instance ID. Contract section 5.3.
    pub map_node_id: NodeInstanceId,
    /// Expected parent version. Contract section 5.3.
    pub expected_node_version: Version,
    /// Verified canonical ordered aggregate. Contract section 5.3.
    pub aggregate: VerifiedObjectRef,
}

/// Parameters for durable approval request creation. Contract section 5.3.
pub struct RequestApproval {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Approval node instance ID. Contract section 5.3.
    pub node_id: NodeInstanceId,
    /// Expected node version. Contract section 5.3.
    pub expected_node_version: Version,
    /// Deterministic gate ID. Contract section 3.5.
    pub gate_id: Id,
    /// Verified canonical approval request. Contract section 5.3.
    pub request: VerifiedObjectRef,
}

/// Parameters for an authenticated human gate decision. Contract section 5.3.
pub struct DecideApproval {
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Gate ID. Contract section 5.3.
    pub gate_id: Id,
    /// Expected run version. Contract section 5.3.
    pub expected_run_version: Version,
    /// Expected gate version. Contract section 5.3.
    pub expected_gate_version: Version,
    /// Approve or reject. Contract section 3.5.
    pub decision: ApprovalDecision,
    /// Optional verified human decision payload. Contract section 5.3.
    pub decision_payload: Option<VerifiedObjectRef>,
    /// Required exact ApprovalResult object for approval. Contract section 5.3.
    pub approval_output: Option<VerifiedObjectRef>,
    /// Scope-bound authenticated principal. Contract section 5.3.
    pub principal: AuthenticatedPrincipal,
}

/// Parameters for database-clock gate expiry. Contract section 5.3.
pub struct ExpireApproval {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Gate ID. Contract section 5.3.
    pub gate_id: Id,
    /// Exact expiry ApprovalResult when policy approves. Contract section 5.3.
    pub approval_output: Option<VerifiedObjectRef>,
}

/// Parameters for a Ready Succeed or Fail node. Contract section 5.3.
pub struct ResolveTerminalNode {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Terminal node instance ID. Contract section 5.3.
    pub node_id: NodeInstanceId,
    /// Expected node version. Contract section 5.3.
    pub expected_node_version: Version,
    /// Verified output for Succeed and none for Fail. Contract section 5.3.
    pub output: Option<VerifiedObjectRef>,
}

/// Parameters for explicit runtime contract failure. Contract section 5.3.
pub struct FailContract {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Affected node instance ID. Contract section 5.3.
    pub node_id: NodeInstanceId,
    /// Expected node version. Contract section 5.3.
    pub expected_node_version: Version,
    /// Closed legal contract failure kind. Contract section 5.3.
    pub closed_failure_kind: NodeFailureKind,
    /// Optional verified closed diagnostics. Contract section 5.3.
    pub diagnostics: Option<VerifiedObjectRef>,
}

/// One observed pending-gate CAS input for cancellation. Contract section 5.3.
pub struct ExpectedGateVersion {
    /// Gate ID. Contract section 5.3.
    pub gate_id: Id,
    /// Observed gate version. Contract section 5.3.
    pub version: Version,
}

/// Parameters for idempotent authenticated cancellation. Contract section 5.3.
pub struct CancelRun {
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Observed run version. Contract section 5.3.
    pub expected_run_version: Version,
    /// Complete sorted pending-gate version set. Contract section 5.3.
    pub expected_pending_gate_versions: Vec<ExpectedGateVersion>,
    /// Scope-bound authenticated principal. Contract section 5.3.
    pub principal: AuthenticatedPrincipal,
    /// Persistence-safe reason code. Contract section 5.3.
    pub reason_code: String,
    /// Opaque 128-bit-or-stronger token. Contract section 1.11.
    pub idempotency_token: String,
}

/// Parameters for database-clock lifetime expiry. Contract section 5.3.
pub struct ExpireRunLifetime {
    /// Live engine permit. Contract section 5.3.
    pub permit: EnginePermit,
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
}

/// Parameters for proof-backed storage corruption. Contract section 5.3.
pub struct MarkCorruptStorage {
    /// Run ID. Contract section 5.3.
    pub run_id: Id,
    /// Already-committed bad ArtifactRef. Contract section 5.3.
    pub bad_ref: ArtifactRef,
    /// Opaque failed-read proof. Contract section 12.3.
    pub proof: FailedReadProof,
    /// Optional node owning the bad ref. Contract section 5.3.
    pub owner_node_id: Option<NodeInstanceId>,
}

/// An opaque scope-bound cutoff/keyset cursor. Contract section 5.4.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanCursor(String);

impl ScanCursor {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    pub(crate) fn encoded(&self) -> &str {
        &self.0
    }
}

/// Keyset page request. Contract section 5.4.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageRequest {
    /// Opaque cursor or none for a cutoff-capturing first page. Contract section 5.4.
    pub cursor: Option<ScanCursor>,
    /// Requested row count from 1 through 1000. Contract section 5.4.
    pub page_size: u16,
}

/// One keyset result page. Contract section 5.4.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Page<T> {
    /// Ordered page items. Contract section 5.4.
    pub items: Vec<T>,
    /// Opaque continuation cursor. Contract section 5.4.
    pub next_cursor: Option<ScanCursor>,
}

/// Event-page request preserving complete atomic batches. Contract section 5.4.
pub struct EventPageRequest {
    /// Sequence after which events are returned. Contract section 5.4.
    pub after_event_seq: u64,
    /// Preferred event count. Contract section 5.4.
    pub page_size: u16,
    /// Hard serialized response-byte cap. Contract section 5.4.
    pub hard_response_byte_limit: u64,
}

/// Scope-confined atomic workflow control-plane store. Contract section 5.1.
pub trait WorkflowStore: Send + Sync {
    /// Creates mutable definition metadata. Contract section 5.2.
    async fn create_definition(
        &self,
        scope: &ExecutionScope,
        command: CreateDefinition,
    ) -> Result<DefinitionRecord, StoreError>;

    /// Updates definition metadata with CAS. Contract section 5.2.
    async fn update_definition_metadata(
        &self,
        scope: &ExecutionScope,
        command: UpdateDefinitionMetadata,
    ) -> Result<DefinitionRecord, StoreError>;

    /// Publishes one immutable validated revision. Contract section 5.2.
    async fn publish_revision(
        &self,
        scope: &ExecutionScope,
        command: PublishRevision,
    ) -> Result<WorkflowRevision, StoreError>;

    /// Acquires or takes over the scoped singleton engine claim. Contract section 5.2.
    async fn acquire_engine_claim(
        &self,
        scope: &ExecutionScope,
        instance_id: Id,
    ) -> Result<AcquiredEngineClaim, StoreError>;

    /// Renews a live engine claim. Contract section 5.2.
    async fn heartbeat_engine_claim(
        &self,
        scope: &ExecutionScope,
        permit: &EnginePermit,
    ) -> Result<EngineClaim, StoreError>;

    /// Gracefully expires a matching engine claim. Contract section 5.2.
    async fn release_engine_claim(
        &self,
        scope: &ExecutionScope,
        permit: &EnginePermit,
    ) -> Result<(), StoreError>;

    /// Creates a Pending run and its complete static graph. Contract section 5.3.
    async fn create_run(
        &self,
        scope: &ExecutionScope,
        command: CreateRun,
    ) -> Result<CommandReceipt, StoreError>;

    /// Starts a pin-compatible Pending run. Contract section 5.3.
    async fn start_run(
        &self,
        scope: &ExecutionScope,
        command: StartRun,
    ) -> Result<WorkflowRun, StoreError>;

    /// Suspends a Pending or recovered Running run. Contract section 5.3.
    async fn suspend_incompatible(
        &self,
        scope: &ExecutionScope,
        command: SuspendIncompatible,
    ) -> Result<WorkflowRun, StoreError>;

    /// Resumes a run only when every exact pin is available. Contract section 5.3.
    async fn resume_compatible(
        &self,
        scope: &ExecutionScope,
        command: ResumeCompatible,
    ) -> Result<WorkflowRun, StoreError>;

    /// Atomically claims and reserves one Action attempt. Contract section 5.3.
    async fn claim_node_attempt(
        &self,
        scope: &ExecutionScope,
        command: ClaimNodeAttempt,
    ) -> Result<ClaimNodeAttemptResult, StoreError>;

    /// Accepts or observes a credential-authenticated result. Contract section 5.3.
    async fn complete_attempt(
        &self,
        scope: &ExecutionScope,
        command: CompleteAttempt,
    ) -> Result<CompleteAttemptResult, StoreError>;

    /// Applies a due database-clock timeout. Contract section 5.3.
    async fn timeout_attempt(
        &self,
        scope: &ExecutionScope,
        command: TimeoutAttempt,
    ) -> Result<NodeAttempt, StoreError>;

    /// Recovers the complete lower-generation Started set for one run. Contract section 5.3.
    async fn recover_abandoned_attempts_for_run(
        &self,
        scope: &ExecutionScope,
        command: RecoverAbandonedAttemptsForRun,
    ) -> Result<Vec<NodeAttempt>, StoreError>;

    /// Releases a due persisted retry to Ready. Contract section 5.3.
    async fn release_retry(
        &self,
        scope: &ExecutionScope,
        command: ReleaseRetry,
    ) -> Result<NodeRun, StoreError>;

    /// Commits one deterministic Choice decision and frontier fixed point. Contract section 5.3.
    async fn record_choice(
        &self,
        scope: &ExecutionScope,
        command: RecordChoice,
    ) -> Result<NodeRun, StoreError>;

    /// Atomically expands a bounded Map child set. Contract section 5.3.
    async fn expand_map(
        &self,
        scope: &ExecutionScope,
        command: ExpandMap,
    ) -> Result<NodeRun, StoreError>;

    /// Commits an exact ordered Map aggregate. Contract section 5.3.
    async fn complete_map(
        &self,
        scope: &ExecutionScope,
        command: CompleteMap,
    ) -> Result<NodeRun, StoreError>;

    /// Creates a durable approval gate. Contract section 5.3.
    async fn request_approval(
        &self,
        scope: &ExecutionScope,
        command: RequestApproval,
    ) -> Result<ApprovalGate, StoreError>;

    /// Commits the first valid authenticated human decision. Contract section 5.3.
    async fn decide_approval(
        &self,
        scope: &ExecutionScope,
        command: DecideApproval,
    ) -> Result<ApprovalGate, StoreError>;

    /// Commits a due database-clock gate expiry. Contract section 5.3.
    async fn expire_approval(
        &self,
        scope: &ExecutionScope,
        command: ExpireApproval,
    ) -> Result<ApprovalGate, StoreError>;

    /// Resolves a Ready Succeed or Fail node. Contract section 5.3.
    async fn resolve_terminal_node(
        &self,
        scope: &ExecutionScope,
        command: ResolveTerminalNode,
    ) -> Result<WorkflowRun, StoreError>;

    /// Applies a closed runtime contract failure. Contract section 5.3.
    async fn fail_contract(
        &self,
        scope: &ExecutionScope,
        command: FailContract,
    ) -> Result<WorkflowRun, StoreError>;

    /// Idempotently cancels a nonterminal run. Contract section 5.3.
    async fn cancel_run(
        &self,
        scope: &ExecutionScope,
        command: CancelRun,
    ) -> Result<CommandReceipt, StoreError>;

    /// Cancels a run whose database lifetime deadline is due. Contract section 5.3.
    async fn expire_run_lifetime(
        &self,
        scope: &ExecutionScope,
        command: ExpireRunLifetime,
    ) -> Result<WorkflowRun, StoreError>;

    /// Applies proof-backed committed-object corruption. Contract section 5.3.
    async fn mark_corrupt_storage(
        &self,
        scope: &ExecutionScope,
        command: MarkCorruptStorage,
    ) -> Result<WorkflowRun, StoreError>;

    /// Reads one definition projection. Contract section 5.4.
    async fn get_definition(
        &self,
        scope: &ExecutionScope,
        definition_id: &Id,
    ) -> Result<DefinitionRecord, StoreError>;

    /// Reads one immutable revision. Contract section 5.4.
    async fn get_revision(
        &self,
        scope: &ExecutionScope,
        definition_id: &Id,
        revision_hash: &Digest,
    ) -> Result<WorkflowRevision, StoreError>;

    /// Reads a run and its derived operational view. Contract section 5.4.
    async fn get_run(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
    ) -> Result<WorkflowRunView, StoreError>;

    /// Reads one node instance. Contract section 5.4.
    async fn get_node(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        node_id: &NodeInstanceId,
    ) -> Result<NodeRun, StoreError>;

    /// Reads one attempt. Contract section 5.4.
    async fn get_attempt(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        attempt_id: &Id,
    ) -> Result<NodeAttempt, StoreError>;

    /// Reads one approval gate. Contract section 5.4.
    async fn get_gate(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        gate_id: &Id,
    ) -> Result<ApprovalGate, StoreError>;

    /// Lists runs with scope-bound keyset pagination. Contract section 5.4.
    async fn list_runs(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError>;

    /// Lists nodes in one scoped run. Contract section 5.4.
    async fn list_nodes(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError>;

    /// Lists events after a sequence without splitting a batch. Contract section 5.4.
    async fn list_events_after(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        page: EventPageRequest,
    ) -> Result<Vec<WorkflowEvent>, StoreError>;

    /// Scans Ready nodes at a captured cutoff. Contract section 5.4.
    async fn scan_ready_nodes(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError>;

    /// Scans BudgetWaiting nodes at a captured cutoff. Contract section 5.4.
    async fn scan_budget_waiters(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError>;

    /// Scans due active attempt deadlines. Contract section 5.4.
    async fn scan_due_deadlines(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeAttempt>, StoreError>;

    /// Scans due persisted retries. Contract section 5.4.
    async fn scan_due_retries(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError>;

    /// Scans runs containing lower-generation Started attempts. Contract section 5.4.
    async fn scan_recovery_runs(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError>;

    /// Scans nonterminal runs for compatibility rechecks. Contract section 5.4.
    async fn scan_compatibility_rechecks(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError>;

    /// Scans due Pending gates. Contract section 5.4.
    async fn scan_due_gates(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<ApprovalGate>, StoreError>;

    /// Scans due run lifetime deadlines. Contract section 5.4.
    async fn scan_due_run_lifetimes(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError>;
}
