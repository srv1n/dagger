//! Primitive identifiers and deterministic derivations from contract sections 1 and 7.

use crate::artifact::ArtifactKind;
use crate::scope::ExecutionScope;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};

/// An opaque case-sensitive entity identifier. Contract section 1.1.
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

/// An ID validation failure. Contract section 1.1.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IdError {
    /// The input contains no bytes.
    #[error("ID is empty")]
    Empty,
    /// The input exceeds the contract byte limit.
    #[error("ID exceeds 128 bytes")]
    TooLong,
}

/// A lowercase SHA-256 digest with its algorithm prefix. Contract section 1.1.
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

/// A digest validation failure. Contract section 1.1.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("digest must be sha256: followed by 64 lowercase hexadecimal characters")]
pub enum DigestError {
    /// The input is not canonical `sha256:` plus lowercase hexadecimal.
    Invalid,
}

/// A database-clock UTC Unix epoch millisecond value. Contract section 1.1.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

/// Opaque host-defined budget units. Contract section 1.1.
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
    let mut bytes = lp(b"dagger-edge-v1");
    bytes.extend(lp(&digest_bytes(_revision_hash)));
    bytes.extend(lp(_from_node_id.0.as_bytes()));
    bytes.extend(lp(_edge_label.as_bytes()));
    bytes.extend(lp(_to_node_id.0.as_bytes()));
    Id::new(format!("edge_{}", hex(&Sha256::digest(bytes)))).expect("derived ID is valid")
}

/// Derives the typed content-use identifier. Contract section 1.10.
pub fn artifact_ref_id(_identity: ArtifactRefIdentity<'_>) -> Id {
    let mut bytes = lp(b"dagger-artifact-ref-v1");
    bytes.extend(lp(_identity.scope.tenant_id.as_str().as_bytes()));
    bytes.extend(lp(_identity.scope.namespace.as_str().as_bytes()));
    bytes.extend(lp(&digest_bytes(_identity.digest)));
    bytes.extend(lp(artifact_kind_name(_identity.kind).as_bytes()));
    bytes.extend(optional_lp(
        _identity.producer_run_id.map(|value| value.0.as_bytes()),
    ));
    bytes.extend(optional_lp(
        _identity.producer_node_id.map(|value| value.0.as_bytes()),
    ));
    bytes.extend(optional_lp(
        _identity
            .producer_attempt_id
            .map(|value| value.0.as_bytes()),
    ));
    bytes.extend(lp(&_identity.ordinal.to_be_bytes()));
    Id::new(format!("artifact_{}", hex(&Sha256::digest(bytes)))).expect("derived ID is valid")
}

/// Derives a static logical node's external idempotency key. Contract section 7.1.
pub fn idempotency_key(
    _scope: &ExecutionScope,
    _run_id: &Id,
    _node_instance_id: &NodeInstanceId,
) -> String {
    let bytes = idem_prefix(_scope, _run_id, _node_instance_id);
    format!("dwf-idem-v1:{}", hex(&Sha256::digest(bytes)))
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
    let mut bytes = idem_prefix(_scope, _run_id, _child_node_instance_id);
    bytes.extend(lp(b"map-child"));
    bytes.extend(lp(_map_parent_node_instance_id.0.as_bytes()));
    bytes.extend(lp(&_map_item_index.to_be_bytes()));
    bytes.extend(lp(_map_item_digest.as_str().as_bytes()));
    format!("dwf-idem-v1:{}", hex(&Sha256::digest(bytes)))
}

/// Derives a synthetic Map child node identifier. Contract section 10.1.
pub fn map_child_id(
    _run_id: &Id,
    _map_node_instance_id: &NodeInstanceId,
    _item_index: u32,
    _item_digest: &Digest,
) -> NodeInstanceId {
    let mut bytes = lp(b"dagger-map-child-v1");
    bytes.extend(lp(_run_id.0.as_bytes()));
    bytes.extend(lp(_map_node_instance_id.0.as_bytes()));
    bytes.extend(&_item_index.to_be_bytes());
    bytes.extend(digest_bytes(_item_digest));
    Id::new(format!("mapchild_{}", hex(&Sha256::digest(bytes)))).expect("derived ID is valid")
}

/// Derives the ordered Map expansion digest. Contract section 10.1.
pub fn map_expansion_digest(_children: &[MapChildIdentity]) -> Digest {
    let mut bytes = lp(b"dagger-map-expansion-v1");
    for child in _children {
        bytes.extend(lp(&child.item_index.to_be_bytes()));
        bytes.extend(lp(&digest_bytes(&child.item_digest)));
        bytes.extend(lp(child.child_id.0.as_bytes()));
    }
    Digest::new(format!("sha256:{}", hex(&Sha256::digest(bytes)))).expect("SHA-256 output is valid")
}

fn lp(value: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(8 + value.len());
    encoded.extend((value.len() as u64).to_be_bytes());
    encoded.extend(value);
    encoded
}

fn optional_lp(value: Option<&[u8]>) -> Vec<u8> {
    match value {
        Some(value) => lp(value),
        None => lp(b""),
    }
}

fn idem_prefix(scope: &ExecutionScope, run_id: &Id, node_instance_id: &NodeInstanceId) -> Vec<u8> {
    let mut bytes = lp(b"dagger-idem-v1");
    bytes.extend(lp(scope.tenant_id.as_str().as_bytes()));
    bytes.extend(lp(scope.namespace.as_str().as_bytes()));
    bytes.extend(lp(run_id.0.as_bytes()));
    bytes.extend(lp(node_instance_id.0.as_bytes()));
    bytes
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
