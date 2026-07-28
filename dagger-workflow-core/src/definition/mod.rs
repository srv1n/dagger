//! Definition-format model and validation boundary from contract sections 8, 9, 10, 13, and 14.

use crate::approval::{ApprovalExpiryPolicy, DecisionAuthorizationPolicy};
use crate::ids::{CostUnits, Digest, Id, TopologicalRank};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The strict, normalized workflow definition document. Contract section 14.1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    /// Required format version, exactly `0.1`. Contract section 13.1.
    pub definition_format_version: String,
    /// Definition identity matched at publication. Contract section 13.1.
    pub definition_id: Id,
    /// Human-readable definition name. Contract section 14.1.
    pub name: String,
    /// Human-readable definition description. Contract section 14.1.
    #[serde(default)]
    pub description: String,
    /// Pinned root input schema digest. Contract section 13.1.
    pub run_input_schema_digest: Digest,
    /// Pinned root output schema digest. Contract section 13.1.
    pub run_output_schema_digest: Digest,
    /// Unique entry node identifier. Contract section 14.1.
    pub entry_node_id: Id,
    /// Closed list of definition nodes. Contract section 14.1.
    pub nodes: Vec<NodeDefinition>,
}

/// The closed definition node vocabulary. Contract section 14.1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum NodeDefinition {
    /// An executable action node. Contract section 14.1.
    Action {
        /// Node identifier. Contract section 14.1.
        id: Id,
        /// Pinned action contract. Contract section 13.2.
        action: ActionReference,
        /// Explicit action input bindings. Contract section 8.1.
        bindings: Vec<Binding>,
        /// Retry policy. Contract section 14.1.
        retry: RetryPolicy,
        /// Attempt timeout policy. Contract section 14.1.
        timeout: TimeoutPolicy,
        /// Maximum reserved cost represented as decimal in JSON. Contract section 13.1.
        declared_max_cost_units: CostUnits,
        /// Normal outgoing targets. Contract section 14.2.
        next: Vec<Id>,
    },
    /// A bounded action fan-out node. Contract section 10.
    Map {
        /// Node identifier. Contract section 14.1.
        id: Id,
        /// Source resolving to the input array. Contract section 10.1.
        items: ValueSource,
        /// Maximum number of child items. Contract section 10.3.
        max_items: u32,
        /// Maximum concurrent children. Contract section 10.3.
        max_concurrency: u32,
        /// Pinned child action contract. Contract section 13.2.
        action: ActionReference,
        /// Explicit child input bindings. Contract section 10.2.
        bindings: Vec<MapBinding>,
        /// Child retry policy. Contract section 14.1.
        retry: RetryPolicy,
        /// Child timeout policy. Contract section 14.1.
        timeout: TimeoutPolicy,
        /// Per-child maximum reserved cost. Contract section 10.5.
        declared_max_cost_units: CostUnits,
        /// Normal outgoing targets. Contract section 14.2.
        next: Vec<Id>,
    },
    /// A deterministic first-match branch node. Contract section 9.
    Choice {
        /// Node identifier. Contract section 14.1.
        id: Id,
        /// Source forming the immutable Choice input. Contract section 9.
        input: ValueSource,
        /// Selector RFC 6901 pointer. Contract section 9.
        selector: String,
        /// Ordered non-empty cases. Contract section 9.
        cases: Vec<ChoiceCase>,
        /// Required default target. Contract section 9.
        default: Id,
    },
    /// A durable human approval node. Contract section 3.5.
    Approval {
        /// Node identifier. Contract section 14.1.
        id: Id,
        /// Source forming the approval request. Contract section 14.1.
        request: ValueSource,
        /// Immutable gate configuration. Contract section 3.5.
        gate: ApprovalGateConfig,
        /// Normal outgoing targets. Contract section 14.2.
        next: Vec<Id>,
    },
    /// The unique successful terminal node. Contract section 14.2.
    Succeed {
        /// Node identifier. Contract section 14.1.
        id: Id,
        /// Source forming the root workflow output. Contract section 14.1.
        output: ValueSource,
    },
    /// An explicit failing terminal node. Contract section 14.2.
    Fail {
        /// Node identifier. Contract section 14.1.
        id: Id,
        /// Persistence-safe failure code. Contract section 14.1.
        code: String,
        /// Persistence-safe failure message. Contract section 14.1.
        message: String,
    },
}

