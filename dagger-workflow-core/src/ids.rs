//! Primitive identifiers and deterministic derivations.

use crate::artifact::ArtifactKind;
use crate::scope::ExecutionScope;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};

/// An opaque case-sensitive entity identifier.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Id(pub(crate) String);

impl Id {
    /// Validates and constructs an ID.
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(IdError::Empty);
        }
        if value.len() > 128 {
            return Err(IdError::TooLong);
        }
        Ok(Self(value))
    }

    /// Returns the opaque ID text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Id {
    type Error = IdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)
            .and_then(|value| Self::new(value).map_err(serde::de::Error::custom))
    }
}

/// An ID validation failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IdError {
    /// The input contains no bytes.
    #[error("ID is empty")]
    Empty,
    /// The input exceeds the contract byte limit.
    #[error("ID exceeds 128 bytes")]
    TooLong,
}

/// A lowercase SHA-256 digest with its algorithm prefix.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Digest(pub(crate) String);

impl Digest {
    /// Validates and constructs a SHA-256 digest.
    pub fn new(value: impl Into<String>) -> Result<Self, DigestError> {
        let value = value.into();
        let valid = value.len() == 71
            && value.starts_with("sha256:")
            && value.as_bytes()[7..].iter().all(u8::is_ascii_hexdigit)
            && value.as_bytes()[7..]
                .iter()
                .all(|byte| !byte.is_ascii_uppercase());
        if valid {
            Ok(Self(value))
        } else {
            Err(DigestError::Invalid)
        }
    }

    /// Returns the canonical digest text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Digest {
    type Error = DigestError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)
            .and_then(|value| Self::new(value).map_err(serde::de::Error::custom))
    }
}

/// A digest validation failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("digest must be sha256: followed by 64 lowercase hexadecimal characters")]
pub enum DigestError {
    /// The input is not canonical `sha256:` plus lowercase hexadecimal.
    Invalid,
}

/// A database-clock UTC Unix epoch millisecond value.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

/// Opaque host-defined budget units.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CostUnits(pub u64);

impl Serialize for CostUnits {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for CostUnits {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value == "0"
            || (!value.starts_with('0') && value.bytes().all(|byte| byte.is_ascii_digit()))
        {
            value.parse().map(Self).map_err(serde::de::Error::custom)
        } else {
            Err(serde::de::Error::custom(
                "cost units must be a canonical u64 decimal string",
            ))
        }
    }
}

/// A mutable-row compare-and-swap counter.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Version(pub u64);

/// A static definition node ID or synthetic Map child ID.
pub type NodeInstanceId = Id;

/// A persisted canonical Kahn rank used for deterministic recovery.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TopologicalRank(pub u32);

/// Correlation inputs to deterministic ArtifactRef ID derivation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactRefIdentity<'a> {
    /// Scope included in the derivation.
    pub scope: &'a ExecutionScope,
    /// Content digest included in the derivation.
    pub digest: &'a Digest,
    /// Typed artifact use included in the derivation.
    pub kind: ArtifactKind,
    /// Optional producer run correlation.
    pub producer_run_id: Option<&'a Id>,
    /// Optional producer node correlation.
    pub producer_node_id: Option<&'a NodeInstanceId>,
    /// Optional producer attempt correlation.
    pub producer_attempt_id: Option<&'a Id>,
    /// Producer-local ordinal.
    pub ordinal: u32,
}

/// Derives the stable control-edge identifier.
pub fn edge_id(
    _revision_hash: &Digest,
    _from_node_id: &Id,
    _edge_label: &str,
    _to_node_id: &Id,
) -> Id {
    let mut hash = Sha256::new();
    hash_lp(&mut hash, b"dagger-edge-v1");
    hash_lp(&mut hash, &digest_bytes(_revision_hash));
    hash_lp(&mut hash, _from_node_id.0.as_bytes());
    hash_lp(&mut hash, _edge_label.as_bytes());
    hash_lp(&mut hash, _to_node_id.0.as_bytes());
    Id::new(finish_hash("edge_", hash)).expect("derived ID is valid")
}

/// Derives the typed content-use identifier.
pub fn artifact_ref_id(_identity: ArtifactRefIdentity<'_>) -> Id {
    let mut hash = Sha256::new();
    hash_lp(&mut hash, b"dagger-artifact-ref-v1");
    hash_lp(&mut hash, _identity.scope.tenant_id.as_str().as_bytes());
    hash_lp(&mut hash, _identity.scope.namespace.as_str().as_bytes());
    hash_lp(&mut hash, &digest_bytes(_identity.digest));
    hash_lp(&mut hash, artifact_kind_name(_identity.kind).as_bytes());
    hash_lp(
        &mut hash,
        _identity
            .producer_run_id
            .map_or(b"", |value| value.0.as_bytes()),
    );
    hash_lp(
        &mut hash,
        _identity
            .producer_node_id
            .map_or(b"", |value| value.0.as_bytes()),
    );
    hash_lp(
        &mut hash,
        _identity
            .producer_attempt_id
            .map_or(b"", |value| value.0.as_bytes()),
    );
    hash_lp(&mut hash, &_identity.ordinal.to_be_bytes());
    Id::new(finish_hash("artifact_", hash)).expect("derived ID is valid")
}

