//! Atomic domain-command and scoped read boundary.

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
use std::future::Future;

/// Mutable definition metadata entity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionRecord {
    /// Definition scope.
    pub scope: ExecutionScope,
    /// Definition ID.
    pub definition_id: Id,
    /// Mutable display name.
    pub display_name: String,
    /// Mutable description.
    pub description: String,
    /// Creation timestamp.
    pub created_at: Timestamp,
    /// Creating principal ID.
    pub created_by: String,
    /// Latest immutable revision digest.
    pub latest_revision_hash: Option<Digest>,
    /// Definition CAS version.
    pub version: Version,
}

/// Opaque scheduler authorization capability.
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
    /// Returns the non-secret engine label.
    pub fn instance_id(&self) -> &Id {
        &self.instance_id
    }

    /// Returns the fenced engine generation.
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

/// Durable engine-claim projection without the raw token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineClaim {
    /// Claim scope.
    pub scope: ExecutionScope,
    /// Fixed control-plane ID.
    pub control_plane_id: String,
    /// Current engine label.
    pub instance_id: Id,
    /// Current generation.
    pub generation: u64,
    /// Acquisition timestamp.
    pub claimed_at: Timestamp,
    /// Last heartbeat timestamp.
    pub heartbeat_at: Timestamp,
    /// Database-clock expiry.
    pub expires_at: Timestamp,
    /// Claim CAS version.
    pub version: Version,
}

/// A successfully acquired claim and one-time raw permit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquiredEngineClaim {
    /// Durable claim projection.
    pub claim: EngineClaim,
    /// Opaque one-time raw session capability.
    pub permit: EnginePermit,
}

/// Closed receipt command names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandKind {
    /// Create-run receipt.
    CreateRun,
    /// Cancel-run receipt.
    CancelRun,
}

/// Closed immutable receipt outcome vocabulary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandReceiptOutcome {
    /// A create-run transaction committed.
    CreateRunCommitted {
        /// Created run ID.
        run_id: Id,
        /// Always Pending.
        status: RunState,
        /// Committed run version.
        run_version: Version,
        /// Committed event batch ID.
        batch_id: Id,
        /// Inclusive first event sequence.
        first_event_seq: u64,
        /// Inclusive last event sequence.
        last_event_seq: u64,
    },
    /// A cancel-run transaction committed.
    CancelRunCommitted {
        /// Cancelled run ID.
        run_id: Id,
        /// Nonterminal prior status.
        prior_status: RunState,
        /// Always Cancelled.
        status: RunState,
        /// Committed run version.
        run_version: Version,
        /// Committed event batch ID.
        batch_id: Id,
        /// Inclusive first event sequence.
        first_event_seq: u64,
        /// Inclusive last event sequence.
        last_event_seq: u64,
    },
}

/// One immutable idempotent command receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandReceipt {
    /// Receipt scope.
    pub scope: ExecutionScope,
    /// Closed command kind.
    pub command_kind: CommandKind,
    /// Opaque host token.
    pub idempotency_token: String,
    /// Scope-bound request fingerprint.
    pub request_fingerprint: Digest,
    /// Correlated run ID.
    pub run_id: Id,
    /// Closed committed outcome.
    pub outcome: CommandReceiptOutcome,
    /// Committed batch ID.
    pub batch_id: Id,
    /// Database commit timestamp.
    pub committed_at: Timestamp,
}

