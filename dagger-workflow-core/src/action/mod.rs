//! Action registry, invocation, context, and outcome contracts.

use crate::artifact::{ArtifactRefValue, JsonRef, VerifiedObjectRef};
use crate::definition::ActionPin;
use crate::ids::{CostUnits, Digest, Id, NodeInstanceId, Timestamp};
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};
use tokio::sync::Notify;

/// Opaque per-attempt result-intake capability.
#[derive(Clone, Eq, PartialEq)]
pub struct CompletionCredential(Vec<u8>);

impl std::fmt::Debug for CompletionCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompletionCredential")
            .field("digest", &self.digest())
            .finish()
    }
}

impl CompletionCredential {
    /// Mints an opaque credential from exactly 256 bits of store-generated entropy.
    ///
    /// Callers must use a cryptographically secure store mint at A01; this method
    /// intentionally exposes no way to recover the supplied raw bytes afterwards.
    pub fn from_minted_bytes(raw: [u8; 32]) -> Self {
        Self(raw.to_vec())
    }

    /// Returns the only persistence-safe representation of this credential.
    pub fn digest(&self) -> Digest {
        digest_bytes(&self.0)
    }
}

/// Cooperative advisory cancellation observed by actions.
pub trait CancellationToken: Send + Sync {
    /// Reports whether cancellation was requested.
    fn is_cancelled(&self) -> bool;

    /// Waits until cancellation is requested.
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// A cancellation source paired with a cooperative token.
#[derive(Clone, Debug, Default)]
pub struct CancellationSource {
    state: Arc<CancellationState>,
}

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancellationSource {
    /// Creates an uncancelled source.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cooperative cancellation for every clone of this token.
    pub fn cancel(&self) {
        if !self.state.cancelled.swap(true, Ordering::AcqRel) {
            self.state.notify.notify_waiters();
        }
    }

    /// Returns a token suitable for an [`ActionContext`].
    pub fn token(&self) -> Arc<dyn CancellationToken> {
        Arc::new(self.clone())
    }
}

impl CancellationToken for CancellationSource {
    fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            loop {
                let notified = self.state.notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.is_cancelled() {
                    return;
                }
                notified.await;
            }
        })
    }
}

/// Read-only declared budget made available to an action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetHandle {
    /// Provider-enforceable declared maximum.
    pub declared_max_cost_units: CostUnits,
}

/// The exact context for one action attempt.
#[derive(Clone)]
pub struct ActionContext {
    /// Execution scope.
    pub scope: ExecutionScope,
    /// Owning run ID.
    pub run_id: Id,
    /// Pinned revision hash.
    pub revision_hash: Digest,
    /// Logical node instance ID.
    pub node_instance_id: NodeInstanceId,
    /// Per-attempt ID.
    pub attempt_id: Id,
    /// One-based attempt number.
    pub attempt_number: u32,
    /// Retry-stable scope-bound external key.
    pub idempotency_key: String,
    /// Result-intake capability for this attempt only.
    pub completion_credential: CompletionCredential,
    /// Persisted database-clock deadline.
    pub deadline: Timestamp,
    /// Cooperative advisory cancellation.
    pub cancellation_token: Arc<dyn CancellationToken>,
    /// Read-only declared budget.
    pub budget: BudgetHandle,
}

