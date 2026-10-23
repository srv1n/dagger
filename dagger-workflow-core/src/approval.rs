//! Durable approval types.

use crate::artifact::JsonRef;
use crate::ids::{Digest, Id, NodeInstanceId, Timestamp, Version};
use crate::run::GateState;
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};

/// The closed decision vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Approve the gate.
    Approve,
    /// Reject the gate.
    Reject,
}

/// The closed gate expiry policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalExpiryPolicy {
    /// Approve when the gate expires.
    Approve,
    /// Reject when the gate expires.
    #[default]
    Reject,
}

/// The closed gate resolution source vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalResolutionSource {
    /// An authenticated human decision.
    Human,
    /// Database-clock expiry.
    Expiry,
    /// Run terminalization cancellation.
    Cancellation,
}

/// Immutable principal and role allowlists.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAuthorizationPolicy {
    /// Ordered unique allowed principal IDs.
    pub allowed_principal_ids: Vec<String>,
    /// Ordered unique allowed role IDs.
    pub allowed_role_ids: Vec<String>,
}

/// An opaque host-authentication capability bound to one scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedPrincipal {
    scope: ExecutionScope,
    principal_id: String,
    role_ids: Vec<String>,
    authentication_context_digest: Digest,
}

impl AuthenticatedPrincipal {
    /// Mints a scope-bound principal from host-authenticated facts.
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

    /// Returns the bound scope.
    pub fn scope(&self) -> &ExecutionScope {
        &self.scope
    }

    /// Returns the authenticated principal ID.
    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }

    /// Returns the authenticated role IDs.
    pub fn role_ids(&self) -> &[String] {
        &self.role_ids
    }

    /// Returns the authentication-context digest.
    pub fn authentication_context_digest(&self) -> &Digest {
        &self.authentication_context_digest
    }
}

/// A principal-capability construction failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PrincipalError {
    /// The principal ID violates its byte bounds.
    #[error("invalid principal id")]
    InvalidPrincipalId,
    /// A role ID violates its byte bounds.
    #[error("invalid role id")]
    InvalidRoleId,
}

/// One durable first-valid-decision gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalGate {
    /// Gate scope.
    pub scope: ExecutionScope,
    /// Owning run ID.
    pub run_id: Id,
    /// Gate ID.
    pub gate_id: Id,
    /// Owning node instance.
    pub node_instance_id: NodeInstanceId,
    /// Immutable request ref.
    pub request_ref: JsonRef,
    /// Durable gate state.
    pub status: GateState,
    /// Database-clock expiry.
    pub expires_at: Timestamp,
    /// Immutable expiry behavior.
    pub on_expiry: ApprovalExpiryPolicy,
    /// Immutable decision authorization.
    pub authorization_policy: DecisionAuthorizationPolicy,
    /// Optional human decision payload.
    pub decision_payload_ref: Option<JsonRef>,
    /// Optional authenticated deciding principal ID.
    pub deciding_principal: Option<String>,
    /// Optional terminal resolution source.
    pub resolution_source: Option<ApprovalResolutionSource>,
    /// Optional decision timestamp.
    pub decided_at: Option<Timestamp>,
    /// Optional exact replay fingerprint.
    pub decision_fingerprint: Option<Digest>,
    /// Gate CAS version.
    pub version: Version,
}

/// The fixed successful approval-node output envelope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalResult {
    /// Always approve for a successful approval result.
    pub decision: ApprovalDecision,
    /// Human or expiry resolution source.
    pub source: ApprovalResultSource,
    /// Exact canonical decision-payload ArtifactRef value.
    pub payload_ref: Option<crate::artifact::ArtifactRefValue>,
    /// Human principal ID or null for expiry.
    pub principal: Option<String>,
}

/// The closed successful ApprovalResult source.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalResultSource {
    /// An authenticated human resolution.
    Human,
    /// A database-clock expiry resolution.
    Expiry,
}

/// Constructs the canonical human approval result bytes.
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

/// Constructs the canonical expiry approval result bytes.
pub fn canonical_expiry_approval_result() -> Vec<u8> {
    serde_jcs::to_vec(&ApprovalResult {
        decision: ApprovalDecision::Approve,
        source: ApprovalResultSource::Expiry,
        payload_ref: None,
        principal: None,
    })
    .expect("closed approval result serializes")
}

/// Successful output of an Approval configured to resolve decisions as data.
///
/// Human payloads are checked against the pinned decision schema. Expiry is a
/// separate source and always carries null, irrespective of that human schema.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalDecisionResult {
    /// Approve or reject; both are successful node resolutions in this mode.
    pub decision: ApprovalDecision,
    /// Human or expiry resolution source.
    pub source: ApprovalResultSource,
    /// Direct JSON available to downstream bindings and Choice selectors.
    pub payload: serde_json::Value,
    /// Optional durable typed reference to the same human payload bytes.
    pub payload_ref: Option<crate::artifact::ArtifactRefValue>,
    /// Human principal ID or null for expiry.
    pub principal: Option<String>,
}