/// The closed section 5.5 domain error taxonomy.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StoreError {
    /// No entity exists in the supplied scope.
    #[error("not found")]
    NotFound,
    /// An immutable scoped ID already exists.
    #[error("already exists")]
    AlreadyExists,
    /// An idempotency identity was reused with different inputs.
    #[error("idempotency conflict")]
    IdempotencyConflict,
    /// A mutable row version or status changed.
    #[error("compare-and-swap conflict")]
    CasConflict,
    /// The requested transition is absent from section 3.
    #[error("illegal transition")]
    IllegalTransition,
    /// A parameter is malformed before domain evaluation.
    #[error("invalid field")]
    InvalidField,
    /// Publication validation failed before a revision existed.
    #[error("revision invalid at {path}: {message}")]
    RevisionInvalid {
        /// Closed validation category.
        code: ValidationErrorKind,
        /// Bounded document path.
        path: String,
        /// Bounded corrective message.
        message: String,
        /// Bounded alternatives.
        valid_alternatives: Vec<String>,
    },
    /// Creation validation failed before a run existed.
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
    /// Runtime validation atomically produced ContractFailed.
    #[error("contract validation applied")]
    ContractValidationApplied {
        /// Closed runtime validation failure that was durably applied.
        code: String,
    },
    /// The blocked-run command fence rejected the command.
    #[error("run is blocked incompatible")]
    RunBlockedIncompatible,
    /// Creation limits are inconsistent.
    #[error("run limits invalid")]
    RunLimitsInvalid,
    /// A durable run limit terminalized the run.
    #[error("run limit applied")]
    RunLimitApplied {
        /// Closed run-limit failure that was durably applied.
        code: String,
    },
    /// A non-expired engine claim exists.
    #[error("engine already live")]
    EngineAlreadyLive {
        /// Current scoped engine owner label.
        owner: Id,
        /// Database-clock expiry of the current live claim.
        expires_at: Timestamp,
    },
    /// The permit no longer owns the generation.
    #[error("engine claim lost")]
    EngineClaimLost,
    /// The permit's claim expired.
    #[error("engine claim expired")]
    EngineClaimExpired,
    /// Exact action pins are unavailable.
    #[error("incompatible action pins")]
    IncompatiblePins,
    /// Resume evidence remains incompatible.
    #[error("still incompatible")]
    StillIncompatible {
        /// Exact pin locations that remain unavailable.
        pins: Vec<String>,
    },
    /// Compatibility evidence is malformed or oversized.
    #[error("compatibility evidence invalid")]
    EvidenceInvalid,
    /// A semantic digest substitution was proposed.
    #[error("compatibility override forbidden")]
    CompatibilityOverrideForbidden,
    /// Result intake lacked the attempt capability.
    #[error("invalid completion credential")]
    InvalidCompletionCredential,
    /// Diagnostics violated the closed envelope.
    #[error("diagnostics invalid at {path}: {code}")]
    DiagnosticsInvalid {
        /// Failing diagnostics path.
        path: String,
        /// Closed validation code.
        code: String,
    },
    /// Diagnostics exceeded 65,536 canonical bytes.
    #[error("diagnostics too large: {observed_bytes} > {limit_bytes}")]
    DiagnosticsTooLarge {
        /// Mandatory byte limit.
        limit_bytes: u64,
        /// Observed canonical byte length.
        observed_bytes: u64,
    },
    /// Attempt ID conflicts with immutable inputs.
    #[error("attempt id conflict")]
    AttemptIdConflict,
    /// Attempt or active-node fencing failed.
    #[error("attempt fenced")]
    AttemptFenced,
    /// Recovery encountered a current-generation attempt.
    #[error("current generation attempt present")]
    CurrentGenerationAttemptPresent,
    /// The attempt deadline is not due.
    #[error("deadline not due")]
    DeadlineNotDue {
        /// Database time observed inside the rejected transaction.
        database_now: Timestamp,
        /// Persisted attempt deadline.
        deadline: Timestamp,
    },
    /// The retry timestamp is not due.
    #[error("retry not due")]
    RetryNotDue {
        /// Database time observed inside the rejected transaction.
        database_now: Timestamp,
        /// Persisted next retry eligibility time.
        next_eligible_at: Timestamp,
    },
    /// The gate expiry is not due.
    #[error("expiry not due")]
    ExpiryNotDue {
        /// Database time observed inside the rejected transaction.
        database_now: Timestamp,
        /// Persisted approval expiry.
        expires_at: Timestamp,
    },
    /// The run lifetime is not due.
    #[error("lifetime not due")]
    LifetimeNotDue {
        /// Database time observed inside the rejected transaction.
        database_now: Timestamp,
        /// Persisted run lifetime deadline.
        lifetime_deadline_at: Timestamp,
    },
    /// The authenticated principal is unauthorized.
    #[error("approval unauthorized")]
    ApprovalUnauthorized,
    /// A different decision already resolved the gate.
    #[error("approval already resolved")]
    ApprovalAlreadyResolved,
    /// Expiry or cancellation lost the gate CAS.
    #[error("approval race lost")]
    ApprovalRaceLost,
    /// Host cancellation lost an observed-version race.
    #[error("cancellation race lost")]
    CancellationRaceLost,
    /// The run is already terminal.
    #[error("run already terminal")]
    RunAlreadyTerminal,
    /// A Map still has incomplete children.
    #[error("children incomplete")]
    ChildrenIncomplete,
    /// A Map aggregate does not match its child set.
    #[error("aggregate mismatch")]
    AggregateMismatch,
    /// A proposed object lacks verified capability proof.
    #[error("object not verified")]
    ObjectNotVerified,
    /// Proposed bytes or metadata do not match a digest.
    #[error("digest mismatch")]
    DigestMismatch,
    /// Same-digest artifact metadata conflicts.
    #[error("artifact metadata conflict")]
    ArtifactMetadataConflict,
    /// A corruption proof is invalid.
    #[error("invalid failed read proof")]
    InvalidFailedReadProof,
    /// A committed prerequisite object failed verification during hydration.
    ///
    /// This is the only variant that reports object-store corruption. It carries
    /// the proof minted by the first failed read, so the caller never has to
    /// repeat the read to obtain a `mark_corrupt_storage` capability.
    /// `CorruptControlPlane` never means this.
    #[error("committed object corrupt")]
    CommittedObjectCorrupt {
        /// The exact committed typed use that failed verification.
        bad_ref: ArtifactRef,
        /// The original proof minted by the first failed read.
        proof: FailedReadProof,
    },
    /// Publication target and canonical definition IDs differ.
    #[error("revision definition id mismatch")]
    RevisionDefinitionIdMismatch,
    /// A schema document uses unsupported features.
    #[error("schema subset unsupported")]
    SchemaSubsetUnsupported,
    /// A complete event batch exceeds response capacity.
    #[error("event batch too large")]
    BatchTooLarge,
    /// Database time regressed relative to claim state.
    #[error("database clock is non-monotonic")]
    ClockNonMonotonic,
    /// A derived view found impossible durable state.
    ///
    /// This never represents object-store corruption. It is reserved for a
    /// durable control-plane projection that cannot exist under section 3;
    /// object corruption is always `CommittedObjectCorrupt`.
    #[error("corrupt control plane")]
    CorruptControlPlane,
    /// Checked arithmetic overflowed or found a broken invariant.
    #[error("arithmetic overflow")]
    ArithmeticOverflow,
    /// Durable storage is unavailable.
    #[error("storage unavailable")]
    StorageUnavailable,
    /// The control-plane transaction failed.
    #[error("transaction failed")]
    TransactionFailed,
}