impl ActionContext {
    /// Creates a static-node attempt context with the contract-mandated
    /// scope-bound key derived from its logical run and node identity.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        scope: ExecutionScope,
        run_id: Id,
        revision_hash: Digest,
        node_instance_id: NodeInstanceId,
        attempt_id: Id,
        attempt_number: u32,
        completion_credential: CompletionCredential,
        deadline: Timestamp,
        cancellation_token: Arc<dyn CancellationToken>,
        budget: BudgetHandle,
    ) -> Self {
        let idempotency_key = crate::ids::idempotency_key(&scope, &run_id, &node_instance_id);
        Self {
            scope,
            run_id,
            revision_hash,
            node_instance_id,
            attempt_id,
            attempt_number,
            idempotency_key,
            completion_credential,
            deadline,
            cancellation_token,
            budget,
        }
    }

    /// Creates a Map-child attempt context with the complete child identity
    /// included in the retry-stable scope-bound external key.
    #[allow(clippy::too_many_arguments)]
    pub fn new_map_child(
        scope: ExecutionScope,
        run_id: Id,
        revision_hash: Digest,
        child_node_instance_id: NodeInstanceId,
        map_parent_node_instance_id: NodeInstanceId,
        map_item_index: u32,
        map_item_digest: Digest,
        attempt_id: Id,
        attempt_number: u32,
        completion_credential: CompletionCredential,
        deadline: Timestamp,
        cancellation_token: Arc<dyn CancellationToken>,
        budget: BudgetHandle,
    ) -> Self {
        let idempotency_key = crate::ids::map_child_idempotency_key(
            &scope,
            &run_id,
            &child_node_instance_id,
            &map_parent_node_instance_id,
            map_item_index,
            &map_item_digest,
        );
        Self {
            scope,
            run_id,
            revision_hash,
            node_instance_id: child_node_instance_id,
            attempt_id,
            attempt_number,
            idempotency_key,
            completion_credential,
            deadline,
            cancellation_token,
            budget,
        }
    }
}

/// One immutable action invocation snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionInvocation {
    /// Invocation scope.
    pub scope: ExecutionScope,
    /// Owning run ID.
    pub run_id: Id,
    /// Invocation ID, equal to the attempt ID.
    pub invocation_id: Id,
    /// Owning node instance.
    pub node_instance_id: NodeInstanceId,
    /// Owning attempt ID.
    pub attempt_id: Id,
    /// Node or Map action reference location.
    pub action_reference_location: String,
    /// Pinned action name.
    pub action_name: String,
    /// Pinned contract version.
    pub contract_version: String,
    /// Owning revision hash.
    pub revision_hash: Digest,
    /// Pinned input schema digest.
    pub input_schema_digest: Digest,
    /// Pinned output schema digest.
    pub output_schema_digest: Digest,
    /// Pinned semantic implementation requirement.
    pub compatible_implementation_requirement: Digest,
    /// Exact canonical bound input ref.
    pub bound_input_ref: JsonRef,
    /// Digest of exact delivered bytes.
    pub bound_input_digest: Digest,
    /// Exact delivered byte length.
    pub bound_input_size_bytes: u64,
    /// Ordered binding derivation digest.
    pub binding_derivation_digest: Digest,
    /// Creation database timestamp.
    pub created_at: Timestamp,
}

/// One action-produced artifact before domain registration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactOutput {
    /// Normalized artifact media type.
    pub media_type: String,
    /// Verified durable object capability.
    pub object: VerifiedObjectRef,
}

/// One bounded diagnostics fact value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiagnosticScalar {
    /// String scalar.
    String(String),
    /// Boolean scalar.
    Boolean(bool),
    /// JSON number scalar.
    Number(serde_json::Number),
    /// Null scalar.
    Null,
}

/// One named diagnostics fact.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticFact {
    /// Unique persistence-safe fact name.
    pub name: String,
    /// Scalar fact value.
    pub value: DiagnosticScalar,
}

/// The closed persistence-safe diagnostics envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsEnvelope {
    /// Optional bounded summary.
    pub summary: Option<String>,
    /// Ordered bounded facts.
    pub facts: Vec<DiagnosticFact>,
    /// Ordered related artifact refs.
    pub related_artifact_refs: Vec<ArtifactRefValue>,
}

/// The closed action outcome vocabulary.
#[derive(Clone, Debug, PartialEq)]
pub enum ActionOutcome {
    /// Typed JSON success with ordered artifacts.
    Success {
        /// Typed JSON output.
        output: Value,
        /// Ordered verified artifacts.
        artifacts: Vec<ArtifactOutput>,
        /// Trusted reported actual cost.
        actual_cost_units: CostUnits,
        /// Optional closed diagnostics.
        diagnostics: Option<DiagnosticsEnvelope>,
    },
    /// Structured retryable action error.
    Retryable {
        /// Namespaced action code.
        code: String,
        /// Persistence-safe message.
        message: String,
        /// Optional closed diagnostics.
        diagnostics: Option<DiagnosticsEnvelope>,
        /// Trusted reported actual cost.
        actual_cost_units: CostUnits,
    },
    /// Structured permanent action error.
    Permanent {
        /// Namespaced action code.
        code: String,
        /// Persistence-safe message.
        message: String,
        /// Optional closed diagnostics.
        diagnostics: Option<DiagnosticsEnvelope>,
        /// Trusted reported actual cost.
        actual_cost_units: CostUnits,
    },
}

