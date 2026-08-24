//! Content-addressed object and typed artifact APIs.

use crate::ids::{Digest, Id, NodeInstanceId, Timestamp};
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};
use std::future::Future;

/// The closed typed-use vocabulary for artifact references.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum ArtifactKind {
    /// Immutable run input.
    RunInput,
    /// A supported-subset schema document.
    SchemaDocument,
    /// Canonical workflow definition bytes.
    Definition,
    /// A successful node output.
    NodeOutput,
    /// Exact action invocation input bytes.
    ActionInvocationInput,
    /// An ordered action-produced artifact.
    ActionArtifact,
    /// A persistence-safe diagnostics envelope.
    Diagnostics,
    /// Compatibility evidence.
    CompatibilityEvidence,
    /// A committed Choice input.
    ChoiceInput,
    /// A committed Map input.
    MapInput,
    /// An ordered Map aggregate.
    MapAggregate,
    /// An approval request.
    ApprovalRequest,
    /// A human approval decision payload.
    ApprovalDecisionPayload,
}

/// Reusable content metadata scoped by digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectRecord {
    /// Object scope.
    pub scope: ExecutionScope,
    /// Verified content digest.
    pub digest: Digest,
    /// Verified byte length.
    pub size_bytes: u64,
    /// Store-private scope-qualified key.
    pub object_key: String,
    /// First registration timestamp.
    pub created_at: Timestamp,
}

/// One immutable typed use of an ObjectRecord.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactRef {
    /// Artifact scope.
    pub scope: ExecutionScope,
    /// Deterministic typed-use ID.
    pub artifact_ref_id: Id,
    /// Referenced content digest.
    pub digest: Digest,
    /// Referenced content size.
    pub size_bytes: u64,
    /// Normalized media type.
    pub media_type: String,
    /// Closed artifact role.
    pub kind: ArtifactKind,
    /// Optional producer run.
    pub producer_run_id: Option<Id>,
    /// Optional producer node.
    pub producer_node_id: Option<NodeInstanceId>,
    /// Optional producer attempt.
    pub producer_attempt_id: Option<Id>,
    /// Producer-local ordinal.
    pub ordinal: u32,
    /// Registration timestamp.
    pub created_at: Timestamp,
}

/// The canonical JSON-safe ArtifactRef projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRefValue {
    /// Typed-use identifier.
    pub artifact_ref_id: Id,
    /// Content digest.
    pub digest: Digest,
    /// Decimal byte length.
    pub size_bytes: String,
    /// Normalized media type.
    pub media_type: String,
}

/// An ArtifactRef proven to contain canonical JSON.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonRef(pub ArtifactRef);

/// Opaque proof that an object was durably published and verified.
///
/// `Debug` is implemented by hand and redacts the store-instance nonce and the
/// verified bytes. Both are capability material, and this type rides inside
/// errors and values that are routinely logged.
#[derive(Clone, Eq, PartialEq)]
pub struct VerifiedObjectRef {
    scope: ExecutionScope,
    digest: Digest,
    size_bytes: u64,
    media_type: String,
    object_key: String,
    store_instance_nonce: Vec<u8>,
    verified_bytes: Vec<u8>,
}

impl VerifiedObjectRef {
    /// Constructs a verified capability for an object-store implementation.
    pub(crate) fn new(
        scope: ExecutionScope,
        digest: Digest,
        size_bytes: u64,
        media_type: String,
        object_key: String,
        store_instance_nonce: Vec<u8>,
        verified_bytes: Vec<u8>,
    ) -> Self {
        Self {
            scope,
            digest,
            size_bytes,
            media_type,
            object_key,
            store_instance_nonce,
            verified_bytes,
        }
    }

    /// Returns the capability scope.
    pub fn scope(&self) -> &ExecutionScope {
        &self.scope
    }
    /// Returns the verified content digest.
    pub fn digest(&self) -> &Digest {
        &self.digest
    }
    /// Returns the verified byte length.
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }
    /// Returns the normalized media type.
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    /// Returns the store-private key to the implementing control-plane store.
    pub(crate) fn object_key(&self) -> &str {
        &self.object_key
    }

    /// Returns the opaque object-store instance binding to the control plane.
    pub(crate) fn store_instance_nonce(&self) -> &[u8] {
        &self.store_instance_nonce
    }

    /// Returns the bytes authenticated by this in-process capability.
    pub(crate) fn verified_bytes(&self) -> &[u8] {
        &self.verified_bytes
    }
}

