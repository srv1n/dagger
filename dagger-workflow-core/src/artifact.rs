//! Content-addressed object and typed artifact APIs from contract sections 1.10 and 12.

use crate::ids::{Digest, Id, NodeInstanceId, Timestamp};
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};

/// The closed typed-use vocabulary for artifact references. Contract section 1.10.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum ArtifactKind {
    /// Immutable run input. Contract section 1.10.
    RunInput,
    /// A supported-subset schema document. Contract section 1.10.
    SchemaDocument,
    /// Canonical workflow definition bytes. Contract section 1.10.
    Definition,
    /// A successful node output. Contract section 1.10.
    NodeOutput,
    /// Exact action invocation input bytes. Contract section 1.10.
    ActionInvocationInput,
    /// An ordered action-produced artifact. Contract section 1.10.
    ActionArtifact,
    /// A persistence-safe diagnostics envelope. Contract section 1.10.
    Diagnostics,
    /// Compatibility evidence. Contract section 1.10.
    CompatibilityEvidence,
    /// A committed Choice input. Contract section 1.10.
    ChoiceInput,
    /// A committed Map input. Contract section 1.10.
    MapInput,
    /// An ordered Map aggregate. Contract section 1.10.
    MapAggregate,
    /// An approval request. Contract section 1.10.
    ApprovalRequest,
    /// A human approval decision payload. Contract section 1.10.
    ApprovalDecisionPayload,
}

/// Reusable content metadata scoped by digest. Contract section 1.10.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectRecord {
    /// Object scope. Contract section 1.10.
    pub scope: ExecutionScope,
    /// Verified content digest. Contract section 1.10.
    pub digest: Digest,
    /// Verified byte length. Contract section 1.10.
    pub size_bytes: u64,
    /// Store-private scope-qualified key. Contract section 1.10.
    pub object_key: String,
    /// First registration timestamp. Contract section 1.10.
    pub created_at: Timestamp,
}

/// One immutable typed use of an ObjectRecord. Contract section 1.10.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactRef {
    /// Artifact scope. Contract section 1.10.
    pub scope: ExecutionScope,
    /// Deterministic typed-use ID. Contract section 1.10.
    pub artifact_ref_id: Id,
    /// Referenced content digest. Contract section 1.10.
    pub digest: Digest,
    /// Referenced content size. Contract section 1.10.
    pub size_bytes: u64,
    /// Normalized media type. Contract section 1.10.
    pub media_type: String,
    /// Closed artifact role. Contract section 1.10.
    pub kind: ArtifactKind,
    /// Optional producer run. Contract section 1.10.
    pub producer_run_id: Option<Id>,
    /// Optional producer node. Contract section 1.10.
    pub producer_node_id: Option<NodeInstanceId>,
    /// Optional producer attempt. Contract section 1.10.
    pub producer_attempt_id: Option<Id>,
    /// Producer-local ordinal. Contract section 1.10.
    pub ordinal: u32,
    /// Registration timestamp. Contract section 1.10.
    pub created_at: Timestamp,
}

/// The canonical JSON-safe ArtifactRef projection. Contract section 8.1.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRefValue {
    /// Typed-use identifier. Contract section 8.1.
    pub artifact_ref_id: Id,
    /// Content digest. Contract section 8.1.
    pub digest: Digest,
    /// Decimal byte length. Contract section 8.1.
    pub size_bytes: String,
    /// Normalized media type. Contract section 8.1.
    pub media_type: String,
}

/// An ArtifactRef proven to contain canonical JSON. Contract section 1.1.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonRef(pub ArtifactRef);

/// Opaque proof that an object was durably published and verified. Contract section 5.1.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedObjectRef {
    scope: ExecutionScope,
    digest: Digest,
    size_bytes: u64,
    media_type: String,
    object_key: String,
}

impl VerifiedObjectRef {
    /// Constructs a verified capability for an object-store implementation.
    pub(crate) fn new(
        scope: ExecutionScope,
        digest: Digest,
        size_bytes: u64,
        media_type: String,
        object_key: String,
    ) -> Self {
        Self {
            scope,
            digest,
            size_bytes,
            media_type,
            object_key,
        }
    }

    /// Returns the capability scope. Contract section 5.1.
    pub fn scope(&self) -> &ExecutionScope {
        &self.scope
    }
    /// Returns the verified content digest. Contract section 5.1.
    pub fn digest(&self) -> &Digest {
        &self.digest
    }
    /// Returns the verified byte length. Contract section 5.1.
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }
    /// Returns the normalized media type. Contract section 5.1.
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    /// Returns the store-private key to the implementing control-plane store.
    pub(crate) fn object_key(&self) -> &str {
        &self.object_key
    }
}