/// An executable action reference as represented in definition JSON. Contract section 14.1.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionReference {
    /// Registry action name. Contract section 13.2.
    pub name: String,
    /// Action contract version. Contract section 13.2.
    pub contract_version: String,
    /// Pinned input schema digest. Contract section 13.2.
    pub input_schema_digest: Digest,
    /// Pinned output schema digest. Contract section 13.2.
    pub output_schema_digest: Digest,
    /// Required semantic compatibility digest. Contract section 13.2.
    pub compatible_implementation_requirement: Digest,
}

/// A published executable pin keyed by its derived reference location. Contract section 1.3.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionPin {
    /// Node ID or derived `node_id/map_action` location. Contract section 1.3.
    pub reference_location: String,
    /// Registry action name. Contract section 13.2.
    pub name: String,
    /// Action contract version. Contract section 13.2.
    pub contract_version: String,
    /// Pinned input schema digest. Contract section 13.2.
    pub input_schema_digest: Digest,
    /// Pinned output schema digest. Contract section 13.2.
    pub output_schema_digest: Digest,
    /// Required semantic compatibility digest. Contract section 13.2.
    pub compatible_implementation_requirement: Digest,
    /// Durable supported-subset input schema ref. Contract section 1.3.
    pub input_schema_ref: crate::artifact::JsonRef,
    /// Durable supported-subset output schema ref. Contract section 1.3.
    pub output_schema_ref: crate::artifact::JsonRef,
}

/// A timeout policy for one attempt. Contract section 14.1.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeoutPolicy {
    /// Positive timeout in milliseconds. Contract section 14.1.
    pub timeout_ms: u64,
}

/// Retry ceiling and deterministic backoff. Contract section 14.1.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    /// Maximum number of started attempts. Contract section 14.2.
    pub max_attempts: u32,
    /// Closed no-jitter backoff policy. Contract section 14.2.
    pub backoff: BackoffPolicy,
}

/// The closed retry backoff vocabulary. Contract section 14.2.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackoffPolicy {
    /// A constant delay. Contract section 14.2.
    Fixed {
        /// Delay in milliseconds. Contract section 14.1.
        delay_ms: u64,
    },
    /// A capped deterministic exponential delay. Contract section 14.2.
    Exponential {
        /// Initial delay in milliseconds. Contract section 14.1.
        initial_delay_ms: u64,
        /// Integer exponential multiplier. Contract section 14.1.
        multiplier: u32,
        /// Maximum delay in milliseconds. Contract section 14.1.
        max_delay_ms: u64,
    },
}

/// One explicit target-field assignment. Contract section 8.1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Target RFC 6901 pointer. Contract section 8.1.
    pub target: String,
    /// Closed source descriptor. Contract section 8.1.
    pub source: BindingSource,
}

/// One explicit Map-child target-field assignment. Contract section 10.2.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapBinding {
    /// Target RFC 6901 pointer. Contract section 8.1.
    pub target: String,
    /// Closed Map-aware source descriptor. Contract section 10.2.
    pub source: MapBindingSource,
}

/// A single-value source used by Choice, Map, Approval, and Succeed. Contract section 14.1.
pub type ValueSource = BindingSource;

