//! Definition-format model and validation boundary.

use crate::approval::{ApprovalExpiryPolicy, DecisionAuthorizationPolicy};
use crate::ids::{CostUnits, Digest, Id, TopologicalRank};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The strict, normalized workflow definition document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    /// Required format version, exactly `0.1`.
    pub definition_format_version: String,
    /// Definition identity matched at publication.
    pub definition_id: Id,
    /// Human-readable definition name.
    pub name: String,
    /// Human-readable definition description.
    #[serde(default)]
    pub description: String,
    /// Pinned root input schema digest.
    pub run_input_schema_digest: Digest,
    /// Pinned root output schema digest.
    pub run_output_schema_digest: Digest,
    /// Closed list of definition nodes.
    pub nodes: Vec<NodeDefinition>,
}

/// The closed definition node vocabulary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum NodeDefinition {
    /// An executable action node.
    Action {
        /// Node identifier.
        id: Id,
        /// Pinned action contract.
        action: ActionReference,
        /// Explicit action input bindings.
        bindings: Vec<Binding>,
        /// Retry policy.
        retry: RetryPolicy,
        /// Attempt timeout policy.
        timeout: TimeoutPolicy,
        /// Maximum reserved cost represented as decimal in JSON.
        declared_max_cost_units: CostUnits,
        /// Normal outgoing targets.
        next: Vec<Id>,
    },
    /// A bounded action fan-out node.
    Map {
        /// Node identifier.
        id: Id,
        /// Source resolving to the input array.
        items: ValueSource,
        /// Maximum number of child items.
        max_items: u32,
        /// Maximum concurrent children.
        max_concurrency: u32,
        /// Pinned child action contract.
        action: ActionReference,
        /// Explicit child input bindings.
        bindings: Vec<MapBinding>,
        /// Child retry policy.
        retry: RetryPolicy,
        /// Child timeout policy.
        timeout: TimeoutPolicy,
        /// Per-child maximum reserved cost.
        declared_max_cost_units: CostUnits,
        /// Normal outgoing targets.
        next: Vec<Id>,
    },
    /// A deterministic first-match branch node.
    Choice {
        /// Node identifier.
        id: Id,
        /// Source forming the immutable Choice input.
        input: ValueSource,
        /// Selector RFC 6901 pointer.
        selector: String,
        /// Ordered non-empty cases.
        cases: Vec<ChoiceCase>,
        /// Required default outcome.
        default: ChoiceTarget,
    },
    /// A durable human approval node.
    Approval {
        /// Node identifier.
        id: Id,
        /// Source forming the approval request.
        request: ValueSource,
        /// Immutable gate configuration.
        gate: ApprovalGateConfig,
        /// Normal outgoing targets.
        next: Vec<Id>,
    },
    /// The unique successful terminal node.
    Succeed {
        /// Node identifier.
        id: Id,
        /// Source forming the root workflow output.
        output: ValueSource,
    },
    /// An explicit failing terminal node.
    Fail {
        /// Node identifier.
        id: Id,
        /// Persistence-safe failure code.
        code: String,
        /// Persistence-safe failure message.
        message: String,
    },
}

/// An executable action reference as represented in definition JSON.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionReference {
    /// Registry action name.
    pub name: String,
    /// Action contract version.
    pub contract_version: String,
    /// Pinned input schema digest.
    pub input_schema_digest: Digest,
    /// Pinned output schema digest.
    pub output_schema_digest: Digest,
    /// Required semantic compatibility digest.
    pub compatible_implementation_requirement: Digest,
}

/// A published executable pin keyed by its derived reference location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionPin {
    /// Node ID or derived `node_id/map_action` location.
    pub reference_location: String,
    /// Registry action name.
    pub name: String,
    /// Action contract version.
    pub contract_version: String,
    /// Pinned input schema digest.
    pub input_schema_digest: Digest,
    /// Pinned output schema digest.
    pub output_schema_digest: Digest,
    /// Required semantic compatibility digest.
    pub compatible_implementation_requirement: Digest,
    /// Durable supported-subset input schema ref.
    pub input_schema_ref: crate::artifact::JsonRef,
    /// Durable supported-subset output schema ref.
    pub output_schema_ref: crate::artifact::JsonRef,
}

/// An action pin extracted from a validated definition before durable schema refs exist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractedActionPin {
    /// Node ID or `node_id/map_action`; this is the durable pin key.
    pub reference_location: String,
    /// Registry action name.
    pub name: String,
    /// Pinned action contract version.
    pub contract_version: String,
    /// Pinned action input schema digest.
    pub input_schema_digest: Digest,
    /// Pinned action output schema digest.
    pub output_schema_digest: Digest,
    /// Exact semantic implementation requirement.
    pub compatible_implementation_requirement: Digest,
}

/// A timeout policy for one attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeoutPolicy {
    /// Positive timeout in milliseconds.
    pub timeout_ms: u64,
}

/// Retry ceiling and deterministic backoff.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    /// Maximum number of started attempts.
    pub max_attempts: u32,
    /// Closed no-jitter backoff policy.
    pub backoff: BackoffPolicy,
}

/// The closed retry backoff vocabulary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackoffPolicy {
    /// A constant delay.
    Fixed {
        /// Delay in milliseconds.
        delay_ms: u64,
    },
    /// A capped deterministic exponential delay.
    Exponential {
        /// Initial delay in milliseconds.
        initial_delay_ms: u64,
        /// Integer exponential multiplier.
        multiplier: u32,
        /// Maximum delay in milliseconds.
        max_delay_ms: u64,
    },
}

/// One explicit target-field assignment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Target RFC 6901 pointer.
    pub target: String,
    /// Closed source descriptor.
    pub source: BindingSource,
}

/// One explicit Map-child target-field assignment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapBinding {
    /// Target RFC 6901 pointer.
    pub target: String,
    /// Closed Map-aware source descriptor.
    pub source: MapBindingSource,
}

/// A single-value source used by Choice, Map, Approval, and Succeed.
pub type ValueSource = BindingSource;

/// The closed ordinary binding-source vocabulary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BindingSource {
    /// An exact literal JSON value.
    Constant {
        /// Literal value.
        value: Value,
    },
    /// A pointer into immutable run input.
    RunInput {
        /// Source RFC 6901 pointer.
        pointer: String,
    },
    /// A pointer into a named successful upstream output.
    NodeOutput {
        /// Static upstream node identifier.
        node_id: Id,
        /// Source RFC 6901 pointer.
        pointer: String,
    },
    /// The ordered projection of one pointer from every child output of a Map.
    MapAggregate {
        /// Static upstream Map node identifier.
        node_id: Id,
        /// Source RFC 6901 pointer inside each child output.
        pointer: String,
    },
    /// A deterministic JSON object assembled from named closed sources.
    Object {
        /// Named object fields in canonical lexical order.
        fields: BTreeMap<String, BindingSource>,
    },
    /// A deterministic JSON array assembled from closed sources in authored order.
    Array {
        /// Ordered array items.
        items: Vec<BindingSource>,
    },
    /// A typed ArtifactRef locator.
    ArtifactRef {
        /// Closed artifact locator.
        source: ArtifactLocator,
    },
}

/// The closed Map-child binding-source vocabulary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MapBindingSource {
    /// An exact literal JSON value.
    Constant {
        /// Literal value.
        value: Value,
    },
    /// A pointer into immutable run input.
    RunInput {
        /// Source RFC 6901 pointer.
        pointer: String,
    },
    /// A pointer into a named successful upstream output.
    NodeOutput {
        /// Static upstream node identifier.
        node_id: Id,
        /// Source RFC 6901 pointer.
        pointer: String,
    },
    /// The ordered projection of one pointer from every child output of a Map.
    MapAggregate {
        /// Static upstream Map node identifier.
        node_id: Id,
        /// Source RFC 6901 pointer inside each child output.
        pointer: String,
    },
    /// A deterministic JSON object assembled from named closed sources.
    Object {
        /// Named object fields in canonical lexical order.
        fields: BTreeMap<String, BindingSource>,
    },
    /// A deterministic JSON array assembled from closed sources in authored order.
    Array {
        /// Ordered array items.
        items: Vec<BindingSource>,
    },
    /// A typed ArtifactRef locator.
    ArtifactRef {
        /// Closed artifact locator.
        source: ArtifactLocator,
    },
    /// The current Map item or a pointer within it.
    MapItem {
        /// Item RFC 6901 pointer.
        pointer: String,
    },
    /// The current zero-based Map item index.
    MapIndex,
}

/// The closed ArtifactRef locator vocabulary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactLocator {
    /// An exact pre-existing scoped artifact identity.
    Literal {
        /// ArtifactRef identifier.
        artifact_ref_id: Id,
        /// Required content digest.
        digest: Digest,
        /// Required normalized media type.
        media_type: String,
    },
    /// An ArtifactRef value selected from run input.
    RunInput {
        /// Source RFC 6901 pointer.
        pointer: String,
    },
    /// An ArtifactRef value selected from upstream output.
    NodeOutput {
        /// Static upstream node identifier.
        node_id: Id,
        /// Source RFC 6901 pointer.
        pointer: String,
    },
}