/// Constructs the canonical successful human decision envelope.
pub fn canonical_human_decision_result(
    decision: ApprovalDecision,
    payload: serde_json::Value,
    payload_ref: Option<crate::artifact::ArtifactRefValue>,
    principal: &AuthenticatedPrincipal,
) -> Vec<u8> {
    serde_jcs::to_vec(&ApprovalDecisionResult {
        decision,
        source: ApprovalResultSource::Human,
        payload,
        payload_ref,
        principal: Some(principal.principal_id().to_owned()),
    })
    .expect("closed approval decision result serializes")
}

/// Constructs the canonical expiry decision envelope, whose payload is null.
pub fn canonical_expiry_decision_result(decision: ApprovalDecision) -> Vec<u8> {
    serde_jcs::to_vec(&ApprovalDecisionResult {
        decision,
        source: ApprovalResultSource::Expiry,
        payload: serde_json::Value::Null,
        payload_ref: None,
        principal: None,
    })
    .expect("closed expiry decision result serializes")
}

/// Publishes a JSON decision and its exact envelope, then submits the store CAS.
///
/// The store enforces the scope, allowlist, schema, immutable payload bytes, and
/// exact replay fingerprint. This helper supplies current versions; concurrent
/// decisions can still lose the race. Legacy gates retain rejection failure.
#[allow(clippy::too_many_arguments)]
pub async fn decide_approval_json<
    S: crate::store::WorkflowStore,
    O: crate::artifact::ObjectStore,
>(
    store: &S,
    objects: &O,
    scope: &ExecutionScope,
    run_id: Id,
    gate_id: Id,
    decision: ApprovalDecision,
    payload: Option<serde_json::Value>,
    principal: AuthenticatedPrincipal,
) -> Result<ApprovalGate, crate::store::StoreError> {
    use crate::artifact::{ArtifactKind, ArtifactRefValue};
    use crate::ids::{artifact_ref_id, ArtifactRefIdentity};
    use crate::store::{DecideApproval, StoreError};

    if principal.scope() != scope {
        return Err(StoreError::ApprovalUnauthorized);
    }
    let gate = store.get_gate(scope, &run_id, &gate_id).await?;
    let run = store.get_run(scope, &run_id).await?.run;
    let revision = store
        .get_revision(scope, &run.definition_id, &run.revision_hash)
        .await?;
    let object = objects
        .get(scope, &revision.canonical_definition_ref.0.digest)
        .await
        .map_err(|error| match error {
            crate::artifact::ObjectReadError::StorageUnavailable => StoreError::StorageUnavailable,
            crate::artifact::ObjectReadError::Corrupt(proof) => {
                StoreError::CommittedObjectCorrupt {
                    bad_ref: revision.canonical_definition_ref.0.clone(),
                    proof,
                }
            }
        })?;
    let definition: crate::definition::WorkflowDefinition =
        serde_json::from_slice(&object.bytes).map_err(|_| StoreError::CorruptControlPlane)?;
    let node = store
        .get_node(scope, &run_id, &gate.node_instance_id)
        .await?;
    let config = definition
        .nodes
        .iter()
        .find_map(|candidate| match candidate {
            crate::definition::NodeDefinition::Approval { id, gate, .. }
                if id == &node.definition_node_id =>
            {
                Some(gate)
            }
            _ => None,
        })
        .ok_or(StoreError::CorruptControlPlane)?;
    let decision_payload = if let Some(value) = &payload {
        let bytes =
            serde_jcs::to_vec(value).map_err(|_| StoreError::ApprovalDecisionPayloadInvalid)?;
        Some(
            objects
                .put(scope, &bytes, "application/json")
                .await
                .map_err(|_| StoreError::StorageUnavailable)?,
        )
    } else {
        None
    };
    let payload_ref = decision_payload.as_ref().map(|payload| ArtifactRefValue {
        artifact_ref_id: artifact_ref_id(ArtifactRefIdentity {
            scope,
            digest: payload.digest(),
            kind: ArtifactKind::ApprovalDecisionPayload,
            producer_run_id: Some(&run_id),
            producer_node_id: Some(&gate.node_instance_id),
            producer_attempt_id: None,
            ordinal: 0,
        }),
        digest: payload.digest().clone(),
        size_bytes: payload.size_bytes().to_string(),
        media_type: payload.media_type().to_owned(),
    });
    let output_bytes = if config.decision_output {
        Some(canonical_human_decision_result(
            decision,
            payload.unwrap_or(serde_json::Value::Null),
            payload_ref,
            &principal,
        ))
    } else if decision == ApprovalDecision::Approve {
        Some(canonical_human_approval_result(payload_ref, &principal))
    } else {
        None
    };
    let approval_output = if let Some(bytes) = output_bytes {
        Some(
            objects
                .put(scope, &bytes, "application/json")
                .await
                .map_err(|_| StoreError::StorageUnavailable)?,
        )
    } else {
        None
    };
    store
        .decide_approval(
            scope,
            DecideApproval {
                run_id,
                gate_id,
                expected_run_version: run.version,
                expected_gate_version: gate.version,
                decision,
                decision_payload,
                approval_output,
                principal,
            },
        )
        .await
}