/// The closed ordinary binding-source vocabulary. Contract section 8.1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BindingSource {
    /// An exact literal JSON value. Contract section 8.1.
    Constant {
        /// Literal value. Contract section 8.1.
        value: Value,
    },
    /// A pointer into immutable run input. Contract section 8.1.
    RunInput {
        /// Source RFC 6901 pointer. Contract section 8.1.
        pointer: String,
    },
    /// A pointer into a named successful upstream output. Contract section 8.1.
    NodeOutput {
        /// Static upstream node identifier. Contract section 8.1.
        node_id: Id,
        /// Source RFC 6901 pointer. Contract section 8.1.
        pointer: String,
    },
    /// A typed ArtifactRef locator. Contract section 8.1.
    ArtifactRef {
        /// Closed artifact locator. Contract section 8.1.
        source: ArtifactLocator,
    },
}

/// The closed Map-child binding-source vocabulary. Contract section 10.2.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MapBindingSource {
    /// An exact literal JSON value. Contract section 8.1.
    Constant {
        /// Literal value. Contract section 8.1.
        value: Value,
    },
    /// A pointer into immutable run input. Contract section 8.1.
    RunInput {
        /// Source RFC 6901 pointer. Contract section 8.1.
        pointer: String,
    },
    /// A pointer into a named successful upstream output. Contract section 8.1.
    NodeOutput {
        /// Static upstream node identifier. Contract section 8.1.
        node_id: Id,
        /// Source RFC 6901 pointer. Contract section 8.1.
        pointer: String,
    },
    /// A typed ArtifactRef locator. Contract section 8.1.
    ArtifactRef {
        /// Closed artifact locator. Contract section 8.1.
        source: ArtifactLocator,
    },
    /// The current Map item or a pointer within it. Contract section 10.2.
    MapItem {
        /// Item RFC 6901 pointer. Contract section 10.2.
        pointer: String,
    },
    /// The current zero-based Map item index. Contract section 10.2.
    MapIndex,
}

/// The closed ArtifactRef locator vocabulary. Contract section 8.1.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactLocator {
    /// An exact pre-existing scoped artifact identity. Contract section 8.1.
    Literal {
        /// ArtifactRef identifier. Contract section 8.1.
        artifact_ref_id: Id,
        /// Required content digest. Contract section 8.1.
        digest: Digest,
        /// Required normalized media type. Contract section 8.1.
        media_type: String,
    },
    /// An ArtifactRef value selected from run input. Contract section 8.1.
    RunInput {
        /// Source RFC 6901 pointer. Contract section 8.1.
        pointer: String,
    },
    /// An ArtifactRef value selected from upstream output. Contract section 8.1.
    NodeOutput {
        /// Static upstream node identifier. Contract section 8.1.
        node_id: Id,
        /// Source RFC 6901 pointer. Contract section 8.1.
        pointer: String,
    },
}

/// One ordered Choice comparison and target. Contract section 9.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ChoiceCase {
    /// Exact scalar equality. Contract section 9.
    Equals {
        /// Scalar to compare exactly. Contract section 9.
        equals: Value,
        /// Singular case target. Contract section 9.
        next: Id,
    },
    /// Exact membership in a non-empty scalar set. Contract section 9.
    In {
        /// Canonical-unique scalar candidates. Contract section 9.
        r#in: Vec<Value>,
        /// Singular case target. Contract section 9.
        next: Id,
    },
}

/// Immutable durable gate configuration. Contract section 14.1.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalGateConfig {
    /// Gate lifetime after request in milliseconds. Contract section 14.1.
    pub expires_after_ms: u64,
    /// Closed expiry behavior, defaulting to reject. Contract section 3.5.
    #[serde(default)]
    pub on_expiry: ApprovalExpiryPolicy,
    /// Immutable decision authorization policy. Contract section 3.5.
    pub authorization: DecisionAuthorizationPolicy,
}