/// One ordered Choice comparison and target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ChoiceCase {
    /// Exact scalar equality.
    Equals {
        /// Scalar to compare exactly.
        equals: Value,
        /// Case outcome.
        target: ChoiceTarget,
    },
    /// Exact membership in a non-empty scalar set.
    In {
        /// Canonical-unique scalar candidates.
        r#in: Vec<Value>,
        /// Case outcome.
        target: ChoiceTarget,
    },
}

/// One closed Choice outcome.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChoiceTarget {
    /// Activate one authored control target.
    Node {
        /// Singular target node.
        next: Id,
    },
    /// Deterministically skip one authored control target.
    Skip {
        /// Singular guarded target node.
        next: Id,
    },
}

/// Immutable durable gate configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalGateConfig {
    /// Gate lifetime after request in milliseconds.
    pub expires_after_ms: u64,
    /// Closed expiry behavior, defaulting to reject.
    #[serde(default)]
    pub on_expiry: ApprovalExpiryPolicy,
    /// Immutable decision authorization policy.
    pub authorization: DecisionAuthorizationPolicy,
}

/// Structured, closed validation error categories.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationErrorKind {
    /// A field failed syntactic constraints.
    InvalidField,
    /// An unknown structural field was present.
    UnknownField,
    /// A node ID was duplicated.
    DuplicateNodeId,
    /// An entry or edge target does not exist.
    MissingNode,
    /// A graph contains a cycle.
    Cycle,
    /// A node is unreachable from the entry.
    UnreachableNode,
    /// A maximal path has no explicit terminal.
    UnterminatedPath,
    /// The definition does not contain exactly one reachable Succeed node.
    InvalidSucceedCount,
    /// A Choice is missing its required default.
    ChoiceDefaultMissing,
    /// Choice cases overlap or target the same branch.
    ChoiceCaseInvalid,
    /// A Map is unbounded or has inconsistent bounds.
    MapBoundsInvalid,
    /// A retry policy is invalid.
    RetryPolicyInvalid,
    /// A timeout policy is invalid.
    TimeoutPolicyInvalid,
    /// An action or root schema pin is unresolved.
    SchemaPinUnresolved,
    /// A schema uses unsupported features.
    SchemaSubsetUnsupported,
    /// A binding target is absent, duplicated, or overlapping.
    BindingTargetInvalid,
    /// A binding source is missing or unsafe on an activating path.
    BindingSourceInvalid,
    /// Source and target schema types are not assignable.
    BindingTypeMismatch,
    /// An RFC 6901 pointer is invalid.
    JsonPointerInvalid,
    /// A literal artifact does not resolve exactly.
    ArtifactLocatorInvalid,
    /// A decimal cost value overflows u64.
    CostUnitsInvalid,
    /// The canonical definition exceeds its byte limit.
    DefinitionTooLarge,
    /// Approval authorization is empty or invalid.
    ApprovalAuthorizationInvalid,
}

/// Parses one JSON definition through the strict definition boundary.
pub fn parse_json_definition(input: &str) -> Result<WorkflowDefinition, DefinitionValidationError> {
    let value: NoDuplicateJson =
        serde_json::from_str(input).map_err(|error| parse_error("$", error.to_string()))?;
    serde_json::from_value(value.0).map_err(|error| parse_error("$", error.to_string()))
}

/// Parses one YAML definition through the swappable internal YAML seam.
pub fn parse_yaml_definition(input: &str) -> Result<WorkflowDefinition, DefinitionValidationError> {
    yaml::parse(input)
}

/// Returns the normative Draft 2020-12 workflow definition schema.
pub fn definition_json_schema() -> String {
    include_str!("../../schema/workflow-definition-0.1.json").to_owned()
}

/// Extracts every executable action pin in deterministic reference-location order.
pub fn extract_action_pins(definition: &WorkflowDefinition) -> Vec<ExtractedActionPin> {
    let mut pins = definition
        .nodes
        .iter()
        .filter_map(|node| match node {
            NodeDefinition::Action { id, action, .. } => Some((id.0.clone(), action)),
            NodeDefinition::Map { id, action, .. } => {
                Some((format!("{}/map_action", id.0), action))
            }
            _ => None,
        })
        .map(|(reference_location, action)| ExtractedActionPin {
            reference_location,
            name: action.name.clone(),
            contract_version: action.contract_version.clone(),
            input_schema_digest: action.input_schema_digest.clone(),
            output_schema_digest: action.output_schema_digest.clone(),
            compatible_implementation_requirement: action
                .compatible_implementation_requirement
                .clone(),
        })
        .collect::<Vec<_>>();
    pins.sort_by(|left, right| left.reference_location.cmp(&right.reference_location));
    pins
}

/// One LLM-correctable definition validation failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{kind:?} at {path}: {message}")]
pub struct DefinitionValidationError {
    /// Closed validation category.
    pub kind: ValidationErrorKind,
    /// Bounded document path.
    pub path: String,
    /// Bounded corrective message.
    pub message: String,
    /// Bounded valid alternatives.
    pub valid_alternatives: Vec<String>,
}

/// Validates syntax and all mandatory semantic constraints.
pub fn validate_definition(
    definition: &WorkflowDefinition,
) -> Result<UnresolvedDefinition, Vec<DefinitionValidationError>> {
    let normalized = match normalize_definition(definition.clone()) {
        Ok(value) => value,
        Err(error) => return Err(vec![error]),
    };
    let mut errors = Vec::new();
    validate_root(&normalized, &mut errors);
    let ids: BTreeMap<_, _> = normalized
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node_id(node).clone(), i))
        .collect();
    if ids.len() != normalized.nodes.len() {
        let mut seen = BTreeSet::new();
        for node in &normalized.nodes {
            if !seen.insert(node_id(node).clone()) {
                errors.push(error(
                    ValidationErrorKind::DuplicateNodeId,
                    format!("/nodes/{}/id", node_id(node).0),
                    "node IDs must be unique",
                    &["choose a new unique node id"],
                ));
            }
        }
    }
    let control_edges: BTreeMap<Id, Vec<(String, Id)>> = normalized
        .nodes
        .iter()
        .map(|node| (node_id(node).clone(), outgoing(node)))
        .collect();
    for node in &normalized.nodes {
        for (label, target) in control_edges.get(node_id(node)).into_iter().flatten() {
            if !ids.contains_key(target) {
                errors.push(error(
                    ValidationErrorKind::MissingNode,
                    format!("/nodes/{}/{}", node_id(node).0, label),
                    format!("target `{}` does not exist", target.0),
                    &["use an existing node id"],
                ));
            }
        }
        validate_node(node, &ids, &mut errors);
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let reference_edges = reference_edges(&normalized);
    let execution_edges = merged_edges(&control_edges, &reference_edges);
    if let Err(error) = ranks_from_edges(&normalized, &execution_edges) {
        errors.push(error);
    }
    let ranks = match ranks_from_edges(&normalized, &reference_edges) {
        Ok(ranks) => ranks,
        Err(error) => {
            errors.push(error);
            BTreeMap::new()
        }
    };
    let root_node_ids = roots_from_edges(&normalized, &execution_edges);
    let reachable = reachable_from(&root_node_ids, &execution_edges);
    for node in &normalized.nodes {
        if !reachable.contains(node_id(node)) {
            errors.push(error(
                ValidationErrorKind::UnreachableNode,
                format!("/nodes/{}/id", node_id(node).0),
                "every node must be reachable from the derived root set",
                &[
                    "add an incoming edge from the reachable graph",
                    "remove the node",
                ],
            ));
        }
    }
    let succeeds: Vec<_> = normalized
        .nodes
        .iter()
        .filter(|node| {
            matches!(node, NodeDefinition::Succeed { .. }) && reachable.contains(node_id(node))
        })
        .collect();
    if succeeds.len() != 1 {
        errors.push(error(
            ValidationErrorKind::InvalidSucceedCount,
            "/nodes",
            "exactly one reachable Succeed node is required",
            &[
                "add one reachable Succeed node",
                "remove extra Succeed nodes",
            ],
        ));
    }
    for node in &normalized.nodes {
        if !matches!(
            node,
            NodeDefinition::Succeed { .. } | NodeDefinition::Fail { .. }
        ) && outgoing(node).is_empty()
        {
            errors.push(error(
                ValidationErrorKind::UnterminatedPath,
                format!("/nodes/{}/next", node_id(node).0),
                "every non-terminal node must have an outgoing target",
                &["add a target ending in Succeed or Fail"],
            ));
        }
    }
    validate_bindings(&normalized, &mut errors);
    if errors.is_empty() {
        Ok(UnresolvedDefinition {
            definition: normalized,
            root_node_ids,
            topological_ranks: ranks,
        })
    } else {
        Err(errors)
    }
}

