//! Action registry, invocation, context, and outcome contracts from contract sections 1.8, 7, and 13.

use crate::artifact::{ArtifactRefValue, JsonRef, VerifiedObjectRef};
use crate::definition::ActionPin;
use crate::ids::{CostUnits, Digest, Id, NodeInstanceId, Timestamp};
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Opaque per-attempt result-intake capability. Contract section 1.1.
#[derive(Clone, Eq, PartialEq)]
pub struct CompletionCredential(Vec<u8>);

impl std::fmt::Debug for CompletionCredential {
    fn fmt(&self, _formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

/// Cooperative advisory cancellation observed by actions. Contract section 7.1.
pub trait CancellationToken: Send + Sync {
    /// Reports whether cancellation was requested. Contract section 7.1.
    fn is_cancelled(&self) -> bool;
}

/// Read-only declared budget made available to an action. Contract section 7.1.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetHandle {
    /// Provider-enforceable declared maximum. Contract section 7.1.
    pub declared_max_cost_units: CostUnits,
}

/// The exact context for one action attempt. Contract section 7.1.
#[derive(Clone)]
pub struct ActionContext {
    /// Execution scope. Contract section 7.1.
    pub scope: ExecutionScope,
    /// Owning run ID. Contract section 7.1.
    pub run_id: Id,
    /// Pinned revision hash. Contract section 7.1.
    pub revision_hash: Digest,
    /// Logical node instance ID. Contract section 7.1.
    pub node_instance_id: NodeInstanceId,
    /// Per-attempt ID. Contract section 7.1.
    pub attempt_id: Id,
    /// One-based attempt number. Contract section 7.1.
    pub attempt_number: u32,
    /// Retry-stable scope-bound external key. Contract section 7.1.
    pub idempotency_key: String,
    /// Result-intake capability for this attempt only. Contract section 7.1.
    pub completion_credential: CompletionCredential,
    /// Persisted database-clock deadline. Contract section 7.1.
    pub deadline: Timestamp,
    /// Cooperative advisory cancellation. Contract section 7.1.
    pub cancellation_token: Arc<dyn CancellationToken>,
    /// Read-only declared budget. Contract section 7.1.
    pub budget: BudgetHandle,
}

/// One immutable action invocation snapshot. Contract section 1.8.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionInvocation {
    /// Invocation scope. Contract section 1.8.
    pub scope: ExecutionScope,
    /// Owning run ID. Contract section 1.8.
    pub run_id: Id,
    /// Invocation ID, equal to attempt ID in v0.1. Contract section 1.8.
    pub invocation_id: Id,
    /// Owning node instance. Contract section 1.8.
    pub node_instance_id: NodeInstanceId,
    /// Owning attempt ID. Contract section 1.8.
    pub attempt_id: Id,
    /// Node or Map action reference location. Contract section 1.8.
    pub action_reference_location: String,
    /// Pinned action name. Contract section 1.8.
    pub action_name: String,
    /// Pinned contract version. Contract section 1.8.
    pub contract_version: String,
    /// Owning revision hash. Contract section 1.8.
    pub revision_hash: Digest,
    /// Pinned input schema digest. Contract section 1.8.
    pub input_schema_digest: Digest,
    /// Pinned output schema digest. Contract section 1.8.
    pub output_schema_digest: Digest,
    /// Pinned semantic implementation requirement. Contract section 1.8.
    pub compatible_implementation_requirement: Digest,
    /// Exact canonical bound input ref. Contract section 1.8.
    pub bound_input_ref: JsonRef,
    /// Digest of exact delivered bytes. Contract section 1.8.
    pub bound_input_digest: Digest,
    /// Exact delivered byte length. Contract section 1.8.
    pub bound_input_size_bytes: u64,
    /// Ordered binding derivation digest. Contract section 1.8.
    pub binding_derivation_digest: Digest,
    /// Creation database timestamp. Contract section 1.8.
    pub created_at: Timestamp,
}

/// One action-produced artifact before domain registration. Contract section 7.2.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactOutput {
    /// Normalized artifact media type. Contract section 7.2.
    pub media_type: String,
    /// Verified durable object capability. Contract section 7.2.
    pub object: VerifiedObjectRef,
}

/// One bounded diagnostics fact value. Contract section 1.1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiagnosticScalar {
    /// String scalar. Contract section 1.1.
    String(String),
    /// Boolean scalar. Contract section 1.1.
    Boolean(bool),
    /// JSON number scalar. Contract section 1.1.
    Number(serde_json::Number),
    /// Null scalar. Contract section 1.1.
    Null,
}