/// Parameters for definition creation.
pub struct CreateDefinition {
    /// Caller-chosen scoped definition ID.
    pub definition_id: Id,
    /// Display name.
    pub display_name: String,
    /// Description.
    pub description: String,
    /// Authenticated creating principal.
    pub principal: AuthenticatedPrincipal,
}

/// Parameters for metadata update.
pub struct UpdateDefinitionMetadata {
    /// Definition ID.
    pub definition_id: Id,
    /// Expected definition version.
    pub expected_version: Version,
    /// Replacement display name.
    pub display_name: String,
    /// Replacement description.
    pub description: String,
}

/// Resolved schema objects for one action pin.
pub struct ResolvedActionSchemas {
    /// Verified input schema object.
    pub input_schema: VerifiedObjectRef,
    /// Verified output schema object.
    pub output_schema: VerifiedObjectRef,
}

/// Parameters for immutable revision publication.
pub struct PublishRevision {
    /// Target definition ID.
    pub definition_id: Id,
    /// Expected mutable definition version.
    pub expected_definition_version: Version,
    /// Verified canonical definition object.
    pub canonical_definition: VerifiedObjectRef,
    /// Verified root input schema.
    pub run_input_schema: VerifiedObjectRef,
    /// Verified root output schema.
    pub run_output_schema: VerifiedObjectRef,
    /// Schema objects keyed by action reference location.
    pub resolved_action_schema_objects: BTreeMap<String, ResolvedActionSchemas>,
    /// Fully validated parsed revision.
    pub parsed_revision: PublishableDefinition,
    /// Authenticated publishing principal.
    pub principal: AuthenticatedPrincipal,
}