impl std::fmt::Debug for VerifiedObjectRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedObjectRef")
            .field("scope", &self.scope)
            .field("digest", &self.digest)
            .field("size_bytes", &self.size_bytes)
            .field("media_type", &self.media_type)
            .field("object_key", &self.object_key)
            .field("store_instance_nonce", &"<redacted>")
            .field("verified_bytes", &"<redacted>")
            .finish()
    }
}

/// Closed failed-read classes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FailedReadClass {
    /// Committed content is missing.
    Missing,
    /// Committed bytes do not match the requested digest.
    DigestInvalid,
}

/// Opaque object-store proof for a failed committed read.
///
/// `Debug` is implemented by hand and redacts both nonces. This proof is the
/// only capability that authorizes `mark_corrupt_storage`, and it now travels
/// inside `StoreError::CommittedObjectCorrupt`, which lands in log lines and
/// panic messages.
#[derive(Clone, Eq, PartialEq)]
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

    /// Returns the closed read-failure class.
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

    /// Returns the object-store instance binding to the control-plane verifier.
    pub(crate) fn store_instance_nonce(&self) -> &[u8] {
        &self.store_instance_nonce
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

impl std::fmt::Debug for FailedReadProof {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FailedReadProof")
            .field("scope", &self.scope)
            .field("requested_digest", &self.requested_digest)
            .field("error_class", &self.error_class)
            .field("observed_digest", &self.observed_digest)
            .field("store_instance_nonce", &"<redacted>")
            .field("proof_nonce", &"<redacted>")
            .field("checked_at", &self.checked_at)
            .finish()
    }
}

/// Bytes returned only after read verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedObject {
    /// Verified object capability.
    pub reference: VerifiedObjectRef,
    /// Exact verified bytes.
    pub bytes: Vec<u8>,
}

/// A failed read of a committed object.
///
/// The two variants are not interchangeable. `Corrupt` asserts an integrity
/// failure and carries the only capability that may invoke
/// `mark_corrupt_storage`; it is reserved for authoritative absence of a
/// committed component and for a completed read whose digest does not match.
/// `StorageUnavailable` asserts nothing about the object: the store could not
/// complete a verification, so no proof is minted and no workflow transition is
/// authorized. Every transport, permission, descriptor, and interruption
/// failure belongs here, which is what allows a networked object store to
/// implement this trait without corrupting runs on routine operational errors.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ObjectReadError {
    /// Verified integrity failure carrying the only valid corruption proof.
    #[error("committed object failed verification")]
    Corrupt(FailedReadProof),
    /// The store could not complete a verification. Mints no proof.
    #[error("object storage unavailable")]
    StorageUnavailable,
}

/// A conflicting same-digest publication.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("artifact metadata conflicts with existing scoped content")]
pub struct ArtifactMetadataConflict {
    /// Candidate digest.
    pub digest: Digest,
    /// Existing verified byte length.
    pub existing_size_bytes: u64,
    /// Candidate byte length.
    pub candidate_size_bytes: u64,
}

/// Object-store infrastructure and publication errors.
#[derive(Debug, thiserror::Error)]
pub enum ObjectStoreError {
    /// Same-digest content or metadata conflicts.
    #[error(transparent)]
    ArtifactMetadataConflict(#[from] ArtifactMetadataConflict),
    /// Durable storage is unavailable.
    #[error("object storage unavailable")]
    StorageUnavailable,
    /// Input or metadata is malformed.
    #[error("invalid object field")]
    InvalidField,
}

/// Scope-confined content-addressed object storage.
pub trait ObjectStore: Send + Sync {
    /// Durably publishes bytes if absent and returns a verified capability.
    fn put<'a>(
        &'a self,
        scope: &'a ExecutionScope,
        bytes: &'a [u8],
        media_type: &'a str,
    ) -> impl Future<Output = Result<VerifiedObjectRef, ObjectStoreError>> + Send + 'a;

    /// Reads and verifies committed bytes or mints a failed-read proof.
    async fn get(
        &self,
        scope: &ExecutionScope,
        digest: &Digest,
    ) -> Result<VerifiedObject, ObjectReadError>;

    /// Publishes a prepared object without replacement.
    async fn publish_if_absent(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError>;
}