/// One named diagnostics fact. Contract section 1.1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticFact {
    /// Unique persistence-safe fact name. Contract section 1.1.
    pub name: String,
    /// Scalar fact value. Contract section 1.1.
    pub value: DiagnosticScalar,
}

/// The closed persistence-safe diagnostics envelope. Contract section 1.1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsEnvelope {
    /// Optional bounded summary. Contract section 1.1.
    pub summary: Option<String>,
    /// Ordered bounded facts. Contract section 1.1.
    pub facts: Vec<DiagnosticFact>,
    /// Ordered related artifact refs. Contract section 1.1.
    pub related_artifact_refs: Vec<ArtifactRefValue>,
}

/// The closed action outcome vocabulary. Contract section 7.2.
#[derive(Clone, Debug, PartialEq)]
pub enum ActionOutcome {
    /// Typed JSON success with ordered artifacts. Contract section 7.2.
    Success {
        /// Typed JSON output. Contract section 7.2.
        output: Value,
        /// Ordered verified artifacts. Contract section 7.2.
        artifacts: Vec<ArtifactOutput>,
        /// Trusted reported actual cost. Contract section 7.2.
        actual_cost_units: CostUnits,
        /// Optional closed diagnostics. Contract section 7.2.
        diagnostics: Option<DiagnosticsEnvelope>,
    },
    /// Structured retryable action error. Contract section 7.2.
    Retryable {
        /// Namespaced action code. Contract section 7.2.
        code: String,
        /// Persistence-safe message. Contract section 7.2.
        message: String,
        /// Optional closed diagnostics. Contract section 7.2.
        diagnostics: Option<DiagnosticsEnvelope>,
        /// Trusted reported actual cost. Contract section 7.2.
        actual_cost_units: CostUnits,
    },
    /// Structured permanent action error. Contract section 7.2.
    Permanent {
        /// Namespaced action code. Contract section 7.2.
        code: String,
        /// Persistence-safe message. Contract section 7.2.
        message: String,
        /// Optional closed diagnostics. Contract section 7.2.
        diagnostics: Option<DiagnosticsEnvelope>,
        /// Trusted reported actual cost. Contract section 7.2.
        actual_cost_units: CostUnits,
    },
}

/// A registered implementation's exact compatibility descriptor. Contract section 13.2.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionDescriptor {
    /// Registry action name. Contract section 13.2.
    pub name: String,
    /// Action contract version. Contract section 13.2.
    pub contract_version: String,
    /// Input schema digest. Contract section 13.2.
    pub input_schema_digest: Digest,
    /// Output schema digest. Contract section 13.2.
    pub output_schema_digest: Digest,
    /// Advertised semantic compatibility digest. Contract section 13.2.
    pub implementation_compatibility_digest: Digest,
}

/// A compatibility mismatch dimension. Contract section 13.2.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatibilityMismatch {
    /// Action name differs. Contract section 13.2.
    Name,
    /// Contract version differs. Contract section 13.2.
    ContractVersion,
    /// Input schema digest differs. Contract section 13.2.
    InputSchemaDigest,
    /// Output schema digest differs. Contract section 13.2.
    OutputSchemaDigest,
    /// Semantic compatibility digest differs. Contract section 13.2.
    ImplementationCompatibilityDigest,
}

/// Checks all five compatibility dimensions byte-for-byte. Contract section 13.2.
pub fn check_compatibility(
    _pin: &ActionPin,
    _implementation: &ActionDescriptor,
) -> Result<(), CompatibilityMismatch> {
    todo!()
}

/// One host-provided workflow action. Contract section 7.
pub trait WorkflowAction: Send + Sync {
    /// Returns the implementation's exact descriptor. Contract section 13.2.
    fn descriptor(&self) -> &ActionDescriptor;

    /// Executes against exact canonical invocation bytes. Contract section 7.1.
    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        canonical_bound_input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>>;
}

/// Name-addressed action implementation registry. Contract section 13.3.
pub trait ActionRegistry: Send + Sync {
    /// Resolves a name-addressed action implementation. Contract section 13.2.
    fn resolve(&self, name: &str) -> Option<Arc<dyn WorkflowAction>>;

    /// Checks whether every exact revision pin is available. Contract section 13.3.
    fn check_pins(&self, pins: &[ActionPin]) -> CompatibilityReport;
}

/// Exact compatibility evidence produced from one registry snapshot. Contract section 13.3.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatibilityReport {
    /// Digest of the complete canonical evidence. Contract section 13.3.
    pub evidence_digest: Digest,
    /// Reference locations whose exact pin is unavailable. Contract section 13.3.
    pub incompatible_reference_locations: Vec<String>,
}