/// Parameters for idempotent run creation.
pub struct CreateRun {
    /// Caller-chosen run ID.
    pub run_id: Id,
    /// Definition ID.
    pub definition_id: Id,
    /// Immutable revision hash.
    pub revision_hash: Digest,
    /// Verified canonical run input.
    pub input: VerifiedObjectRef,
    /// Immutable budget limit.
    pub budget_limit: CostUnits,
    /// Fully resolved seven run limits.
    pub limits: RunLimits,
    /// Authenticated creating principal.
    pub principal: AuthenticatedPrincipal,
    /// Opaque 128-bit-or-stronger token.
    pub idempotency_token: String,
}

/// Parameters for starting a compatible run.
pub struct StartRun {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
    /// Fresh complete compatibility report.
    pub compatibility_evidence: CompatibilityReport,
}

/// Parameters for compatibility suspension.
pub struct SuspendIncompatible {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
    /// Verified bounded incompatibility evidence object.
    pub incompatibilities: VerifiedObjectRef,
    /// Fresh exact registry evidence.
    pub evidence: CompatibilityReport,
}

/// Parameters for compatible resume.
pub struct ResumeCompatible {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
    /// Fresh complete availability evidence.
    pub availability_evidence: CompatibilityReport,
}

/// Parameters for an atomic action attempt claim.
pub struct ClaimNodeAttempt {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
    /// Action node instance ID.
    pub node_id: NodeInstanceId,
    /// Expected node version.
    pub expected_node_version: Version,
    /// Caller-chosen attempt ID.
    pub attempt_id: Id,
    /// Worker label.
    pub worker_id: Id,
    /// Verified exact canonical bound input.
    pub bound_input: VerifiedObjectRef,
    /// Ordered binding derivation digest.
    pub binding_derivation_digest: Digest,
}

/// Closed atomic claim results.
pub enum ClaimNodeAttemptResult {
    /// Attempt and invocation committed.
    Claimed {
        /// Immutable invocation.
        invocation: ActionInvocation,
        /// One-time raw completion capability.
        completion_credential: CompletionCredential,
    },
    /// Reservation-only shortage persisted BudgetWaiting.
    BudgetWaitingApplied(NodeRun),
    /// A Map concurrency slot is temporarily unavailable; no state changed.
    MapConcurrencyLimited,
    /// Permanent budget exhaustion terminalized the run.
    BudgetExhaustedApplied(WorkflowRun),
    /// A run-limit outcome was atomically applied.
    RunLimitApplied(WorkflowRun),
}

/// Verified refs supplied alongside a submitted action outcome.
pub struct CompletionObjects {
    /// Verified success output when submitted.
    pub output: Option<VerifiedObjectRef>,
    /// Ordered verified action artifacts.
    pub artifacts: Vec<VerifiedObjectRef>,
    /// Verified closed diagnostics when submitted.
    pub diagnostics: Option<VerifiedObjectRef>,
}