/// A definition that passed only local structural and semantic validation.
///
/// This value is deliberately not publishable: publication must still resolve
/// every schema document, literal artifact, and action registry pin. Contract
/// sections 5.2, 8.3, 13.1, 13.2, and 14.3.
#[derive(Clone, Debug, PartialEq)]
pub struct UnresolvedDefinition {
    /// Normalized typed definition.
    pub definition: WorkflowDefinition,
    /// Lexically ordered nodes with no incoming output reference.
    pub root_node_ids: Vec<Id>,
    /// Canonical Kahn ranks keyed by node ID.
    pub topological_ranks: BTreeMap<Id, TopologicalRank>,
}

/// A schema document supplied by publication's external resolution boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct PublicationSchemaDocument {
    /// Digest under which the document was durably resolved.
    pub digest: Digest,
    /// The supported-subset schema JSON.
    pub value: Value,
}

/// External facts required to turn a locally valid definition into a revision.
///
/// Store implementations provide durable resolution; tests and authoring tools may use an
/// in-memory resolver. No external lookup is performed during stage 1.
pub trait PublicationResolver {
    /// Resolves a durable supported-subset schema document by its exact digest.
    fn schema_document(&self, digest: &Digest) -> Option<PublicationSchemaDocument>;
    /// Confirms a literal ArtifactRef exists with its exact identity and metadata.
    fn artifact_exists(&self, artifact_ref_id: &Id, digest: &Digest, media_type: &str) -> bool;
    /// Confirms one complete five-field action pin is available in the registry.
    fn action_pin_available(&self, pin: &ExtractedActionPin) -> bool;
}

/// A definition that is safe to hand to `publish_revision`.
#[derive(Clone, Debug, PartialEq)]
pub struct PublishableDefinition {
    /// Locally normalized definition and canonical ranks.
    pub definition: WorkflowDefinition,
    /// Canonical Kahn ranks keyed by node ID.
    pub topological_ranks: BTreeMap<Id, TopologicalRank>,
}

/// Resolves publication-only dependencies and returns a publishable revision.
///
/// This is intentionally a second stage. A successful `validate_definition`
/// result alone does not certify publishability.
pub fn resolve_publication(
    unresolved: UnresolvedDefinition,
    resolver: &dyn PublicationResolver,
) -> Result<PublishableDefinition, Vec<DefinitionValidationError>> {
    let mut errors = Vec::new();
    let mut schemas = BTreeMap::new();
    require_publication_schema(
        resolver,
        &unresolved.definition.run_input_schema_digest,
        "/run_input_schema_digest".to_owned(),
        &mut schemas,
        &mut errors,
    );
    require_publication_schema(
        resolver,
        &unresolved.definition.run_output_schema_digest,
        "/run_output_schema_digest".to_owned(),
        &mut schemas,
        &mut errors,
    );
    for pin in extract_action_pins(&unresolved.definition) {
        require_publication_schema(
            resolver,
            &pin.input_schema_digest,
            format!(
                "/nodes/{}/action/input_schema_digest",
                pin.reference_location
            ),
            &mut schemas,
            &mut errors,
        );
        require_publication_schema(
            resolver,
            &pin.output_schema_digest,
            format!(
                "/nodes/{}/action/output_schema_digest",
                pin.reference_location
            ),
            &mut schemas,
            &mut errors,
        );
        if !resolver.action_pin_available(&pin) {
            errors.push(error(
                ValidationErrorKind::SchemaPinUnresolved,
                format!("/nodes/{}/action", pin.reference_location),
                "the exact five-field action pin is unavailable in the registry",
                &["register a matching action implementation"],
            ));
        }
    }
    for node in &unresolved.definition.nodes {
        validate_publication_artifacts(node, resolver, &mut errors);
    }
    if errors.is_empty() {
        validate_publication_bindings(&unresolved.definition, &schemas, &mut errors);
    }
    if errors.is_empty() {
        Ok(PublishableDefinition {
            definition: unresolved.definition,
            topological_ranks: unresolved.topological_ranks,
        })
    } else {
        Err(errors)
    }
}

fn require_publication_schema(
    resolver: &dyn PublicationResolver,
    digest: &Digest,
    path: String,
    schemas: &mut BTreeMap<Digest, Value>,
    errors: &mut Vec<DefinitionValidationError>,
) {
    match resolver.schema_document(digest) {
        Some(document) if document.digest == *digest => {
            let canonical = serde_jcs::to_vec(&document.value).ok();
            if canonical.as_deref().map(revision_hash) != Some(digest.clone()) {
                errors.push(error(
                    ValidationErrorKind::SchemaPinUnresolved,
                    path,
                    "schema document bytes do not match its pinned digest",
                    &["store the exact supported-subset schema document"],
                ));
            } else {
                schemas.insert(digest.clone(), document.value);
            }
        }
        _ => errors.push(error(
            ValidationErrorKind::SchemaPinUnresolved,
            path,
            "pinned schema document is unavailable",
            &["register the exact schema document"],
        )),
    }
}

fn validate_publication_source(
    id: &Id,
    source: &BindingSource,
    resolver: &dyn PublicationResolver,
    errors: &mut Vec<DefinitionValidationError>,
) {
    match source {
        BindingSource::ArtifactRef {
            source:
                ArtifactLocator::Literal {
                    artifact_ref_id,
                    digest,
                    media_type,
                },
        } if !resolver.artifact_exists(artifact_ref_id, digest, media_type) => {
            errors.push(error(
                ValidationErrorKind::ArtifactLocatorInvalid,
                format!("/nodes/{}/source", id.as_str()),
                "literal ArtifactRef does not resolve under the publication scope",
                &["register the exact ArtifactRef"],
            ));
        }
        BindingSource::Object { fields } => {
            for field in fields.values() {
                validate_publication_source(id, field, resolver, errors);
            }
        }
        BindingSource::Array { items } => {
            for item in items {
                validate_publication_source(id, item, resolver, errors);
            }
        }
        _ => {}
    }
}

fn validate_publication_artifacts(
    node: &NodeDefinition,
    resolver: &dyn PublicationResolver,
    errors: &mut Vec<DefinitionValidationError>,
) {
    let id = node_id(node);
    match node {
        NodeDefinition::Action { bindings, .. } => bindings
            .iter()
            .for_each(|binding| validate_publication_source(id, &binding.source, resolver, errors)),
        NodeDefinition::Map {
            items, bindings, ..
        } => {
            validate_publication_source(id, items, resolver, errors);
            for binding in bindings {
                if let MapBindingSource::Object { fields } = &binding.source {
                    for field in fields.values() {
                        validate_publication_source(id, field, resolver, errors);
                    }
                }
                if let MapBindingSource::Array { items } = &binding.source {
                    for item in items {
                        validate_publication_source(id, item, resolver, errors);
                    }
                }
                if let MapBindingSource::ArtifactRef {
                    source:
                        ArtifactLocator::Literal {
                            artifact_ref_id,
                            digest,
                            media_type,
                        },
                } = &binding.source
                {
                    if !resolver.artifact_exists(artifact_ref_id, digest, media_type) {
                        errors.push(error(
                            ValidationErrorKind::ArtifactLocatorInvalid,
                            format!("/nodes/{}/bindings", id.as_str()),
                            "literal ArtifactRef does not resolve under the publication scope",
                            &["register the exact ArtifactRef"],
                        ));
                    }
                }
            }
        }
        NodeDefinition::Choice { input, .. }
        | NodeDefinition::Approval { request: input, .. }
        | NodeDefinition::Succeed { output: input, .. } => {
            validate_publication_source(id, input, resolver, errors)
        }
        NodeDefinition::Fail { .. } => {}
    }
}