/// Closed failed-read classes. Contract section 1.1.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FailedReadClass {
    /// Committed content is missing. Contract section 12.3.
    Missing,
    /// Committed bytes do not match the requested digest. Contract section 12.3.
    DigestInvalid,
}

/// Opaque object-store proof for a failed committed read. Contract section 12.3.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailedReadProof {
    scope: ExecutionScope,
    requested_digest: Digest,
    error_class: FailedReadClass,
    observed_digest: Option<Digest>,
    store_instance_nonce: Vec<u8>,
    proof_nonce: Vec<u8>,
    checked_at: Timestamp,
}

impl FailedReadProof {
    /// Mints a failed-read capability inside an object-store implementation.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mint(
        scope: ExecutionScope,
        requested_digest: Digest,
        error_class: FailedReadClass,
        observed_digest: Option<Digest>,
        store_instance_nonce: Vec<u8>,
        proof_nonce: Vec<u8>,
        checked_at: Timestamp,
    ) -> Self {
        Self {
            scope,
            requested_digest,
            error_class,
            observed_digest,
            store_instance_nonce,
            proof_nonce,
            checked_at,
        }
    }

    /// Returns the closed read-failure class. Contract section 1.1.
    pub fn error_class(&self) -> FailedReadClass {
        self.error_class
    }

    /// Returns the proof scope to the control-plane verifier.
    pub(crate) fn scope(&self) -> &ExecutionScope {
        &self.scope
    }

    /// Returns the requested digest to the control-plane verifier.
    pub(crate) fn requested_digest(&self) -> &Digest {
        &self.requested_digest
    }

    /// Returns a stable persistence-safe proof fingerprint.
    pub(crate) fn fingerprint_material(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(self.scope.tenant_id.as_str().as_bytes());
        bytes.push(0);
        bytes.extend(self.scope.namespace.as_str().as_bytes());
        bytes.push(0);
        bytes.extend(self.requested_digest.as_str().as_bytes());
        bytes.push(self.error_class as u8);
        if let Some(observed) = &self.observed_digest {
            bytes.extend(observed.as_str().as_bytes());
        }
        bytes.extend(&self.store_instance_nonce);
        bytes.extend(&self.proof_nonce);
        bytes.extend(self.checked_at.0.to_be_bytes());
        bytes
    }
}

/// Bytes returned only after read verification. Contract section 12.3.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedObject {
    /// Verified object capability. Contract section 12.3.
    pub reference: VerifiedObjectRef,
    /// Exact verified bytes. Contract section 12.3.
    pub bytes: Vec<u8>,
}

/// A verified read failure carrying the only valid corruption proof. Contract section 12.3.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("verified object read failed")]
pub struct ObjectReadError {
    /// Opaque proof minted for this failed read. Contract section 12.3.
    pub proof: FailedReadProof,
}

/// A conflicting same-digest publication. Contract section 12.1.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("artifact metadata conflicts with existing scoped content")]
pub struct ArtifactMetadataConflict {
    /// Candidate digest. Contract section 12.1.
    pub digest: Digest,
    /// Existing verified byte length. Contract section 12.1.
    pub existing_size_bytes: u64,
    /// Candidate byte length. Contract section 12.1.
    pub candidate_size_bytes: u64,
}

/// Object-store infrastructure and publication errors. Contract section 12.
#[derive(Debug, thiserror::Error)]
pub enum ObjectStoreError {
    /// Same-digest content or metadata conflicts. Contract section 12.1.
    #[error(transparent)]
    ArtifactMetadataConflict(#[from] ArtifactMetadataConflict),
    /// Durable storage is unavailable. Contract section 12.1.
    #[error("object storage unavailable")]
    StorageUnavailable,
    /// Input or metadata is malformed. Contract section 12.1.
    #[error("invalid object field")]
    InvalidField,
}

/// Scope-confined content-addressed object storage. Contract sections 12.1 and 12.3.
pub trait ObjectStore: Send + Sync {
    /// Durably publishes bytes if absent and returns a verified capability. Contract section 12.1.
    async fn put(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError>;

    /// Reads and verifies committed bytes or mints a failed-read proof. Contract section 12.3.
    async fn get(
        &self,
        scope: &ExecutionScope,
        digest: &Digest,
    ) -> Result<VerifiedObject, ObjectReadError>;

    /// Publishes a prepared object without replacement. Contract section 12.1.
    async fn publish_if_absent(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError>;
}