/// Parameters for credential-authenticated result intake.
pub struct CompleteAttempt {
    /// Per-attempt completion capability.
    pub completion_credential: CompletionCredential,
    /// Run ID.
    pub run_id: Id,
    /// Node instance ID.
    pub node_id: NodeInstanceId,
    /// Attempt ID.
    pub attempt_id: Id,
    /// Closed submitted action outcome.
    pub submitted_outcome: ActionOutcome,
    /// Verified objects corresponding to the outcome.
    pub objects: CompletionObjects,
}

/// Closed complete-attempt command outcomes.
pub enum CompleteAttemptResult {
    /// The active attempt and node outcome applied.
    Applied(NodeAttempt),
    /// A persisted retry was scheduled.
    RetryScheduled(NodeRun),
    /// The accepted outcome terminalized the run.
    TerminalRun(WorkflowRun),
    /// A due result timed out and was observed stale.
    TimedOutAndStaleRecorded(NodeAttempt),
    /// A stale completion observation committed.
    StaleRecorded(NodeAttempt),
    /// The terminal attempt was already observed.
    AlreadyObserved(NodeAttempt),
}

/// Parameters for database-clock attempt timeout.
pub struct TimeoutAttempt {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
    /// Node instance ID.
    pub node_id: NodeInstanceId,
    /// Attempt ID.
    pub attempt_id: Id,
}

/// Parameters for one-run abandoned-attempt recovery.
pub struct RecoverAbandonedAttemptsForRun {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
}

/// Parameters for retry eligibility release.
pub struct ReleaseRetry {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
    /// Node instance ID.
    pub node_id: NodeInstanceId,
    /// Expected node version.
    pub expected_node_version: Version,
}

/// Parameters for one committed Choice decision.
pub struct RecordChoice {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
    /// Choice node instance ID.
    pub node_id: NodeInstanceId,
    /// Expected node version.
    pub expected_node_version: Version,
    /// Verified canonical Choice input.
    pub input: VerifiedObjectRef,
    /// Digest of the resolved selector scalar.
    pub evaluated_selector_digest: Digest,
    /// Deterministic first-match or default selection.
    pub selection: ChoiceSelection,
}

/// One validated ordered Map expansion item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderedMapItem {
    /// Zero-based item index.
    pub index: u32,
    /// Canonical item digest.
    pub item_digest: Digest,
    /// Deterministic child ID.
    pub child_id: NodeInstanceId,
}

/// Parameters for atomic all-child Map expansion.
pub struct ExpandMap {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
    /// Map node instance ID.
    pub map_node_id: NodeInstanceId,
    /// Expected parent version.
    pub expected_node_version: Version,
    /// Verified canonical Map input.
    pub input: VerifiedObjectRef,
    /// Complete ordered item identity set.
    pub ordered_items: Vec<OrderedMapItem>,
    /// Ordered expansion digest.
    pub expansion_digest: Digest,
}

/// Parameters for ordered Map aggregation.
pub struct CompleteMap {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
    /// Map node instance ID.
    pub map_node_id: NodeInstanceId,
    /// Expected parent version.
    pub expected_node_version: Version,
    /// Verified canonical ordered aggregate.
    pub aggregate: VerifiedObjectRef,
}

/// Parameters for durable approval request creation.
pub struct RequestApproval {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
    /// Approval node instance ID.
    pub node_id: NodeInstanceId,
    /// Expected node version.
    pub expected_node_version: Version,
    /// Deterministic gate ID.
    pub gate_id: Id,
    /// Verified canonical approval request.
    pub request: VerifiedObjectRef,
}

/// Parameters for an authenticated human gate decision.
pub struct DecideApproval {
    /// Run ID.
    pub run_id: Id,
    /// Gate ID.
    pub gate_id: Id,
    /// Expected run version.
    pub expected_run_version: Version,
    /// Expected gate version.
    pub expected_gate_version: Version,
    /// Approve or reject.
    pub decision: ApprovalDecision,
    /// Optional verified human decision payload.
    pub decision_payload: Option<VerifiedObjectRef>,
    /// Required exact ApprovalResult object for approval.
    pub approval_output: Option<VerifiedObjectRef>,
    /// Scope-bound authenticated principal.
    pub principal: AuthenticatedPrincipal,
}

