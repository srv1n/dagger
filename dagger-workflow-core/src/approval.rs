//! Durable approval types from contract sections 1.9 and 3.5.

use crate::artifact::JsonRef;
use crate::ids::{Digest, Id, NodeInstanceId, Timestamp, Version};
use crate::run::GateState;
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};

/// The closed decision vocabulary. Contract section 3.5.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Approve the gate. Contract section 3.5.
    Approve,
    /// Reject the gate. Contract section 3.5.
    Reject,
}

/// The closed gate expiry policy. Contract section 1.9.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalExpiryPolicy {
    /// Approve when the gate expires. Contract section 3.5.
    Approve,
    /// Reject when the gate expires. Contract section 3.5.
    #[default]
    Reject,
}

/// The closed gate resolution source vocabulary. Contract section 1.9.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalResolutionSource {
    /// An authenticated human decision. Contract section 3.5.
    Human,
    /// Database-clock expiry. Contract section 3.5.
    Expiry,
    /// Run terminalization cancellation. Contract section 3.5.
    Cancellation,
}

/// Immutable principal and role allowlists. Contract section 1.9.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAuthorizationPolicy {
    /// Ordered unique allowed principal IDs. Contract section 1.9.
    pub allowed_principal_ids: Vec<String>,
    /// Ordered unique allowed role IDs. Contract section 1.9.
    pub allowed_role_ids: Vec<String>,
}

/// An opaque host-authentication capability bound to one scope. Contract section 1.1.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedPrincipal {
    scope: ExecutionScope,
    principal_id: String,
    role_ids: Vec<String>,
    authentication_context_digest: Digest,
}

impl AuthenticatedPrincipal {
    /// Mints a scope-bound principal from host-authenticated facts. Contract section 16.1.
    pub fn mint(
        scope: ExecutionScope,
        principal_id: String,
        mut role_ids: Vec<String>,
        authentication_context_digest: Digest,
    ) -> Result<Self, PrincipalError> {
        if principal_id.is_empty() || principal_id.len() > 256 {
            return Err(PrincipalError::InvalidPrincipalId);
        }
        if role_ids
            .iter()
            .any(|role| role.is_empty() || role.len() > 256)
        {
            return Err(PrincipalError::InvalidRoleId);
        }
        role_ids.sort();
        role_ids.dedup();
        Ok(Self {
            scope,
            principal_id,
            role_ids,
            authentication_context_digest,
        })
    }

    /// Returns the bound scope. Contract section 16.1.
    pub fn scope(&self) -> &ExecutionScope {
        &self.scope
    }

    /// Returns the authenticated principal ID. Contract section 3.5.
    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }

    /// Returns the authenticated role IDs. Contract section 3.5.
    pub fn role_ids(&self) -> &[String] {
        &self.role_ids
    }

    /// Returns the authentication-context digest. Contract section 3.5.
    pub fn authentication_context_digest(&self) -> &Digest {
        &self.authentication_context_digest
    }
}

/// A principal-capability construction failure. Contract section 16.1.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PrincipalError {
    /// The principal ID violates its byte bounds. Contract section 3.5.
    #[error("invalid principal id")]
    InvalidPrincipalId,
    /// A role ID violates its byte bounds. Contract section 3.5.
    #[error("invalid role id")]
    InvalidRoleId,
}

/// One durable first-valid-decision gate. Contract section 1.9.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalGate {
    /// Gate scope. Contract section 1.9.
    pub scope: ExecutionScope,
    /// Owning run ID. Contract section 1.9.
    pub run_id: Id,
    /// Gate ID. Contract section 1.9.
    pub gate_id: Id,
    /// Owning node instance. Contract section 1.9.
    pub node_instance_id: NodeInstanceId,
    /// Immutable request ref. Contract section 1.9.
    pub request_ref: JsonRef,
    /// Durable gate state. Contract section 2.4.
    pub status: GateState,
    /// Database-clock expiry. Contract section 1.9.
    pub expires_at: Timestamp,
    /// Immutable expiry behavior. Contract section 1.9.
    pub on_expiry: ApprovalExpiryPolicy,
    /// Immutable decision authorization. Contract section 1.9.
    pub authorization_policy: DecisionAuthorizationPolicy,
    /// Optional human decision payload. Contract section 1.9.
    pub decision_payload_ref: Option<JsonRef>,
    /// Optional authenticated deciding principal ID. Contract section 1.9.
    pub deciding_principal: Option<String>,
    /// Optional terminal resolution source. Contract section 1.9.
    pub resolution_source: Option<ApprovalResolutionSource>,
    /// Optional decision timestamp. Contract section 1.9.
    pub decided_at: Option<Timestamp>,
    /// Optional exact replay fingerprint. Contract section 3.5.
    pub decision_fingerprint: Option<Digest>,
    /// Gate CAS version. Contract section 1.9.
    pub version: Version,
}

/// The fixed successful approval-node output envelope. Contract section 3.5.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalResult {
    /// Always approve for a successful approval result. Contract section 3.5.
    pub decision: ApprovalDecision,
    /// Human or expiry resolution source. Contract section 3.5.
    pub source: ApprovalResultSource,
    /// Exact canonical decision-payload ArtifactRef value. Contract section 3.5.
    pub payload_ref: Option<crate::artifact::ArtifactRefValue>,
    /// Human principal ID or null for expiry. Contract section 3.5.
    pub principal: Option<String>,
}

/// The closed successful ApprovalResult source. Contract section 3.5.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalResultSource {
    /// An authenticated human approval. Contract section 3.5.
    Human,
    /// An approve-on-expiry resolution. Contract section 3.5.
    Expiry,
}

/// Constructs the canonical human approval result bytes. Contract section 3.5.
pub fn canonical_human_approval_result(
    payload_ref: Option<crate::artifact::ArtifactRefValue>,
    principal: &AuthenticatedPrincipal,
) -> Vec<u8> {
    serde_jcs::to_vec(&ApprovalResult {
        decision: ApprovalDecision::Approve,
        source: ApprovalResultSource::Human,
        payload_ref,
        principal: Some(principal.principal_id().to_owned()),
    })
    .expect("closed approval result serializes")
}

/// Constructs the canonical expiry approval result bytes. Contract section 3.5.
pub fn canonical_expiry_approval_result() -> Vec<u8> {
    serde_jcs::to_vec(&ApprovalResult {
        decision: ApprovalDecision::Approve,
        source: ApprovalResultSource::Expiry,
        payload_ref: None,
        principal: None,
    })
    .expect("closed approval result serializes")
}