/// Derives a static logical node's external idempotency key.
pub fn idempotency_key(
    _scope: &ExecutionScope,
    _run_id: &Id,
    _node_instance_id: &NodeInstanceId,
) -> String {
    finish_hash(
        "dwf-idem-v1:",
        idem_prefix(_scope, _run_id, _node_instance_id),
    )
}

/// Derives a Map child's external idempotency key.
pub fn map_child_idempotency_key(
    _scope: &ExecutionScope,
    _run_id: &Id,
    _child_node_instance_id: &NodeInstanceId,
    _map_parent_node_instance_id: &NodeInstanceId,
    _map_item_index: u32,
    _map_item_digest: &Digest,
) -> String {
    let mut hash = idem_prefix(_scope, _run_id, _child_node_instance_id);
    hash_lp(&mut hash, b"map-child");
    hash_lp(&mut hash, _map_parent_node_instance_id.0.as_bytes());
    hash_lp(&mut hash, &_map_item_index.to_be_bytes());
    hash_lp(&mut hash, _map_item_digest.as_str().as_bytes());
    finish_hash("dwf-idem-v1:", hash)
}

/// Derives a synthetic Map child node identifier.
pub fn map_child_id(
    _run_id: &Id,
    _map_node_instance_id: &NodeInstanceId,
    _item_index: u32,
    _item_digest: &Digest,
) -> NodeInstanceId {
    let mut hash = Sha256::new();
    hash_lp(&mut hash, b"dagger-map-child-v1");
    hash_lp(&mut hash, _run_id.0.as_bytes());
    hash_lp(&mut hash, _map_node_instance_id.0.as_bytes());
    // These two fields are deliberately raw, not length-prefixed. They are
    // part of the persisted v1 identity contract.
    hash.update(_item_index.to_be_bytes());
    hash.update(digest_bytes(_item_digest));
    Id::new(finish_hash("mapchild_", hash)).expect("derived ID is valid")
}

/// Derives the ordered Map expansion digest.
pub fn map_expansion_digest(_children: &[MapChildIdentity]) -> Digest {
    let mut hash = Sha256::new();
    hash_lp(&mut hash, b"dagger-map-expansion-v1");
    for child in _children {
        hash_lp(&mut hash, &child.item_index.to_be_bytes());
        hash_lp(&mut hash, &digest_bytes(&child.item_digest));
        hash_lp(&mut hash, child.child_id.0.as_bytes());
    }
    Digest::new(finish_hash("sha256:", hash)).expect("SHA-256 output is valid")
}

// Stream the same u64 big-endian length prefix and bytes as the v1 encoding.
// In particular, an absent optional field is a zero-length field, not omitted.
fn hash_lp(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value);
}

fn idem_prefix(scope: &ExecutionScope, run_id: &Id, node_instance_id: &NodeInstanceId) -> Sha256 {
    let mut hash = Sha256::new();
    hash_lp(&mut hash, b"dagger-idem-v1");
    hash_lp(&mut hash, scope.tenant_id.as_str().as_bytes());
    hash_lp(&mut hash, scope.namespace.as_str().as_bytes());
    hash_lp(&mut hash, run_id.0.as_bytes());
    hash_lp(&mut hash, node_instance_id.0.as_bytes());
    hash
}

fn artifact_kind_name(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::RunInput => "RunInput",
        ArtifactKind::SchemaDocument => "SchemaDocument",
        ArtifactKind::Definition => "Definition",
        ArtifactKind::NodeOutput => "NodeOutput",
        ArtifactKind::ActionInvocationInput => "ActionInvocationInput",
        ArtifactKind::ActionArtifact => "ActionArtifact",
        ArtifactKind::Diagnostics => "Diagnostics",
        ArtifactKind::CompatibilityEvidence => "CompatibilityEvidence",
        ArtifactKind::ChoiceInput => "ChoiceInput",
        ArtifactKind::MapInput => "MapInput",
        ArtifactKind::MapAggregate => "MapAggregate",
        ArtifactKind::ApprovalRequest => "ApprovalRequest",
        ArtifactKind::ApprovalDecisionPayload => "ApprovalDecisionPayload",
    }
}

fn finish_hash(prefix: &str, hash: Sha256) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(prefix.len() + 64);
    output.push_str(prefix);
    for byte in hash.finalize() {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn digest_bytes(digest: &Digest) -> [u8; 32] {
    let hex = digest
        .0
        .strip_prefix("sha256:")
        .expect("Digest must be sha256");
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
            .expect("Digest must be lowercase hex");
    }
    bytes
}

/// One ordered tuple included in the Map expansion digest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapChildIdentity {
    /// Zero-based item index.
    pub item_index: u32,
    /// Digest of canonical item JSON.
    pub item_digest: Digest,
    /// Derived synthetic child ID.
    pub child_id: NodeInstanceId,
}
