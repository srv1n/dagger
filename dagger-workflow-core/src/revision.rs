//! Immutable published revision entity.

use crate::artifact::JsonRef;
use crate::definition::ActionPin;
use crate::ids::{Digest, Id, Timestamp, TopologicalRank};
use crate::scope::ExecutionScope;
use std::collections::BTreeMap;

/// One immutable validated workflow revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowRevision {
    /// Revision scope.
    pub scope: ExecutionScope,
    /// Owning definition ID.
    pub definition_id: Id,
    /// Canonical definition digest.
    pub revision_hash: Digest,
    /// Exact definition format version.
    pub definition_format_version: String,
    /// Canonical definition object ref.
    pub canonical_definition_ref: JsonRef,
    /// Validated root input schema ref.
    pub run_input_schema_ref: JsonRef,
    /// Validated root output schema ref.
    pub run_output_schema_ref: JsonRef,
    /// Root input schema digest.
    pub run_input_schema_digest: Digest,
    /// Root output schema digest.
    pub run_output_schema_digest: Digest,
    /// Lexically ordered nodes with no incoming output reference.
    pub root_node_ids: Vec<Id>,
    /// Definition node count.
    pub node_count: u32,
    /// Canonical Kahn ranks.
    pub node_topological_ranks: BTreeMap<Id, TopologicalRank>,
    /// Ordered executable action pins.
    pub action_pins: Vec<ActionPin>,
    /// Publication database timestamp.
    pub published_at: Timestamp,
    /// Persistence-safe publishing principal ID.
    pub published_by: String,
}
