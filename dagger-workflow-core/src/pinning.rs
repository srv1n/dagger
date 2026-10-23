//! Immutable run options and explicit reuse of previously committed action outputs.

use crate::artifact::{ObjectReadError, ObjectStore};
use crate::definition::{NodeDefinition, ValidationErrorKind, WorkflowDefinition};
use crate::ids::{Id, NodeInstanceId};
use crate::memoization::{CacheMode, ReuseProvenance};
use crate::revision::WorkflowRevision;
use crate::run::{AttemptState, NodeAttempt, NodeKind, NodeRun, NodeState, WorkflowRun};
use crate::scope::ExecutionScope;
use crate::store::{StoreError, WorkflowStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// A node or stable Map item whose output the owner has locked.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PinTarget {
    /// One authored Action node.
    Node {
        /// Authored Action ID.
        node_id: Id,
    },
    /// One child selected by the Map's `item_key_pointer`.
    MapItem {
        /// Authored Map ID.
        map_node_id: Id,
        /// Nonempty, unique string key in the Map input.
        item_key: String,
    },
}

/// A lock points at a successful committed attempt, never caller-supplied bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedOutput {
    /// Destination in the new run.
    pub target: PinTarget,
    /// Source run in the same execution scope.
    pub source_run_id: Id,
    /// Successful source node instance.
    pub source_node_id: NodeInstanceId,
    /// Accepted source attempt that owns the committed output and artifacts.
    pub source_attempt_id: Id,
}

/// Immutable execution choices included in run-creation idempotency.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOptions {
    /// Cross-run cache policy for this run.
    #[serde(default)]
    pub cache_mode: CacheMode,
    /// Explicit output locks; they take precedence over automatic caching.
    #[serde(default)]
    pub pinned_outputs: Vec<PinnedOutput>,
}

impl RunOptions {
    /// Whether legacy run creation produces these exact options.
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// Checks target shape before the store creates any run state.
    pub(crate) fn validate(
        &self,
        run_id: &Id,
        definition: &WorkflowDefinition,
    ) -> Result<(), StoreError> {
        let mut targets = BTreeSet::new();
        for pin in &self.pinned_outputs {
            if pin.source_run_id == *run_id || !targets.insert(&pin.target) {
                return Err(invalid_pin(
                    "pins must have distinct targets and a prior source run",
                ));
            }
            let valid = match &pin.target {
                PinTarget::Node { node_id } => definition.nodes.iter().any(|node| {
                    matches!(node, NodeDefinition::Action { id, .. } if id == node_id)
                }),
                PinTarget::MapItem {
                    map_node_id,
                    item_key,
                } => !item_key.is_empty() && definition.nodes.iter().any(|node| {
                    matches!(node, NodeDefinition::Map { id, item_key_pointer: Some(_), .. } if id == map_node_id)
                }),
            };
            if !valid {
                return Err(invalid_pin(
                    "pin target must be an Action or a Map with stable item keys",
                ));
            }
        }
        Ok(())
    }
}

fn invalid_pin(message: &str) -> StoreError {
    StoreError::ContractValidation {
        kind: ValidationErrorKind::BindingTypeMismatch,
        path: "/options/pinned_outputs".to_owned(),
        message: message.to_owned(),
        valid_alternatives: Vec::new(),
    }
}

/// Checks immutable pin authorization inside the attempt-claim transaction.
pub(crate) fn validate_pin_reuse(
    run: &WorkflowRun,
    node: &NodeRun,
    provenance: &ReuseProvenance,
    source_attempt: &NodeAttempt,
    source_node: &NodeRun,
    source_revision: &WorkflowRevision,
    target_revision: &WorkflowRevision,
) -> Result<(), StoreError> {
    let target = match &node.parent_map_instance_id {
        Some(parent) => PinTarget::MapItem {
            map_node_id: parent.clone(),
            item_key: node
                .map_item_key
                .clone()
                .ok_or_else(|| invalid_pin("Map pin requires a committed item key"))?,
        },
        None => PinTarget::Node {
            node_id: node.definition_node_id.clone(),
        },
    };
    if !run.options.pinned_outputs.iter().any(|pin| {
        pin.target == target
            && pin.source_run_id == provenance.source_run_id
            && pin.source_node_id == provenance.source_node_id
            && pin.source_attempt_id == provenance.source_attempt_id
    }) {
        return Err(invalid_pin(
            "reused source is not authorized by this node's immutable output pin",
        ));
    }
    if source_node.kind != NodeKind::Action
        || source_node.status != NodeState::Succeeded
        || source_attempt.status != AttemptState::Succeeded
        || source_attempt.output_ref.is_none()
        || source_node.result_ref != source_attempt.output_ref
        || source_node.node_instance_id != source_attempt.node_instance_id
        || source_attempt.scope != run.scope
        || source_node.scope != run.scope
        || source_revision.scope != run.scope
        || target_revision.scope != run.scope
    {
        return Err(invalid_pin(
            "pin source must be a successful committed Action in the same scope",
        ));
    }
    let source_schema = output_schema(source_revision, source_node)
        .ok_or_else(|| invalid_pin("pin source action schema is unavailable"))?;
    let target_schema =
        output_schema(target_revision, node).ok_or(StoreError::CorruptControlPlane)?;
    if source_schema != target_schema {
        return Err(invalid_pin(
            "pin source output schema differs from the destination schema",
        ));
    }
    Ok(())
}

