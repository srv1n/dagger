//! Primitive identifiers and deterministic derivations from contract sections 1 and 7.

use crate::artifact::ArtifactKind;
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};

macro_rules! string_newtype {
    ($name:ident, $doc:literal, $section:literal) => {
        #[doc = concat!($doc, " Contract section ", $section, ".")]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);
    };
}

string_newtype!(Id, "An opaque case-sensitive entity identifier.", "1.1");
string_newtype!(
    Digest,
    "A lowercase SHA-256 digest with its algorithm prefix.",
    "1.1"
);

/// A database-clock UTC Unix epoch millisecond value. Contract section 1.1.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

/// Opaque host-defined budget units. Contract section 1.1.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CostUnits(pub u64);

/// A mutable-row compare-and-swap counter. Contract section 1.1.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Version(pub u64);

/// A static definition node ID or synthetic Map child ID. Contract section 1.1.
pub type NodeInstanceId = Id;

/// A persisted canonical Kahn rank used for deterministic recovery. Contract section 1.5.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TopologicalRank(pub u32);

/// Correlation inputs to deterministic ArtifactRef ID derivation. Contract section 1.10.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactRefIdentity<'a> {
    /// Scope included in the derivation. Contract section 1.10.
    pub scope: &'a ExecutionScope,
    /// Content digest included in the derivation. Contract section 1.10.
    pub digest: &'a Digest,
    /// Typed artifact use included in the derivation. Contract section 1.10.
    pub kind: ArtifactKind,
    /// Optional producer run correlation. Contract section 1.10.
    pub producer_run_id: Option<&'a Id>,
    /// Optional producer node correlation. Contract section 1.10.
    pub producer_node_id: Option<&'a NodeInstanceId>,
    /// Optional producer attempt correlation. Contract section 1.10.
    pub producer_attempt_id: Option<&'a Id>,
    /// Producer-local ordinal. Contract section 1.10.
    pub ordinal: u32,
}

/// Derives the stable control-edge identifier. Contract section 1.6.
pub fn edge_id(
    _revision_hash: &Digest,
    _from_node_id: &Id,
    _edge_label: &str,
    _to_node_id: &Id,
) -> Id {
    todo!()
}

/// Derives the typed content-use identifier. Contract section 1.10.
pub fn artifact_ref_id(_identity: ArtifactRefIdentity<'_>) -> Id {
    todo!()
}

/// Derives a static logical node's external idempotency key. Contract section 7.1.
pub fn idempotency_key(
    _scope: &ExecutionScope,
    _run_id: &Id,
    _node_instance_id: &NodeInstanceId,
) -> String {
    todo!()
}

/// Derives a Map child's external idempotency key. Contract section 7.1.
pub fn map_child_idempotency_key(
    _scope: &ExecutionScope,
    _run_id: &Id,
    _child_node_instance_id: &NodeInstanceId,
    _map_parent_node_instance_id: &NodeInstanceId,
    _map_item_index: u32,
    _map_item_digest: &Digest,
) -> String {
    todo!()
}

/// Derives a synthetic Map child node identifier. Contract section 10.1.
pub fn map_child_id(
    _run_id: &Id,
    _map_node_instance_id: &NodeInstanceId,
    _item_index: u32,
    _item_digest: &Digest,
) -> NodeInstanceId {
    todo!()
}

/// Derives the ordered Map expansion digest. Contract section 10.1.
pub fn map_expansion_digest(_children: &[MapChildIdentity]) -> Digest {
    todo!()
}

/// One ordered tuple included in the Map expansion digest. Contract section 10.1.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapChildIdentity {
    /// Zero-based item index. Contract section 10.1.
    pub item_index: u32,
    /// Digest of canonical item JSON. Contract section 10.1.
    pub item_digest: Digest,
    /// Derived synthetic child ID. Contract section 10.1.
    pub child_id: NodeInstanceId,
}
