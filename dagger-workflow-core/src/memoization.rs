//! Scoped action memoization and the provenance of reused completions.

use crate::action::{ActionOutcome, ArtifactOutput, DiagnosticsEnvelope};
use crate::artifact::{ArtifactRef, ArtifactRefValue, ObjectReadError, ObjectStore};
use crate::definition::ActionReference;
use crate::ids::{artifact_ref_id, ArtifactRefIdentity, CostUnits, Digest, Id, NodeInstanceId};
use crate::run::NodeAttempt;
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

/// Immutable cache policy selected when a run is created.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    /// Reuse verified successful entries and publish new successful entries.
    #[default]
    Use,
    /// Execute actions and publish their accepted successful completions.
    Refresh,
    /// Execute actions without consulting or publishing cache entries.
    Off,
}

/// Why an action completion was reused.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReuseKind {
    /// A successful completion with identical effective inputs and action pins.
    CacheHit,
    /// A caller explicitly selected a successful source attempt.
    Pinned,
}

/// Durable source identity for a reused completion, within the attempt's scope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReuseProvenance {
    /// Source run identity.
    pub source_run_id: Id,
    /// Source static or synthetic node identity.
    pub source_node_id: NodeInstanceId,
    /// Source successful attempt identity.
    pub source_attempt_id: Id,
    /// Reuse policy that selected this source.
    pub kind: ReuseKind,
}

impl ReuseProvenance {
    pub(crate) fn from_attempt(attempt: &NodeAttempt, kind: ReuseKind) -> Self {
        Self {
            source_run_id: attempt.run_id.clone(),
            source_node_id: attempt.node_instance_id.clone(),
            source_attempt_id: attempt.attempt_id.clone(),
            kind,
        }
    }
}

/// Derives a key from scope, all five action pins and canonical effective inputs.
/// Typed artifact uses compare by content and metadata, so a new run's typed-use
/// identity does not prevent reuse of identical artifact bytes.
pub(crate) fn action_cache_key(
    scope: &ExecutionScope,
    action: &ActionReference,
    canonical_input: &[u8],
    artifact_uses: &[ArtifactRefValue],
) -> Result<Digest, crate::engine::EngineError> {
    let mut input: Value =
        serde_json::from_slice(canonical_input).map_err(|_| crate::engine::EngineError::Binding)?;
    let confirmed: BTreeMap<_, _> = artifact_uses
        .iter()
        .map(|reference| (reference.artifact_ref_id.clone(), reference))
        .collect();
    normalize_artifact_uses(&mut input, &confirmed);
    let bytes = serde_jcs::to_vec(&(
        "dagger-action-cache-v1",
        scope,
        &action.name,
        &action.contract_version,
        &action.input_schema_digest,
        &action.output_schema_digest,
        &action.compatible_implementation_requirement,
        input,
    ))
    .map_err(|_| crate::engine::EngineError::Binding)?;
    let hash = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Digest::new(format!("sha256:{hash}")).map_err(|_| crate::engine::EngineError::Binding)
}

fn normalize_artifact_uses(value: &mut Value, confirmed: &BTreeMap<Id, &ArtifactRefValue>) {
    if serde_json::from_value::<ArtifactRefValue>(value.clone()).is_ok_and(|reference| {
        confirmed
            .get(&reference.artifact_ref_id)
            .is_some_and(|known| **known == reference)
    }) {
        value
            .as_object_mut()
            .expect("artifact projection is object")
            .remove("artifact_ref_id");
        return;
    }
    match value {
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| normalize_artifact_uses(value, confirmed)),
        Value::Object(fields) => fields
            .values_mut()
            .for_each(|value| normalize_artifact_uses(value, confirmed)),
        _ => {}
    }
}

fn collect_artifact_uses(value: &Value, uses: &mut Vec<ArtifactRefValue>) {
    if let Ok(reference) = serde_json::from_value::<ArtifactRefValue>(value.clone()) {
        uses.push(reference);
        return;
    }
    match value {
        Value::Array(values) => values
            .iter()
            .for_each(|value| collect_artifact_uses(value, uses)),
        Value::Object(fields) => fields
            .values()
            .for_each(|value| collect_artifact_uses(value, uses)),
        _ => {}
    }
}