/// Rejection from the closed diagnostics persistence boundary.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DiagnosticsValidationError {
    /// A structural field violates the closed diagnostics format.
    #[error("invalid diagnostics at {path}: {code}")]
    Invalid {
        /// JSON-path-like location of the offending value.
        path: String,
        /// Stable machine-readable rejection code.
        code: &'static str,
    },
    /// Canonical JSON exceeds the fixed contract maximum.
    #[error("diagnostics exceed {limit_bytes} bytes (observed {observed_bytes})")]
    TooLarge {
        /// Fixed contract maximum.
        limit_bytes: usize,
        /// Exact canonical byte count observed.
        observed_bytes: usize,
    },
}

impl DiagnosticsEnvelope {
    /// Constructs a diagnostics envelope only if it crosses the closed
    /// persistence-safe boundary. The rejection is pre-transition/no-write at
    /// the store boundary, so callers may retry completion without diagnostics.
    pub fn new(
        summary: Option<String>,
        facts: Vec<DiagnosticFact>,
        related_artifact_refs: Vec<ArtifactRefValue>,
    ) -> Result<Self, DiagnosticsValidationError> {
        let envelope = Self {
            summary,
            facts,
            related_artifact_refs,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    /// Validates this value before it is persisted or accepted in an outcome.
    pub fn validate(&self) -> Result<(), DiagnosticsValidationError> {
        const LIMIT: usize = 65_536;
        if self
            .summary
            .as_ref()
            .is_some_and(|value| value.len() > 2_000)
        {
            return Err(invalid_diagnostics("/summary", "summary_too_long"));
        }
        if self.facts.len() > 512 {
            return Err(invalid_diagnostics("/facts", "too_many_facts"));
        }
        if self.related_artifact_refs.len() > 32 {
            return Err(invalid_diagnostics(
                "/related_artifact_refs",
                "too_many_related_artifact_refs",
            ));
        }

        let mut names = BTreeSet::new();
        for (index, fact) in self.facts.iter().enumerate() {
            let path = format!("/facts/{index}");
            if !valid_fact_name(&fact.name) {
                return Err(invalid_diagnostics(
                    &format!("{path}/name"),
                    "invalid_fact_name",
                ));
            }
            if is_sensitive_fact_name(&fact.name) {
                return Err(invalid_diagnostics(
                    &format!("{path}/name"),
                    "sensitive_fact_name",
                ));
            }
            if !names.insert(fact.name.as_str()) {
                return Err(invalid_diagnostics(
                    &format!("{path}/name"),
                    "duplicate_fact_name",
                ));
            }
            if let DiagnosticScalar::String(value) = &fact.value {
                if value.len() > 2_000 {
                    return Err(invalid_diagnostics(
                        &format!("{path}/value"),
                        "string_too_long",
                    ));
                }
            }
        }

        let observed_bytes = serde_json::to_vec(self)
            .expect("DiagnosticsEnvelope contains only serializable closed fields")
            .len();
        if observed_bytes > LIMIT {
            return Err(DiagnosticsValidationError::TooLarge {
                limit_bytes: LIMIT,
                observed_bytes,
            });
        }
        Ok(())
    }
}

impl ActionOutcome {
    /// Builds a validated success outcome.
    pub fn success(
        output: Value,
        artifacts: Vec<ArtifactOutput>,
        actual_cost_units: CostUnits,
        diagnostics: Option<DiagnosticsEnvelope>,
    ) -> Result<Self, ActionOutcomeValidationError> {
        let outcome = Self::Success {
            output,
            artifacts,
            actual_cost_units,
            diagnostics,
        };
        outcome.validate()?;
        Ok(outcome)
    }

    /// Builds a validated retryable action error.
    pub fn retryable(
        code: String,
        message: String,
        diagnostics: Option<DiagnosticsEnvelope>,
        actual_cost_units: CostUnits,
    ) -> Result<Self, ActionOutcomeValidationError> {
        let outcome = Self::Retryable {
            code,
            message,
            diagnostics,
            actual_cost_units,
        };
        outcome.validate()?;
        Ok(outcome)
    }

    /// Builds a validated permanent action error.
    pub fn permanent(
        code: String,
        message: String,
        diagnostics: Option<DiagnosticsEnvelope>,
        actual_cost_units: CostUnits,
    ) -> Result<Self, ActionOutcomeValidationError> {
        let outcome = Self::Permanent {
            code,
            message,
            diagnostics,
            actual_cost_units,
        };
        outcome.validate()?;
        Ok(outcome)
    }

    /// Validates the mechanical action-outcome persistence boundary.
    pub fn validate(&self) -> Result<(), ActionOutcomeValidationError> {
        match self {
            Self::Success { diagnostics, .. } => validate_optional_diagnostics(diagnostics),
            Self::Retryable {
                code,
                message,
                diagnostics,
                ..
            }
            | Self::Permanent {
                code,
                message,
                diagnostics,
                ..
            } => {
                if !valid_action_code(code) {
                    return Err(ActionOutcomeValidationError::InvalidErrorCode);
                }
                if message.is_empty() || message.len() > 2_000 {
                    return Err(ActionOutcomeValidationError::InvalidMessage);
                }
                validate_optional_diagnostics(diagnostics)
            }
        }
    }
}

/// Rejection from closed action-outcome construction.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ActionOutcomeValidationError {
    /// A namespaced action error code is malformed.
    #[error("action error code must be a bounded namespace-qualified identifier")]
    InvalidErrorCode,
    /// An action error message is empty or too large to persist safely.
    #[error("action error message must contain 1 through 2000 bytes")]
    InvalidMessage,
    /// Diagnostics did not cross the persistence-safe boundary.
    #[error(transparent)]
    Diagnostics(#[from] DiagnosticsValidationError),
}

/// A registered implementation's exact compatibility descriptor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ActionDescriptor {
    /// Registry action name.
    pub name: String,
    /// Action contract version.
    pub contract_version: String,
    /// Input schema digest.
    pub input_schema_digest: Digest,
    /// Output schema digest.
    pub output_schema_digest: Digest,
    /// Advertised semantic compatibility digest.
    pub implementation_compatibility_digest: Digest,
}

/// A compatibility mismatch dimension.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum CompatibilityMismatch {
    /// Action name differs.
    Name,
    /// Contract version differs.
    ContractVersion,
    /// Input schema digest differs.
    InputSchemaDigest,
    /// Output schema digest differs.
    OutputSchemaDigest,
    /// Semantic compatibility digest differs.
    ImplementationCompatibilityDigest,
}