/// Parameters for database-clock gate expiry.
pub struct ExpireApproval {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
    /// Gate ID.
    pub gate_id: Id,
    /// Exact expiry ApprovalResult when policy approves.
    pub approval_output: Option<VerifiedObjectRef>,
}

/// Parameters for a Ready Succeed or Fail node.
pub struct ResolveTerminalNode {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
    /// Terminal node instance ID.
    pub node_id: NodeInstanceId,
    /// Expected node version.
    pub expected_node_version: Version,
    /// Verified output for Succeed and none for Fail.
    pub output: Option<VerifiedObjectRef>,
}

/// Parameters for explicit runtime contract failure.
pub struct FailContract {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
    /// Affected node instance ID.
    pub node_id: NodeInstanceId,
    /// Expected node version.
    pub expected_node_version: Version,
    /// Closed legal contract failure kind.
    pub closed_failure_kind: NodeFailureKind,
    /// Optional verified closed diagnostics.
    pub diagnostics: Option<VerifiedObjectRef>,
}

/// One observed pending-gate CAS input for cancellation.
pub struct ExpectedGateVersion {
    /// Gate ID.
    pub gate_id: Id,
    /// Observed gate version.
    pub version: Version,
}

/// Parameters for idempotent authenticated cancellation.
pub struct CancelRun {
    /// Run ID.
    pub run_id: Id,
    /// Observed run version.
    pub expected_run_version: Version,
    /// Complete sorted pending-gate version set.
    pub expected_pending_gate_versions: Vec<ExpectedGateVersion>,
    /// Scope-bound authenticated principal.
    pub principal: AuthenticatedPrincipal,
    /// Persistence-safe reason code.
    pub reason_code: String,
    /// Opaque 128-bit-or-stronger token.
    pub idempotency_token: String,
}

/// Parameters for database-clock lifetime expiry.
pub struct ExpireRunLifetime {
    /// Live engine permit.
    pub permit: EnginePermit,
    /// Run ID.
    pub run_id: Id,
}

/// Parameters for proof-backed storage corruption.
pub struct MarkCorruptStorage {
    /// Run ID.
    pub run_id: Id,
    /// Already-committed bad ArtifactRef.
    pub bad_ref: ArtifactRef,
    /// Opaque failed-read proof.
    pub proof: FailedReadProof,
    /// Optional node owning the bad ref.
    pub owner_node_id: Option<NodeInstanceId>,
}

/// An opaque scope-bound cutoff/keyset cursor.
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

/// Keyset page request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageRequest {
    /// Opaque cursor or none for a cutoff-capturing first page.
    pub cursor: Option<ScanCursor>,
    /// Requested row count from 1 through 1000.
    pub page_size: u16,
}

/// One keyset result page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Page<T> {
    /// Ordered page items.
    pub items: Vec<T>,
    /// Opaque continuation cursor.
    pub next_cursor: Option<ScanCursor>,
}

/// Event-page request preserving complete atomic batches.
pub struct EventPageRequest {
    /// Sequence after which events are returned.
    pub after_event_seq: u64,
    /// Preferred event count.
    pub page_size: u16,
    /// Hard serialized response-byte cap.
    pub hard_response_byte_limit: u64,
}

/// Scope-confined atomic workflow control-plane store.
pub trait WorkflowStore: Send + Sync {
    /// Creates mutable definition metadata.
    async fn create_definition(
        &self,
        scope: &ExecutionScope,
        command: CreateDefinition,
    ) -> Result<DefinitionRecord, StoreError>;