fn validate_publication_bindings(
    definition: &WorkflowDefinition,
    schemas: &BTreeMap<Digest, Value>,
    errors: &mut Vec<DefinitionValidationError>,
) {
    for node in &definition.nodes {
        let (action, bindings): (
            Option<&ActionReference>,
            Vec<(&str, Option<&Value>, Option<&BindingSource>)>,
        ) = match node {
            NodeDefinition::Action {
                action, bindings, ..
            } => (
                Some(action),
                bindings
                    .iter()
                    .map(|binding| match &binding.source {
                        BindingSource::Constant { value } => {
                            (binding.target.as_str(), Some(value), None)
                        }
                        source => (binding.target.as_str(), None, Some(source)),
                    })
                    .collect(),
            ),
            NodeDefinition::Map {
                action, bindings, ..
            } => (
                Some(action),
                bindings
                    .iter()
                    .map(|binding| match &binding.source {
                        MapBindingSource::Constant { value } => {
                            (binding.target.as_str(), Some(value), None)
                        }
                        _ => (binding.target.as_str(), None, None),
                    })
                    .collect(),
            ),
            _ => (None, Vec::new()),
        };
        let Some(action) = action else { continue };
        let Some(input_schema) = schemas.get(&action.input_schema_digest) else {
            continue;
        };
        let required = input_schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        for leaf in required {
            let target = format!("/{}", leaf.replace('~', "~0").replace('/', "~1"));
            if !bindings.iter().any(|(bound, _, _)| {
                *bound == target
                    || bound
                        .strip_prefix(&target)
                        .is_some_and(|rest| rest.starts_with('/'))
            }) {
                errors.push(error(
                    ValidationErrorKind::BindingTargetInvalid,
                    format!("/nodes/{}/bindings", node_id(node).as_str()),
                    format!("required input leaf `{leaf}` has no binding"),
                    &["bind every required leaf"],
                ));
            }
        }
        for (target, constant, source) in bindings {
            let target_schema = match schema_at_binding_target(input_schema, target) {
                Ok(Some(schema)) => schema,
                Ok(None) => continue,
                Err(ancestor) => {
                    errors.push(error(
                        ValidationErrorKind::BindingTargetInvalid,
                        format!("/nodes/{}/bindings", node_id(node).as_str()),
                        format!(
                            "node `{}` binding target `{target}` cannot be resolved; nearest resolvable ancestor is `{}`",
                            node_id(node).as_str(),
                            if ancestor.is_empty() { "<root>" } else { &ancestor }
                        ),
                        &["bind a target present in the action input schema"],
                    ));
                    continue;
                }
            };
            if let Some(value) = constant {
                if !json_value_matches_schema(value, target_schema) {
                    errors.push(error(
                        ValidationErrorKind::BindingTypeMismatch,
                        format!("/nodes/{}/bindings", node_id(node).as_str()),
                        format!(
                            "constant bound at `{target}` is not assignable to the target schema"
                        ),
                        &["use a value accepted by the target schema"],
                    ));
                }
            } else if let Some(source) = source {
                match binding_source_schema(definition, schemas, source) {
                    Some(source_schema)
                        if schemas_are_assignable(&source_schema, target_schema) => {}
                    _ => errors.push(error(
                        ValidationErrorKind::BindingTypeMismatch,
                        format!("/nodes/{}/bindings", node_id(node).as_str()),
                        format!(
                            "source bound at `{target}` is not assignable to the target schema"
                        ),
                        &["use a schema-compatible source"],
                    )),
                }
            }
        }
    }
}

fn binding_source_schema(
    definition: &WorkflowDefinition,
    schemas: &BTreeMap<Digest, Value>,
    source: &BindingSource,
) -> Option<Value> {
    match source {
        BindingSource::RunInput { pointer } => {
            schema_at_pointer(schemas.get(&definition.run_input_schema_digest)?, pointer).cloned()
        }
        BindingSource::NodeOutput {
            node_id: source_node_id,
            pointer,
        } => {
            let source_node = definition
                .nodes
                .iter()
                .find(|candidate| node_id(candidate) == source_node_id)?;
            let source_schema = match source_node {
                NodeDefinition::Action { action, .. } => {
                    schemas.get(&action.output_schema_digest)?.clone()
                }
                // A Map node's output is the ordered aggregate of its children
                // (), not one child output. The pinned
                // action schema describes a single child, so the schema a
                // downstream binding must satisfy is that schema wrapped in an
                // array. Resolving to the bare child schema made every
                // Map-to-Action binding unsatisfiable: a correctly typed array
                // target failed as BindingTypeMismatch, and dropping `type` from
                // the target to dodge the check was then rejected by the schema
                // subset validator, which requires `type` on every node.
                NodeDefinition::Map { action, .. } => {
                    let mut aggregate = serde_json::Map::new();
                    aggregate.insert("type".to_string(), Value::String("array".to_string()));
                    aggregate.insert(
                        "items".to_string(),
                        schemas.get(&action.output_schema_digest)?.clone(),
                    );
                    Value::Object(aggregate)
                }
                _ => return None,
            };
            schema_at_pointer(&source_schema, pointer).cloned()
        }
        BindingSource::MapAggregate {
            node_id: source_node_id,
            pointer,
        } => {
            let NodeDefinition::Map { action, .. } = definition
                .nodes
                .iter()
                .find(|candidate| node_id(candidate) == source_node_id)?
            else {
                return None;
            };
            let mut aggregate = serde_json::Map::new();
            aggregate.insert("type".to_string(), Value::String("array".to_string()));
            aggregate.insert(
                "items".to_string(),
                schema_at_pointer(schemas.get(&action.output_schema_digest)?, pointer)?.clone(),
            );
            Some(Value::Object(aggregate))
        }
        BindingSource::Object { fields } => {
            let mut properties = serde_json::Map::new();
            for (name, source) in fields {
                properties.insert(
                    name.clone(),
                    binding_source_schema(definition, schemas, source)
                        .unwrap_or_else(|| serde_json::json!({})),
                );
            }
            let mut object = serde_json::Map::new();
            object.insert("type".to_string(), Value::String("object".to_string()));
            object.insert("properties".to_string(), Value::Object(properties));
            Some(Value::Object(object))
        }
        BindingSource::Array { items } => {
            let item_schemas = items
                .iter()
                .map(|item| binding_source_schema(definition, schemas, item))
                .collect::<Option<Vec<_>>>()?;
            let item_schema = item_schemas
                .first()
                .filter(|first| item_schemas.iter().all(|item| item == *first))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            Some(serde_json::json!({"type":"array","items":item_schema}))
        }
        BindingSource::Constant { .. } | BindingSource::ArtifactRef { .. } => None,
    }
}

fn schemas_are_assignable(source: &Value, target: &Value) -> bool {
    match (
        source.get("type").and_then(Value::as_str),
        target.get("type").and_then(Value::as_str),
    ) {
        (_, None) | (None, _) => true,
        (Some("integer"), Some("number")) => true,
        (Some(source), Some(target)) => source == target,
    }
}

fn schema_at_pointer<'a>(schema: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer.is_empty() {
        return Some(schema);
    }
    pointer
        .strip_prefix('/')?
        .split('/')
        .try_fold(schema, |current, segment| {
            let segment = segment.replace("~1", "/").replace("~0", "~");
            current
                .get("properties")?
                .get(&segment)
                .or_else(|| current.get("items"))
        })
}

fn schema_at_binding_target<'a>(
    schema: &'a Value,
    pointer: &str,
) -> Result<Option<&'a Value>, String> {
    let mut current = schema;
    let mut ancestor = String::new();
    for segment in pointer.strip_prefix('/').unwrap_or(pointer).split('/') {
        if current.as_object().is_some_and(serde_json::Map::is_empty) {
            return Ok(None);
        }
        let decoded = segment.replace("~1", "/").replace("~0", "~");
        let Some(next) = current
            .get("properties")
            .and_then(|properties| properties.get(&decoded))
            .or_else(|| current.get("items"))
        else {
            return Err(ancestor);
        };
        current = next;
        ancestor.push('/');
        ancestor.push_str(segment);
    }
    if current.as_object().is_some_and(serde_json::Map::is_empty) {
        Ok(None)
    } else {
        Ok(Some(current))
    }
}

fn json_value_matches_schema(value: &Value, schema: &Value) -> bool {
    match schema.get("type").and_then(Value::as_str) {
        None => true,
        Some("null") => value.is_null(),
        Some("boolean") => value.is_boolean(),
        Some("string") => value.is_string(),
        Some("integer") => value.as_i64().is_some() || value.as_u64().is_some(),
        Some("number") => value.is_number(),
        Some("array") => value.is_array(),
        Some("object") => value.is_object(),
        _ => false,
    }
}

/// Expands schema defaults into a normalized typed definition.
pub fn normalize_definition(
    mut definition: WorkflowDefinition,
) -> Result<WorkflowDefinition, DefinitionValidationError> {
    if definition.definition_format_version != "0.1" {
        return Err(error(
            ValidationErrorKind::InvalidField,
            "/definition_format_version",
            "definition_format_version must be exactly `0.1`",
            &["0.1"],
        ));
    }
    if definition.description.len() > 4000 {
        return Err(error(
            ValidationErrorKind::InvalidField,
            "/description",
            "description is limited to 4000 UTF-8 bytes",
            &["shorten description"],
        ));
    }
    // Serde applies both format defaults; assigning explicitly documents the invariant.
    if definition.description.is_empty() {
        definition.description = String::new();
    }
    for node in &mut definition.nodes {
        if let NodeDefinition::Approval { gate, .. } = node {
            if matches!(gate.on_expiry, ApprovalExpiryPolicy::Reject) {
                gate.on_expiry = ApprovalExpiryPolicy::Reject;
            }
        }
    }
    Ok(definition)
}

/// Produces RFC 8785 canonical definition bytes.
pub fn canonical_definition_json(
    definition: &WorkflowDefinition,
) -> Result<Vec<u8>, DefinitionValidationError> {
    let normalized = normalize_definition(definition.clone())?;
    let bytes =
        serde_jcs::to_vec(&normalized).map_err(|error| parse_error("$", error.to_string()))?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(error(
            ValidationErrorKind::DefinitionTooLarge,
            "$",
            "canonical definition exceeds the 4 MiB limit",
            &["reduce definition size"],
        ));
    }
    Ok(bytes)
}

/// Computes the revision digest from canonical definition bytes.
pub fn revision_hash(canonical_definition: &[u8]) -> Digest {
    Digest::new(format!(
        "sha256:{}",
        hex(&Sha256::digest(canonical_definition))
    ))
    .expect("SHA-256 output is valid")
}