/// Checks all five compatibility dimensions byte-for-byte.
pub fn check_compatibility(
    pin: &ActionPin,
    implementation: &ActionDescriptor,
) -> Result<(), CompatibilityMismatch> {
    if pin.name != implementation.name {
        Err(CompatibilityMismatch::Name)
    } else if pin.contract_version != implementation.contract_version {
        Err(CompatibilityMismatch::ContractVersion)
    } else if pin.input_schema_digest != implementation.input_schema_digest {
        Err(CompatibilityMismatch::InputSchemaDigest)
    } else if pin.output_schema_digest != implementation.output_schema_digest {
        Err(CompatibilityMismatch::OutputSchemaDigest)
    } else if pin.compatible_implementation_requirement
        != implementation.implementation_compatibility_digest
    {
        Err(CompatibilityMismatch::ImplementationCompatibilityDigest)
    } else {
        Ok(())
    }
}

/// One host-provided workflow action.
pub trait WorkflowAction: Send + Sync {
    /// Returns the implementation's exact descriptor.
    fn descriptor(&self) -> &ActionDescriptor;

    /// Executes against exact canonical invocation bytes.
    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        canonical_bound_input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>>;
}

/// Name-addressed action implementation registry.
pub trait ActionRegistry: Send + Sync {
    /// Resolves a name-addressed action implementation.
    fn resolve(&self, name: &str) -> Option<Arc<dyn WorkflowAction>>;