/// Collects only exact scoped durable artifact uses; matching user dictionaries
/// that do not name a registered typed use remain ordinary action input data.
pub(crate) async fn confirmed_artifact_uses<S: crate::store::WorkflowStore>(
    store: &S,
    scope: &ExecutionScope,
    input: &[u8],
) -> Result<Vec<ArtifactRefValue>, crate::engine::EngineError> {
    let value: Value =
        serde_json::from_slice(input).map_err(|_| crate::engine::EngineError::Binding)?;
    let mut uses = Vec::new();
    collect_artifact_uses(&value, &mut uses);
    let mut confirmed = Vec::new();
    for projection in uses {
        match store
            .lookup_artifact_ref(scope, &projection.artifact_ref_id)
            .await
        {
            Ok(reference)
                if reference.digest == projection.digest
                    && reference.size_bytes.to_string() == projection.size_bytes
                    && reference.media_type == projection.media_type =>
            {
                confirmed.push(projection)
            }
            Ok(_) | Err(crate::store::StoreError::NotFound) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(confirmed)
}

fn pair_input_artifact_uses(old: &Value, current: &Value, uses: &mut BTreeMap<Id, Id>) {
    if let (Ok(old), Ok(current)) = (
        serde_json::from_value::<ArtifactRefValue>(old.clone()),
        serde_json::from_value::<ArtifactRefValue>(current.clone()),
    ) {
        if old.digest == current.digest
            && old.size_bytes == current.size_bytes
            && old.media_type == current.media_type
        {
            uses.insert(old.artifact_ref_id, current.artifact_ref_id);
        }
        return;
    }
    match (old, current) {
        (Value::Array(old), Value::Array(current)) => old
            .iter()
            .zip(current)
            .for_each(|(old, current)| pair_input_artifact_uses(old, current, uses)),
        (Value::Object(old), Value::Object(current)) => {
            for (name, old) in old {
                if let Some(current) = current.get(name) {
                    pair_input_artifact_uses(old, current, uses);
                }
            }
        }
        _ => {}
    }
}

fn rehome_artifact_uses(value: &mut Value, uses: &BTreeMap<Id, (ArtifactRefValue, Id)>) {
    if let Ok(mut reference) = serde_json::from_value::<ArtifactRefValue>(value.clone()) {
        if let Some((_, id)) = uses
            .get(&reference.artifact_ref_id)
            .filter(|(original, _)| *original == reference)
        {
            reference.artifact_ref_id = id.clone();
            *value = serde_json::to_value(reference).expect("artifact projection serializes");
        }
        return;
    }
    match value {
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| rehome_artifact_uses(value, uses)),
        Value::Object(fields) => fields
            .values_mut()
            .for_each(|value| rehome_artifact_uses(value, uses)),
        _ => {}
    }
}

fn metadata_matches(reference: &ArtifactRef, object: &crate::artifact::VerifiedObjectRef) -> bool {
    reference.scope == *object.scope()
        && reference.digest == *object.digest()
        && reference.size_bytes == object.size_bytes()
        && reference.media_type == object.media_type()
}

/// Verifies a source without a committed-read corruption transition. A corrupt
/// cache entry is a miss; storage unavailability remains an infrastructure error.
/// Binary artifacts are verified through the streaming ObjectStore boundary.
pub(crate) async fn reuse_attempt_outcome<S: crate::store::WorkflowStore, O: ObjectStore>(
    store: &S,
    objects: &O,
    scope: &ExecutionScope,
    source: &NodeAttempt,
    run_id: &Id,
    node_id: &NodeInstanceId,
    attempt_id: &Id,
    bound_input: &[u8],
) -> Result<Option<ActionOutcome>, crate::engine::EngineError> {
    let Some(output_ref) = &source.output_ref else {
        return Ok(None);
    };
    let output_object = match objects.get(scope, &output_ref.0.digest).await {
        Ok(object) if metadata_matches(&output_ref.0, &object.reference) => object,
        Ok(_) | Err(ObjectReadError::Corrupt(_)) => return Ok(None),
        Err(ObjectReadError::StorageUnavailable) => {
            return Err(crate::engine::EngineError::StorageUnavailable)
        }
    };
    let mut output: Value = serde_json::from_slice(&output_object.bytes)
        .map_err(|_| crate::engine::EngineError::ObjectRead)?;
    let mut input_correspondence = BTreeMap::new();
    let bound: Value =
        serde_json::from_slice(bound_input).map_err(|_| crate::engine::EngineError::Binding)?;
    let invocation = match store
        .get_action_invocation(scope, &source.run_id, &source.invocation_id)
        .await
    {
        Ok(invocation) => invocation,
        Err(crate::store::StoreError::NotFound) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let source_input = match objects
        .get(scope, &invocation.bound_input_ref.0.digest)
        .await
    {
        Ok(object) if metadata_matches(&invocation.bound_input_ref.0, &object.reference) => object,
        Ok(_) | Err(ObjectReadError::Corrupt(_)) => return Ok(None),
        Err(ObjectReadError::StorageUnavailable) => {
            return Err(crate::engine::EngineError::StorageUnavailable)
        }
    };
    let source_bound: Value = serde_json::from_slice(&source_input.bytes)
        .map_err(|_| crate::engine::EngineError::ObjectRead)?;
    pair_input_artifact_uses(&source_bound, &bound, &mut input_correspondence);
    let old_confirmed = confirmed_artifact_uses(store, scope, &source_input.bytes).await?;
    let current_confirmed = confirmed_artifact_uses(store, scope, bound_input).await?;
    for projection in &current_confirmed {
        match objects.verify(scope, &projection.digest).await {
            Ok(object)
                if object.scope() == scope
                    && object.digest() == &projection.digest
                    && object.size_bytes().to_string() == projection.size_bytes
                    && object.media_type() == projection.media_type => {}
            Ok(_) | Err(ObjectReadError::Corrupt(_)) => return Ok(None),
            Err(ObjectReadError::StorageUnavailable) => {
                return Err(crate::engine::EngineError::StorageUnavailable)
            }
        }
    }
    input_correspondence.retain(|old, current| {
        old_confirmed
            .iter()
            .any(|reference| reference.artifact_ref_id == *old)
            && current_confirmed
                .iter()
                .any(|reference| reference.artifact_ref_id == *current)
    });
    let mut replacements = BTreeMap::new();
    let mut artifacts = Vec::with_capacity(source.artifact_refs.len());
    for reference in &source.artifact_refs {
        let object = match objects.verify(scope, &reference.digest).await {
            Ok(object) if metadata_matches(reference, &object) => object,
            Ok(_) | Err(ObjectReadError::Corrupt(_)) => return Ok(None),
            Err(ObjectReadError::StorageUnavailable) => {
                return Err(crate::engine::EngineError::StorageUnavailable)
            }
        };
        let id = artifact_ref_id(ArtifactRefIdentity {
            scope,
            digest: &reference.digest,
            kind: reference.kind,
            producer_run_id: Some(run_id),
            producer_node_id: Some(node_id),
            producer_attempt_id: Some(attempt_id),
            ordinal: reference.ordinal,
        });
        replacements.insert(
            reference.artifact_ref_id.clone(),
            (
                ArtifactRefValue {
                    artifact_ref_id: reference.artifact_ref_id.clone(),
                    digest: reference.digest.clone(),
                    size_bytes: reference.size_bytes.to_string(),
                    media_type: reference.media_type.clone(),
                },
                id,
            ),
        );
        artifacts.push(ArtifactOutput {
            media_type: reference.media_type.clone(),
            object,
        });
    }
    let mut diagnostics = match &source.diagnostics_ref {
        Some(reference) => {
            let object = match objects.get(scope, &reference.0.digest).await {
                Ok(object) if metadata_matches(&reference.0, &object.reference) => object,
                Ok(_) | Err(ObjectReadError::Corrupt(_)) => return Ok(None),
                Err(ObjectReadError::StorageUnavailable) => {
                    return Err(crate::engine::EngineError::StorageUnavailable)
                }
            };
            let value: Value = serde_json::from_slice(&object.bytes)
                .map_err(|_| crate::engine::EngineError::ObjectRead)?;
            Some(value)
        }
        None => None,
    };
    let mut projected_refs = Vec::new();
    collect_artifact_uses(&output, &mut projected_refs);
    if let Some(value) = &diagnostics {
        collect_artifact_uses(value, &mut projected_refs);
    }
    for projection in projected_refs {
        let old_id = projection.artifact_ref_id.clone();
        if replacements
            .get(&old_id)
            .is_some_and(|(original, _)| *original == projection)
        {
            continue;
        }
        // Corresponding current-input uses identify the same bytes, but output
        // uses always belong to this consumer attempt so source retention cannot
        // remove them. Only exact authenticated projections are rewritten.
        let lookup_id = if old_confirmed.contains(&projection) {
            input_correspondence.get(&old_id).unwrap_or(&old_id)
        } else {
            &old_id
        };
        let reference = match store.lookup_artifact_ref(scope, lookup_id).await {
            Ok(reference) => reference,
            Err(crate::store::StoreError::NotFound) => continue,
            Err(error) => return Err(error.into()),
        };
        if reference.digest != projection.digest
            || reference.size_bytes.to_string() != projection.size_bytes
            || reference.media_type != projection.media_type
        {
            continue;
        }
        let object = match objects.verify(scope, &reference.digest).await {
            Ok(object) if metadata_matches(&reference, &object) => object,
            Ok(_) | Err(ObjectReadError::Corrupt(_)) => return Ok(None),
            Err(ObjectReadError::StorageUnavailable) => {
                return Err(crate::engine::EngineError::StorageUnavailable)
            }
        };
        let ordinal =
            u32::try_from(artifacts.len()).map_err(|_| crate::engine::EngineError::ObjectWrite)?;
        let id = artifact_ref_id(ArtifactRefIdentity {
            scope,
            digest: &reference.digest,
            kind: crate::artifact::ArtifactKind::ActionArtifact,
            producer_run_id: Some(run_id),
            producer_node_id: Some(node_id),
            producer_attempt_id: Some(attempt_id),
            ordinal,
        });
        replacements.insert(old_id, (projection, id));
        artifacts.push(ArtifactOutput {
            media_type: reference.media_type,
            object,
        });
    }
    rehome_artifact_uses(&mut output, &replacements);
    if let Some(value) = &mut diagnostics {
        rehome_artifact_uses(value, &replacements);
    }
    let diagnostics = diagnostics
        .map(serde_json::from_value::<DiagnosticsEnvelope>)
        .transpose()
        .map_err(|_| crate::engine::EngineError::ObjectRead)?;
    Ok(Some(ActionOutcome::Success {
        output,
        artifacts,
        diagnostics,
        actual_cost_units: CostUnits(0),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::ScopeAtom;
    fn digest(value: &str) -> Digest {
        Digest::new(format!("sha256:{:x}", Sha256::digest(value.as_bytes()))).unwrap()
    }
    #[test]
    fn every_pin_and_input_and_scope_dimension_enters_the_key() {
        let scope = ExecutionScope {
            tenant_id: ScopeAtom::new("tenant").unwrap(),
            namespace: ScopeAtom::new("namespace").unwrap(),
        };
        let action = ActionReference {
            name: "test".into(),
            contract_version: "1".into(),
            input_schema_digest: digest("input"),
            output_schema_digest: digest("output"),
            compatible_implementation_requirement: digest("implementation"),
        };
        let key = action_cache_key(&scope, &action, br#"{"value":1}"#, &[]).unwrap();
        for dimension in 0..5 {
            let mut changed = action.clone();
            match dimension {
                0 => changed.name.push('2'),
                1 => changed.contract_version.push('2'),
                2 => changed.input_schema_digest = digest("changed-input"),
                3 => changed.output_schema_digest = digest("changed-output"),
                _ => {
                    changed.compatible_implementation_requirement = digest("changed-implementation")
                }
            }
            assert_ne!(
                key,
                action_cache_key(&scope, &changed, br#"{"value":1}"#, &[]).unwrap()
            );
        }
        assert_ne!(
            key,
            action_cache_key(&scope, &action, br#"{"value":2}"#, &[]).unwrap()
        );
        let mut foreign = scope.clone();
        foreign.namespace = ScopeAtom::new("other").unwrap();
        assert_ne!(
            key,
            action_cache_key(&foreign, &action, br#"{"value":1}"#, &[]).unwrap()
        );
        foreign = scope.clone();
        foreign.tenant_id = ScopeAtom::new("other").unwrap();
        assert_ne!(
            key,
            action_cache_key(&foreign, &action, br#"{"value":1}"#, &[]).unwrap()
        );
    }
}