/// Computes lexical Kahn topological ranks.
pub fn canonical_topological_ranks(
    definition: &WorkflowDefinition,
) -> Result<BTreeMap<Id, TopologicalRank>, DefinitionValidationError> {
    ranks_from_edges(definition, &reference_edges(definition))
}

/// Returns the lexically ordered nodes with no incoming output reference.
pub fn canonical_root_node_ids(definition: &WorkflowDefinition) -> Vec<Id> {
    let edges = reference_edges(definition);
    roots_from_edges(definition, &edges)
}

pub(crate) fn reference_outgoing(
    definition: &WorkflowDefinition,
) -> BTreeMap<Id, Vec<(String, Id)>> {
    reference_edges(definition)
}

mod yaml {
    use super::*;
    /// The only parser-specific seam; no parser values or errors escape it.
    pub(super) fn parse(input: &str) -> Result<WorkflowDefinition, DefinitionValidationError> {
        if input.len() > 4 * 1024 * 1024 {
            return Err(error(
                ValidationErrorKind::DefinitionTooLarge,
                "$",
                "YAML input exceeds the 4 MiB definition limit",
                &["reduce definition size"],
            ));
        }
        let options = serde_saphyr::options! {
            budget: serde_saphyr::budget! { max_documents: 1, max_events: 100_000, max_nodes: 20_000, max_depth: 64, max_aliases: 128, max_anchors: 128, max_total_scalar_bytes: 4 * 1024 * 1024 },
            duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
            merge_keys: serde_saphyr::MergeKeyPolicy::Error,
            strict_booleans: true,
            alias_limits: serde_saphyr::alias_limits! { max_total_replayed_events: 20_000, max_replay_stack_depth: 32, max_alias_expansions_per_anchor: 32 },
        };
        serde_saphyr::from_str_with_options(input, options)
            .map_err(|error| parse_error("$", error.to_string()))
    }
}