    /// Checks whether every exact revision pin is available.
    fn check_pins(&self, pins: &[ActionPin]) -> CompatibilityReport;
}

/// A registry registration failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ActionRegistrationError {
    /// A registry name may have one current implementation only.
    #[error("action {name:?} is already registered")]
    DuplicateName {
        /// Name that already resolves in this registry.
        name: String,
    },
    /// Empty names cannot be resolved by a definition pin.
    #[error("action name is empty")]
    EmptyName,
}

/// In-process name-addressed registry for hosts and deterministic fixtures.
#[derive(Default)]
pub struct InMemoryActionRegistry {
    actions: RwLock<HashMap<String, Arc<dyn WorkflowAction>>>,
}

impl std::fmt::Debug for InMemoryActionRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = self
            .actions
            .read()
            .expect("action registry lock is not poisoned")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        formatter
            .debug_struct("InMemoryActionRegistry")
            .field("names", &names)
            .finish()
    }
}

impl InMemoryActionRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an implementation without replacing an existing name.
    pub fn register(&self, action: Arc<dyn WorkflowAction>) -> Result<(), ActionRegistrationError> {
        let name = action.descriptor().name.clone();
        if name.is_empty() {
            return Err(ActionRegistrationError::EmptyName);
        }
        let mut actions = self
            .actions
            .write()
            .expect("action registry lock is not poisoned");
        if actions.contains_key(&name) {
            return Err(ActionRegistrationError::DuplicateName { name });
        }
        actions.insert(name, action);
        Ok(())
    }
}

impl ActionRegistry for InMemoryActionRegistry {
    fn resolve(&self, name: &str) -> Option<Arc<dyn WorkflowAction>> {
        self.actions
            .read()
            .expect("action registry lock is not poisoned")
            .get(name)
            .cloned()
    }

    fn check_pins(&self, pins: &[ActionPin]) -> CompatibilityReport {
        let actions = self
            .actions
            .read()
            .expect("action registry lock is not poisoned");
        let mut evidence = pins
            .iter()
            .map(|pin| {
                let implementation = actions
                    .get(&pin.name)
                    .map(|action| action.descriptor().clone());
                let mismatch = implementation
                    .as_ref()
                    .and_then(|descriptor| check_compatibility(pin, descriptor).err());
                PinCompatibilityEvidence {
                    reference_location: pin.reference_location.clone(),
                    pin: PinnedActionDescriptor::from(pin),
                    implementation,
                    mismatch,
                }
            })
            .collect::<Vec<_>>();
        evidence.sort_by(|left, right| left.reference_location.cmp(&right.reference_location));
        let incompatible_reference_locations = evidence
            .iter()
            .filter(|entry| entry.mismatch.is_some() || entry.implementation.is_none())
            .map(|entry| entry.reference_location.clone())
            .collect();
        CompatibilityReport {
            evidence_digest: compatibility_evidence_digest(&evidence),
            incompatible_reference_locations,
            evidence,
        }
    }
}

/// The five pin values included in persistence-safe compatibility evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PinnedActionDescriptor {
    /// Pinned action name.
    pub name: String,
    /// Pinned contract version.
    pub contract_version: String,
    /// Pinned input schema digest.
    pub input_schema_digest: Digest,
    /// Pinned output schema digest.
    pub output_schema_digest: Digest,
    /// Pinned semantic implementation requirement.
    pub compatible_implementation_requirement: Digest,
}

impl From<&ActionPin> for PinnedActionDescriptor {
    fn from(pin: &ActionPin) -> Self {
        Self {
            name: pin.name.clone(),
            contract_version: pin.contract_version.clone(),
            input_schema_digest: pin.input_schema_digest.clone(),
            output_schema_digest: pin.output_schema_digest.clone(),
            compatible_implementation_requirement: pin
                .compatible_implementation_requirement
                .clone(),
        }
    }
}