/// Structured, closed validation error categories. Contract sections 5.5 and 14.2.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationErrorKind {
    /// A field failed syntactic constraints. Contract section 14.1.
    InvalidField,
    /// An unknown structural field was present. Contract section 14.1.
    UnknownField,
    /// A node ID was duplicated. Contract section 14.2.
    DuplicateNodeId,
    /// An entry or edge target does not exist. Contract section 14.2.
    MissingNode,
    /// A graph contains a cycle. Contract section 14.2.
    Cycle,
    /// A node is unreachable from the entry. Contract section 14.2.
    UnreachableNode,
    /// A maximal path has no explicit terminal. Contract section 14.2.
    UnterminatedPath,
    /// The definition does not contain exactly one reachable Succeed node. Contract section 14.2.
    InvalidSucceedCount,
    /// A Choice is missing its required default. Contract section 9.
    ChoiceDefaultMissing,
    /// Choice cases overlap or target the same branch. Contract section 9.
    ChoiceCaseInvalid,
    /// A Map is unbounded or has inconsistent bounds. Contract section 10.3.
    MapBoundsInvalid,
    /// A retry policy is invalid. Contract section 14.2.
    RetryPolicyInvalid,
    /// A timeout policy is invalid. Contract section 14.1.
    TimeoutPolicyInvalid,
    /// An action or root schema pin is unresolved. Contract section 13.1.
    SchemaPinUnresolved,
    /// A schema uses unsupported features. Contract section 14.3.
    SchemaSubsetUnsupported,
    /// A binding target is absent, duplicated, or overlapping. Contract section 8.3.
    BindingTargetInvalid,
    /// A binding source is missing or unsafe on an activating path. Contract section 8.3.
    BindingSourceInvalid,
    /// Source and target schema types are not assignable. Contract section 8.3.
    BindingTypeMismatch,
    /// An RFC 6901 pointer is invalid. Contract section 8.3.
    JsonPointerInvalid,
    /// A literal artifact does not resolve exactly. Contract section 8.3.
    ArtifactLocatorInvalid,
    /// A decimal cost value overflows u64. Contract section 13.1.
    CostUnitsInvalid,
    /// The canonical definition exceeds its byte limit. Contract section 14.2.
    DefinitionTooLarge,
    /// Approval authorization is empty or invalid. Contract section 14.2.
    ApprovalAuthorizationInvalid,
}

/// One LLM-correctable definition validation failure. Contract section 5.5.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{kind:?} at {path}: {message}")]
pub struct DefinitionValidationError {
    /// Closed validation category. Contract section 5.5.
    pub kind: ValidationErrorKind,
    /// Bounded document path. Contract section 1.1.
    pub path: String,
    /// Bounded corrective message. Contract section 1.1.
    pub message: String,
    /// Bounded valid alternatives. Contract section 5.5.
    pub valid_alternatives: Vec<String>,
}

/// Validates syntax and all mandatory semantic constraints. Contract section 14.2.
pub fn validate_definition(
    _definition: &WorkflowDefinition,
) -> Result<ValidatedDefinition, Vec<DefinitionValidationError>> {
    todo!()
}

/// A definition that passed the full publication validation phase. Contract section 14.2.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedDefinition {
    /// Normalized typed definition. Contract section 13.1.
    pub definition: WorkflowDefinition,
    /// Canonical Kahn ranks keyed by node ID. Contract section 1.5.
    pub topological_ranks: BTreeMap<Id, TopologicalRank>,
}

/// Expands schema defaults into a normalized typed definition. Contract section 14.2.
pub fn normalize_definition(
    _definition: WorkflowDefinition,
) -> Result<WorkflowDefinition, DefinitionValidationError> {
    todo!()
}

/// Produces RFC 8785 canonical definition bytes. Contract section 13.1.
pub fn canonical_definition_json(
    _definition: &WorkflowDefinition,
) -> Result<Vec<u8>, DefinitionValidationError> {
    todo!()
}

/// Computes the revision digest from canonical definition bytes. Contract section 13.1.
pub fn revision_hash(_canonical_definition: &[u8]) -> Digest {
    todo!()
}

/// Computes lexical Kahn topological ranks. Contract section 1.5.
pub fn canonical_topological_ranks(
    _definition: &WorkflowDefinition,
) -> Result<BTreeMap<Id, TopologicalRank>, DefinitionValidationError> {
    todo!()
}