fn output_schema<'a>(
    revision: &'a WorkflowRevision,
    node: &NodeRun,
) -> Option<&'a crate::ids::Digest> {
    let location = match &node.parent_map_instance_id {
        Some(_) => format!("{}/map_action", node.definition_node_id.as_str()),
        None => node.definition_node_id.as_str().to_owned(),
    };
    revision
        .action_pins
        .iter()
        .find(|pin| pin.reference_location == location)
        .map(|pin| &pin.output_schema_digest)
}

/// Resolves and validates stable keys before a Map creates children.
pub(crate) fn map_item_keys(pointer: Option<&str>, input: &Value) -> Result<Vec<String>, String> {
    let items = input.as_array().ok_or("Map input must be an array")?;
    let Some(pointer) = pointer else {
        return Ok(Vec::new());
    };
    let mut seen = BTreeSet::new();
    items
        .iter()
        .map(|item| {
            let key = item
                .pointer(pointer)
                .and_then(Value::as_str)
                .filter(|key| !key.is_empty())
                .ok_or("Map item key must be a nonempty string")?;
            if !seen.insert(key.to_owned()) {
                return Err("Map item keys must be unique".to_owned());
            }
            Ok(key.to_owned())
        })
        .collect()
}

/// Rejects an output lock that selects no item in the committed Map input.
pub(crate) fn validate_map_pins(
    options: &RunOptions,
    map_node_id: &Id,
    pointer: Option<&str>,
    input: &Value,
) -> Result<(), String> {
    let keys = map_item_keys(pointer, input)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    for pin in &options.pinned_outputs {
        if let PinTarget::MapItem {
            map_node_id: target,
            item_key,
        } = &pin.target
        {
            if target == map_node_id && !keys.contains(item_key) {
                return Err("pinned Map item key is absent from the input".to_owned());
            }
        }
    }
    Ok(())
}

/// Finds a locked source before automatic memoization or action invocation.
pub(crate) async fn resolve_pin_attempt<S: WorkflowStore, O: ObjectStore>(
    store: &S,
    objects: &O,
    scope: &ExecutionScope,
    run: &WorkflowRun,
    node: &NodeRun,
    definition: &WorkflowDefinition,
) -> Result<Option<NodeAttempt>, StoreError> {
    if run.options.pinned_outputs.is_empty() {
        return Ok(None);
    }
    let target = if let Some(parent_id) = &node.parent_map_instance_id {
        let pointer = definition
            .nodes
            .iter()
            .find_map(|definition_node| match definition_node {
                NodeDefinition::Map {
                    id,
                    item_key_pointer,
                    ..
                } if id == parent_id => item_key_pointer.as_deref(),
                _ => None,
            });
        let Some(pointer) = pointer else {
            return Ok(None);
        };
        let parent = store.get_node(scope, &run.run_id, parent_id).await?;
        let reference = parent
            .map_input_ref
            .ok_or_else(|| invalid_pin("Map input is not committed"))?;
        let object =
            objects
                .get(scope, &reference.0.digest)
                .await
                .map_err(|error| match error {
                    ObjectReadError::StorageUnavailable => StoreError::StorageUnavailable,
                    ObjectReadError::Corrupt(_) => {
                        invalid_pin("pinned Map input bytes are missing or corrupt")
                    }
                })?;
        if object.reference.scope() != scope
            || object.reference.digest() != &reference.0.digest
            || object.reference.size_bytes() != reference.0.size_bytes
            || object.reference.media_type() != reference.0.media_type
        {
            return Err(invalid_pin("pinned Map input metadata does not match"));
        }
        let input: Value = serde_json::from_slice(&object.bytes)
            .map_err(|_| invalid_pin("invalid committed Map input"))?;
        let keys = map_item_keys(Some(pointer), &input).map_err(|message| invalid_pin(&message))?;
        let item_key = keys
            .get(node.map_item_index.ok_or(StoreError::CorruptControlPlane)? as usize)
            .ok_or(StoreError::CorruptControlPlane)?
            .clone();
        PinTarget::MapItem {
            map_node_id: parent_id.clone(),
            item_key,
        }
    } else {
        PinTarget::Node {
            node_id: node.definition_node_id.clone(),
        }
    };
    let Some(pin) = run
        .options
        .pinned_outputs
        .iter()
        .find(|pin| pin.target == target)
    else {
        return Ok(None);
    };
    let source_run = store.get_run(scope, &pin.source_run_id).await?.run;
    let source_node = store
        .get_node(scope, &pin.source_run_id, &pin.source_node_id)
        .await?;
    let source_attempt = store
        .get_attempt(scope, &pin.source_run_id, &pin.source_attempt_id)
        .await?;
    if source_node.status != NodeState::Succeeded
        || source_attempt.status != AttemptState::Succeeded
        || source_attempt.node_instance_id != source_node.node_instance_id
        || source_attempt.output_ref.is_none()
        || source_attempt.output_ref != source_node.result_ref
    {
        return Err(invalid_pin(
            "pin source must be the node's successful committed attempt",
        ));
    }
    let source_revision = store
        .get_revision(scope, &source_run.definition_id, &source_run.revision_hash)
        .await?;
    let target_revision = store
        .get_revision(scope, &run.definition_id, &run.revision_hash)
        .await?;
    let source_schema = output_schema(&source_revision, &source_node)
        .ok_or_else(|| invalid_pin("pin source is not an executable Action"))?;
    let target_schema =
        output_schema(&target_revision, node).ok_or(StoreError::CorruptControlPlane)?;
    if source_schema != target_schema {
        return Err(invalid_pin(
            "pin source output schema differs from the destination schema",
        ));
    }
    Ok(Some(source_attempt))
}