/// One exact registry observation used to suspend or resume a pinned run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PinCompatibilityEvidence {
    /// Revision pin reference location.
    pub reference_location: String,
    /// All expected pin dimensions.
    pub pin: PinnedActionDescriptor,
    /// Current descriptor for the name, if any.
    pub implementation: Option<ActionDescriptor>,
    /// First mismatched dimension in contract comparison order, if any.
    pub mismatch: Option<CompatibilityMismatch>,
}

/// Exact compatibility evidence produced from one registry snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatibilityReport {
    /// Digest of the complete canonical evidence.
    pub evidence_digest: Digest,
    /// Reference locations whose exact pin is unavailable.
    pub incompatible_reference_locations: Vec<String>,
    /// Complete deterministic observation supporting suspension or resume.
    pub evidence: Vec<PinCompatibilityEvidence>,
}

/// A resolved canonical input delivered across the definition/action boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalBoundInput {
    bytes: Arc<[u8]>,
    digest: Digest,
}

impl CanonicalBoundInput {
    /// Accepts already-resolved canonical JSON bytes and computes their digest.
    pub fn from_canonical_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        let bytes = Arc::<[u8]>::from(bytes.into());
        Self {
            digest: digest_bytes(&bytes),
            bytes,
        }
    }

    /// Accepts resolved bytes only when their supplied digest is exact.
    pub fn with_digest(bytes: impl Into<Vec<u8>>, digest: Digest) -> Result<Self, InvocationError> {
        let bytes = Arc::<[u8]>::from(bytes.into());
        let actual = digest_bytes(&bytes);
        if actual != digest {
            return Err(InvocationError::BoundInputDigestMismatch {
                expected: digest,
                actual,
            });
        }
        Ok(Self { bytes, digest })
    }

    /// Returns the exact immutable bytes supplied by binding resolution.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the SHA-256 digest of those exact bytes.
    pub fn digest(&self) -> &Digest {
        &self.digest
    }
}

/// Invocation rejection before a registered action is allowed to run.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvocationError {
    /// No implementation resolves for the pinned action name.
    #[error("action {name:?} is not registered")]
    ActionNotRegistered {
        /// Pinned action name.
        name: String,
    },
    /// A resolved implementation differs from the immutable action pin.
    #[error("registered action does not match pin: {mismatch:?}")]
    IncompatibleAction {
        /// First incompatible contract dimension.
        mismatch: CompatibilityMismatch,
    },
    /// Invocation metadata says a different digest than the delivered bytes.
    #[error("bound input digest does not match exact delivered bytes")]
    BoundInputDigestMismatch {
        /// Digest expected by invocation metadata.
        expected: Digest,
        /// Digest calculated from delivery bytes.
        actual: Digest,
    },
    /// Invocation metadata says a different byte size than delivered.
    #[error("bound input size does not match exact delivered bytes")]
    BoundInputSizeMismatch {
        /// Size expected by invocation metadata.
        expected: u64,
        /// Exact delivered byte count.
        actual: u64,
    },
    /// The database-clock deadline already passed before action delivery.
    #[error("action deadline is expired")]
    DeadlineExpired {
        /// Persisted action deadline.
        deadline: Timestamp,
        /// Database-clock time used for this delivery decision.
        database_now: Timestamp,
    },
    /// The action returned an outcome that cannot cross the persistence boundary.
    #[error("action returned an invalid outcome: {0}")]
    InvalidOutcome(#[from] ActionOutcomeValidationError),
}