    /// Updates definition metadata with CAS.
    async fn update_definition_metadata(
        &self,
        scope: &ExecutionScope,
        command: UpdateDefinitionMetadata,
    ) -> Result<DefinitionRecord, StoreError>;

    /// Publishes one immutable validated revision.
    async fn publish_revision(
        &self,
        scope: &ExecutionScope,
        command: PublishRevision,
    ) -> Result<WorkflowRevision, StoreError>;

    /// Acquires or takes over the scoped singleton engine claim.
    async fn acquire_engine_claim(
        &self,
        scope: &ExecutionScope,
        instance_id: Id,
    ) -> Result<AcquiredEngineClaim, StoreError>;

    /// Renews a live engine claim.
    fn heartbeat_engine_claim<'a>(
        &'a self,
        scope: &'a ExecutionScope,
        permit: &'a EnginePermit,
    ) -> impl Future<Output = Result<EngineClaim, StoreError>> + Send + 'a;

    /// Gracefully expires a matching engine claim.
    async fn release_engine_claim(
        &self,
        scope: &ExecutionScope,
        permit: &EnginePermit,
    ) -> Result<(), StoreError>;

    /// Creates a Pending run and its complete static graph.
    async fn create_run(
        &self,
        scope: &ExecutionScope,
        command: CreateRun,
    ) -> Result<CommandReceipt, StoreError>;

    /// Starts a pin-compatible Pending run.
    async fn start_run(
        &self,
        scope: &ExecutionScope,
        command: StartRun,
    ) -> Result<WorkflowRun, StoreError>;

    /// Suspends a Pending or recovered Running run.
    async fn suspend_incompatible(
        &self,
        scope: &ExecutionScope,
        command: SuspendIncompatible,
    ) -> Result<WorkflowRun, StoreError>;

    /// Resumes a run only when every exact pin is available.
    async fn resume_compatible(
        &self,
        scope: &ExecutionScope,
        command: ResumeCompatible,
    ) -> Result<WorkflowRun, StoreError>;

    /// Atomically claims and reserves one Action attempt.
    async fn claim_node_attempt(
        &self,
        scope: &ExecutionScope,
        command: ClaimNodeAttempt,
    ) -> Result<ClaimNodeAttemptResult, StoreError>;

    /// Accepts or observes a credential-authenticated result.
    fn complete_attempt<'a>(
        &'a self,
        scope: &'a ExecutionScope,
        command: CompleteAttempt,
    ) -> impl Future<Output = Result<CompleteAttemptResult, StoreError>> + Send + 'a;

    /// Applies a due database-clock timeout.
    async fn timeout_attempt(
        &self,
        scope: &ExecutionScope,
        command: TimeoutAttempt,
    ) -> Result<NodeAttempt, StoreError>;

    /// Recovers the complete lower-generation Started set for one run.
    async fn recover_abandoned_attempts_for_run(
        &self,
        scope: &ExecutionScope,
        command: RecoverAbandonedAttemptsForRun,
    ) -> Result<Vec<NodeAttempt>, StoreError>;

    /// Releases a due persisted retry to Ready.
    async fn release_retry(
        &self,
        scope: &ExecutionScope,
        command: ReleaseRetry,
    ) -> Result<NodeRun, StoreError>;

    /// Commits one deterministic Choice decision and frontier fixed point.
    async fn record_choice(
        &self,
        scope: &ExecutionScope,
        command: RecordChoice,
    ) -> Result<NodeRun, StoreError>;

    /// Atomically expands a bounded Map child set.
    async fn expand_map(
        &self,
        scope: &ExecutionScope,
        command: ExpandMap,
    ) -> Result<NodeRun, StoreError>;

    /// Commits an exact ordered Map aggregate.
    async fn complete_map(
        &self,
        scope: &ExecutionScope,
        command: CompleteMap,
    ) -> Result<NodeRun, StoreError>;

    /// Creates a durable approval gate.
    async fn request_approval(
        &self,
        scope: &ExecutionScope,
        command: RequestApproval,
    ) -> Result<ApprovalGate, StoreError>;

    /// Commits the first valid authenticated human decision.
    async fn decide_approval(
        &self,
        scope: &ExecutionScope,
        command: DecideApproval,
    ) -> Result<ApprovalGate, StoreError>;

    /// Commits a due database-clock gate expiry.
    async fn expire_approval(
        &self,
        scope: &ExecutionScope,
        command: ExpireApproval,
    ) -> Result<ApprovalGate, StoreError>;

    /// Resolves a Ready Succeed or Fail node.
    async fn resolve_terminal_node(
        &self,
        scope: &ExecutionScope,
        command: ResolveTerminalNode,
    ) -> Result<WorkflowRun, StoreError>;

    /// Applies a closed runtime contract failure.
    async fn fail_contract(
        &self,
        scope: &ExecutionScope,
        command: FailContract,
    ) -> Result<WorkflowRun, StoreError>;

    /// Idempotently cancels a nonterminal run.
    async fn cancel_run(
        &self,
        scope: &ExecutionScope,
        command: CancelRun,
    ) -> Result<CommandReceipt, StoreError>;

    /// Cancels a run whose database lifetime deadline is due.
    async fn expire_run_lifetime(
        &self,
        scope: &ExecutionScope,
        command: ExpireRunLifetime,
    ) -> Result<WorkflowRun, StoreError>;

    /// Applies proof-backed committed-object corruption.
    async fn mark_corrupt_storage(
        &self,
        scope: &ExecutionScope,
        command: MarkCorruptStorage,
    ) -> Result<WorkflowRun, StoreError>;

    /// Reads one definition projection.
    async fn get_definition(
        &self,
        scope: &ExecutionScope,
        definition_id: &Id,
    ) -> Result<DefinitionRecord, StoreError>;

    /// Reads one immutable revision.
    async fn get_revision(
        &self,
        scope: &ExecutionScope,
        definition_id: &Id,
        revision_hash: &Digest,
    ) -> Result<WorkflowRevision, StoreError>;

    /// Reads a run and its derived operational view.
    fn get_run<'a>(
        &'a self,
        scope: &'a ExecutionScope,
        run_id: &'a Id,
    ) -> impl Future<Output = Result<WorkflowRunView, StoreError>> + Send + 'a;

    /// Reads one node instance.
    async fn get_node(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        node_id: &NodeInstanceId,
    ) -> Result<NodeRun, StoreError>;

    /// Reads one attempt.
    async fn get_attempt(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        attempt_id: &Id,
    ) -> Result<NodeAttempt, StoreError>;

    /// Reads one approval gate.
    async fn get_gate(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        gate_id: &Id,
    ) -> Result<ApprovalGate, StoreError>;

    /// Lists runs with scope-bound keyset pagination.
    async fn list_runs(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError>;

    /// Lists nodes in one scoped run.
    async fn list_nodes(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError>;

    /// Lists events after a sequence without splitting a batch.
    async fn list_events_after(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        page: EventPageRequest,
    ) -> Result<Vec<WorkflowEvent>, StoreError>;

    /// Scans Ready nodes at a captured cutoff.
    async fn scan_ready_nodes(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError>;

    /// Scans BudgetWaiting nodes at a captured cutoff.
    async fn scan_budget_waiters(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError>;

    /// Scans due active attempt deadlines.
    async fn scan_due_deadlines(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeAttempt>, StoreError>;

    /// Scans due persisted retries.
    async fn scan_due_retries(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError>;

    /// Scans runs containing lower-generation Started attempts.
    async fn scan_recovery_runs(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError>;

    /// Scans nonterminal runs for compatibility rechecks.
    async fn scan_compatibility_rechecks(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError>;

    /// Scans due Pending gates.
    async fn scan_due_gates(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<ApprovalGate>, StoreError>;

    /// Scans due run lifetime deadlines.
    async fn scan_due_run_lifetimes(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError>;
}