fn error(
    kind: ValidationErrorKind,
    path: impl Into<String>,
    message: impl Into<String>,
    alternatives: &[&str],
) -> DefinitionValidationError {
    DefinitionValidationError {
        kind,
        path: path.into(),
        message: message.into(),
        valid_alternatives: alternatives
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
    }
}
fn parse_error(path: impl Into<String>, message: String) -> DefinitionValidationError {
    let kind = if message.contains("unknown field") {
        ValidationErrorKind::UnknownField
    } else {
        ValidationErrorKind::InvalidField
    };
    error(
        kind,
        path,
        format!("invalid definition syntax: {message}"),
        &["use the closed definition schema"],
    )
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn node_id(node: &NodeDefinition) -> &Id {
    match node {
        NodeDefinition::Action { id, .. }
        | NodeDefinition::Map { id, .. }
        | NodeDefinition::Choice { id, .. }
        | NodeDefinition::Approval { id, .. }
        | NodeDefinition::Succeed { id, .. }
        | NodeDefinition::Fail { id, .. } => id,
    }
}
fn outgoing(node: &NodeDefinition) -> Vec<(String, Id)> {
    match node {
        NodeDefinition::Action { next, .. }
        | NodeDefinition::Map { next, .. }
        | NodeDefinition::Approval { next, .. } => next
            .iter()
            .enumerate()
            .map(|(index, target)| (format!("next/{index}"), target.clone()))
            .collect(),
        NodeDefinition::Choice { cases, default, .. } => cases
            .iter()
            .enumerate()
            .map(|(index, case)| {
                (
                    format!("case/{index}"),
                    choice_target(case_target(case)).clone(),
                )
            })
            .chain(std::iter::once((
                "default".to_owned(),
                choice_target(default).clone(),
            )))
            .collect(),
        NodeDefinition::Succeed { .. } | NodeDefinition::Fail { .. } => Vec::new(),
    }
}
fn case_target(case: &ChoiceCase) -> &ChoiceTarget {
    match case {
        ChoiceCase::Equals { target, .. } | ChoiceCase::In { target, .. } => target,
    }
}

fn choice_target(target: &ChoiceTarget) -> &Id {
    match target {
        ChoiceTarget::Node { next } | ChoiceTarget::Skip { next } => next,
    }
}

fn validate_root(definition: &WorkflowDefinition, errors: &mut Vec<DefinitionValidationError>) {
    if !valid_id(&definition.definition_id.0) {
        errors.push(error(
            ValidationErrorKind::InvalidField,
            "/definition_id",
            "definition_id must be 1–128 bytes and match the ID grammar",
            &["use letters/digits followed by . _ : or -"],
        ));
    }
    if definition.name.is_empty() || definition.name.len() > 200 {
        errors.push(error(
            ValidationErrorKind::InvalidField,
            "/name",
            "name must be 1–200 UTF-8 bytes",
            &["provide a non-empty shorter name"],
        ));
    }
    if definition.nodes.is_empty() || definition.nodes.len() > 1024 {
        errors.push(error(
            ValidationErrorKind::InvalidField,
            "/nodes",
            "nodes must contain 1–1024 entries",
            &["add nodes", "reduce node count"],
        ));
    }
    for (path, digest) in [
        (
            "/run_input_schema_digest",
            &definition.run_input_schema_digest,
        ),
        (
            "/run_output_schema_digest",
            &definition.run_output_schema_digest,
        ),
    ] {
        if !valid_digest(&digest.0) {
            errors.push(error(
                ValidationErrorKind::InvalidField,
                path,
                "digest must be sha256: followed by 64 lowercase hex characters",
                &["use a SHA-256 digest"],
            ));
        }
    }
}

fn validate_node(
    node: &NodeDefinition,
    ids: &BTreeMap<Id, usize>,
    errors: &mut Vec<DefinitionValidationError>,
) {
    let id = node_id(node);
    if !valid_id(&id.0) {
        errors.push(error(
            ValidationErrorKind::InvalidField,
            format!("/nodes/{}/id", id.0),
            "node id must be 1–128 bytes and match the ID grammar",
            &["use letters/digits followed by . _ : or -"],
        ));
    }
    match node {
        NodeDefinition::Action {
            action,
            bindings,
            retry,
            timeout,
            declared_max_cost_units,
            next,
            ..
        } => {
            validate_action(
                id,
                action,
                retry,
                timeout,
                *declared_max_cost_units,
                next,
                errors,
            );
            validate_binding_targets(id, bindings.iter().map(|binding| &binding.target), errors);
            for binding in bindings {
                validate_source(id, &binding.source, false, errors);
            }
        }
        NodeDefinition::Map {
            items,
            max_items,
            max_concurrency,
            action,
            bindings,
            retry,
            timeout,
            declared_max_cost_units,
            next,
            ..
        } => {
            validate_source(id, items, false, errors);
            if *max_items == 0
                || *max_items > 10_000
                || *max_concurrency == 0
                || *max_concurrency > 1024
                || max_concurrency > max_items
            {
                errors.push(error(ValidationErrorKind::MapBoundsInvalid, format!("/nodes/{}/max_concurrency", id.0), "Map requires 1 <= max_concurrency <= max_items <= 10000 and max_concurrency <= 1024", &["set positive bounded max_items and max_concurrency"]));
            }
            validate_action(
                id,
                action,
                retry,
                timeout,
                *declared_max_cost_units,
                next,
                errors,
            );
            validate_binding_targets(id, bindings.iter().map(|binding| &binding.target), errors);
            for binding in bindings {
                validate_map_source(id, &binding.source, errors);
            }
        }
        NodeDefinition::Choice {
            input,
            selector,
            cases,
            default,
            ..
        } => {
            validate_source(id, input, false, errors);
            if !valid_source_pointer(selector) {
                errors.push(error(
                    ValidationErrorKind::JsonPointerInvalid,
                    format!("/nodes/{}/selector", id.0),
                    "selector must be an RFC 6901 source pointer",
                    &["use /property or empty pointer"],
                ));
            }
            if cases.is_empty() || cases.len() > 100 {
                errors.push(error(
                    ValidationErrorKind::ChoiceCaseInvalid,
                    format!("/nodes/{}/cases", id.0),
                    "Choice requires 1–100 ordered cases",
                    &["add a case"],
                ));
            }
            if !ids.contains_key(choice_target(default)) {
                errors.push(error(
                    ValidationErrorKind::MissingNode,
                    format!("/nodes/{}/default", id.0),
                    "Choice default must target an existing node",
                    &["use an existing node id"],
                ));
            }
            let mut targets = BTreeSet::new();
            targets.insert((
                matches!(default, ChoiceTarget::Node { .. }),
                choice_target(default).clone(),
            ));
            let mut values = BTreeSet::new();
            for case in cases {
                let outcome = case_target(case);
                let target = choice_target(outcome);
                if !targets.insert((matches!(outcome, ChoiceTarget::Node { .. }), target.clone())) {
                    errors.push(error(
                        ValidationErrorKind::ChoiceCaseInvalid,
                        format!("/nodes/{}/cases", id.0),
                        "Choice case and default targets must be unique",
                        &["use distinct targets"],
                    ));
                }
                if !ids.contains_key(target) {
                    errors.push(error(
                        ValidationErrorKind::MissingNode,
                        format!("/nodes/{}/cases", id.0),
                        "Choice case must target an existing node",
                        &["use an existing node id or skip"],
                    ));
                }
                let candidates: Vec<&Value> = match case {
                    ChoiceCase::Equals { equals, .. } => vec![equals],
                    ChoiceCase::In { r#in, .. } => r#in.iter().collect(),
                };
                if candidates.is_empty()
                    || candidates.len() > 100
                    || candidates.iter().any(|value| !is_scalar(value))
                {
                    errors.push(error(
                        ValidationErrorKind::ChoiceCaseInvalid,
                        format!("/nodes/{}/cases", id.0),
                        "Choice values must be non-empty unique JSON scalars",
                        &["use string, number, boolean, or null"],
                    ));
                }
                for candidate in candidates {
                    let encoded = serde_jcs::to_string(candidate).unwrap_or_default();
                    if !values.insert(encoded) {
                        errors.push(error(
                            ValidationErrorKind::ChoiceCaseInvalid,
                            format!("/nodes/{}/cases", id.0),
                            "Choice case values must not overlap",
                            &["remove duplicate scalar values"],
                        ));
                    }
                }
            }
        }
        NodeDefinition::Approval {
            request,
            gate,
            next,
            ..
        } => {
            validate_source(id, request, false, errors);
            if gate.expires_after_ms == 0 || gate.expires_after_ms > 31_536_000_000 {
                errors.push(error(
                    ValidationErrorKind::InvalidField,
                    format!("/nodes/{}/gate/expires_after_ms", id.0),
                    "expires_after_ms must be 1..31536000000",
                    &["use a bounded positive duration"],
                ));
            }
            let principals_ok = gate
                .authorization
                .allowed_principal_ids
                .iter()
                .all(|value| !value.is_empty() && value.len() <= 256)
                && unique(&gate.authorization.allowed_principal_ids);
            let roles_ok = gate
                .authorization
                .allowed_role_ids
                .iter()
                .all(|value| !value.is_empty() && value.len() <= 256)
                && unique(&gate.authorization.allowed_role_ids);
            if (!principals_ok || !roles_ok)
                || (gate.authorization.allowed_principal_ids.is_empty()
                    && gate.authorization.allowed_role_ids.is_empty())
                || gate.authorization.allowed_principal_ids.len() > 256
                || gate.authorization.allowed_role_ids.len() > 256
            {
                errors.push(error(ValidationErrorKind::ApprovalAuthorizationInvalid, format!("/nodes/{}/gate/authorization", id.0), "approval authorization requires a non-empty unique principal or role allowlist", &["add allowed_principal_ids", "add allowed_role_ids"]));
            }
            validate_targets(id, next, errors);
        }
        NodeDefinition::Succeed { output, .. } => validate_source(id, output, false, errors),
        NodeDefinition::Fail { code, message, .. } => {
            if !valid_id(code) {
                errors.push(error(
                    ValidationErrorKind::InvalidField,
                    format!("/nodes/{}/code", id.0),
                    "Fail code must match the ID grammar",
                    &["use a namespaced safe code"],
                ));
            }
            if message.is_empty() || message.len() > 2000 {
                errors.push(error(
                    ValidationErrorKind::InvalidField,
                    format!("/nodes/{}/message", id.0),
                    "Fail message must be 1–2000 UTF-8 bytes",
                    &["provide a bounded message"],
                ));
            }
        }
    }
}

fn validate_action(
    id: &Id,
    action: &ActionReference,
    retry: &RetryPolicy,
    timeout: &TimeoutPolicy,
    _cost: CostUnits,
    next: &[Id],
    errors: &mut Vec<DefinitionValidationError>,
) {
    if action.name.is_empty()
        || action.name.len() > 200
        || action.contract_version.is_empty()
        || action.contract_version.len() > 64
        || [
            &action.input_schema_digest,
            &action.output_schema_digest,
            &action.compatible_implementation_requirement,
        ]
        .iter()
        .any(|digest| !valid_digest(&digest.0))
    {
        errors.push(error(
            ValidationErrorKind::InvalidField,
            format!("/nodes/{}/action", id.0),
            "action pin requires bounded name/version and three SHA-256 digests",
            &["supply all five action pin fields"],
        ));
    }
    if retry.max_attempts == 0 || retry.max_attempts > 100 {
        errors.push(error(
            ValidationErrorKind::RetryPolicyInvalid,
            format!("/nodes/{}/retry/max_attempts", id.0),
            "max_attempts must be 1..100",
            &["use a value between 1 and 100"],
        ));
    }
    match retry.backoff {
        BackoffPolicy::Fixed { delay_ms } if delay_ms <= 86_400_000 => {}
        BackoffPolicy::Exponential {
            initial_delay_ms,
            multiplier,
            max_delay_ms,
        } if initial_delay_ms > 0
            && initial_delay_ms <= 86_400_000
            && (2..=16).contains(&multiplier)
            && max_delay_ms >= initial_delay_ms
            && max_delay_ms <= 86_400_000 => {}
        _ => errors.push(error(
            ValidationErrorKind::RetryPolicyInvalid,
            format!("/nodes/{}/retry/backoff", id.0),
            "backoff exceeds caps or exponential max_delay_ms is below initial_delay_ms",
            &["use fixed 0..86400000", "use bounded exponential backoff"],
        )),
    }
    if timeout.timeout_ms == 0 || timeout.timeout_ms > 86_400_000 {
        errors.push(error(
            ValidationErrorKind::TimeoutPolicyInvalid,
            format!("/nodes/{}/timeout/timeout_ms", id.0),
            "timeout_ms must be 1..86400000",
            &["use a bounded positive timeout"],
        ));
    }
    validate_targets(id, next, errors);
}
fn validate_targets(id: &Id, targets: &[Id], errors: &mut Vec<DefinitionValidationError>) {
    if targets.is_empty() || targets.len() > 64 || !unique(targets) {
        errors.push(error(
            ValidationErrorKind::InvalidField,
            format!("/nodes/{}/next", id.0),
            "next must contain 1–64 unique targets",
            &["use unique existing target IDs"],
        ));
    }
}
fn validate_binding_targets<'a>(
    id: &Id,
    targets: impl Iterator<Item = &'a String>,
    errors: &mut Vec<DefinitionValidationError>,
) {
    let mut values: Vec<String> = targets.cloned().collect();
    values.sort();
    for target in &values {
        if !valid_target_pointer(target) {
            errors.push(error(
                ValidationErrorKind::JsonPointerInvalid,
                format!("/nodes/{}/bindings", id.0),
                "binding target must be a non-empty RFC 6901 pointer",
                &["use /field"],
            ));
        }
    }
    for pair in values.windows(2) {
        if pair[0] == pair[1] || pair[1].starts_with(&(pair[0].clone() + "/")) {
            errors.push(error(
                ValidationErrorKind::BindingTargetInvalid,
                format!("/nodes/{}/bindings", id.0),
                format!("binding targets `{}` and `{}` overlap", pair[0], pair[1]),
                &["bind only one leaf per target"],
            ));
        }
    }
}
fn validate_source(
    id: &Id,
    source: &BindingSource,
    map_allowed: bool,
    errors: &mut Vec<DefinitionValidationError>,
) {
    match source {
        BindingSource::RunInput { pointer }
        | BindingSource::NodeOutput { pointer, .. }
        | BindingSource::MapAggregate { pointer, .. } => validate_pointer(id, pointer, errors),
        BindingSource::Object { fields } => {
            if fields.is_empty()
                || fields.len() > 64
                || fields.keys().any(|name| name.is_empty() || name.len() > 200)
            {
                errors.push(error(
                    ValidationErrorKind::InvalidField,
                    format!("/nodes/{}/source/fields", id.0),
                    "object source requires 1–64 non-empty field names of at most 200 bytes",
                    &["use a small named field map"],
                ));
            }
            for field in fields.values() {
                validate_source(id, field, map_allowed, errors);
            }
        }
        BindingSource::Array { items } => {
            if items.len() > 1_024 {
                errors.push(error(
                    ValidationErrorKind::InvalidField,
                    format!("/nodes/{}/source/items", id.0),
                    "array source permits at most 1,024 items",
                    &["split the value before binding"],
                ));
            }
            for item in items {
                validate_source(id, item, map_allowed, errors);
            }
        }
        BindingSource::ArtifactRef { source } => match source {
            ArtifactLocator::Literal {
                artifact_ref_id,
                digest,
                media_type,
            } if valid_id(&artifact_ref_id.0)
                && valid_digest(&digest.0)
                && !media_type.is_empty()
                && media_type.len() <= 200 => {}
            ArtifactLocator::RunInput { pointer }
            | ArtifactLocator::NodeOutput { pointer, .. }
                if valid_source_pointer(pointer) => {}
            _ => errors.push(error(
                ValidationErrorKind::ArtifactLocatorInvalid,
                format!("/nodes/{}/source", id.0),
                "artifact locator requires an exact identity/digest/media tuple or valid source pointer",
                &["use a complete literal locator"],
            )),
        },
        BindingSource::Constant { .. } => {}
    }
    let _ = map_allowed;
}
fn validate_map_source(
    id: &Id,
    source: &MapBindingSource,
    errors: &mut Vec<DefinitionValidationError>,
) {
    match source {
        MapBindingSource::Constant { .. } | MapBindingSource::MapIndex => {}
        MapBindingSource::RunInput { pointer }
        | MapBindingSource::NodeOutput { pointer, .. }
        | MapBindingSource::MapAggregate { pointer, .. }
        | MapBindingSource::MapItem { pointer } => validate_pointer(id, pointer, errors),
        MapBindingSource::Object { fields } => validate_source(
            id,
            &BindingSource::Object {
                fields: fields.clone(),
            },
            true,
            errors,
        ),
        MapBindingSource::Array { items } => validate_source(
            id,
            &BindingSource::Array {
                items: items.clone(),
            },
            true,
            errors,
        ),
        MapBindingSource::ArtifactRef { source } => validate_source(
            id,
            &BindingSource::ArtifactRef {
                source: source.clone(),
            },
            true,
            errors,
        ),
    }
}
fn validate_pointer(id: &Id, pointer: &str, errors: &mut Vec<DefinitionValidationError>) {
    if !valid_source_pointer(pointer) {
        errors.push(error(
            ValidationErrorKind::JsonPointerInvalid,
            format!("/nodes/{}/source/pointer", id.0),
            "source pointer must be RFC 6901",
            &["use /property or empty pointer"],
        ));
    }
}
fn validate_bindings(definition: &WorkflowDefinition, errors: &mut Vec<DefinitionValidationError>) {
    for node in &definition.nodes {
        let consumer = node_id(node);
        let ordinary_sources = match node {
            NodeDefinition::Action { bindings, .. } => bindings
                .iter()
                .map(|binding| &binding.source)
                .collect::<Vec<_>>(),
            NodeDefinition::Map { items, .. }
            | NodeDefinition::Choice { input: items, .. }
            | NodeDefinition::Approval { request: items, .. }
            | NodeDefinition::Succeed { output: items, .. } => vec![items],
            NodeDefinition::Fail { .. } => Vec::new(),
        };
        for source in ordinary_sources {
            validate_map_aggregate_source(definition, consumer, source, errors);
        }
        if let NodeDefinition::Map { bindings, .. } = node {
            for binding in bindings {
                validate_map_binding_aggregate_source(
                    definition,
                    consumer,
                    &binding.source,
                    errors,
                );
            }
        }
        let sources: Vec<&Id> = match node {
            NodeDefinition::Action { bindings, .. } => bindings
                .iter()
                .flat_map(|binding| source_nodes(&binding.source))
                .collect(),
            NodeDefinition::Map {
                items, bindings, ..
            } => source_nodes(items)
                .into_iter()
                .chain(
                    bindings
                        .iter()
                        .flat_map(|binding| map_source_nodes(&binding.source)),
                )
                .collect(),
            NodeDefinition::Choice { input, .. }
            | NodeDefinition::Approval { request: input, .. }
            | NodeDefinition::Succeed { output: input, .. } => source_nodes(input),
            NodeDefinition::Fail { .. } => vec![],
        };
        let _ = sources;
    }
}

fn validate_map_aggregate_source(
    definition: &WorkflowDefinition,
    consumer: &Id,
    source: &BindingSource,
    errors: &mut Vec<DefinitionValidationError>,
) {
    match source {
        BindingSource::MapAggregate {
            node_id: map_node_id,
            ..
        } if !matches!(
            definition
                .nodes
                .iter()
                .find(|candidate| node_id(candidate) == map_node_id),
            Some(NodeDefinition::Map { .. })
        ) =>
        {
            errors.push(error(
                ValidationErrorKind::BindingSourceInvalid,
                format!("/nodes/{}/source", consumer.0),
                "map_aggregate must name an authored Map node",
                &["use a Map node id"],
            ));
        }
        BindingSource::Object { fields } => {
            for field in fields.values() {
                validate_map_aggregate_source(definition, consumer, field, errors);
            }
        }
        BindingSource::Array { items } => {
            for item in items {
                validate_map_aggregate_source(definition, consumer, item, errors);
            }
        }
        _ => {}
    }
}

fn validate_map_binding_aggregate_source(
    definition: &WorkflowDefinition,
    consumer: &Id,
    source: &MapBindingSource,
    errors: &mut Vec<DefinitionValidationError>,
) {
    match source {
        MapBindingSource::MapAggregate { node_id, pointer } => validate_map_aggregate_source(
            definition,
            consumer,
            &BindingSource::MapAggregate {
                node_id: node_id.clone(),
                pointer: pointer.clone(),
            },
            errors,
        ),
        MapBindingSource::Object { fields } => {
            for field in fields.values() {
                validate_map_aggregate_source(definition, consumer, field, errors);
            }
        }
        MapBindingSource::Array { items } => {
            for item in items {
                validate_map_aggregate_source(definition, consumer, item, errors);
            }
        }
        _ => {}
    }
}
fn map_source_nodes(source: &MapBindingSource) -> Vec<&Id> {
    match source {
        MapBindingSource::NodeOutput { node_id, .. }
        | MapBindingSource::MapAggregate { node_id, .. } => vec![node_id],
        MapBindingSource::Object { fields } => fields.values().flat_map(source_nodes).collect(),
        MapBindingSource::Array { items } => items.iter().flat_map(source_nodes).collect(),
        MapBindingSource::ArtifactRef {
            source: ArtifactLocator::NodeOutput { node_id, .. },
        } => vec![node_id],
        _ => Vec::new(),
    }
}
fn source_nodes(source: &BindingSource) -> Vec<&Id> {
    match source {
        BindingSource::NodeOutput { node_id, .. } | BindingSource::MapAggregate { node_id, .. } => {
            vec![node_id]
        }
        BindingSource::ArtifactRef {
            source: ArtifactLocator::NodeOutput { node_id, .. },
        } => vec![node_id],
        BindingSource::Object { fields } => fields.values().flat_map(source_nodes).collect(),
        BindingSource::Array { items } => items.iter().flat_map(source_nodes).collect(),
        _ => Vec::new(),
    }
}
fn reference_edges(definition: &WorkflowDefinition) -> BTreeMap<Id, Vec<(String, Id)>> {
    let mut edges = definition
        .nodes
        .iter()
        .map(|node| (node_id(node).clone(), Vec::new()))
        .collect::<BTreeMap<_, _>>();
    for node in &definition.nodes {
        let consumer = node_id(node);
        let sources: Vec<&Id> = match node {
            NodeDefinition::Action { bindings, .. } => bindings
                .iter()
                .flat_map(|binding| source_nodes(&binding.source))
                .collect(),
            NodeDefinition::Map {
                items, bindings, ..
            } => source_nodes(items)
                .into_iter()
                .chain(
                    bindings
                        .iter()
                        .flat_map(|binding| map_source_nodes(&binding.source)),
                )
                .collect(),
            NodeDefinition::Choice { input, .. }
            | NodeDefinition::Approval { request: input, .. }
            | NodeDefinition::Succeed { output: input, .. } => source_nodes(input),
            NodeDefinition::Fail { .. } => Vec::new(),
        };
        for (index, source) in sources.into_iter().enumerate() {
            if let Some(outgoing) = edges.get_mut(source) {
                outgoing.push((
                    format!("reference/{}/{index}", consumer.0),
                    consumer.clone(),
                ));
            }
        }
    }
    edges
}

fn merged_edges(
    control_edges: &BTreeMap<Id, Vec<(String, Id)>>,
    reference_edges: &BTreeMap<Id, Vec<(String, Id)>>,
) -> BTreeMap<Id, Vec<(String, Id)>> {
    let mut merged = control_edges.clone();
    for (source, targets) in reference_edges {
        merged
            .entry(source.clone())
            .or_default()
            .extend(targets.clone());
    }
    merged
}
fn roots_from_edges(
    definition: &WorkflowDefinition,
    edges: &BTreeMap<Id, Vec<(String, Id)>>,
) -> Vec<Id> {
    let mut incoming = BTreeSet::new();
    for targets in edges.values() {
        incoming.extend(targets.iter().map(|(_, target)| target.clone()));
    }
    for node in &definition.nodes {
        incoming.extend(outgoing(node).into_iter().map(|(_, target)| target));
    }
    definition
        .nodes
        .iter()
        .map(|node| node_id(node).clone())
        .filter(|id| !incoming.contains(id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn ranks_from_edges(
    definition: &WorkflowDefinition,
    edges: &BTreeMap<Id, Vec<(String, Id)>>,
) -> Result<BTreeMap<Id, TopologicalRank>, DefinitionValidationError> {
    let mut indegree: BTreeMap<Id, u32> = definition
        .nodes
        .iter()
        .map(|node| (node_id(node).clone(), 0))
        .collect();
    for targets in edges.values() {
        for (_, target) in targets {
            if let Some(value) = indegree.get_mut(target) {
                *value += 1;
            }
        }
    }
    let mut ready: BTreeSet<Id> = indegree
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(id, _)| id.clone())
        .collect();
    let mut ranks = BTreeMap::new();
    while let Some(id) = ready.pop_first() {
        let rank = ranks.len() as u32;
        ranks.insert(id.clone(), TopologicalRank(rank));
        let mut targets = edges.get(&id).cloned().unwrap_or_default();
        targets.sort_by(|left, right| left.0.cmp(&right.0));
        for (_, target) in targets {
            if let Some(count) = indegree.get_mut(&target) {
                *count -= 1;
                if *count == 0 {
                    ready.insert(target);
                }
            }
        }
    }
    if ranks.len() != definition.nodes.len() {
        Err(error(
            ValidationErrorKind::Cycle,
            "/nodes",
            "control graph must be acyclic",
            &["remove the cycle", "use an explicit bounded Map"],
        ))
    } else {
        Ok(ranks)
    }
}
fn reachable_from(entries: &[Id], edges: &BTreeMap<Id, Vec<(String, Id)>>) -> BTreeSet<Id> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from(entries.to_vec());
    while let Some(id) = queue.pop_front() {
        if seen.insert(id.clone()) {
            if let Some(targets) = edges.get(&id) {
                queue.extend(targets.iter().map(|(_, target)| target.clone()));
            }
        }
    }
    seen
}
fn graph_reaches(
    edges: &BTreeMap<Id, Vec<(String, Id)>>,
    start: &Id,
    target: &Id,
    blocked: Option<&Id>,
) -> bool {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([start.clone()]);
    while let Some(id) = queue.pop_front() {
        if blocked == Some(&id) {
            continue;
        }
        if id == *target {
            return true;
        }
        if seen.insert(id.clone()) {
            queue.extend(
                edges
                    .get(&id)
                    .into_iter()
                    .flatten()
                    .map(|(_, next)| next.clone()),
            );
        }
    }
    false
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}
fn valid_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_source_pointer(value: &str) -> bool {
    value.is_empty()
        || (value.starts_with('/') && value.split('/').skip(1).all(valid_pointer_token))
}
fn valid_target_pointer(value: &str) -> bool {
    !value.is_empty() && value.starts_with('/') && value.split('/').skip(1).all(valid_pointer_token)
}
fn valid_pointer_token(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'~' {
            if index + 1 >= bytes.len() || !matches!(bytes[index + 1], b'0' | b'1') {
                return false;
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    true
}
fn unique<T: Ord + Clone>(values: &[T]) -> bool {
    let mut seen = BTreeSet::new();
    values.iter().all(|value| seen.insert(value.clone()))
}
fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_))
}

struct NoDuplicateJson(Value);
impl<'de> Deserialize<'de> for NoDuplicateJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = Value;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a JSON value without duplicate object keys")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
                Ok(Value::Bool(value))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
                Ok(Value::Number(value.into()))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
                Ok(Value::Number(value.into()))
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
                serde_json::Number::from_f64(value)
                    .map(Value::Number)
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
                Ok(Value::String(value.to_owned()))
            }
            fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
                Ok(Value::String(value))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<Value, A::Error> {
                let mut result = Vec::new();
                while let Some(value) = values.next_element::<NoDuplicateJson>()? {
                    result.push(value.0);
                }
                Ok(Value::Array(result))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut values: A) -> Result<Value, A::Error> {
                let mut result = serde_json::Map::new();
                while let Some(key) = values.next_key::<String>()? {
                    let value = values.next_value::<NoDuplicateJson>()?.0;
                    if result.insert(key.clone(), value).is_some() {
                        return Err(de::Error::custom(format!("duplicate object key `{key}`")));
                    }
                }
                Ok(Value::Object(result))
            }
        }
        deserializer.deserialize_any(JsonVisitor).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id(value: &str) -> Id {
        Id::new(value).unwrap()
    }

    fn digest() -> Digest {
        Digest::new(format!("sha256:{}", "0".repeat(64))).unwrap()
    }

    fn action(id_value: &str, bindings: Vec<Binding>, next: Vec<&str>) -> NodeDefinition {
        NodeDefinition::Action {
            id: id(id_value),
            action: ActionReference {
                name: format!("test.{id_value}"),
                contract_version: "v1".to_owned(),
                input_schema_digest: digest(),
                output_schema_digest: digest(),
                compatible_implementation_requirement: digest(),
            },
            bindings,
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: BackoffPolicy::Fixed { delay_ms: 0 },
            },
            timeout: TimeoutPolicy { timeout_ms: 1 },
            declared_max_cost_units: CostUnits(0),
            next: next.into_iter().map(id).collect(),
        }
    }

    fn multiple_root_definition(order: [&str; 2]) -> WorkflowDefinition {
        let mut roots = order
            .into_iter()
            .map(|root| action(root, Vec::new(), vec!["join"]))
            .collect::<Vec<_>>();
        roots.push(action(
            "join",
            vec![
                Binding {
                    target: "/alfa".to_owned(),
                    source: BindingSource::NodeOutput {
                        node_id: id("alfa"),
                        pointer: String::new(),
                    },
                },
                Binding {
                    target: "/zulu".to_owned(),
                    source: BindingSource::NodeOutput {
                        node_id: id("zulu"),
                        pointer: String::new(),
                    },
                },
            ],
            vec!["done"],
        ));
        roots.push(NodeDefinition::Succeed {
            id: id("done"),
            output: BindingSource::NodeOutput {
                node_id: id("join"),
                pointer: String::new(),
            },
        });
        WorkflowDefinition {
            definition_format_version: "0.1".to_owned(),
            definition_id: id("multiple-roots"),
            name: "Multiple roots".to_owned(),
            description: String::new(),
            run_input_schema_digest: digest(),
            run_output_schema_digest: digest(),
            nodes: roots,
        }
    }

    #[test]
    fn multiple_root() {
        let definition = multiple_root_definition(["zulu", "alfa"]);
        let json = serde_json::to_string(&definition).unwrap();
        assert!(!json.contains("entry_node_id"));
        let parsed = parse_json_definition(&json).unwrap();
        let unresolved = validate_definition(&parsed).unwrap();
        assert_eq!(unresolved.root_node_ids, vec![id("alfa"), id("zulu")]);
        assert_eq!(unresolved.definition.nodes.len(), 4);
        assert!(!definition_json_schema().contains("entry_node_id"));
    }

    #[test]
    fn root_set_determinism() {
        let left = validate_definition(&multiple_root_definition(["alfa", "zulu"])).unwrap();
        let right = validate_definition(&multiple_root_definition(["zulu", "alfa"])).unwrap();
        assert_eq!(left.root_node_ids, right.root_node_ids);
        assert_eq!(left.topological_ranks, right.topological_ranks);
        assert!(left.topological_ranks[&id("alfa")] < left.topological_ranks[&id("zulu")]);
        assert!(left.topological_ranks[&id("zulu")] < left.topological_ranks[&id("join")]);
    }

    #[test]
    fn multiple_root_invalid() {
        let mut unknown = multiple_root_definition(["alfa", "zulu"]);
        if let NodeDefinition::Action { bindings, .. } = &mut unknown.nodes[2] {
            bindings[0].source = BindingSource::NodeOutput {
                node_id: id("missing"),
                pointer: String::new(),
            };
        }
        assert!(validate_definition(&unknown)
            .unwrap_err()
            .iter()
            .any(|error| error.kind == ValidationErrorKind::BindingSourceInvalid));

        let mut cycle = multiple_root_definition(["alfa", "zulu"]);
        if let NodeDefinition::Succeed { output, .. } = &mut cycle.nodes[3] {
            *output = BindingSource::NodeOutput {
                node_id: id("done"),
                pointer: String::new(),
            };
        }
        assert!(validate_definition(&cycle)
            .unwrap_err()
            .iter()
            .any(|error| error.kind == ValidationErrorKind::BindingSourceInvalid));

        let mut overlap = multiple_root_definition(["alfa", "zulu"]);
        if let NodeDefinition::Action { bindings, .. } = &mut overlap.nodes[2] {
            bindings[1].target = "/alfa/value".to_owned();
        }
        assert!(validate_definition(&overlap)
            .unwrap_err()
            .iter()
            .any(|error| error.kind == ValidationErrorKind::BindingTargetInvalid));

        let mut control_cycle = multiple_root_definition(["alfa", "zulu"]);
        if let NodeDefinition::Action { next, .. } = &mut control_cycle.nodes[2] {
            next.push(id("alfa"));
        }
        assert!(validate_definition(&control_cycle)
            .unwrap_err()
            .iter()
            .any(|error| error.kind == ValidationErrorKind::Cycle));

        let fake_root = serde_json::to_string(&multiple_root_definition(["alfa", "zulu"]))
            .unwrap()
            .replace("\"nodes\"", "\"entry_node_id\":\"alfa\",\"nodes\"");
        assert_eq!(
            parse_json_definition(&fake_root).unwrap_err().kind,
            ValidationErrorKind::UnknownField
        );
        assert_eq!(
            json!(canonical_root_node_ids(&multiple_root_definition([
                "alfa", "zulu"
            ]))),
            json!(["alfa", "zulu"])
        );
    }
}