/// Delivers an immutable invocation to its exact compatible registered action.
///
/// Binding resolution is deliberately outside this API: callers provide the
/// already-resolved canonical bytes and this boundary verifies their digest
/// immediately before delivery.
pub async fn invoke_registered_at(
    registry: &dyn ActionRegistry,
    invocation: &ActionInvocation,
    context: ActionContext,
    canonical_bound_input: &CanonicalBoundInput,
    database_now: Timestamp,
) -> Result<ActionOutcome, InvocationError> {
    if database_now >= context.deadline {
        return Err(InvocationError::DeadlineExpired {
            deadline: context.deadline,
            database_now,
        });
    }
    if invocation.bound_input_ref.0.digest != invocation.bound_input_digest {
        return Err(InvocationError::BoundInputDigestMismatch {
            expected: invocation.bound_input_ref.0.digest.clone(),
            actual: invocation.bound_input_digest.clone(),
        });
    }
    if invocation.bound_input_ref.0.size_bytes != invocation.bound_input_size_bytes {
        return Err(InvocationError::BoundInputSizeMismatch {
            expected: invocation.bound_input_ref.0.size_bytes,
            actual: invocation.bound_input_size_bytes,
        });
    }
    if canonical_bound_input.digest != invocation.bound_input_digest {
        return Err(InvocationError::BoundInputDigestMismatch {
            expected: invocation.bound_input_digest.clone(),
            actual: canonical_bound_input.digest.clone(),
        });
    }
    let actual_size = canonical_bound_input.bytes.len() as u64;
    if actual_size != invocation.bound_input_size_bytes {
        return Err(InvocationError::BoundInputSizeMismatch {
            expected: invocation.bound_input_size_bytes,
            actual: actual_size,
        });
    }
    let action = registry.resolve(&invocation.action_name).ok_or_else(|| {
        InvocationError::ActionNotRegistered {
            name: invocation.action_name.clone(),
        }
    })?;
    let descriptor = action.descriptor();
    if descriptor.name != invocation.action_name {
        return Err(InvocationError::IncompatibleAction {
            mismatch: CompatibilityMismatch::Name,
        });
    }
    if descriptor.contract_version != invocation.contract_version {
        return Err(InvocationError::IncompatibleAction {
            mismatch: CompatibilityMismatch::ContractVersion,
        });
    }
    if descriptor.input_schema_digest != invocation.input_schema_digest {
        return Err(InvocationError::IncompatibleAction {
            mismatch: CompatibilityMismatch::InputSchemaDigest,
        });
    }
    if descriptor.output_schema_digest != invocation.output_schema_digest {
        return Err(InvocationError::IncompatibleAction {
            mismatch: CompatibilityMismatch::OutputSchemaDigest,
        });
    }
    if descriptor.implementation_compatibility_digest
        != invocation.compatible_implementation_requirement
    {
        return Err(InvocationError::IncompatibleAction {
            mismatch: CompatibilityMismatch::ImplementationCompatibilityDigest,
        });
    }
    let outcome = action.invoke(context, canonical_bound_input.bytes()).await;
    outcome.validate()?;
    Ok(outcome)
}

fn digest_bytes(bytes: &[u8]) -> Digest {
    let hash = Sha256::digest(bytes);
    Digest::new(format!("sha256:{hash:x}")).expect("SHA-256 output is valid")
}

/// Hashes compatibility evidence over RFC 8785 canonical JSON bytes.
pub fn compatibility_evidence_digest<T: Serialize>(value: &T) -> Digest {
    let bytes = serde_jcs::to_vec(value).expect("compatibility evidence is serializable");
    digest_bytes(&bytes)
}

fn invalid_diagnostics(path: &str, code: &'static str) -> DiagnosticsValidationError {
    DiagnosticsValidationError::Invalid {
        path: path.to_owned(),
        code,
    }
}

fn valid_fact_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b':' | b'-'))
}

fn is_sensitive_fact_name(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "password"
            | "passwd"
            | "secret"
            | "secret_key"
            | "token"
            | "access_token"
            | "refresh_token"
            | "api_key"
            | "apikey"
            | "credential"
            | "credentials"
            | "private_key"
    )
}

fn valid_action_code(value: &str) -> bool {
    valid_fact_name(value) && value.len() <= 128 && (value.contains('.') || value.contains(':'))
}

fn validate_optional_diagnostics(
    diagnostics: &Option<DiagnosticsEnvelope>,
) -> Result<(), ActionOutcomeValidationError> {
    if let Some(diagnostics) = diagnostics {
        diagnostics.validate()?;
    }
    Ok(())
}

/// Deterministic no-network mock actions shared by the reference workflow tests.
pub mod fixtures;
