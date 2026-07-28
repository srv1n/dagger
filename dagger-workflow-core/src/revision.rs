//! Immutable published revision entity from contract sections 1.3 and 13.

use crate::artifact::JsonRef;
use crate::definition::ActionPin;
use crate::ids::{Digest, Id, Timestamp, TopologicalRank};
use crate::scope::ExecutionScope;
use std::collections::BTreeMap;

/// One immutable validated workflow revision. Contract section 1.3.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowRevision {
    /// Revision scope. Contract section 1.3.
    pub scope: ExecutionScope,
    /// Owning definition ID. Contract section 1.3.
    pub definition_id: Id,
    /// Canonical definition digest. Contract section 13.1.
    pub revision_hash: Digest,
    /// Exact definition format version. Contract section 1.3.
    pub definition_format_version: String,
    /// Canonical definition object ref. Contract section 1.3.
    pub canonical_definition_ref: JsonRef,
    /// Validated root input schema ref. Contract section 1.3.
    pub run_input_schema_ref: JsonRef,
    /// Validated root output schema ref. Contract section 1.3.
    pub run_output_schema_ref: JsonRef,
    /// Root input schema digest. Contract section 1.3.
    pub run_input_schema_digest: Digest,
    /// Root output schema digest. Contract section 1.3.
    pub run_output_schema_digest: Digest,
    /// Entry node ID. Contract section 1.3.
    pub entry_node_id: Id,
    /// Definition node count. Contract section 1.3.
    pub node_count: u32,
    /// Canonical Kahn ranks. Contract section 1.5.
    pub node_topological_ranks: BTreeMap<Id, TopologicalRank>,
    /// Ordered executable action pins. Contract section 1.3.
    pub action_pins: Vec<ActionPin>,
    /// Publication database timestamp. Contract section 1.3.
    pub published_at: Timestamp,
    /// Persistence-safe publishing principal ID. Contract section 1.3.
    pub published_by: String,
}
