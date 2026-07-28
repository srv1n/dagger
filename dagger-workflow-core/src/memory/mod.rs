//! Deterministic in-memory implementations of the frozen store boundaries.

use crate::action::{ActionInvocation, ActionOutcome, CompletionCredential};
use crate::approval::{ApprovalDecision, ApprovalGate, ApprovalResolutionSource};
use crate::artifact::{
    ArtifactKind, ArtifactMetadataConflict, ArtifactRef, FailedReadClass, FailedReadProof, JsonRef,
    ObjectReadError, ObjectStore, ObjectStoreError, VerifiedObject, VerifiedObjectRef,
};
use crate::budget::{BudgetLedgerEntry, BudgetLedgerKind, BudgetLedgerReason};
use crate::definition::{ActionPin, BackoffPolicy, NodeDefinition, PublishableDefinition};
use crate::engine::Clock;
use crate::event::{EventActorKind, EventType, WorkflowEvent};
use crate::ids::{
    artifact_ref_id, edge_id, idempotency_key, ArtifactRefIdentity, CostUnits, Digest, Id,
    NodeInstanceId, Timestamp, Version,
};
use crate::revision::WorkflowRevision;
use crate::run::{
    AttemptErrorClass, AttemptState, BlockedFromState, ChoiceSelection, EdgeFact, EdgeState,
    NodeAttempt, NodeFailureKind, NodeKind, NodeRun, NodeState, RunFailureKind,
    RunOperationalCounts, RunOperationalView, RunState, WorkflowRun, WorkflowRunView,
};
use crate::scope::ExecutionScope;
use crate::store::*;
use serde::Deserialize;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::{Arc, Mutex};

const CLAIM_LIFETIME_MS: i64 = 20_000;

fn digest(bytes: &[u8]) -> Digest {
    let value = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Digest::new(format!("sha256:{value}")).expect("SHA-256 is a valid digest")
}

fn entropy() -> Result<[u8; 32], StoreError> {
    let mut bytes = [0_u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| StoreError::StorageUnavailable)?;
    Ok(bytes)
}

fn checked_add_time(now: Timestamp, delta: u64) -> Result<Timestamp, StoreError> {
    let delta = i64::try_from(delta).map_err(|_| StoreError::ArithmeticOverflow)?;
    now.0
        .checked_add(delta)
        .map(Timestamp)
        .ok_or(StoreError::ArithmeticOverflow)
}

fn valid_metadata(display_name: &str, description: &str) -> bool {
    !display_name.is_empty() && display_name.len() <= 200 && description.len() <= 4_000
}

/// Scope-confined content-addressed volatile object storage.
pub struct InMemoryObjectStore<C> {
    clock: Arc<C>,
    nonce: Vec<u8>,
    objects: Mutex<BTreeMap<(ExecutionScope, Digest), MemoryObject>>,
}

#[derive(Clone)]
struct MemoryObject {
    bytes: Vec<u8>,
    media_type: String,
    object_key: String,
}

impl<C: Clock> InMemoryObjectStore<C> {
    /// Creates an empty object store using the supplied store clock.
    pub fn new(clock: Arc<C>) -> Self {
        Self {
            clock,
            nonce: entropy().unwrap_or([0x5a; 32]).to_vec(),
            objects: Mutex::new(BTreeMap::new()),
        }
    }

    /// Test-only corruption hook that replaces committed bytes without metadata changes.
    pub fn corrupt_bytes(
        &self,
        scope: &ExecutionScope,
        object_digest: &Digest,
        bytes: Vec<u8>,
    ) -> bool {
        let mut objects = self.objects.lock().expect("object lock poisoned");
        if let Some(object) = objects.get_mut(&(scope.clone(), object_digest.clone())) {
            object.bytes = bytes;
            true
        } else {
            false
        }
    }

    fn proof(
        &self,
        scope: &ExecutionScope,
        requested: &Digest,
        class: FailedReadClass,
        observed: Option<Digest>,
    ) -> FailedReadProof {
        FailedReadProof::mint(
            scope.clone(),
            requested.clone(),
            class,
            observed,
            self.nonce.clone(),
            entropy().unwrap_or([0xa5; 32]).to_vec(),
            self.clock.now(),
        )
    }
}

impl<C: Clock> ObjectStore for InMemoryObjectStore<C> {
    async fn put(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        self.publish_if_absent(scope, bytes, media_type).await
    }

    async fn get(
        &self,
        scope: &ExecutionScope,
        requested: &Digest,
    ) -> Result<VerifiedObject, ObjectReadError> {
        let object = self
            .objects
            .lock()
            .expect("object lock poisoned")
            .get(&(scope.clone(), requested.clone()))
            .cloned()
            .ok_or_else(|| ObjectReadError {
                proof: self.proof(scope, requested, FailedReadClass::Missing, None),
            })?;
        let observed = digest(&object.bytes);
        if &observed != requested {
            return Err(ObjectReadError {
                proof: self.proof(
                    scope,
                    requested,
                    FailedReadClass::DigestInvalid,
                    Some(observed),
                ),
            });
        }
        Ok(VerifiedObject {
            reference: VerifiedObjectRef::new(
                scope.clone(),
                requested.clone(),
                object.bytes.len() as u64,
                object.media_type,
                object.object_key,
                self.nonce.clone(),
            ),
            bytes: object.bytes,
        })
    }

    async fn publish_if_absent(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        if media_type.is_empty() || media_type.len() > 255 {
            return Err(ObjectStoreError::InvalidField);
        }
        let object_digest = digest(bytes);
        let key = (scope.clone(), object_digest.clone());
        let mut objects = self.objects.lock().expect("object lock poisoned");
        if let Some(existing) = objects.get(&key) {
            if existing.bytes != bytes || existing.media_type != media_type {
                return Err(ArtifactMetadataConflict {
                    digest: object_digest,
                    existing_size_bytes: existing.bytes.len() as u64,
                    candidate_size_bytes: bytes.len() as u64,
                }
                .into());
            }
            return Ok(VerifiedObjectRef::new(
                scope.clone(),
                key.1,
                bytes.len() as u64,
                media_type.to_owned(),
                existing.object_key.clone(),
                self.nonce.clone(),
            ));
        }
        let object_key = format!(
            "{}/{}/{}",
            scope.tenant_id.as_str(),
            scope.namespace.as_str(),
            object_digest.as_str()
        );
        objects.insert(
            key,
            MemoryObject {
                bytes: bytes.to_vec(),
                media_type: media_type.to_owned(),
                object_key: object_key.clone(),
            },
        );
        Ok(VerifiedObjectRef::new(
            scope.clone(),
            object_digest,
            bytes.len() as u64,
            media_type.to_owned(),
            object_key,
            self.nonce.clone(),
        ))
    }
}

#[derive(Clone, Default)]
struct MemoryState {
    object_store_nonce: Option<Vec<u8>>,
    object_records: BTreeMap<(ExecutionScope, Digest), crate::artifact::ObjectRecord>,
    artifact_refs: BTreeMap<(ExecutionScope, Id), ArtifactRef>,
    definitions: BTreeMap<(ExecutionScope, Id), DefinitionRecord>,
    revisions: BTreeMap<(ExecutionScope, Id, Digest), WorkflowRevision>,
    parsed_revisions: BTreeMap<(ExecutionScope, Id, Digest), PublishableDefinition>,
    claims: BTreeMap<ExecutionScope, StoredClaim>,
    runs: BTreeMap<(ExecutionScope, Id), WorkflowRun>,
    run_definitions: BTreeMap<(ExecutionScope, Id), PublishableDefinition>,
    nodes: BTreeMap<(ExecutionScope, Id, NodeInstanceId), NodeRun>,
    edges: BTreeMap<(ExecutionScope, Id, Id), EdgeFact>,
    attempts: BTreeMap<(ExecutionScope, Id, Id), NodeAttempt>,
    invocations: BTreeMap<(ExecutionScope, Id, Id), ActionInvocation>,
    gates: BTreeMap<(ExecutionScope, Id, Id), ApprovalGate>,
    receipts: BTreeMap<(ExecutionScope, CommandKindKey, String), CommandReceipt>,
    events: BTreeMap<(ExecutionScope, Id), Vec<WorkflowEvent>>,
    ledger: BTreeMap<(ExecutionScope, Id), Vec<BudgetLedgerEntry>>,
    stale_observed: BTreeSet<(ExecutionScope, Id, Id)>,
    batch_counter: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CommandKindKey {
    Create,
    Cancel,
}

#[derive(Clone)]
struct StoredClaim {
    claim: EngineClaim,
    token_digest: Digest,
    released_token_digest: Option<Digest>,
}

/// Transactional volatile implementation of the real [`WorkflowStore`] trait.
pub struct InMemoryStore<C> {
    clock: Arc<C>,
    state: Mutex<MemoryState>,
}

impl<C: Clock> InMemoryStore<C> {
    /// Creates an empty control-plane store using one database-equivalent clock.
    pub fn new(clock: Arc<C>) -> Self {
        Self {
            clock,
            state: Mutex::new(MemoryState::default()),
        }
    }

    /// Returns the immutable budget ledger for conformance and diagnostics.
    pub fn budget_ledger(&self, scope: &ExecutionScope, run_id: &Id) -> Vec<BudgetLedgerEntry> {
        self.state
            .lock()
            .expect("store lock poisoned")
            .ledger
            .get(&(scope.clone(), run_id.clone()))
            .cloned()
            .unwrap_or_default()
    }

    /// Returns one immutable invocation.
    pub fn get_invocation(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        invocation_id: &Id,
    ) -> Result<ActionInvocation, StoreError> {
        self.state
            .lock()
            .expect("store lock poisoned")
            .invocations
            .get(&(scope.clone(), run_id.clone(), invocation_id.clone()))
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    fn now(&self) -> Timestamp {
        self.clock.now()
    }

    fn transaction<T>(
        &self,
        operation: impl FnOnce(&mut MemoryState, Timestamp) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut committed = self.state.lock().expect("store lock poisoned");
        let now = self.clock.now();
        let mut staged = committed.clone();
        match operation(&mut staged, now) {
            Ok(value) => {
                *committed = staged;
                Ok(value)
            }
            Err(error) => {
                let event_capacity = matches!(
                    &error,
                    StoreError::RunLimitApplied { code }
                        if code == "RunEventLimitExceeded"
                );
                if !event_capacity {
                    return Err(error);
                }
                let affected = staged.runs.iter().find_map(|(key, run)| {
                    committed
                        .runs
                        .get(key)
                        .filter(|prior| prior.version != run.version || prior.status != run.status)
                        .map(|_| key.clone())
                });
                if let Some((scope, run_id)) = affected {
                    let mut capacity_state = committed.clone();
                    let prior = capacity_state
                        .runs
                        .get(&(scope.clone(), run_id.clone()))
                        .cloned()
                        .ok_or(StoreError::NotFound)?;
                    if !prior.status.is_terminal() {
                        let mut specs = Vec::new();
                        terminalize_run(
                            &mut capacity_state,
                            &scope,
                            &run_id,
                            RunState::Cancelled,
                            None,
                            "RunEventLimitExceeded",
                            now,
                            &mut specs,
                        )?;
                        let transition = match prior.status {
                            RunState::Pending => "R11",
                            RunState::Running => "R12",
                            _ => "R13",
                        };
                        specs.push(event_spec(
                            EventType::RunCancelled,
                            transition,
                            None,
                            None,
                            None,
                            json!({"reason_code": "RunEventLimitExceeded", "prior_status": prior.status}),
                        ));
                        append_batch(
                            &mut capacity_state,
                            &scope,
                            &run_id,
                            now,
                            EventActorKind::Clock,
                            "event-capacity".to_owned(),
                            specs,
                        )?;
                        *committed = capacity_state;
                    }
                }
                Err(error)
            }
        }
    }
}

fn verified(
    state: &mut MemoryState,
    scope: &ExecutionScope,
    object: &VerifiedObjectRef,
    now: Timestamp,
) -> Result<(), StoreError> {
    if object.scope() != scope || object.object_key().is_empty() {
        return Err(StoreError::ObjectNotVerified);
    }
    match &state.object_store_nonce {
        Some(nonce) if nonce.as_slice() != object.store_instance_nonce() => {
            return Err(StoreError::ObjectNotVerified);
        }
        None => state.object_store_nonce = Some(object.store_instance_nonce().to_vec()),
        Some(_) => {}
    }
    let key = (scope.clone(), object.digest().clone());
    let record = crate::artifact::ObjectRecord {
        scope: scope.clone(),
        digest: object.digest().clone(),
        size_bytes: object.size_bytes(),
        object_key: object.object_key().to_owned(),
        created_at: now,
    };
    if let Some(existing) = state.object_records.get(&key) {
        if existing.size_bytes != record.size_bytes || existing.object_key != record.object_key {
            return Err(StoreError::ArtifactMetadataConflict);
        }
    } else {
        state.object_records.insert(key, record);
    }
    Ok(())
}

fn artifact(
    state: &mut MemoryState,
    scope: &ExecutionScope,
    object: &VerifiedObjectRef,
    kind: ArtifactKind,
    run_id: Option<&Id>,
    node_id: Option<&NodeInstanceId>,
    attempt_id: Option<&Id>,
    ordinal: u32,
    now: Timestamp,
) -> Result<ArtifactRef, StoreError> {
    verified(state, scope, object, now)?;
    let id = artifact_ref_id(ArtifactRefIdentity {
        scope,
        digest: object.digest(),
        kind,
        producer_run_id: run_id,
        producer_node_id: node_id,
        producer_attempt_id: attempt_id,
        ordinal,
    });
    let reference = ArtifactRef {
        scope: scope.clone(),
        artifact_ref_id: id,
        digest: object.digest().clone(),
        size_bytes: object.size_bytes(),
        media_type: object.media_type().to_owned(),
        kind,
        producer_run_id: run_id.cloned(),
        producer_node_id: node_id.cloned(),
        producer_attempt_id: attempt_id.cloned(),
        ordinal,
        created_at: now,
    };
    let key = (scope.clone(), reference.artifact_ref_id.clone());
    if let Some(existing) = state.artifact_refs.get(&key) {
        if existing != &reference {
            return Err(StoreError::ArtifactMetadataConflict);
        }
    } else {
        state.artifact_refs.insert(key, reference.clone());
    }
    Ok(reference)
}

fn json_ref(
    state: &mut MemoryState,
    scope: &ExecutionScope,
    object: &VerifiedObjectRef,
    kind: ArtifactKind,
    run_id: Option<&Id>,
    node_id: Option<&NodeInstanceId>,
    attempt_id: Option<&Id>,
    ordinal: u32,
    now: Timestamp,
) -> Result<JsonRef, StoreError> {
    if object.media_type() != "application/json" {
        return Err(StoreError::InvalidField);
    }
    Ok(JsonRef(artifact(
        state, scope, object, kind, run_id, node_id, attempt_id, ordinal, now,
    )?))
}

fn fingerprint<T: Serialize>(domain: &str, scope: &ExecutionScope, value: &T) -> Digest {
    let bytes = serde_jcs::to_vec(&(domain, scope, value)).expect("fingerprint input serializes");
    digest(&bytes)
}

fn approval_decision_fingerprint(
    scope: &ExecutionScope,
    run_id: &Id,
    gate_id: &Id,
    decision: ApprovalDecision,
    decision_payload: Option<&VerifiedObjectRef>,
    approval_output: Option<&VerifiedObjectRef>,
    principal: &crate::approval::AuthenticatedPrincipal,
) -> Digest {
    fn extend_lp(bytes: &mut Vec<u8>, value: &[u8]) {
        bytes.extend((value.len() as u64).to_be_bytes());
        bytes.extend(value);
    }
    let mut bytes = Vec::new();
    for value in [
        "dagger-approval-decision-v1",
        scope.tenant_id.as_str(),
        scope.namespace.as_str(),
        run_id.as_str(),
        gate_id.as_str(),
        match decision {
            ApprovalDecision::Approve => "approve",
            ApprovalDecision::Reject => "reject",
        },
        decision_payload.map_or("none", |object| object.digest().as_str()),
        approval_output.map_or("none", |object| object.digest().as_str()),
        principal.principal_id(),
        principal.authentication_context_digest().as_str(),
    ] {
        extend_lp(&mut bytes, value.as_bytes());
    }
    digest(&bytes)
}

fn suspension_fingerprint(
    scope: &ExecutionScope,
    run_id: &Id,
    incompatibilities: &VerifiedObjectRef,
    evidence_digest: &Digest,
) -> Digest {
    fn extend_lp(bytes: &mut Vec<u8>, value: &[u8]) {
        bytes.extend((value.len() as u64).to_be_bytes());
        bytes.extend(value);
    }
    let artifact_id = artifact_ref_id(ArtifactRefIdentity {
        scope,
        digest: incompatibilities.digest(),
        kind: ArtifactKind::CompatibilityEvidence,
        producer_run_id: Some(run_id),
        producer_node_id: None,
        producer_attempt_id: None,
        ordinal: 0,
    });
    let mut bytes = Vec::new();
    let size_bytes = incompatibilities.size_bytes().to_be_bytes();
    for value in [
        b"dagger-suspend-request-v1".as_slice(),
        scope.tenant_id.as_str().as_bytes(),
        scope.namespace.as_str().as_bytes(),
        run_id.as_str().as_bytes(),
        artifact_id.as_str().as_bytes(),
        incompatibilities.digest().as_str().as_bytes(),
        size_bytes.as_slice(),
        evidence_digest.as_str().as_bytes(),
    ] {
        extend_lp(&mut bytes, value);
    }
    digest(&bytes)
}

#[derive(Serialize)]
struct CreateFingerprint<'a> {
    run_id: &'a Id,
    definition_id: &'a Id,
    revision_hash: &'a Digest,
    input_digest: &'a Digest,
    budget_limit: CostUnits,
    limits: &'a crate::run::RunLimits,
    principal: &'a str,
}

fn node_id(node: &NodeDefinition) -> &Id {
    match node {
        NodeDefinition::Action { id, .. }
        | NodeDefinition::Map { id, .. }
        | NodeDefinition::Choice { id, .. }
        | NodeDefinition::Approval { id, .. }
        | NodeDefinition::Succeed { id, .. }
        | NodeDefinition::Fail { id, .. } => id,
    }
}

fn node_kind(node: &NodeDefinition) -> NodeKind {
    match node {
        NodeDefinition::Action { .. } => NodeKind::Action,
        NodeDefinition::Map { .. } => NodeKind::Map,
        NodeDefinition::Choice { .. } => NodeKind::Choice,
        NodeDefinition::Approval { .. } => NodeKind::Approval,
        NodeDefinition::Succeed { .. } => NodeKind::Succeed,
        NodeDefinition::Fail { .. } => NodeKind::Fail,
    }
}

fn outgoing(node: &NodeDefinition) -> Vec<(String, Id, Option<u32>)> {
    match node {
        NodeDefinition::Action { next, .. }
        | NodeDefinition::Map { next, .. }
        | NodeDefinition::Approval { next, .. } => next
            .iter()
            .enumerate()
            .map(|(index, target)| (format!("next/{index}"), target.clone(), None))
            .collect(),
        NodeDefinition::Choice { cases, default, .. } => {
            let mut result = cases
                .iter()
                .enumerate()
                .map(|(index, case)| {
                    let target = match case {
                        crate::definition::ChoiceCase::Equals { next, .. }
                        | crate::definition::ChoiceCase::In { next, .. } => next.clone(),
                    };
                    (format!("case/{index}"), target, Some(index as u32))
                })
                .collect::<Vec<_>>();
            result.push(("default".to_owned(), default.clone(), None));
            result
        }
        NodeDefinition::Succeed { .. } | NodeDefinition::Fail { .. } => Vec::new(),
    }
}

fn action_config<'a>(
    definition: &'a PublishableDefinition,
    node: &NodeRun,
) -> Option<(
    &'a crate::definition::ActionReference,
    &'a crate::definition::RetryPolicy,
    u64,
    CostUnits,
)> {
    definition
        .definition
        .nodes
        .iter()
        .find(|candidate| node_id(candidate) == &node.definition_node_id)
        .and_then(|candidate| match candidate {
            NodeDefinition::Action {
                action,
                retry,
                timeout,
                declared_max_cost_units,
                ..
            }
            | NodeDefinition::Map {
                action,
                retry,
                timeout,
                declared_max_cost_units,
                ..
            } => Some((action, retry, timeout.timeout_ms, *declared_max_cost_units)),
            _ => None,
        })
}

fn retry_at(
    now: Timestamp,
    policy: &crate::definition::RetryPolicy,
    attempt_number: u32,
) -> Result<Timestamp, StoreError> {
    let delay = match policy.backoff {
        BackoffPolicy::Fixed { delay_ms } => delay_ms,
        BackoffPolicy::Exponential {
            initial_delay_ms,
            multiplier,
            max_delay_ms,
        } => {
            let exponent = attempt_number
                .checked_sub(1)
                .ok_or(StoreError::ArithmeticOverflow)?;
            let factor = u64::from(multiplier)
                .checked_pow(exponent)
                .ok_or(StoreError::ArithmeticOverflow)?;
            initial_delay_ms
                .checked_mul(factor)
                .ok_or(StoreError::ArithmeticOverflow)?
                .min(max_delay_ms)
        }
    };
    checked_add_time(now, delay)
}

fn validate_limits(limits: &crate::run::RunLimits) -> bool {
    limits.max_dynamic_node_instances <= 100_000
        && limits.max_total_attempts > 0
        && limits.max_total_attempts <= 1_000_000
        && limits.max_total_events > 0
        && limits.max_total_events <= 10_000_000
        && limits.max_inline_json_bytes_per_value > 0
        && limits.max_inline_json_bytes_per_value <= 16_777_216
        && limits.max_artifacts_per_attempt <= 1_024
        && limits.max_aggregate_object_bytes_per_run > 0
        && limits.max_aggregate_object_bytes_per_run <= 68_719_476_736
        && limits.max_run_lifetime_ms > 0
        && limits.max_run_lifetime_ms <= 31_536_000_000
}

fn event_spec(
    event_type: EventType,
    transition: &str,
    node: Option<&NodeInstanceId>,
    attempt: Option<&Id>,
    gate: Option<&Id>,
    mut payload: Value,
) -> EventSpec {
    if let Value::Object(fields) = &mut payload {
        fields.retain(|_, value| !value.is_null());
    }
    EventSpec {
        event_type,
        transition: transition.to_owned(),
        node: node.cloned(),
        attempt: attempt.cloned(),
        gate: gate.cloned(),
        payload,
    }
}

struct EventSpec {
    event_type: EventType,
    transition: String,
    node: Option<Id>,
    attempt: Option<Id>,
    gate: Option<Id>,
    payload: Value,
}

fn event_order(spec: &EventSpec) -> (u8, String, String, u8, String) {
    let run_event = spec.node.is_none() && spec.attempt.is_none() && spec.gate.is_none();
    let category = if run_event {
        5
    } else if matches!(
        spec.event_type,
        EventType::EdgeSatisfied | EventType::EdgeSkipped
    ) {
        3
    } else if matches!(
        spec.event_type,
        EventType::NodeBecameReady | EventType::NodeSkipped | EventType::NodeCancelled
    ) {
        4
    } else if matches!(
        spec.event_type,
        EventType::MapChildCreated
            | EventType::MapExpanded
            | EventType::MapZeroItemsSucceeded
            | EventType::MapSucceeded
    ) {
        2
    } else {
        1
    };
    let within_subject = match spec.event_type {
        EventType::StaleCompletionObserved => 1,
        EventType::NodeAttemptClaimed
        | EventType::NodeSucceeded
        | EventType::NodeRetryScheduled
        | EventType::NodeFailed
        | EventType::NodeContractFailed
        | EventType::NodeRetriesExhausted
        | EventType::NodeBudgetWaiting
        | EventType::NodeBudgetExhausted
        | EventType::ApprovalRequested
        | EventType::ApprovalApproved
        | EventType::ApprovalRejected
        | EventType::ApprovalExpiredApproved
        | EventType::ApprovalExpiredRejected => 2,
        EventType::BudgetReserved
        | EventType::BudgetSettled
        | EventType::BudgetReservationRefused => 3,
        _ => 0,
    };
    (
        category,
        spec.node
            .as_ref()
            .map_or_else(String::new, |id| id.as_str().to_owned()),
        spec.attempt
            .as_ref()
            .or(spec.gate.as_ref())
            .map_or_else(String::new, |id| id.as_str().to_owned()),
        within_subject,
        spec.transition.clone(),
    )
}

fn event_reserve(
    state: &MemoryState,
    scope: &ExecutionScope,
    run_id: &Id,
) -> Result<u64, StoreError> {
    let nonterminal_nodes = u64::try_from(
        state
            .nodes
            .values()
            .filter(|node| {
                node.scope == *scope && node.run_id == *run_id && !node.status.is_terminal()
            })
            .count(),
    )
    .map_err(|_| StoreError::ArithmeticOverflow)?;
    let started_attempts = u64::try_from(
        state
            .attempts
            .values()
            .filter(|attempt| {
                attempt.scope == *scope
                    && attempt.run_id == *run_id
                    && attempt.status == AttemptState::Started
            })
            .count(),
    )
    .map_err(|_| StoreError::ArithmeticOverflow)?;
    let pending_gates = u64::try_from(
        state
            .gates
            .values()
            .filter(|gate| {
                gate.scope == *scope
                    && gate.run_id == *run_id
                    && gate.status == crate::run::GateState::Pending
            })
            .count(),
    )
    .map_err(|_| StoreError::ArithmeticOverflow)?;
    let unobserved_attempts = u64::try_from(
        state
            .attempts
            .values()
            .filter(|attempt| {
                attempt.scope == *scope
                    && attempt.run_id == *run_id
                    && !state.stale_observed.contains(&(
                        scope.clone(),
                        run_id.clone(),
                        attempt.attempt_id.clone(),
                    ))
            })
            .count(),
    )
    .map_err(|_| StoreError::ArithmeticOverflow)?;
    1_u64
        .checked_add(nonterminal_nodes)
        .and_then(|value| value.checked_add(started_attempts.checked_mul(2)?))
        .and_then(|value| value.checked_add(pending_gates))
        .and_then(|value| value.checked_add(unobserved_attempts))
        .and_then(|value| value.checked_add(2))
        .ok_or(StoreError::ArithmeticOverflow)
}

fn append_batch(
    state: &mut MemoryState,
    scope: &ExecutionScope,
    run_id: &Id,
    now: Timestamp,
    actor_kind: EventActorKind,
    actor_id: String,
    mut specs: Vec<EventSpec>,
) -> Result<(Id, u64, u64), StoreError> {
    for spec in &specs {
        if !spec.payload.is_object()
            || serde_jcs::to_vec(&spec.payload)
                .map_err(|_| StoreError::InvalidField)?
                .len()
                > 65_536
        {
            return Err(StoreError::InvalidField);
        }
    }
    if !specs
        .iter()
        .any(|spec| spec.event_type == EventType::RunCreated)
    {
        specs.sort_by_key(event_order);
    }
    let run_key = (scope.clone(), run_id.clone());
    let run = state.runs.get(&run_key).ok_or(StoreError::NotFound)?;
    let count = u64::try_from(specs.len()).map_err(|_| StoreError::ArithmeticOverflow)?;
    let last = run
        .last_event_seq
        .checked_add(count)
        .ok_or(StoreError::ArithmeticOverflow)?;
    let consumes_reserve = specs.iter().any(|spec| {
        matches!(
            spec.event_type,
            EventType::RunCancelled
                | EventType::StaleCompletionObserved
                | EventType::RunCorruptStorage
        )
    });
    let required = if consumes_reserve {
        last
    } else {
        last.checked_add(event_reserve(state, scope, run_id)?)
            .ok_or(StoreError::ArithmeticOverflow)?
    };
    if required > run.limits.max_total_events {
        return Err(StoreError::RunLimitApplied {
            code: "RunEventLimitExceeded".to_owned(),
        });
    }
    state.batch_counter = state
        .batch_counter
        .checked_add(1)
        .ok_or(StoreError::ArithmeticOverflow)?;
    let batch_id =
        Id::new(format!("batch_{:016x}", state.batch_counter)).expect("batch ID is valid");
    let first = run
        .last_event_seq
        .checked_add(1)
        .ok_or(StoreError::ArithmeticOverflow)?;
    let batch_count = u32::try_from(specs.len()).map_err(|_| StoreError::ArithmeticOverflow)?;
    let events = state.events.entry(run_key.clone()).or_default();
    for (index, spec) in specs.into_iter().enumerate() {
        events.push(WorkflowEvent {
            scope: scope.clone(),
            run_id: run_id.clone(),
            event_seq: first
                .checked_add(u64::try_from(index).map_err(|_| StoreError::ArithmeticOverflow)?)
                .ok_or(StoreError::ArithmeticOverflow)?,
            event_type: spec.event_type,
            transition_id: spec.transition,
            batch_id: batch_id.clone(),
            batch_index: u32::try_from(index).map_err(|_| StoreError::ArithmeticOverflow)?,
            batch_count,
            occurred_at: now,
            actor_kind,
            actor_id: actor_id.clone(),
            node_instance_id: spec.node,
            attempt_id: spec.attempt,
            gate_id: spec.gate,
            payload: spec.payload,
        });
    }
    let run = state.runs.get_mut(&run_key).expect("run remains present");
    run.last_event_seq = last;
    set_run_mutated(run, now)?;
    Ok((batch_id, first, last))
}

fn permit_check(
    state: &MemoryState,
    scope: &ExecutionScope,
    permit: &EnginePermit,
    now: Timestamp,
) -> Result<(), StoreError> {
    let stored = state.claims.get(scope).ok_or(StoreError::EngineClaimLost)?;
    if stored.claim.instance_id != *permit.instance_id()
        || stored.claim.generation != permit.generation()
        || stored.token_digest != digest(permit.session_token())
    {
        return Err(StoreError::EngineClaimLost);
    }
    if stored.claim.expires_at <= now {
        return Err(StoreError::EngineClaimExpired);
    }
    Ok(())
}

fn command_fence(
    state: &MemoryState,
    scope: &ExecutionScope,
    run_id: &Id,
    permit: Option<&EnginePermit>,
    now: Timestamp,
    allow_blocked: bool,
) -> Result<WorkflowRun, StoreError> {
    if let Some(permit) = permit {
        permit_check(state, scope, permit, now)?;
    }
    let run = state
        .runs
        .get(&(scope.clone(), run_id.clone()))
        .cloned()
        .ok_or(StoreError::NotFound)?;
    if now < run.updated_at {
        return Err(StoreError::ClockNonMonotonic);
    }
    if run.status == RunState::BlockedIncompatible && !allow_blocked {
        return Err(StoreError::RunBlockedIncompatible);
    }
    Ok(run)
}

fn running_fence(
    state: &MemoryState,
    scope: &ExecutionScope,
    run_id: &Id,
    permit: Option<&EnginePermit>,
    now: Timestamp,
) -> Result<WorkflowRun, StoreError> {
    let run = command_fence(state, scope, run_id, permit, now, false)?;
    if run.status != RunState::Running {
        return Err(StoreError::IllegalTransition);
    }
    Ok(run)
}

fn set_run_mutated(run: &mut WorkflowRun, now: Timestamp) -> Result<(), StoreError> {
    run.version.0 = run
        .version
        .0
        .checked_add(1)
        .ok_or(StoreError::ArithmeticOverflow)?;
    run.updated_at = now;
    Ok(())
}

fn set_node_mutated(node: &mut NodeRun, now: Timestamp) -> Result<(), StoreError> {
    node.version.0 = node
        .version
        .0
        .checked_add(1)
        .ok_or(StoreError::ArithmeticOverflow)?;
    node.updated_at = now;
    Ok(())
}

fn bump_frontier_epoch(
    state: &mut MemoryState,
    scope: &ExecutionScope,
    run_id: &Id,
) -> Result<(), StoreError> {
    let run = state
        .runs
        .get_mut(&(scope.clone(), run_id.clone()))
        .ok_or(StoreError::NotFound)?;
    run.frontier_epoch = run
        .frontier_epoch
        .checked_add(1)
        .ok_or(StoreError::ArithmeticOverflow)?;
    Ok(())
}

fn frontier_reduce(
    state: &mut MemoryState,
    scope: &ExecutionScope,
    run_id: &Id,
    source_node: &Id,
    selected_edge: Option<&Id>,
    now: Timestamp,
    specs: &mut Vec<EventSpec>,
) -> Result<(), StoreError> {
    let mut frontier_changed = false;
    let mut edge_keys = state
        .edges
        .keys()
        .filter(|(edge_scope, edge_run, _)| edge_scope == scope && edge_run == run_id)
        .cloned()
        .collect::<Vec<_>>();
    edge_keys.sort_by(|left, right| left.2.cmp(&right.2));
    for key in &edge_keys {
        let edge = state.edges.get_mut(key).expect("key captured");
        if edge.from_node_id == *source_node && edge.state == EdgeState::Dormant {
            frontier_changed = true;
            let selected = selected_edge.map_or(true, |selected| selected == &edge.edge_id);
            edge.state = if selected {
                EdgeState::Satisfied
            } else {
                EdgeState::Skipped
            };
            edge.resolved_at = Some(now);
            edge.version.0 = edge
                .version
                .0
                .checked_add(1)
                .ok_or(StoreError::ArithmeticOverflow)?;
            let payload = if selected {
                json!({
                    "edge_id": edge.edge_id,
                    "from_node_id": edge.from_node_id,
                    "to_node_id": edge.to_node_id
                })
            } else {
                json!({
                    "edge_id": edge.edge_id,
                    "from_node_id": edge.from_node_id,
                    "to_node_id": edge.to_node_id,
                    "cause": "choice_unselected"
                })
            };
            specs.push(event_spec(
                if selected {
                    EventType::EdgeSatisfied
                } else {
                    EventType::EdgeSkipped
                },
                if selected { "E01" } else { "E02" },
                Some(source_node),
                None,
                None,
                payload,
            ));
        }
    }

    loop {
        let pending_ids = state
            .nodes
            .iter()
            .filter(|((node_scope, node_run, _), node)| {
                node_scope == scope && node_run == run_id && node.status == NodeState::Pending
            })
            .map(|(key, _)| key.2.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for pending_id in pending_ids {
            let incoming = state
                .edges
                .values()
                .filter(|edge| {
                    edge.scope == *scope && edge.run_id == *run_id && edge.to_node_id == pending_id
                })
                .cloned()
                .collect::<Vec<_>>();
            if incoming.iter().any(|edge| edge.state == EdgeState::Dormant) {
                continue;
            }
            let satisfied = u32::try_from(
                incoming
                    .iter()
                    .filter(|edge| edge.state == EdgeState::Satisfied)
                    .count(),
            )
            .map_err(|_| StoreError::ArithmeticOverflow)?;
            let incoming_count =
                u32::try_from(incoming.len()).map_err(|_| StoreError::ArithmeticOverflow)?;
            let skipped = incoming_count
                .checked_sub(satisfied)
                .ok_or(StoreError::ArithmeticOverflow)?;
            let key = (scope.clone(), run_id.clone(), pending_id.clone());
            let node = state.nodes.get_mut(&key).expect("pending node exists");
            node.incoming_satisfied = satisfied;
            node.incoming_skipped = skipped;
            if satisfied > 0 {
                node.status = NodeState::Ready;
                set_node_mutated(node, now)?;
                specs.push(event_spec(
                    EventType::NodeBecameReady,
                    "N03",
                    Some(&pending_id),
                    None,
                    None,
                    json!({
                        "incoming_satisfied": satisfied,
                        "incoming_skipped": skipped,
                        "incoming_total": node.incoming_total
                    }),
                ));
            } else {
                node.status = NodeState::Skipped;
                set_node_mutated(node, now)?;
                specs.push(event_spec(
                    EventType::NodeSkipped,
                    "N28",
                    Some(&pending_id),
                    None,
                    None,
                    json!({"incoming_skipped": skipped, "incoming_total": node.incoming_total}),
                ));
                for edge_key in &edge_keys {
                    let edge = state.edges.get_mut(edge_key).expect("edge exists");
                    if edge.from_node_id == pending_id && edge.state == EdgeState::Dormant {
                        edge.state = EdgeState::Skipped;
                        edge.resolved_at = Some(now);
                        edge.version.0 = edge
                            .version
                            .0
                            .checked_add(1)
                            .ok_or(StoreError::ArithmeticOverflow)?;
                        specs.push(event_spec(
                            EventType::EdgeSkipped,
                            "E02",
                            Some(&pending_id),
                            None,
                            None,
                            json!({
                                "edge_id": edge.edge_id,
                                "from_node_id": edge.from_node_id,
                                "to_node_id": edge.to_node_id,
                                "cause": "source_skipped"
                            }),
                        ));
                    }
                }
            }
            changed = true;
            frontier_changed = true;
        }
        if !changed {
            break;
        }
    }
    if frontier_changed {
        bump_frontier_epoch(state, scope, run_id)?;
    }
    Ok(())
}

fn settle(
    state: &mut MemoryState,
    scope: &ExecutionScope,
    run_id: &Id,
    attempt: &mut NodeAttempt,
    consumed: CostUnits,
    reason: BudgetLedgerReason,
    now: Timestamp,
) -> Result<(), StoreError> {
    if attempt.settled_cost.is_some() {
        return Ok(());
    }
    let run_key = (scope.clone(), run_id.clone());
    let run_snapshot = state.runs.get(&run_key).ok_or(StoreError::NotFound)?;
    let reserved_after = run_snapshot
        .budget_reserved
        .0
        .checked_sub(attempt.reserved_cost.0)
        .ok_or(StoreError::ArithmeticOverflow)?;
    let consumed_after = run_snapshot
        .budget_consumed
        .0
        .checked_add(consumed.0)
        .ok_or(StoreError::ArithmeticOverflow)?;
    let allocated_after = consumed_after
        .checked_add(reserved_after)
        .ok_or(StoreError::ArithmeticOverflow)?;
    if allocated_after > run_snapshot.budget_limit.0 {
        return Err(StoreError::ArithmeticOverflow);
    }
    let ledger_seq = u64::try_from(state.ledger.get(&run_key).map_or(0, Vec::len))
        .map_err(|_| StoreError::ArithmeticOverflow)?
        .checked_add(1)
        .ok_or(StoreError::ArithmeticOverflow)?;
    let run = state.runs.get_mut(&run_key).expect("run remains present");
    run.budget_reserved.0 = reserved_after;
    run.budget_consumed.0 = consumed_after;
    set_run_mutated(run, now)?;
    attempt.settled_cost = Some(consumed);
    let ledger = state.ledger.entry(run_key).or_default();
    ledger.push(BudgetLedgerEntry {
        scope: scope.clone(),
        run_id: run_id.clone(),
        ledger_seq,
        attempt_id: attempt.attempt_id.clone(),
        node_instance_id: attempt.node_instance_id.clone(),
        kind: if consumed == attempt.reserved_cost {
            BudgetLedgerKind::SettleFullUnknown
        } else {
            BudgetLedgerKind::SettleActual
        },
        reserved_delta: -(i128::from(attempt.reserved_cost.0)),
        consumed_delta: consumed,
        reservation_amount: attempt.reserved_cost,
        reason,
        created_at: now,
    });
    Ok(())
}

fn cancellation_cascade(
    state: &mut MemoryState,
    scope: &ExecutionScope,
    run_id: &Id,
    terminal: RunState,
    reason: &str,
    now: Timestamp,
    specs: &mut Vec<EventSpec>,
) -> Result<(), StoreError> {
    let mut frontier_changed = false;
    let mut node_keys = state
        .nodes
        .keys()
        .filter(|(node_scope, node_run, _)| node_scope == scope && node_run == run_id)
        .cloned()
        .collect::<Vec<_>>();
    node_keys.sort_by(|left, right| left.2.cmp(&right.2));
    for key in node_keys {
        let snapshot = state.nodes.get(&key).expect("node exists").clone();
        if snapshot.status.is_terminal() {
            continue;
        }
        frontier_changed = true;
        if let Some(attempt_id) = snapshot.active_attempt_id.clone() {
            let attempt_key = (scope.clone(), run_id.clone(), attempt_id.clone());
            if let Some(mut attempt) = state.attempts.remove(&attempt_key) {
                if attempt.status == AttemptState::Started {
                    let reservation = attempt.reserved_cost;
                    settle(
                        state,
                        scope,
                        run_id,
                        &mut attempt,
                        reservation,
                        BudgetLedgerReason::Cancelled,
                        now,
                    )?;
                    attempt.status = AttemptState::Cancelled;
                    attempt.finished_at = Some(now);
                    specs.push(event_spec(
                        EventType::AttemptCancelled,
                        "A08",
                        Some(&snapshot.node_instance_id),
                        Some(&attempt_id),
                        None,
                        json!({"reason_code": reason, "charged_cost_units": attempt.reserved_cost}),
                    ));
                    specs.push(budget_settled_spec(state, scope, run_id, &attempt)?);
                }
                state.attempts.insert(attempt_key, attempt);
            }
        }
        let node = state.nodes.get_mut(&key).expect("node exists");
        let prior = node.status;
        node.status = NodeState::Cancelled;
        node.active_attempt_id = None;
        node.next_eligible_at = None;
        node.budget_wait_amount = None;
        node.blocked_from_status = None;
        set_node_mutated(node, now)?;
        specs.push(event_spec(
            EventType::NodeCancelled,
            match prior {
                NodeState::Pending => "N35",
                NodeState::Ready => "N36",
                NodeState::Running => "N37",
                NodeState::RetryWaiting => "N38",
                NodeState::WaitingApproval => "N39",
                NodeState::WaitingChildren => "N40",
                NodeState::BlockedIncompatible => "N41",
                NodeState::BudgetWaiting => "N63",
                _ => "N35",
            },
            Some(&node.node_instance_id),
            None,
            None,
            json!({"prior_status": prior, "terminal_run_status": terminal, "reason_code": reason}),
        ));
    }
    for gate in state.gates.values_mut().filter(|gate| {
        gate.scope == *scope
            && gate.run_id == *run_id
            && gate.status == crate::run::GateState::Pending
    }) {
        gate.status = crate::run::GateState::Cancelled;
        gate.resolution_source = Some(ApprovalResolutionSource::Cancellation);
        gate.decided_at = Some(now);
        gate.version.0 = gate
            .version
            .0
            .checked_add(1)
            .ok_or(StoreError::ArithmeticOverflow)?;
        specs.push(event_spec(
            EventType::ApprovalGateCancelled,
            "G06",
            Some(&gate.node_instance_id),
            None,
            Some(&gate.gate_id),
            json!({"terminal_run_status": terminal, "reason_code": reason}),
        ));
    }
    if frontier_changed {
        bump_frontier_epoch(state, scope, run_id)?;
    }
    Ok(())
}

fn terminalize_run(
    state: &mut MemoryState,
    scope: &ExecutionScope,
    run_id: &Id,
    status: RunState,
    failure: Option<RunFailureKind>,
    reason: &str,
    now: Timestamp,
    specs: &mut Vec<EventSpec>,
) -> Result<WorkflowRun, StoreError> {
    cancellation_cascade(state, scope, run_id, status, reason, now, specs)?;
    let run = state
        .runs
        .get_mut(&(scope.clone(), run_id.clone()))
        .ok_or(StoreError::NotFound)?;
    run.status = status;
    run.failure_kind = failure;
    run.finished_at = Some(now);
    set_run_mutated(run, now)?;
    Ok(run.clone())
}

#[derive(Deserialize, Serialize)]
struct MemoryCursor {
    scope: ExecutionScope,
    query: String,
    cutoff: Timestamp,
    last: Vec<String>,
}

fn scan_page_context(
    page: &PageRequest,
    scope: &ExecutionScope,
    query: &str,
    now: Timestamp,
) -> Result<(usize, Timestamp, Vec<String>), StoreError> {
    if page.page_size == 0 || page.page_size > 1000 {
        return Err(StoreError::InvalidField);
    }
    match &page.cursor {
        Some(cursor) => {
            let decoded: MemoryCursor =
                serde_json::from_str(cursor.encoded()).map_err(|_| StoreError::InvalidField)?;
            if decoded.scope != *scope || decoded.query != query {
                return Err(StoreError::InvalidField);
            }
            Ok((page.page_size as usize, decoded.cutoff, decoded.last))
        }
        None => Ok((page.page_size as usize, now, Vec::new())),
    }
}

fn next_scan_cursor(
    scope: &ExecutionScope,
    query: &str,
    cutoff: Timestamp,
    last: Vec<String>,
) -> Result<ScanCursor, StoreError> {
    serde_jcs::to_string(&MemoryCursor {
        scope: scope.clone(),
        query: query.to_owned(),
        cutoff,
        last,
    })
    .map(ScanCursor::new)
    .map_err(|_| StoreError::TransactionFailed)
}

fn finish_scan_page<T>(
    mut items: Vec<T>,
    limit: usize,
    scope: &ExecutionScope,
    query: &str,
    cutoff: Timestamp,
    key: impl Fn(&T) -> Vec<String>,
) -> Result<Page<T>, StoreError> {
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if has_more {
        items
            .last()
            .map(|item| next_scan_cursor(scope, query, cutoff, key(item)))
            .transpose()?
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}

fn timestamp_cursor_key(timestamp: Timestamp) -> String {
    format!("{:016x}", (timestamp.0 as u64) ^ (1_u64 << 63))
}

impl<C: Clock> WorkflowStore for InMemoryStore<C> {
    async fn create_definition(
        &self,
        scope: &ExecutionScope,
        command: CreateDefinition,
    ) -> Result<DefinitionRecord, StoreError> {
        self.transaction(|state, now| {
            if command.principal.scope() != scope
                || !valid_metadata(&command.display_name, &command.description)
            {
                return Err(StoreError::InvalidField);
            }
            let key = (scope.clone(), command.definition_id.clone());
            if state.definitions.contains_key(&key) {
                return Err(StoreError::AlreadyExists);
            }
            let record = DefinitionRecord {
                scope: scope.clone(),
                definition_id: command.definition_id,
                display_name: command.display_name,
                description: command.description,
                created_at: now,
                created_by: command.principal.principal_id().to_owned(),
                latest_revision_hash: None,
                version: Version(1),
            };
            state.definitions.insert(key, record.clone());
            Ok(record)
        })
    }
    async fn update_definition_metadata(
        &self,
        scope: &ExecutionScope,
        command: UpdateDefinitionMetadata,
    ) -> Result<DefinitionRecord, StoreError> {
        self.transaction(|state, _now| {
            if !valid_metadata(&command.display_name, &command.description) {
                return Err(StoreError::InvalidField);
            }
            let record = state
                .definitions
                .get_mut(&(scope.clone(), command.definition_id))
                .ok_or(StoreError::NotFound)?;
            if record.version != command.expected_version {
                return Err(StoreError::CasConflict);
            }
            record.display_name = command.display_name;
            record.description = command.description;
            record.version.0 = record
                .version
                .0
                .checked_add(1)
                .ok_or(StoreError::ArithmeticOverflow)?;
            Ok(record.clone())
        })
    }
    async fn publish_revision(
        &self,
        scope: &ExecutionScope,
        command: PublishRevision,
    ) -> Result<WorkflowRevision, StoreError> {
        self.transaction(|mut state, now| {
            if command.principal.scope() != scope {
                return Err(StoreError::InvalidField);
            }
            verified(&mut state, scope, &command.canonical_definition, now)?;
            verified(&mut state, scope, &command.run_input_schema, now)?;
            verified(&mut state, scope, &command.run_output_schema, now)?;
            if command.parsed_revision.definition.definition_id != command.definition_id {
                return Err(StoreError::RevisionDefinitionIdMismatch);
            }
            let revision_hash = command.canonical_definition.digest().clone();
            if command.parsed_revision.definition.run_input_schema_digest
                != *command.run_input_schema.digest()
                || command.parsed_revision.definition.run_output_schema_digest
                    != *command.run_output_schema.digest()
            {
                return Err(StoreError::DigestMismatch);
            }
            let definition_key = (scope.clone(), command.definition_id.clone());
            let definition = state
                .definitions
                .get(&definition_key)
                .cloned()
                .ok_or(StoreError::NotFound)?;
            if definition.version != command.expected_definition_version {
                return Err(StoreError::CasConflict);
            }
            let revision_key = (
                scope.clone(),
                command.definition_id.clone(),
                revision_hash.clone(),
            );
            if let Some(existing) = state.revisions.get(&revision_key) {
                return Ok(existing.clone());
            }
            let canonical_definition_ref = json_ref(
                &mut state,
                scope,
                &command.canonical_definition,
                ArtifactKind::Definition,
                None,
                None,
                None,
                0,
                now,
            )?;
            let run_input_schema_ref = json_ref(
                &mut state,
                scope,
                &command.run_input_schema,
                ArtifactKind::SchemaDocument,
                None,
                None,
                None,
                0,
                now,
            )?;
            let run_output_schema_ref = json_ref(
                &mut state,
                scope,
                &command.run_output_schema,
                ArtifactKind::SchemaDocument,
                None,
                None,
                None,
                1,
                now,
            )?;
            let extracted =
                crate::definition::extract_action_pins(&command.parsed_revision.definition);
            let mut pins = Vec::with_capacity(extracted.len());
            for pin in extracted {
                let schemas = command
                    .resolved_action_schema_objects
                    .get(&pin.reference_location)
                    .ok_or(StoreError::NotFound)?;
                if schemas.input_schema.digest() != &pin.input_schema_digest
                    || schemas.output_schema.digest() != &pin.output_schema_digest
                {
                    return Err(StoreError::DigestMismatch);
                }
                pins.push(ActionPin {
                    reference_location: pin.reference_location,
                    name: pin.name,
                    contract_version: pin.contract_version,
                    input_schema_digest: pin.input_schema_digest,
                    output_schema_digest: pin.output_schema_digest,
                    compatible_implementation_requirement: pin
                        .compatible_implementation_requirement,
                    input_schema_ref: json_ref(
                        &mut state,
                        scope,
                        &schemas.input_schema,
                        ArtifactKind::SchemaDocument,
                        None,
                        None,
                        None,
                        0,
                        now,
                    )?,
                    output_schema_ref: json_ref(
                        &mut state,
                        scope,
                        &schemas.output_schema,
                        ArtifactKind::SchemaDocument,
                        None,
                        None,
                        None,
                        1,
                        now,
                    )?,
                });
            }
            let revision = WorkflowRevision {
                scope: scope.clone(),
                definition_id: command.definition_id.clone(),
                revision_hash: revision_hash.clone(),
                definition_format_version: "0.1".to_owned(),
                canonical_definition_ref,
                run_input_schema_ref,
                run_output_schema_ref,
                run_input_schema_digest: command
                    .parsed_revision
                    .definition
                    .run_input_schema_digest
                    .clone(),
                run_output_schema_digest: command
                    .parsed_revision
                    .definition
                    .run_output_schema_digest
                    .clone(),
                entry_node_id: command.parsed_revision.definition.entry_node_id.clone(),
                node_count: u32::try_from(command.parsed_revision.definition.nodes.len())
                    .map_err(|_| StoreError::ArithmeticOverflow)?,
                node_topological_ranks: command.parsed_revision.topological_ranks.clone(),
                action_pins: pins,
                published_at: now,
                published_by: command.principal.principal_id().to_owned(),
            };
            state
                .parsed_revisions
                .insert(revision_key.clone(), command.parsed_revision);
            state.revisions.insert(revision_key, revision.clone());
            let definition = state
                .definitions
                .get_mut(&definition_key)
                .expect("definition remains present");
            definition.latest_revision_hash = Some(revision_hash);
            definition.version.0 = definition
                .version
                .0
                .checked_add(1)
                .ok_or(StoreError::ArithmeticOverflow)?;
            Ok(revision)
        })
    }
    async fn acquire_engine_claim(
        &self,
        scope: &ExecutionScope,
        instance_id: Id,
    ) -> Result<AcquiredEngineClaim, StoreError> {
        self.transaction(|state, now| {
            let raw = entropy()?;
            let (generation, version) = match state.claims.get(scope) {
                Some(stored) => {
                    if now < stored.claim.heartbeat_at {
                        return Err(StoreError::ClockNonMonotonic);
                    }
                    if stored.claim.expires_at > now {
                        return Err(StoreError::EngineAlreadyLive {
                            owner: stored.claim.instance_id.clone(),
                            expires_at: stored.claim.expires_at,
                        });
                    }
                    (
                        stored
                            .claim
                            .generation
                            .checked_add(1)
                            .ok_or(StoreError::ArithmeticOverflow)?,
                        stored
                            .claim
                            .version
                            .0
                            .checked_add(1)
                            .ok_or(StoreError::ArithmeticOverflow)?,
                    )
                }
                None => (1, 1),
            };
            let claim = EngineClaim {
                scope: scope.clone(),
                control_plane_id: "scheduler".to_owned(),
                instance_id: instance_id.clone(),
                generation,
                claimed_at: now,
                heartbeat_at: now,
                expires_at: checked_add_time(now, CLAIM_LIFETIME_MS as u64)?,
                version: Version(version),
            };
            let permit = EnginePermit::mint(instance_id, generation, raw);
            state.claims.insert(
                scope.clone(),
                StoredClaim {
                    claim: claim.clone(),
                    token_digest: digest(permit.session_token()),
                    released_token_digest: None,
                },
            );
            Ok(AcquiredEngineClaim { claim, permit })
        })
    }
    async fn heartbeat_engine_claim(
        &self,
        scope: &ExecutionScope,
        permit: &EnginePermit,
    ) -> Result<EngineClaim, StoreError> {
        self.transaction(|state, now| {
            permit_check(&state, scope, permit, now)?;
            let stored = state.claims.get_mut(scope).expect("checked claim");
            if now < stored.claim.heartbeat_at {
                return Err(StoreError::ClockNonMonotonic);
            }
            stored.claim.heartbeat_at = now;
            stored.claim.expires_at = checked_add_time(now, CLAIM_LIFETIME_MS as u64)?;
            stored.claim.version.0 = stored
                .claim
                .version
                .0
                .checked_add(1)
                .ok_or(StoreError::ArithmeticOverflow)?;
            Ok(stored.claim.clone())
        })
    }
    async fn release_engine_claim(
        &self,
        scope: &ExecutionScope,
        permit: &EnginePermit,
    ) -> Result<(), StoreError> {
        self.transaction(|state, now| {
            let candidate_digest = digest(permit.session_token());
            let stored = state
                .claims
                .get_mut(scope)
                .ok_or(StoreError::EngineClaimLost)?;
            let same = stored.claim.instance_id == *permit.instance_id()
                && stored.claim.generation == permit.generation()
                && stored.token_digest == candidate_digest;
            if !same {
                return Err(StoreError::EngineClaimLost);
            }
            if stored.claim.expires_at <= now
                && stored.released_token_digest.as_ref() == Some(&candidate_digest)
            {
                return Ok(());
            }
            if stored.claim.expires_at <= now {
                return Err(StoreError::EngineClaimLost);
            }
            stored.claim.expires_at = now;
            stored.claim.version.0 = stored
                .claim
                .version
                .0
                .checked_add(1)
                .ok_or(StoreError::ArithmeticOverflow)?;
            stored.released_token_digest = Some(candidate_digest);
            Ok(())
        })
    }
    async fn create_run(
        &self,
        scope: &ExecutionScope,
        command: CreateRun,
    ) -> Result<CommandReceipt, StoreError> {
        self.transaction(|mut state, now| {
        if command.principal.scope() != scope || command.idempotency_token.len() < 16 {
            return Err(StoreError::InvalidField);
        }
        verified(&mut state, scope, &command.input, now)?;
        if command.input.media_type() != "application/json" || !validate_limits(&command.limits) {
            return Err(StoreError::RunLimitsInvalid);
        }
        if command.input.size_bytes() > command.limits.max_inline_json_bytes_per_value
            || command.input.size_bytes() > command.limits.max_aggregate_object_bytes_per_run
        {
            return Err(StoreError::ContractValidation {
                kind: crate::definition::ValidationErrorKind::SchemaSubsetUnsupported,
                path: "/input".to_owned(),
                message: "run input exceeds configured limits".to_owned(),
                valid_alternatives: Vec::new(),
            });
        }
        let request_fingerprint = fingerprint(
            "create-run-v1",
            scope,
            &CreateFingerprint {
                run_id: &command.run_id,
                definition_id: &command.definition_id,
                revision_hash: &command.revision_hash,
                input_digest: command.input.digest(),
                budget_limit: command.budget_limit,
                limits: &command.limits,
                principal: command.principal.principal_id(),
            },
        );
        let receipt_key = (
            scope.clone(),
            CommandKindKey::Create,
            command.idempotency_token.clone(),
        );
        if let Some(receipt) = state.receipts.get(&receipt_key) {
            return if receipt.request_fingerprint == request_fingerprint {
                Ok(receipt.clone())
            } else {
                Err(StoreError::IdempotencyConflict)
            };
        }
        let revision_key = (
            scope.clone(),
            command.definition_id.clone(),
            command.revision_hash.clone(),
        );
        let revision = state
            .revisions
            .get(&revision_key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let parsed = state
            .parsed_revisions
            .get(&revision_key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let static_node_count = u64::try_from(parsed.definition.nodes.len())
            .map_err(|_| StoreError::ArithmeticOverflow)?;
        let creation_event_count = 1_u64
            .checked_add(static_node_count)
            .ok_or(StoreError::ArithmeticOverflow)?;
        let creation_reserve = 1_u64
            .checked_add(
                u64::try_from(parsed.definition.nodes.len())
                    .map_err(|_| StoreError::ArithmeticOverflow)?,
            )
            .and_then(|value| value.checked_add(2))
            .ok_or(StoreError::ArithmeticOverflow)?;
        if command.limits.max_total_events
            < creation_event_count
                .checked_add(creation_reserve)
                .ok_or(StoreError::ArithmeticOverflow)?
        {
            return Err(StoreError::RunLimitsInvalid);
        }
        let run_key = (scope.clone(), command.run_id.clone());
        if state.runs.contains_key(&run_key) {
            return Err(StoreError::AlreadyExists);
        }
        let lifetime_deadline_at = checked_add_time(now, command.limits.max_run_lifetime_ms)?;
        let input_ref = json_ref(
                &mut state,
            scope,
            &command.input,
            ArtifactKind::RunInput,
            Some(&command.run_id),
            None,
            None,
            0,
            now,
        )?;
        let run = WorkflowRun {
            scope: scope.clone(),
            run_id: command.run_id.clone(),
            definition_id: command.definition_id.clone(),
            revision_hash: command.revision_hash.clone(),
            input_ref,
            create_request_fingerprint: request_fingerprint.clone(),
            status: RunState::Pending,
            failure_kind: None,
            failure_diagnostics_ref: None,
            output_ref: None,
            budget_limit: command.budget_limit,
            budget_consumed: CostUnits(0),
            budget_reserved: CostUnits(0),
            dynamic_node_count: 0,
            total_attempt_count: 0,
            aggregate_object_bytes: command.input.size_bytes(),
            limits: command.limits,
            lifetime_deadline_at,
            frontier_epoch: 1,
            last_event_seq: 0,
            created_at: now,
            updated_at: now,
            started_at: None,
            finished_at: None,
            blocked_incompatibilities_ref: None,
            blocked_incompatibility_fingerprint: None,
            corrupt_bad_artifact_ref_id: None,
            corrupt_owner_node_id: None,
            corrupt_error_class: None,
            corrupt_proof_fingerprint: None,
            version: Version(1),
        };
        state.runs.insert(run_key.clone(), run);
        state.run_definitions.insert(run_key, parsed.clone());
        let mut incoming = BTreeMap::<Id, u32>::new();
        let mut specs = vec![event_spec(
            EventType::RunCreated,
            "R01",
            None,
            None,
            None,
            json!({
                "definition_id": command.definition_id,
                "revision_hash": command.revision_hash,
                "input_digest": command.input.digest(),
                "budget_limit": command.budget_limit,
                "limits": state.runs.get(&(scope.clone(), command.run_id.clone())).expect("run inserted").limits,
                "create_request_fingerprint": request_fingerprint
            }),
        )];
        for node in &parsed.definition.nodes {
            for (label, target, case_index) in outgoing(node) {
                let count = incoming.entry(target.clone()).or_default();
                *count = count
                    .checked_add(1)
                    .ok_or(StoreError::ArithmeticOverflow)?;
                let source = node_id(node);
                let id = edge_id(&revision.revision_hash, source, &label, &target);
                state.edges.insert(
                    (scope.clone(), command.run_id.clone(), id.clone()),
                    EdgeFact {
                        scope: scope.clone(),
                        run_id: command.run_id.clone(),
                        edge_id: id,
                        from_node_id: source.clone(),
                        to_node_id: target,
                        choice_case_index: case_index,
                        state: EdgeState::Dormant,
                        resolved_at: None,
                        version: Version(1),
                    },
                );
            }
        }
        let mut ordered_nodes = parsed.definition.nodes.iter().collect::<Vec<_>>();
        ordered_nodes.sort_by_key(|node| node_id(node));
        for definition_node in ordered_nodes {
            let id = node_id(definition_node).clone();
            let ready = id == parsed.definition.entry_node_id;
            let count = incoming.get(&id).copied().unwrap_or_default();
            let node = NodeRun {
                scope: scope.clone(),
                run_id: command.run_id.clone(),
                node_instance_id: id.clone(),
                definition_node_id: id.clone(),
                kind: node_kind(definition_node),
                parent_map_instance_id: None,
                map_item_index: None,
                map_item_digest: None,
                topological_rank: *parsed
                    .topological_ranks
                    .get(&id)
                    .expect("published node has rank"),
                status: if ready {
                    NodeState::Ready
                } else {
                    NodeState::Pending
                },
                blocked_from_status: None,
                active_attempt_id: None,
                attempt_count: 0,
                next_eligible_at: None,
                budget_wait_amount: None,
                result_ref: None,
                failure_kind: None,
                failure_diagnostics_ref: None,
                incoming_total: count,
                incoming_satisfied: 0,
                incoming_skipped: 0,
                choice_input_ref: None,
                choice_selected_case: None,
                map_input_ref: None,
                map_expansion_digest: None,
                map_child_count: None,
                approval_gate_id: None,
                created_at: now,
                updated_at: now,
                version: Version(1),
            };
            let payload = if ready {
                json!({
                    "definition_node_id": id,
                    "kind": node.kind,
                    "topological_rank": node.topological_rank
                })
            } else {
                json!({
                    "definition_node_id": id,
                    "kind": node.kind,
                    "incoming_total": count,
                    "topological_rank": node.topological_rank
                })
            };
            specs.push(event_spec(
                if ready {
                    EventType::NodeCreatedReady
                } else {
                    EventType::NodeCreatedPending
                },
                if ready { "N02" } else { "N01" },
                Some(&id),
                None,
                None,
                payload,
            ));
            state
                .nodes
                .insert((scope.clone(), command.run_id.clone(), id), node);
        }
        let (batch_id, first, last) = append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Host,
            command.principal.principal_id().to_owned(),
            specs,
        )?;
        let run = state
            .runs
            .get_mut(&(scope.clone(), command.run_id.clone()))
            .expect("run");
        set_run_mutated(run, now)?;
        let receipt = CommandReceipt {
            scope: scope.clone(),
            command_kind: CommandKind::CreateRun,
            idempotency_token: command.idempotency_token,
            request_fingerprint,
            run_id: command.run_id.clone(),
            outcome: CommandReceiptOutcome::CreateRunCommitted {
                run_id: command.run_id.clone(),
                status: RunState::Pending,
                run_version: run.version,
                batch_id: batch_id.clone(),
                first_event_seq: first,
                last_event_seq: last,
            },
            batch_id,
            committed_at: now,
        };
        state.receipts.insert(receipt_key, receipt.clone());
        Ok(receipt)
        })
    }
    async fn start_run(
        &self,
        scope: &ExecutionScope,
        command: StartRun,
    ) -> Result<WorkflowRun, StoreError> {
        self.transaction(|mut state, now| {
        command_fence(&state, scope, &command.run_id, Some(&command.permit), now, false)?;
        if !command
            .compatibility_evidence
            .incompatible_reference_locations
            .is_empty()
        {
            return Err(StoreError::IncompatiblePins);
        }
        let key = (scope.clone(), command.run_id.clone());
        let run = state.runs.get_mut(&key).ok_or(StoreError::NotFound)?;
        if run.status != RunState::Pending {
            return Err(StoreError::IllegalTransition);
        }
        run.status = RunState::Running;
        run.started_at = Some(now);
        set_run_mutated(run, now)?;
        let revision_hash = run.revision_hash.clone();
        append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Engine,
            command.permit.instance_id().as_str().to_owned(),
            vec![event_spec(
                EventType::RunStarted,
                "R02",
                None,
                None,
                None,
                json!({
                    "revision_hash": revision_hash,
                    "compatibility_evidence_digest": command.compatibility_evidence.evidence_digest
                }),
            )],
        )?;
        Ok(state.runs.get(&key).expect("run").clone())
        })
    }
    async fn suspend_incompatible(
        &self,
        scope: &ExecutionScope,
        command: SuspendIncompatible,
    ) -> Result<WorkflowRun, StoreError> {
        self.transaction(|mut state, now| {
        permit_check(&state, scope, &command.permit, now)?;
        verified(&mut state, scope, &command.incompatibilities, now)?;
        if command.incompatibilities.size_bytes() > 65_536 {
            return Err(StoreError::EvidenceInvalid);
        }
        let key = (scope.clone(), command.run_id.clone());
        let suspension_fingerprint = suspension_fingerprint(
            scope,
            &command.run_id,
            &command.incompatibilities,
            &command.evidence.evidence_digest,
        );
        let current = state.runs.get(&key).cloned().ok_or(StoreError::NotFound)?;
        if current.status == RunState::BlockedIncompatible
            && current.blocked_incompatibility_fingerprint.as_ref() == Some(&suspension_fingerprint)
        {
            return Ok(current);
        }
        if current.status == RunState::BlockedIncompatible {
            return Err(StoreError::RunBlockedIncompatible);
        }
        if !matches!(current.status, RunState::Pending | RunState::Running)
            || command.evidence.incompatible_reference_locations.is_empty()
        {
            return Err(StoreError::IllegalTransition);
        }
        if state.attempts.values().any(|attempt| {
            attempt.scope == *scope
                && attempt.run_id == command.run_id
                && attempt.status == AttemptState::Started
        }) {
            return Err(StoreError::CurrentGenerationAttemptPresent);
        }
        let incompatibilities_ref = json_ref(
                &mut state,
            scope,
            &command.incompatibilities,
            ArtifactKind::CompatibilityEvidence,
            Some(&command.run_id),
            None,
            None,
            0,
            now,
        )?;
        let incompatible = command
            .evidence
            .incompatible_reference_locations
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut specs = Vec::new();
        let mut frontier_changed = false;
        for node in state.nodes.values_mut().filter(|node| {
            node.scope == *scope
                && node.run_id == command.run_id
                && matches!(
                    node.status,
                    NodeState::Pending
                        | NodeState::Ready
                        | NodeState::RetryWaiting
                        | NodeState::BudgetWaiting
                )
                && {
                    let location = if node.parent_map_instance_id.is_some() {
                        format!("{}/map_action", node.definition_node_id.as_str())
                    } else {
                        node.definition_node_id.as_str().to_owned()
                    };
                    incompatible.contains(&location)
                }
        }) {
            frontier_changed = true;
            let prior = node.status;
            node.blocked_from_status = Some(match prior {
                NodeState::Pending => BlockedFromState::Pending,
                NodeState::Ready => BlockedFromState::Ready,
                NodeState::RetryWaiting => BlockedFromState::RetryWaiting,
                NodeState::BudgetWaiting => BlockedFromState::BudgetWaiting,
                _ => unreachable!(),
            });
            node.status = NodeState::BlockedIncompatible;
            set_node_mutated(node, now)?;
            specs.push(event_spec(
                EventType::NodeBlockedIncompatible,
                match prior {
                    NodeState::Pending => "N29",
                    NodeState::Ready => "N30",
                    NodeState::RetryWaiting => "N31",
                    NodeState::BudgetWaiting => "N61",
                    _ => unreachable!(),
                },
                Some(&node.node_instance_id),
                None,
                None,
                json!({
                    "blocked_from_status": prior,
                    "action_reference_location": node.definition_node_id,
                    "required_semantic_digest": command.evidence.evidence_digest
                }),
            ));
        }
        if frontier_changed {
            bump_frontier_epoch(&mut state, scope, &command.run_id)?;
        }
        let run = state.runs.get_mut(&key).expect("run exists");
        run.status = RunState::BlockedIncompatible;
        run.blocked_incompatibilities_ref = Some(incompatibilities_ref);
        run.blocked_incompatibility_fingerprint = Some(suspension_fingerprint.clone());
        set_run_mutated(run, now)?;
        specs.push(event_spec(
            EventType::RunBlockedIncompatible,
            if current.status == RunState::Pending {
                "R03"
            } else {
                "R04"
            },
            None,
            None,
            None,
            json!({
                "incompatibilities_digest": command.incompatibilities.digest(),
                "incompatible_reference_locations": command.evidence.incompatible_reference_locations,
                "suspension_fingerprint": suspension_fingerprint
            }),
        ));
        append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Engine,
            command.permit.instance_id().as_str().to_owned(),
            specs,
        )?;
        Ok(state.runs.get(&key).expect("run").clone())
        })
    }
    async fn resume_compatible(
        &self,
        scope: &ExecutionScope,
        command: ResumeCompatible,
    ) -> Result<WorkflowRun, StoreError> {
        self.transaction(|mut state, now| {
        command_fence(&state, scope, &command.run_id, Some(&command.permit), now, true)?;
        if !command
            .availability_evidence
            .incompatible_reference_locations
            .is_empty()
        {
            return Err(StoreError::StillIncompatible {
                pins: command
                    .availability_evidence
                    .incompatible_reference_locations,
            });
        }
        let key = (scope.clone(), command.run_id.clone());
        if state.runs.get(&key).ok_or(StoreError::NotFound)?.status != RunState::BlockedIncompatible
        {
            return Err(StoreError::IllegalTransition);
        }
        let mut specs = Vec::new();
        let mut frontier_changed = false;
        for node in state.nodes.values_mut().filter(|node| {
            node.scope == *scope
                && node.run_id == command.run_id
                && node.status == NodeState::BlockedIncompatible
        }) {
            frontier_changed = true;
            let restored = match node.blocked_from_status.take() {
                Some(BlockedFromState::Pending) => NodeState::Pending,
                Some(BlockedFromState::Ready) => NodeState::Ready,
                Some(BlockedFromState::RetryWaiting) => NodeState::RetryWaiting,
                Some(BlockedFromState::BudgetWaiting) => NodeState::BudgetWaiting,
                None => return Err(StoreError::CorruptControlPlane),
            };
            node.status = restored;
            set_node_mutated(node, now)?;
            specs.push(event_spec(
                EventType::NodeResumedCompatible,
                match restored {
                    NodeState::Pending => "N32",
                    NodeState::Ready => "N33",
                    NodeState::RetryWaiting => "N34",
                    NodeState::BudgetWaiting => "N62",
                    _ => unreachable!(),
                },
                Some(&node.node_instance_id),
                None,
                None,
                json!({"restored_status": restored, "available_semantic_digest": command.availability_evidence.evidence_digest}),
            ));
        }
        if frontier_changed {
            bump_frontier_epoch(&mut state, scope, &command.run_id)?;
        }
        let run = state.runs.get_mut(&key).expect("run");
        run.status = RunState::Running;
        run.blocked_incompatibilities_ref = None;
        run.blocked_incompatibility_fingerprint = None;
        set_run_mutated(run, now)?;
        specs.push(event_spec(
            EventType::RunResumedCompatible,
            "R05",
            None,
            None,
            None,
            json!({"compatibility_evidence_digest": command.availability_evidence.evidence_digest}),
        ));
        append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Engine,
            command.permit.instance_id().as_str().to_owned(),
            specs,
        )?;
        Ok(state.runs.get(&key).expect("run").clone())
        })
    }
    async fn claim_node_attempt(
        &self,
        scope: &ExecutionScope,
        command: ClaimNodeAttempt,
    ) -> Result<ClaimNodeAttemptResult, StoreError> {
        self.transaction(|mut state, now| {
        running_fence(&state, scope, &command.run_id, Some(&command.permit), now)?;
        verified(&mut state, scope, &command.bound_input, now)?;
        if command.bound_input.media_type() != "application/json" {
            return Err(StoreError::InvalidField);
        }
        let run_key = (scope.clone(), command.run_id.clone());
        let run_snapshot = state
            .runs
            .get(&run_key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if run_snapshot.status == RunState::BlockedIncompatible {
            return Err(StoreError::RunBlockedIncompatible);
        }
        if run_snapshot.status != RunState::Running {
            return Err(StoreError::IllegalTransition);
        }
        if command.bound_input.size_bytes() > run_snapshot.limits.max_inline_json_bytes_per_value {
            return Err(StoreError::RunLimitApplied {
                code: "InlineJsonLimitExceeded".to_owned(),
            });
        }
        let aggregate_after_input = run_snapshot
            .aggregate_object_bytes
            .checked_add(command.bound_input.size_bytes())
            .ok_or(StoreError::ArithmeticOverflow)?;
        if aggregate_after_input > run_snapshot.limits.max_aggregate_object_bytes_per_run {
            return Err(StoreError::RunLimitApplied {
                code: "AggregateObjectLimitExceeded".to_owned(),
            });
        }
        let node_key = (
            scope.clone(),
            command.run_id.clone(),
            command.node_id.clone(),
        );
        let mut node = state
            .nodes
            .get(&node_key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let source_status = node.status;
        if node.version != command.expected_node_version
            || !matches!(node.status, NodeState::Ready | NodeState::BudgetWaiting)
            || node.active_attempt_id.is_some()
        {
            return Err(StoreError::CasConflict);
        }
        if node.kind != NodeKind::Action {
            return Err(StoreError::IllegalTransition);
        }
        let definition = state
            .run_definitions
            .get(&run_key)
            .cloned()
            .ok_or(StoreError::CorruptControlPlane)?;
        let (action, retry, timeout_ms, declared_max) =
            action_config(&definition, &node).ok_or(StoreError::CorruptControlPlane)?;
        if node.attempt_count >= retry.max_attempts {
            return Err(StoreError::IllegalTransition);
        }
        if state.attempts.contains_key(&(
            scope.clone(),
            command.run_id.clone(),
            command.attempt_id.clone(),
        )) {
            return Err(StoreError::AttemptIdConflict);
        }
        if run_snapshot.total_attempt_count >= run_snapshot.limits.max_total_attempts {
            let mut specs = vec![event_spec(
                EventType::NodeContractFailed,
                if node.status == NodeState::BudgetWaiting {
                    "N64"
                } else {
                    "N46"
                },
                Some(&node.node_instance_id),
                None,
                None,
                json!({"failure_kind": "RunAttemptLimitExceeded"}),
            )];
            node.status = NodeState::ContractFailed;
            node.failure_kind = Some(NodeFailureKind::RunAttemptLimitExceeded);
            node.budget_wait_amount = None;
            set_node_mutated(&mut node, now)?;
            state.nodes.insert(node_key, node);
            let run = terminalize_run(
                &mut state,
                scope,
                &command.run_id,
                RunState::ContractFailed,
                Some(RunFailureKind::RunAttemptLimitExceeded),
                "RunAttemptLimitExceeded",
                now,
                &mut specs,
            )?;
            specs.push(event_spec(
                EventType::RunContractFailed,
                "R08",
                None,
                None,
                None,
                json!({"failure_kind": "RunAttemptLimitExceeded"}),
            ));
            append_batch(
                &mut state,
                scope,
                &command.run_id,
                now,
                EventActorKind::Engine,
                command.permit.instance_id().as_str().to_owned(),
                specs,
            )?;
            return Ok(ClaimNodeAttemptResult::RunLimitApplied(run));
        }
        let available = run_snapshot
            .budget_limit
            .0
            .checked_sub(run_snapshot.budget_consumed.0)
            .and_then(|value| value.checked_sub(run_snapshot.budget_reserved.0))
            .ok_or(StoreError::ArithmeticOverflow)?;
        let limit_minus_consumed = run_snapshot
            .budget_limit
            .0
            .checked_sub(run_snapshot.budget_consumed.0)
            .ok_or(StoreError::ArithmeticOverflow)?;
        if available < declared_max.0 {
            if declared_max.0 <= limit_minus_consumed {
                if source_status == NodeState::BudgetWaiting
                    && node.budget_wait_amount == Some(declared_max)
                {
                    return Ok(ClaimNodeAttemptResult::BudgetWaitingApplied(node));
                }
                node.status = NodeState::BudgetWaiting;
                node.budget_wait_amount = Some(declared_max);
                set_node_mutated(&mut node, now)?;
                state.nodes.insert(node_key, node.clone());
                bump_frontier_epoch(&mut state, scope, &command.run_id)?;
                let run = state.runs.get_mut(&run_key).expect("run");
                set_run_mutated(run, now)?;
                append_batch(
                    &mut state,
                    scope,
                    &command.run_id,
                    now,
                    EventActorKind::Engine,
                    command.permit.instance_id().as_str().to_owned(),
                    vec![event_spec(
                        EventType::NodeBudgetWaiting,
                        "N59",
                        Some(&command.node_id),
                        None,
                        None,
                        json!({"requested": declared_max, "available": available.to_string(), "consumed": run_snapshot.budget_consumed, "reserved": run_snapshot.budget_reserved, "limit": run_snapshot.budget_limit}),
                    )],
                )?;
                return Ok(ClaimNodeAttemptResult::BudgetWaitingApplied(node));
            }
            node.status = NodeState::BudgetExhausted;
            node.failure_kind = None;
            node.budget_wait_amount = None;
            set_node_mutated(&mut node, now)?;
            state.nodes.insert(node_key, node);
            let mut specs = vec![
                event_spec(
                    EventType::NodeBudgetExhausted,
                    if source_status == NodeState::BudgetWaiting { "N60" } else { "N27" },
                    Some(&command.node_id),
                    None,
                    None,
                    json!({"requested": declared_max, "available": available.to_string(), "limit_minus_consumed": limit_minus_consumed.to_string()}),
                ),
                event_spec(
                    EventType::BudgetReservationRefused,
                    if source_status == NodeState::BudgetWaiting { "N60" } else { "N27" },
                    Some(&command.node_id),
                    None,
                    None,
                    json!({"requested": declared_max, "consumed": run_snapshot.budget_consumed, "reserved": run_snapshot.budget_reserved, "limit": run_snapshot.budget_limit, "available": available.to_string(), "permanently_infeasible": true}),
                ),
            ];
            let run = terminalize_run(
                &mut state,
                scope,
                &command.run_id,
                RunState::BudgetExhausted,
                None,
                "BudgetExhausted",
                now,
                &mut specs,
            )?;
            specs.push(event_spec(
                EventType::RunBudgetExhausted,
                "R10",
                None,
                None,
                None,
                json!({"node_instance_id": command.node_id, "requested": declared_max, "available": available.to_string(), "limit_minus_consumed": limit_minus_consumed.to_string(), "permanently_infeasible": true}),
            ));
            append_batch(
                &mut state,
                scope,
                &command.run_id,
                now,
                EventActorKind::Engine,
                command.permit.instance_id().as_str().to_owned(),
                specs,
            )?;
            return Ok(ClaimNodeAttemptResult::BudgetExhaustedApplied(run));
        }
        let credential = CompletionCredential::from_minted_bytes(entropy()?);
        let deadline_at = checked_add_time(now, timeout_ms)?;
        let bound_input_ref = json_ref(
                &mut state,
            scope,
            &command.bound_input,
            ArtifactKind::ActionInvocationInput,
            Some(&command.run_id),
            Some(&command.node_id),
            Some(&command.attempt_id),
            0,
            now,
        )?;
        let reference_location = if node.parent_map_instance_id.is_some() {
            format!("{}/map_action", node.definition_node_id.as_str())
        } else {
            node.definition_node_id.as_str().to_owned()
        };
        let invocation = ActionInvocation {
            scope: scope.clone(),
            run_id: command.run_id.clone(),
            invocation_id: command.attempt_id.clone(),
            node_instance_id: command.node_id.clone(),
            attempt_id: command.attempt_id.clone(),
            action_reference_location: reference_location,
            action_name: action.name.clone(),
            contract_version: action.contract_version.clone(),
            revision_hash: run_snapshot.revision_hash.clone(),
            input_schema_digest: action.input_schema_digest.clone(),
            output_schema_digest: action.output_schema_digest.clone(),
            compatible_implementation_requirement: action
                .compatible_implementation_requirement
                .clone(),
            bound_input_ref,
            bound_input_digest: command.bound_input.digest().clone(),
            bound_input_size_bytes: command.bound_input.size_bytes(),
            binding_derivation_digest: command.binding_derivation_digest,
            created_at: now,
        };
        let attempt_number = node
            .attempt_count
            .checked_add(1)
            .ok_or(StoreError::ArithmeticOverflow)?;
        let attempt = NodeAttempt {
            scope: scope.clone(),
            run_id: command.run_id.clone(),
            attempt_id: command.attempt_id.clone(),
            node_instance_id: command.node_id.clone(),
            attempt_number,
            worker_id: command.worker_id.clone(),
            engine_instance_id: command.permit.instance_id().clone(),
            engine_generation: command.permit.generation(),
            completion_credential_digest: credential.digest(),
            invocation_id: invocation.invocation_id.clone(),
            idempotency_key: idempotency_key(scope, &command.run_id, &command.node_id),
            status: AttemptState::Started,
            declared_max_cost: declared_max,
            reserved_cost: declared_max,
            settled_cost: None,
            deadline_at,
            started_at: now,
            finished_at: None,
            output_ref: None,
            artifact_refs: Vec::new(),
            error_class: None,
            error_code: None,
            diagnostics_ref: None,
        };
        node.status = NodeState::Running;
        node.active_attempt_id = Some(command.attempt_id.clone());
        node.attempt_count = attempt_number;
        node.budget_wait_amount = None;
        set_node_mutated(&mut node, now)?;
        state.nodes.insert(node_key, node);
        state.attempts.insert(
            (
                scope.clone(),
                command.run_id.clone(),
                command.attempt_id.clone(),
            ),
            attempt.clone(),
        );
        state.invocations.insert(
            (
                scope.clone(),
                command.run_id.clone(),
                invocation.invocation_id.clone(),
            ),
            invocation.clone(),
        );
        bump_frontier_epoch(&mut state, scope, &command.run_id)?;
        let run = state.runs.get_mut(&run_key).expect("run");
        run.total_attempt_count = run
            .total_attempt_count
            .checked_add(1)
            .ok_or(StoreError::ArithmeticOverflow)?;
        run.budget_reserved.0 = run
            .budget_reserved
            .0
            .checked_add(declared_max.0)
            .ok_or(StoreError::ArithmeticOverflow)?;
        run.aggregate_object_bytes = run
            .aggregate_object_bytes
            .checked_add(command.bound_input.size_bytes())
            .ok_or(StoreError::ArithmeticOverflow)?;
        if run.aggregate_object_bytes > run.limits.max_aggregate_object_bytes_per_run {
            return Err(StoreError::RunLimitApplied {
                code: "AggregateObjectLimitExceeded".to_owned(),
            });
        }
        set_run_mutated(run, now)?;
        let ledger_key = (scope.clone(), command.run_id.clone());
        let ledger_seq = u64::try_from(state.ledger.get(&ledger_key).map_or(0, Vec::len))
            .map_err(|_| StoreError::ArithmeticOverflow)?
            .checked_add(1)
            .ok_or(StoreError::ArithmeticOverflow)?;
        state
            .ledger
            .entry(ledger_key.clone())
            .or_default()
            .push(BudgetLedgerEntry {
                scope: scope.clone(),
                run_id: command.run_id.clone(),
                ledger_seq,
                attempt_id: command.attempt_id.clone(),
                node_instance_id: command.node_id.clone(),
                kind: BudgetLedgerKind::Reserve,
                reserved_delta: i128::from(declared_max.0),
                consumed_delta: CostUnits(0),
                reservation_amount: declared_max,
                reason: BudgetLedgerReason::Started,
                created_at: now,
            });
        let available_after = available
            .checked_sub(declared_max.0)
            .ok_or(StoreError::ArithmeticOverflow)?;
        append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Engine,
            command.permit.instance_id().as_str().to_owned(),
            vec![
                event_spec(
                    EventType::AttemptStarted,
                    "A01",
                    Some(&command.node_id),
                    Some(&command.attempt_id),
                    None,
                    json!({"attempt_number": attempt_number, "worker_id": command.worker_id, "engine_generation": command.permit.generation(), "deadline_at": deadline_at, "declared_max_cost_units": declared_max, "idempotency_key_digest": digest(attempt.idempotency_key.as_bytes()), "bound_input_digest": invocation.bound_input_digest, "completion_credential_digest": credential.digest()}),
                ),
                event_spec(
                    EventType::NodeAttemptClaimed,
                    "N05",
                    Some(&command.node_id),
                    Some(&command.attempt_id),
                    None,
                    json!({"attempt_id": command.attempt_id, "invocation_id": invocation.invocation_id, "attempt_number": attempt_number, "worker_id": attempt.worker_id}),
                ),
                event_spec(
                    EventType::BudgetReserved,
                    "A01",
                    Some(&command.node_id),
                    Some(&attempt.attempt_id),
                    None,
                    json!({"ledger_seq": ledger_seq, "amount": declared_max, "available_after": available_after.to_string()}),
                ),
            ],
        )?;
        Ok(ClaimNodeAttemptResult::Claimed {
            invocation,
            completion_credential: credential,
        })
        })
    }
    async fn complete_attempt(
        &self,
        scope: &ExecutionScope,
        command: CompleteAttempt,
    ) -> Result<CompleteAttemptResult, StoreError> {
        self.transaction(|mut state, now| {
        command
            .submitted_outcome
            .validate()
            .map_err(|_| StoreError::InvalidField)?;
        let attempt_key = (
            scope.clone(),
            command.run_id.clone(),
            command.attempt_id.clone(),
        );
        let mut attempt = state
            .attempts
            .get(&attempt_key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if attempt.node_instance_id != command.node_id
            || attempt.completion_credential_digest != command.completion_credential.digest()
        {
            state.attempts.insert(attempt_key, attempt);
            return Err(StoreError::InvalidCompletionCredential);
        }
        if attempt.status != AttemptState::Started {
            if state.stale_observed.insert((
                scope.clone(),
                command.run_id.clone(),
                command.attempt_id.clone(),
            )) {
                let immutable = attempt.status;
                state.attempts.insert(attempt_key, attempt.clone());
                append_batch(
                    &mut state,
                    scope,
                    &command.run_id,
                    now,
                    EventActorKind::ActionCompletion,
                    "completion".to_owned(),
                    vec![event_spec(
                        EventType::StaleCompletionObserved,
                        match immutable {
                            AttemptState::Succeeded => "A10",
                            AttemptState::RetryableFailed => "A11",
                            AttemptState::PermanentFailed => "A12",
                            AttemptState::ContractFailed => "A13",
                            AttemptState::TimedOut => "A14",
                            AttemptState::UnknownOutcome => "A15",
                            AttemptState::Cancelled => "A16",
                            AttemptState::Stale => "A17",
                            AttemptState::Started => unreachable!(),
                        },
                        Some(&command.node_id),
                        Some(&command.attempt_id),
                        None,
                        json!({"immutable_terminal_state": immutable, "submitted_outcome_category": outcome_category(&command.submitted_outcome), "submitted_payload_digest": outcome_digest(&command.submitted_outcome), "database_arrival_at": now}),
                    )],
                )?;
                return Ok(CompleteAttemptResult::StaleRecorded(attempt));
            }
            state.attempts.insert(attempt_key, attempt.clone());
            return Ok(CompleteAttemptResult::AlreadyObserved(attempt));
        }
        running_fence(&state, scope, &command.run_id, None, now)?;
        let node_key = (
            scope.clone(),
            command.run_id.clone(),
            command.node_id.clone(),
        );
        let mut node = state
            .nodes
            .get(&node_key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let active = node.active_attempt_id.as_ref() == Some(&command.attempt_id);
        if !active {
            let reservation = attempt.reserved_cost;
            settle(
                &mut state,
                scope,
                &command.run_id,
                &mut attempt,
                reservation,
                BudgetLedgerReason::Stale,
                now,
            )?;
            attempt.status = AttemptState::Stale;
            attempt.finished_at = Some(now);
            state.attempts.insert(attempt_key, attempt.clone());
            let settled = budget_settled_spec(&state, scope, &command.run_id, &attempt)?;
            append_batch(
                &mut state,
                scope,
                &command.run_id,
                now,
                EventActorKind::ActionCompletion,
                "completion".to_owned(),
                vec![
                    event_spec(
                        EventType::AttemptMarkedStale,
                        "A09",
                        Some(&command.node_id),
                        Some(&command.attempt_id),
                        None,
                        json!({"active_attempt_id": node.active_attempt_id, "submitted_outcome_category": outcome_category(&command.submitted_outcome), "submitted_payload_digest": outcome_digest(&command.submitted_outcome), "charged_cost_units": reservation}),
                    ),
                    settled,
                ],
            )?;
            return Ok(CompleteAttemptResult::StaleRecorded(attempt));
        }
        if state
            .runs
            .get(&(scope.clone(), command.run_id.clone()))
            .ok_or(StoreError::NotFound)?
            .status
            != RunState::Running
        {
            state.attempts.insert(attempt_key, attempt);
            return Err(StoreError::AttemptFenced);
        }
        if let Some(diagnostics) = &command.objects.diagnostics {
            verified(&mut state, scope, diagnostics, now)?;
            if diagnostics.media_type() != "application/json" {
                state.attempts.insert(attempt_key, attempt);
                return Err(StoreError::DiagnosticsInvalid {
                    path: "/".to_owned(),
                    code: "media_type".to_owned(),
                });
            }
            if diagnostics.size_bytes() > 65_536 {
                state.attempts.insert(attempt_key, attempt);
                return Err(StoreError::DiagnosticsTooLarge {
                    limit_bytes: 65_536,
                    observed_bytes: diagnostics.size_bytes(),
                });
            }
        }
        if let ActionOutcome::Success { artifacts, .. } = &command.submitted_outcome {
            let output = command
                .objects
                .output
                .as_ref()
                .ok_or(StoreError::ObjectNotVerified)?;
            verified(&mut state, scope, output, now)?;
            if output.media_type() != "application/json"
                || artifacts.len() != command.objects.artifacts.len()
            {
                state.attempts.insert(attempt_key, attempt);
                return Err(StoreError::ObjectNotVerified);
            }
            for object in &command.objects.artifacts {
                verified(&mut state, scope, object, now)?;
            }
            let run = state
                .runs
                .get(&(scope.clone(), command.run_id.clone()))
                .expect("run");
            if u64::try_from(artifacts.len()).map_err(|_| StoreError::ArithmeticOverflow)?
                > run.limits.max_artifacts_per_attempt
            {
                state.attempts.insert(attempt_key, attempt);
                return Err(StoreError::RunLimitApplied {
                    code: "ArtifactsPerAttemptLimitExceeded".to_owned(),
                });
            }
            let artifact_bytes =
                command
                    .objects
                    .artifacts
                    .iter()
                    .try_fold(0_u64, |total, object| {
                        total
                            .checked_add(object.size_bytes())
                            .ok_or(StoreError::ArithmeticOverflow)
                    })?;
            let charged_bytes = output
                .size_bytes()
                .checked_add(artifact_bytes)
                .ok_or(StoreError::ArithmeticOverflow)?;
            if run
                .aggregate_object_bytes
                .checked_add(charged_bytes)
                .ok_or(StoreError::ArithmeticOverflow)?
                > run.limits.max_aggregate_object_bytes_per_run
            {
                state.attempts.insert(attempt_key, attempt);
                return Err(StoreError::RunLimitApplied {
                    code: "AggregateObjectLimitExceeded".to_owned(),
                });
            }
        }
        if now >= attempt.deadline_at {
            let reservation = attempt.reserved_cost;
            let (attempt, exhausted) =
                timeout_active(&mut state, scope, &command.run_id, &mut node, attempt, now)?;
            state.attempts.insert(attempt_key.clone(), attempt.clone());
            state.nodes.insert(node_key.clone(), node.clone());
            let mut specs = timeout_specs(&state, scope, &command.run_id, &attempt, reservation, now, exhausted, node.next_eligible_at)?;
            if exhausted {
                terminalize_run(
                    &mut state,
                    scope,
                    &command.run_id,
                    RunState::RetriesExhausted,
                    None,
                    "RetriesExhausted",
                    now,
                    &mut specs,
                )?;
                specs.push(event_spec(
                    EventType::RunRetriesExhausted,
                    "R09",
                    None,
                    None,
                    None,
                    json!({"node_instance_id": command.node_id, "attempt_id": command.attempt_id, "max_attempts": attempt.attempt_number}),
                ));
            }
            specs.push(event_spec(
                EventType::StaleCompletionObserved,
                "A14",
                Some(&command.node_id),
                Some(&command.attempt_id),
                None,
                json!({"immutable_terminal_state": AttemptState::TimedOut, "submitted_outcome_category": outcome_category(&command.submitted_outcome), "submitted_payload_digest": outcome_digest(&command.submitted_outcome), "database_arrival_at": now}),
            ));
            state.stale_observed.insert((
                scope.clone(),
                command.run_id.clone(),
                command.attempt_id.clone(),
            ));
            state.attempts.insert(attempt_key, attempt.clone());
            state.nodes.insert(node_key, node);
            append_batch(
                &mut state,
                scope,
                &command.run_id,
                now,
                EventActorKind::ActionCompletion,
                "completion".to_owned(),
                specs,
            )?;
            return if exhausted {
                Ok(CompleteAttemptResult::TimedOutAndStaleRecorded(attempt))
            } else {
                Ok(CompleteAttemptResult::TimedOutAndStaleRecorded(attempt))
            };
        }
        let (actual, reason) = outcome_cost_reason(&command.submitted_outcome);
        let contract_cost_failure = actual.0 > attempt.reserved_cost.0;
        let charged = if contract_cost_failure {
            attempt.reserved_cost
        } else {
            actual
        };
        settle(
            &mut state,
            scope,
            &command.run_id,
            &mut attempt,
            charged,
            reason,
            now,
        )?;
        let mut specs = Vec::new();
        if contract_cost_failure {
            attempt.status = AttemptState::ContractFailed;
            attempt.error_class = Some(AttemptErrorClass::Contract);
            attempt.finished_at = Some(now);
            node.status = NodeState::ContractFailed;
            node.active_attempt_id = None;
            node.failure_kind = Some(NodeFailureKind::ActionCostProtocolViolation);
            set_node_mutated(&mut node, now)?;
            specs.push(event_spec(EventType::AttemptContractFailed, "A05", Some(&command.node_id), Some(&command.attempt_id), None, json!({"charged_cost_units": charged, "failure_kind": "ActionCostProtocolViolation"})));
            specs.push(event_spec(EventType::NodeContractFailed, "N21", Some(&command.node_id), Some(&command.attempt_id), None, json!({"attempt_id": command.attempt_id, "failure_kind": "ActionCostProtocolViolation"})));
            specs.push(budget_settled_spec(&state, scope, &command.run_id, &attempt)?);
            state.nodes.insert(node_key, node);
            state.attempts.insert(attempt_key, attempt);
            let run = terminalize_run(
                &mut state,
                scope,
                &command.run_id,
                RunState::ContractFailed,
                Some(RunFailureKind::ActionCostProtocolViolation),
                "ActionCostProtocolViolation",
                now,
                &mut specs,
            )?;
            specs.push(event_spec(
                EventType::RunContractFailed,
                "R08",
                None,
                None,
                None,
                json!({"failure_kind": "ActionCostProtocolViolation"}),
            ));
            append_batch(
                &mut state,
                scope,
                &command.run_id,
                now,
                EventActorKind::ActionCompletion,
                "completion".to_owned(),
                specs,
            )?;
            return Ok(CompleteAttemptResult::TerminalRun(run));
        }
        match &command.submitted_outcome {
            ActionOutcome::Success { artifacts, .. } => {
                let output = command
                    .objects
                    .output
                    .as_ref()
                    .ok_or(StoreError::ObjectNotVerified)?;
                if artifacts.len() != command.objects.artifacts.len() {
                    return Err(StoreError::ObjectNotVerified);
                }
                let run_snapshot = state
                    .runs
                    .get(&(scope.clone(), command.run_id.clone()))
                    .expect("run")
                    .clone();
                if u64::try_from(artifacts.len())
                    .map_err(|_| StoreError::ArithmeticOverflow)?
                    > run_snapshot.limits.max_artifacts_per_attempt
                {
                    return Err(StoreError::RunLimitApplied {
                        code: "ArtifactsPerAttemptLimitExceeded".to_owned(),
                    });
                }
                let output_ref = json_ref(
                &mut state,
                    scope,
                    output,
                    ArtifactKind::NodeOutput,
                    Some(&command.run_id),
                    Some(&command.node_id),
                    Some(&command.attempt_id),
                    0,
                    now,
                )?;
                let mut refs = Vec::new();
                for (index, object) in command.objects.artifacts.iter().enumerate() {
                    verified(&mut state, scope, object, now)?;
                    refs.push(artifact(
                        &mut state,
                        scope,
                        object,
                        ArtifactKind::ActionArtifact,
                        Some(&command.run_id),
                        Some(&command.node_id),
                        Some(&command.attempt_id),
                        u32::try_from(index).map_err(|_| StoreError::ArithmeticOverflow)?,
                        now,
                    )?);
                }
                let bytes = command.objects.artifacts.iter().try_fold(
                    output.size_bytes(),
                    |total, object| {
                        total
                            .checked_add(object.size_bytes())
                            .ok_or(StoreError::ArithmeticOverflow)
                    },
                )?;
                let run = state
                    .runs
                    .get_mut(&(scope.clone(), command.run_id.clone()))
                    .expect("run");
                run.aggregate_object_bytes = run
                    .aggregate_object_bytes
                    .checked_add(bytes)
                    .ok_or(StoreError::ArithmeticOverflow)?;
                if run.aggregate_object_bytes > run.limits.max_aggregate_object_bytes_per_run {
                    return Err(StoreError::RunLimitApplied {
                        code: "AggregateObjectLimitExceeded".to_owned(),
                    });
                }
                set_run_mutated(run, now)?;
                attempt.status = AttemptState::Succeeded;
                attempt.output_ref = Some(output_ref.clone());
                attempt.artifact_refs = refs.clone();
                attempt.finished_at = Some(now);
                node.status = NodeState::Succeeded;
                node.active_attempt_id = None;
                node.result_ref = Some(output_ref);
                set_node_mutated(&mut node, now)?;
                specs.push(event_spec(EventType::AttemptSucceeded, "A02", Some(&command.node_id), Some(&command.attempt_id), None, json!({"actual_cost_units": actual, "output_digest": output.digest(), "artifact_digests": refs.iter().map(|reference| reference.digest.clone()).collect::<Vec<_>>()})));
                specs.push(event_spec(EventType::NodeSucceeded, "N18", Some(&command.node_id), Some(&command.attempt_id), None, json!({"attempt_id": command.attempt_id, "output_digest": output.digest(), "artifact_digests": refs.iter().map(|reference| reference.digest.clone()).collect::<Vec<_>>()})));
                specs.push(budget_settled_spec(&state, scope, &command.run_id, &attempt)?);
                state.nodes.insert(node_key, node);
                state.attempts.insert(attempt_key, attempt.clone());
                frontier_reduce(
                    &mut state,
                    scope,
                    &command.run_id,
                    &command.node_id,
                    None,
                    now,
                    &mut specs,
                )?;
                append_batch(
                    &mut state,
                    scope,
                    &command.run_id,
                    now,
                    EventActorKind::ActionCompletion,
                    "completion".to_owned(),
                    specs,
                )?;
                Ok(CompleteAttemptResult::Applied(attempt))
            }
            ActionOutcome::Retryable { code, .. } => {
                attempt.status = AttemptState::RetryableFailed;
                attempt.error_class = Some(AttemptErrorClass::Retryable);
                attempt.error_code = Some(code.clone());
                attempt.finished_at = Some(now);
                let definition = state
                    .run_definitions
                    .get(&(scope.clone(), command.run_id.clone()))
                    .expect("definition")
                    .clone();
                let (_, retry, _, _) =
                    action_config(&definition, &node).ok_or(StoreError::CorruptControlPlane)?;
                specs.push(event_spec(
                    EventType::AttemptRetryableFailed,
                    "A03",
                    Some(&command.node_id),
                    Some(&command.attempt_id),
                    None,
                    json!({"actual_cost_units": actual, "error_code": code}),
                ));
                if attempt.attempt_number >= retry.max_attempts {
                    node.status = NodeState::RetriesExhausted;
                    node.active_attempt_id = None;
                    set_node_mutated(&mut node, now)?;
                    specs.push(event_spec(EventType::NodeRetriesExhausted, "N24", Some(&command.node_id), Some(&command.attempt_id), None, json!({"attempt_id": command.attempt_id, "attempt_number": attempt.attempt_number, "max_attempts": retry.max_attempts, "cause": "retryable"})));
                    specs.push(budget_settled_spec(&state, scope, &command.run_id, &attempt)?);
                    state.nodes.insert(node_key, node);
                    state.attempts.insert(attempt_key, attempt.clone());
                    let run = terminalize_run(
                        &mut state,
                        scope,
                        &command.run_id,
                        RunState::RetriesExhausted,
                        None,
                        "RetriesExhausted",
                        now,
                        &mut specs,
                    )?;
                    specs.push(event_spec(EventType::RunRetriesExhausted, "R09", None, None, None, json!({"node_instance_id": command.node_id, "attempt_id": command.attempt_id, "max_attempts": retry.max_attempts})));
                    append_batch(
                        &mut state,
                        scope,
                        &command.run_id,
                        now,
                        EventActorKind::ActionCompletion,
                        "completion".to_owned(),
                        specs,
                    )?;
                    Ok(CompleteAttemptResult::TerminalRun(run))
                } else {
                    node.status = NodeState::RetryWaiting;
                    node.active_attempt_id = None;
                    node.next_eligible_at = Some(retry_at(now, retry, attempt.attempt_number)?);
                    set_node_mutated(&mut node, now)?;
                    specs.push(event_spec(EventType::NodeRetryScheduled, "N19", Some(&command.node_id), Some(&command.attempt_id), None, json!({"attempt_id": command.attempt_id, "attempt_number": attempt.attempt_number, "next_eligible_at": node.next_eligible_at, "cause": "retryable"})));
                    let returned = node.clone();
                    state.nodes.insert(node_key, node);
                    state.attempts.insert(attempt_key, attempt.clone());
                    specs.push(budget_settled_spec(&state, scope, &command.run_id, &attempt)?);
                    append_batch(
                        &mut state,
                        scope,
                        &command.run_id,
                        now,
                        EventActorKind::ActionCompletion,
                        "completion".to_owned(),
                        specs,
                    )?;
                    Ok(CompleteAttemptResult::RetryScheduled(returned))
                }
            }
            ActionOutcome::Permanent { code, .. } => {
                attempt.status = AttemptState::PermanentFailed;
                attempt.error_class = Some(AttemptErrorClass::Permanent);
                attempt.error_code = Some(code.clone());
                attempt.finished_at = Some(now);
                node.status = NodeState::Failed;
                node.active_attempt_id = None;
                node.failure_kind = Some(NodeFailureKind::ActionPermanent);
                set_node_mutated(&mut node, now)?;
                specs.push(event_spec(
                    EventType::AttemptPermanentFailed,
                    "A04",
                    Some(&command.node_id),
                    Some(&command.attempt_id),
                    None,
                    json!({"actual_cost_units": actual, "error_code": code}),
                ));
                specs.push(event_spec(EventType::NodeFailed, "N20", Some(&command.node_id), Some(&command.attempt_id), None, json!({"attempt_id": command.attempt_id, "failure_kind": "ActionPermanent", "error_code": code})));
                specs.push(budget_settled_spec(&state, scope, &command.run_id, &attempt)?);
                state.nodes.insert(node_key, node);
                state.attempts.insert(attempt_key, attempt.clone());
                let run = terminalize_run(
                    &mut state,
                    scope,
                    &command.run_id,
                    RunState::Failed,
                    Some(RunFailureKind::ActionPermanent),
                    "ActionPermanent",
                    now,
                    &mut specs,
                )?;
                specs.push(event_spec(
                    EventType::RunFailed,
                    "R07",
                    None,
                    None,
                    None,
                    json!({"failure_kind": "ActionPermanent"}),
                ));
                append_batch(
                    &mut state,
                    scope,
                    &command.run_id,
                    now,
                    EventActorKind::ActionCompletion,
                    "completion".to_owned(),
                    specs,
                )?;
                Ok(CompleteAttemptResult::TerminalRun(run))
            }
        }
        })
    }
    async fn timeout_attempt(
        &self,
        scope: &ExecutionScope,
        command: TimeoutAttempt,
    ) -> Result<NodeAttempt, StoreError> {
        self.transaction(|mut state, now| {
        let fenced_run =
            running_fence(&state, scope, &command.run_id, Some(&command.permit), now)?;
        if fenced_run.status != RunState::Running {
            return Err(StoreError::IllegalTransition);
        }
        let attempt_key = (
            scope.clone(),
            command.run_id.clone(),
            command.attempt_id.clone(),
        );
        let attempt = state
            .attempts
            .remove(&attempt_key)
            .ok_or(StoreError::NotFound)?;
        if attempt.status != AttemptState::Started || attempt.node_instance_id != command.node_id {
            return Err(StoreError::AttemptFenced);
        }
        if attempt.deadline_at > now {
            let deadline = attempt.deadline_at;
            state.attempts.insert(attempt_key, attempt);
            return Err(StoreError::DeadlineNotDue {
                database_now: now,
                deadline,
            });
        }
        let node_key = (
            scope.clone(),
            command.run_id.clone(),
            command.node_id.clone(),
        );
        let mut node = state
            .nodes
            .get(&node_key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if node.active_attempt_id.as_ref() != Some(&command.attempt_id) {
            state.attempts.insert(attempt_key, attempt);
            return Err(StoreError::AttemptFenced);
        }
        let reservation = attempt.reserved_cost;
        let (attempt, exhausted) =
            timeout_active(&mut state, scope, &command.run_id, &mut node, attempt, now)?;
        let mut specs = timeout_specs(&state, scope, &command.run_id, &attempt, reservation, now, exhausted, node.next_eligible_at)?;
        state.attempts.insert(attempt_key, attempt.clone());
        state.nodes.insert(node_key, node);
        if exhausted {
            terminalize_run(
                &mut state,
                scope,
                &command.run_id,
                RunState::RetriesExhausted,
                None,
                "RetriesExhausted",
                now,
                &mut specs,
            )?;
            specs.push(event_spec(EventType::RunRetriesExhausted, "R09", None, None, None, json!({"node_instance_id": command.node_id, "attempt_id": command.attempt_id, "max_attempts": attempt.attempt_number})));
        }
        append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Clock,
            command.permit.instance_id().as_str().to_owned(),
            specs,
        )?;
        Ok(attempt)
        })
    }
    async fn recover_abandoned_attempts_for_run(
        &self,
        scope: &ExecutionScope,
        command: RecoverAbandonedAttemptsForRun,
    ) -> Result<Vec<NodeAttempt>, StoreError> {
        self.transaction(|mut state, now| {
        running_fence(&state, scope, &command.run_id, Some(&command.permit), now)?;
        let mut keys = state
            .attempts
            .iter()
            .filter(|((attempt_scope, attempt_run, _), attempt)| {
                attempt_scope == scope
                    && attempt_run == &command.run_id
                    && attempt.status == AttemptState::Started
                    && attempt.engine_generation < command.permit.generation()
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        keys.sort_by_key(|key| {
            let attempt = state.attempts.get(key).expect("attempt");
            let node = state
                .nodes
                .get(&(
                    scope.clone(),
                    command.run_id.clone(),
                    attempt.node_instance_id.clone(),
                ))
                .expect("node");
            (
                node.topological_rank,
                node.map_item_index,
                node.node_instance_id.clone(),
                attempt.attempt_number,
                attempt.attempt_id.clone(),
            )
        });
        if state.attempts.values().any(|attempt| {
            attempt.scope == *scope
                && attempt.run_id == command.run_id
                && attempt.status == AttemptState::Started
                && attempt.engine_generation == command.permit.generation()
        }) {
            return Err(StoreError::CurrentGenerationAttemptPresent);
        }
        let definition = state
            .run_definitions
            .get(&(scope.clone(), command.run_id.clone()))
            .cloned()
            .ok_or(StoreError::CorruptControlPlane)?;
        let mut recovered = Vec::new();
        let mut rows = Vec::new();
        let mut specs = Vec::new();
        for key in keys {
            let mut attempt = state.attempts.remove(&key).expect("frozen key remains present");
            let reservation = attempt.reserved_cost;
            settle(
                &mut state,
                scope,
                &command.run_id,
                &mut attempt,
                reservation,
                BudgetLedgerReason::UnknownOutcome,
                now,
            )?;
            attempt.status = AttemptState::UnknownOutcome;
            attempt.finished_at = Some(now);
            let node_key = (
                scope.clone(),
                command.run_id.clone(),
                attempt.node_instance_id.clone(),
            );
            let node = state
                .nodes
                .get(&node_key)
                .cloned()
                .ok_or(StoreError::NotFound)?;
            if node.active_attempt_id.as_ref() != Some(&attempt.attempt_id) {
                return Err(StoreError::AttemptFenced);
            }
            let (_, retry, _, _) =
                action_config(&definition, &node).ok_or(StoreError::CorruptControlPlane)?;
            let exhausted = attempt.attempt_number >= retry.max_attempts;
            specs.push(event_spec(EventType::AttemptOutcomeUnknown, "A07", Some(&attempt.node_instance_id), Some(&attempt.attempt_id), None, json!({"dead_engine_generation": attempt.engine_generation, "recovery_generation": command.permit.generation(), "charged_cost_units": reservation})));
            specs.push(budget_settled_spec(&state, scope, &command.run_id, &attempt)?);
            state.attempts.insert(key.clone(), attempt.clone());
            recovered.push(attempt.clone());
            rows.push((key, node_key, attempt, node, exhausted, retry.clone()));
        }
        let exhausted_primary = rows.iter().position(|row| row.4);
        if let Some(primary_index) = exhausted_primary {
            let primary = rows[primary_index].2.clone();
            let primary_max_attempts = rows[primary_index].5.max_attempts;
            for (index, (_, node_key, attempt, mut node, _, retry)) in
                rows.into_iter().enumerate()
            {
                node.active_attempt_id = None;
                node.next_eligible_at = None;
                node.budget_wait_amount = None;
                if index == primary_index {
                    node.status = NodeState::RetriesExhausted;
                    specs.push(event_spec(
                        EventType::NodeRetriesExhausted,
                        "N26",
                        Some(&node.node_instance_id),
                        Some(&attempt.attempt_id),
                        None,
                        json!({"attempt_id": attempt.attempt_id, "attempt_number": attempt.attempt_number, "max_attempts": retry.max_attempts, "cause": "unknown"}),
                    ));
                } else {
                    let prior = node.status;
                    node.status = NodeState::Cancelled;
                    specs.push(event_spec(
                        EventType::NodeCancelled,
                        "N66",
                        Some(&node.node_instance_id),
                        None,
                        None,
                        json!({"prior_status": prior, "terminal_run_status": RunState::RetriesExhausted, "reason_code": "RetriesExhausted"}),
                    ));
                }
                set_node_mutated(&mut node, now)?;
                state.nodes.insert(node_key, node);
            }
            terminalize_run(
                &mut state,
                scope,
                &command.run_id,
                RunState::RetriesExhausted,
                None,
                "RetriesExhausted",
                now,
                &mut specs,
            )?;
            specs.push(event_spec(
                EventType::RunRetriesExhausted,
                "R09",
                None,
                None,
                None,
                json!({"node_instance_id": primary.node_instance_id, "attempt_id": primary.attempt_id, "max_attempts": primary_max_attempts}),
            ));
        } else {
            for (_, node_key, attempt, mut node, _, retry) in rows {
                let next_eligible_at = retry_at(now, &retry, attempt.attempt_number)?;
                node.status = NodeState::RetryWaiting;
                node.active_attempt_id = None;
                node.next_eligible_at = Some(next_eligible_at);
                set_node_mutated(&mut node, now)?;
                specs.push(event_spec(
                    EventType::NodeRetryScheduled,
                    "N23",
                    Some(&node.node_instance_id),
                    Some(&attempt.attempt_id),
                    None,
                    json!({"attempt_id": attempt.attempt_id, "attempt_number": attempt.attempt_number, "next_eligible_at": next_eligible_at, "cause": "unknown"}),
                ));
                state.nodes.insert(node_key, node);
            }
        }
        if !specs.is_empty() {
            append_batch(
                &mut state,
                scope,
                &command.run_id,
                now,
                EventActorKind::Recovery,
                command.permit.instance_id().as_str().to_owned(),
                specs,
            )?;
        }
        Ok(recovered)
        })
    }
    async fn release_retry(
        &self,
        scope: &ExecutionScope,
        command: ReleaseRetry,
    ) -> Result<NodeRun, StoreError> {
        self.transaction(|mut state, now| {
            running_fence(&state, scope, &command.run_id, Some(&command.permit), now)?;
            let key = (
                scope.clone(),
                command.run_id.clone(),
                command.node_id.clone(),
            );
            let node = state.nodes.get_mut(&key).ok_or(StoreError::NotFound)?;
            if node.version != command.expected_node_version
                || node.status != NodeState::RetryWaiting
            {
                return Err(StoreError::CasConflict);
            }
            let eligible = node
                .next_eligible_at
                .ok_or(StoreError::CorruptControlPlane)?;
            if now < eligible {
                return Err(StoreError::RetryNotDue {
                    database_now: now,
                    next_eligible_at: eligible,
                });
            }
            node.status = NodeState::Ready;
            node.next_eligible_at = None;
            set_node_mutated(node, now)?;
            let returned = node.clone();
            bump_frontier_epoch(&mut state, scope, &command.run_id)?;
            let run = state
                .runs
                .get_mut(&(scope.clone(), command.run_id.clone()))
                .ok_or(StoreError::NotFound)?;
            set_run_mutated(run, now)?;
            append_batch(
                &mut state,
                scope,
                &command.run_id,
                now,
                EventActorKind::Clock,
                command.permit.instance_id().as_str().to_owned(),
                vec![event_spec(
                    EventType::NodeRetryEligible,
                    "N04",
                    Some(&command.node_id),
                    None,
                    None,
                    json!({"next_eligible_at": eligible, "database_now": now}),
                )],
            )?;
            Ok(returned)
        })
    }
    async fn record_choice(
        &self,
        scope: &ExecutionScope,
        command: RecordChoice,
    ) -> Result<NodeRun, StoreError> {
        self.transaction(|mut state, now| {
        running_fence(&state, scope, &command.run_id, Some(&command.permit), now)?;
        verified(&mut state, scope, &command.input, now)?;
        let key = (
            scope.clone(),
            command.run_id.clone(),
            command.node_id.clone(),
        );
        let mut node = state.nodes.get(&key).cloned().ok_or(StoreError::NotFound)?;
        if let (Some(existing_input), Some(existing_selection)) =
            (&node.choice_input_ref, &node.choice_selected_case)
        {
            return if existing_input.0.digest == *command.input.digest()
                && existing_selection == &command.selection
            {
                Ok(node)
            } else {
                Err(StoreError::IdempotencyConflict)
            };
        }
        if node.version != command.expected_node_version
            || node.status != NodeState::Ready
            || node.kind != NodeKind::Choice
        {
            return Err(StoreError::CasConflict);
        }
        let definition = state
            .run_definitions
            .get(&(scope.clone(), command.run_id.clone()))
            .ok_or(StoreError::CorruptControlPlane)?;
        let (cases, default) = definition
            .definition
            .nodes
            .iter()
            .find_map(|candidate| match candidate {
                NodeDefinition::Choice {
                    id, cases, default, ..
                } if id == &node.definition_node_id => Some((cases, default)),
                _ => None,
            })
            .ok_or(StoreError::CorruptControlPlane)?;
        let mut expected = None;
        for (index, case) in cases.iter().enumerate() {
            let (values, target) = match case {
                crate::definition::ChoiceCase::Equals { equals, next } => {
                    (std::slice::from_ref(equals), next)
                }
                crate::definition::ChoiceCase::In { r#in, next } => (r#in.as_slice(), next),
            };
            if values.iter().any(|value| {
                serde_jcs::to_vec(value)
                    .map(|bytes| digest(&bytes) == command.evaluated_selector_digest)
                    .unwrap_or(false)
            }) {
                expected = Some(ChoiceSelection::Case {
                    case_index: index as u32,
                    edge_id: edge_id(
                        &state
                            .runs
                            .get(&(scope.clone(), command.run_id.clone()))
                            .expect("run")
                            .revision_hash,
                        &node.definition_node_id,
                        &format!("case/{index}"),
                        target,
                    ),
                });
                break;
            }
        }
        let expected = expected.unwrap_or_else(|| ChoiceSelection::Default {
            edge_id: edge_id(
                &state
                    .runs
                    .get(&(scope.clone(), command.run_id.clone()))
                    .expect("run")
                    .revision_hash,
                &node.definition_node_id,
                "default",
                default,
            ),
        });
        if command.selection != expected {
            return Err(StoreError::InvalidField);
        }
        let selected = match &command.selection {
            ChoiceSelection::Case { edge_id, .. } | ChoiceSelection::Default { edge_id } => {
                edge_id.clone()
            }
        };
        let outgoing_ids = state
            .edges
            .values()
            .filter(|edge| {
                edge.scope == *scope
                    && edge.run_id == command.run_id
                    && edge.from_node_id == command.node_id
            })
            .map(|edge| edge.edge_id.clone())
            .collect::<BTreeSet<_>>();
        if !outgoing_ids.contains(&selected) {
            return Err(StoreError::InvalidField);
        }
        node.status = NodeState::Succeeded;
        node.choice_input_ref = Some(json_ref(
                &mut state,
            scope,
            &command.input,
            ArtifactKind::ChoiceInput,
            Some(&command.run_id),
            Some(&command.node_id),
            None,
            0,
            now,
        )?);
        node.choice_selected_case = Some(command.selection.clone());
        set_node_mutated(&mut node, now)?;
        state.nodes.insert(key, node.clone());
        let choice_payload = match &command.selection {
            ChoiceSelection::Case { case_index, .. } => {
                json!({"choice_input_digest": command.input.digest(), "selector_value_digest": command.evaluated_selector_digest, "selection_kind": "case", "case_index": case_index, "edge_id": selected})
            }
            ChoiceSelection::Default { .. } => {
                json!({"choice_input_digest": command.input.digest(), "selector_value_digest": command.evaluated_selector_digest, "selection_kind": "default", "edge_id": selected})
            }
        };
        let mut specs = vec![event_spec(
            EventType::ChoiceSelected,
            "N09",
            Some(&command.node_id),
            None,
            None,
            choice_payload,
        )];
        frontier_reduce(
            &mut state,
            scope,
            &command.run_id,
            &command.node_id,
            Some(&selected),
            now,
            &mut specs,
        )?;
        let run = state
            .runs
            .get_mut(&(scope.clone(), command.run_id.clone()))
            .expect("run");
        set_run_mutated(run, now)?;
        append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Engine,
            command.permit.instance_id().as_str().to_owned(),
            specs,
        )?;
        Ok(node)
        })
    }
    async fn expand_map(
        &self,
        scope: &ExecutionScope,
        command: ExpandMap,
    ) -> Result<NodeRun, StoreError> {
        self.transaction(|mut state, now| {
        running_fence(&state, scope, &command.run_id, Some(&command.permit), now)?;
        verified(&mut state, scope, &command.input, now)?;
        let key = (
            scope.clone(),
            command.run_id.clone(),
            command.map_node_id.clone(),
        );
        let mut node = state.nodes.get(&key).cloned().ok_or(StoreError::NotFound)?;
        if let Some(existing) = &node.map_expansion_digest {
            return if existing == &command.expansion_digest {
                Ok(node)
            } else {
                Err(StoreError::IdempotencyConflict)
            };
        }
        if node.version != command.expected_node_version
            || node.status != NodeState::Ready
            || node.kind != NodeKind::Map
        {
            return Err(StoreError::CasConflict);
        }
        let run_key = (scope.clone(), command.run_id.clone());
        let mut run = state
            .runs
            .get(&run_key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let added_nodes = u64::try_from(command.ordered_items.len())
            .map_err(|_| StoreError::ArithmeticOverflow)?;
        let dynamic_after = run
            .dynamic_node_count
            .checked_add(added_nodes)
            .ok_or(StoreError::ArithmeticOverflow)?;
        if dynamic_after > run.limits.max_dynamic_node_instances {
            return Err(StoreError::RunLimitApplied {
                code: "RunDynamicNodeLimitExceeded".to_owned(),
            });
        }
        let definition = state.run_definitions.get(&run_key).expect("definition");
        let (max_items, max_concurrency) = definition
            .definition
            .nodes
            .iter()
            .find_map(|candidate| match candidate {
                NodeDefinition::Map {
                    id,
                    max_items,
                    max_concurrency,
                    ..
                } if id == &node.definition_node_id => Some((*max_items, *max_concurrency)),
                _ => None,
            })
            .ok_or(StoreError::CorruptControlPlane)?;
        if command.ordered_items.len() > max_items as usize {
            return Err(StoreError::ContractValidationApplied {
                code: "MapBoundExceeded".to_owned(),
            });
        }
        node.map_input_ref = Some(json_ref(
                &mut state,
            scope,
            &command.input,
            ArtifactKind::MapInput,
            Some(&command.run_id),
            Some(&command.map_node_id),
            None,
            0,
            now,
        )?);
        node.map_expansion_digest = Some(command.expansion_digest.clone());
        node.map_child_count = Some(
            u32::try_from(command.ordered_items.len())
                .map_err(|_| StoreError::ArithmeticOverflow)?,
        );
        let mut specs = Vec::new();
        if command.ordered_items.is_empty() {
            node.status = NodeState::Succeeded;
            set_node_mutated(&mut node, now)?;
            state.nodes.insert(key, node.clone());
            specs.push(event_spec(EventType::MapZeroItemsSucceeded, "N07", Some(&command.map_node_id), None, None, json!({"map_input_digest": command.input.digest(), "expansion_digest": command.expansion_digest, "aggregate_digest": command.input.digest()})));
            frontier_reduce(
                &mut state,
                scope,
                &command.run_id,
                &command.map_node_id,
                None,
                now,
                &mut specs,
            )?;
        } else {
            node.status = NodeState::WaitingChildren;
            set_node_mutated(&mut node, now)?;
            state.nodes.insert(key, node.clone());
            for item in command.ordered_items {
                let child_key = (scope.clone(), command.run_id.clone(), item.child_id.clone());
                if state.nodes.contains_key(&child_key) {
                    return Err(StoreError::IdempotencyConflict);
                }
                let child = NodeRun {
                    scope: scope.clone(),
                    run_id: command.run_id.clone(),
                    node_instance_id: item.child_id.clone(),
                    definition_node_id: node.definition_node_id.clone(),
                    kind: NodeKind::Action,
                    parent_map_instance_id: Some(command.map_node_id.clone()),
                    map_item_index: Some(item.index),
                    map_item_digest: Some(item.item_digest.clone()),
                    topological_rank: node.topological_rank,
                    status: NodeState::Ready,
                    blocked_from_status: None,
                    active_attempt_id: None,
                    attempt_count: 0,
                    next_eligible_at: None,
                    budget_wait_amount: None,
                    result_ref: None,
                    failure_kind: None,
                    failure_diagnostics_ref: None,
                    incoming_total: 0,
                    incoming_satisfied: 0,
                    incoming_skipped: 0,
                    choice_input_ref: None,
                    choice_selected_case: None,
                    map_input_ref: None,
                    map_expansion_digest: None,
                    map_child_count: None,
                    approval_gate_id: None,
                    created_at: now,
                    updated_at: now,
                    version: Version(1),
                };
                specs.push(event_spec(EventType::MapChildCreated, "N02M", Some(&item.child_id), None, None, json!({"parent_map_instance_id": command.map_node_id, "item_index": item.index, "item_digest": item.item_digest, "topological_rank": child.topological_rank})));
                state.nodes.insert(child_key, child);
            }
            specs.push(event_spec(EventType::MapExpanded, "N06", Some(&command.map_node_id), None, None, json!({"map_input_digest": command.input.digest(), "expansion_digest": command.expansion_digest, "child_count": node.map_child_count, "max_concurrency": max_concurrency})));
            bump_frontier_epoch(&mut state, scope, &command.run_id)?;
        }
        run.dynamic_node_count = dynamic_after;
        run.aggregate_object_bytes = run
            .aggregate_object_bytes
            .checked_add(command.input.size_bytes())
            .ok_or(StoreError::ArithmeticOverflow)?;
        set_run_mutated(&mut run, now)?;
        state.runs.insert(run_key, run);
        append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Engine,
            command.permit.instance_id().as_str().to_owned(),
            specs,
        )?;
        Ok(node)
        })
    }
    async fn complete_map(
        &self,
        scope: &ExecutionScope,
        command: CompleteMap,
    ) -> Result<NodeRun, StoreError> {
        self.transaction(|mut state, now| {
        running_fence(&state, scope, &command.run_id, Some(&command.permit), now)?;
        verified(&mut state, scope, &command.aggregate, now)?;
        let key = (
            scope.clone(),
            command.run_id.clone(),
            command.map_node_id.clone(),
        );
        let mut node = state.nodes.get(&key).cloned().ok_or(StoreError::NotFound)?;
        if node.status == NodeState::Succeeded {
            return node
                .result_ref
                .as_ref()
                .is_some_and(|reference| reference.0.digest == *command.aggregate.digest())
                .then_some(node)
                .ok_or(StoreError::AggregateMismatch);
        }
        if node.version != command.expected_node_version
            || node.status != NodeState::WaitingChildren
        {
            return Err(StoreError::CasConflict);
        }
        if state.nodes.values().any(|child| {
            child.scope == *scope
                && child.run_id == command.run_id
                && child.parent_map_instance_id.as_ref() == Some(&command.map_node_id)
                && child.status != NodeState::Succeeded
        }) {
            return Err(StoreError::ChildrenIncomplete);
        }
        node.status = NodeState::Succeeded;
        node.result_ref = Some(json_ref(
                &mut state,
            scope,
            &command.aggregate,
            ArtifactKind::MapAggregate,
            Some(&command.run_id),
            Some(&command.map_node_id),
            None,
            0,
            now,
        )?);
        set_node_mutated(&mut node, now)?;
        state.nodes.insert(key, node.clone());
        let mut specs = vec![event_spec(
            EventType::MapSucceeded,
            "N08",
            Some(&command.map_node_id),
            None,
            None,
            json!({"child_count": node.map_child_count, "aggregate_digest": command.aggregate.digest()}),
        )];
        frontier_reduce(
            &mut state,
            scope,
            &command.run_id,
            &command.map_node_id,
            None,
            now,
            &mut specs,
        )?;
        let run = state
            .runs
            .get_mut(&(scope.clone(), command.run_id.clone()))
            .expect("run");
        run.aggregate_object_bytes = run
            .aggregate_object_bytes
            .checked_add(command.aggregate.size_bytes())
            .ok_or(StoreError::ArithmeticOverflow)?;
        set_run_mutated(run, now)?;
        append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Engine,
            command.permit.instance_id().as_str().to_owned(),
            specs,
        )?;
        Ok(node)
        })
    }
    async fn request_approval(
        &self,
        scope: &ExecutionScope,
        command: RequestApproval,
    ) -> Result<ApprovalGate, StoreError> {
        self.transaction(|mut state, now| {
        running_fence(&state, scope, &command.run_id, Some(&command.permit), now)?;
        verified(&mut state, scope, &command.request, now)?;
        let gate_key = (
            scope.clone(),
            command.run_id.clone(),
            command.gate_id.clone(),
        );
        if let Some(gate) = state.gates.get(&gate_key) {
            return Ok(gate.clone());
        }
        let node_key = (
            scope.clone(),
            command.run_id.clone(),
            command.node_id.clone(),
        );
        let mut node = state
            .nodes
            .get(&node_key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if node.version != command.expected_node_version
            || node.status != NodeState::Ready
            || node.kind != NodeKind::Approval
        {
            return Err(StoreError::CasConflict);
        }
        let definition = state
            .run_definitions
            .get(&(scope.clone(), command.run_id.clone()))
            .expect("definition");
        let gate_config = definition
            .definition
            .nodes
            .iter()
            .find_map(|candidate| match candidate {
                NodeDefinition::Approval { id, gate, .. } if id == &node.definition_node_id => {
                    Some(gate.clone())
                }
                _ => None,
            })
            .ok_or(StoreError::CorruptControlPlane)?;
        let gate = ApprovalGate {
            scope: scope.clone(),
            run_id: command.run_id.clone(),
            gate_id: command.gate_id.clone(),
            node_instance_id: command.node_id.clone(),
            request_ref: json_ref(
                &mut state,
                scope,
                &command.request,
                ArtifactKind::ApprovalRequest,
                Some(&command.run_id),
                Some(&command.node_id),
                None,
                0,
                now,
            )?,
            status: crate::run::GateState::Pending,
            expires_at: checked_add_time(now, gate_config.expires_after_ms)?,
            on_expiry: gate_config.on_expiry,
            authorization_policy: gate_config.authorization,
            decision_payload_ref: None,
            deciding_principal: None,
            resolution_source: None,
            decided_at: None,
            decision_fingerprint: None,
            version: Version(1),
        };
        node.status = NodeState::WaitingApproval;
        node.approval_gate_id = Some(command.gate_id.clone());
        set_node_mutated(&mut node, now)?;
        state.nodes.insert(node_key, node);
        state.gates.insert(gate_key, gate.clone());
        bump_frontier_epoch(&mut state, scope, &command.run_id)?;
        let authorization_policy_digest = digest(
            &serde_jcs::to_vec(&gate.authorization_policy)
                .map_err(|_| StoreError::TransactionFailed)?,
        );
        let run = state
            .runs
            .get_mut(&(scope.clone(), command.run_id.clone()))
            .expect("run");
        set_run_mutated(run, now)?;
        append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Engine,
            command.permit.instance_id().as_str().to_owned(),
            vec![
                event_spec(
                    EventType::ApprovalRequested,
                    "N11",
                    Some(&command.node_id),
                    None,
                    Some(&command.gate_id),
                    json!({"gate_id": command.gate_id, "request_digest": command.request.digest(), "expires_at": gate.expires_at, "on_expiry": gate.on_expiry, "authorization_policy_digest": authorization_policy_digest}),
                ),
                event_spec(
                    EventType::ApprovalGateCreated,
                    "G01",
                    Some(&command.node_id),
                    None,
                    Some(&gate.gate_id),
                    json!({"request_digest": command.request.digest(), "expires_at": gate.expires_at, "on_expiry": gate.on_expiry, "authorization_policy_digest": authorization_policy_digest}),
                ),
            ],
        )?;
        Ok(gate)
        })
    }
    async fn decide_approval(
        &self,
        scope: &ExecutionScope,
        command: DecideApproval,
    ) -> Result<ApprovalGate, StoreError> {
        self.transaction(|mut state, now| {
        if command.principal.scope() != scope {
            return Err(StoreError::ApprovalUnauthorized);
        }
        let gate_key = (
            scope.clone(),
            command.run_id.clone(),
            command.gate_id.clone(),
        );
        let mut gate = state
            .gates
            .get(&gate_key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let authorized = gate
            .authorization_policy
            .allowed_principal_ids
            .iter()
            .any(|id| id == command.principal.principal_id())
            || command
                .principal
                .role_ids()
                .iter()
                .any(|role| gate.authorization_policy.allowed_role_ids.contains(role));
        if !authorized {
            return Err(StoreError::ApprovalUnauthorized);
        }
        if let Some(payload) = &command.decision_payload {
            verified(&mut state, scope, payload, now)?;
            if payload.media_type() != "application/json" {
                return Err(StoreError::ObjectNotVerified);
            }
        }
        if let Some(output) = &command.approval_output {
            verified(&mut state, scope, output, now)?;
        }
        if command.decision == ApprovalDecision::Reject && command.approval_output.is_some() {
            return Err(StoreError::InvalidField);
        }
        let decision_fingerprint = approval_decision_fingerprint(
            scope,
            &command.run_id,
            &command.gate_id,
            command.decision,
            command.decision_payload.as_ref(),
            command.approval_output.as_ref(),
            &command.principal,
        );
        if gate.status != crate::run::GateState::Pending {
            return if gate.decision_fingerprint.as_ref() == Some(&decision_fingerprint) {
                Ok(gate)
            } else {
                Err(StoreError::ApprovalAlreadyResolved)
            };
        }
        running_fence(&state, scope, &command.run_id, None, now)?;
        if gate.version != command.expected_gate_version
            || state
                .runs
                .get(&(scope.clone(), command.run_id.clone()))
                .expect("run")
                .version
                != command.expected_run_version
        {
            return Err(StoreError::ApprovalRaceLost);
        }
        let node_key = (
            scope.clone(),
            command.run_id.clone(),
            gate.node_instance_id.clone(),
        );
        let mut node = state.nodes.get(&node_key).cloned().expect("gate node");
        let mut specs = Vec::new();
        let payload_reference = command
            .decision_payload
            .as_ref()
            .map(|payload| {
                artifact(
                    &mut state,
                    scope,
                    payload,
                    ArtifactKind::ApprovalDecisionPayload,
                    Some(&command.run_id),
                    Some(&node.node_instance_id),
                    None,
                    0,
                    now,
                )
            })
            .transpose()?;
        let payload_value = payload_reference
            .as_ref()
            .map(|reference| crate::artifact::ArtifactRefValue {
                artifact_ref_id: reference.artifact_ref_id.clone(),
                digest: reference.digest.clone(),
                size_bytes: reference.size_bytes.to_string(),
                media_type: reference.media_type.clone(),
            });
        match command.decision {
            ApprovalDecision::Approve => {
                let output = command
                    .approval_output
                    .as_ref()
                    .ok_or(StoreError::ObjectNotVerified)?;
                let expected = crate::approval::canonical_human_approval_result(
                    payload_value,
                    &command.principal,
                );
                if output.media_type() != "application/json"
                    || output.digest() != &digest(&expected)
                    || output.size_bytes() != expected.len() as u64
                {
                    return Err(StoreError::ContractValidationApplied {
                        code: "ApprovalPayloadInvalid".to_owned(),
                    });
                }
                gate.status = crate::run::GateState::Approved;
                node.status = NodeState::Succeeded;
                node.result_ref = Some(json_ref(
                &mut state,
                    scope,
                    output,
                    ArtifactKind::NodeOutput,
                    Some(&command.run_id),
                    Some(&node.node_instance_id),
                    None,
                    0,
                    now,
                )?);
                specs.push(event_spec(EventType::ApprovalGateApproved, "G02", Some(&node.node_instance_id), None, Some(&gate.gate_id), json!({"principal": command.principal.principal_id(), "decision_payload_digest": command.decision_payload.as_ref().map(VerifiedObjectRef::digest), "approval_output_digest": output.digest(), "decision_fingerprint": decision_fingerprint})));
                specs.push(event_spec(EventType::ApprovalApproved, "N12", Some(&node.node_instance_id), None, Some(&gate.gate_id), json!({"gate_id": gate.gate_id, "decision_payload_digest": command.decision_payload.as_ref().map(VerifiedObjectRef::digest), "approval_output_digest": output.digest(), "resolution_source": "human"})));
                let node_id = node.node_instance_id.clone();
                set_node_mutated(&mut node, now)?;
                state.nodes.insert(node_key, node);
                frontier_reduce(
                    &mut state,
                    scope,
                    &command.run_id,
                    &node_id,
                    None,
                    now,
                    &mut specs,
                )?;
            }
            ApprovalDecision::Reject => {
                gate.status = crate::run::GateState::Rejected;
                node.status = NodeState::Failed;
                node.failure_kind = Some(NodeFailureKind::ApprovalRejected);
                set_node_mutated(&mut node, now)?;
                state.nodes.insert(node_key, node.clone());
                specs.push(event_spec(EventType::ApprovalGateRejected, "G03", Some(&node.node_instance_id), None, Some(&gate.gate_id), json!({"principal": command.principal.principal_id(), "decision_payload_digest": command.decision_payload.as_ref().map(VerifiedObjectRef::digest), "decision_fingerprint": decision_fingerprint})));
                specs.push(event_spec(
                    EventType::ApprovalRejected,
                    "N13",
                    Some(&node.node_instance_id),
                    None,
                    Some(&gate.gate_id),
                    json!({"gate_id": gate.gate_id, "decision_payload_digest": command.decision_payload.as_ref().map(VerifiedObjectRef::digest), "resolution_source": "human"}),
                ));
                terminalize_run(
                    &mut state,
                    scope,
                    &command.run_id,
                    RunState::Failed,
                    Some(RunFailureKind::ApprovalRejected),
                    "ApprovalRejected",
                    now,
                    &mut specs,
                )?;
                specs.push(event_spec(
                    EventType::RunFailed,
                    "R07",
                    None,
                    None,
                    None,
                    json!({"failure_kind": "ApprovalRejected"}),
                ));
            }
        }
        gate.deciding_principal = Some(command.principal.principal_id().to_owned());
        gate.decision_payload_ref = payload_reference.map(JsonRef);
        gate.resolution_source = Some(ApprovalResolutionSource::Human);
        gate.decided_at = Some(now);
        gate.decision_fingerprint = Some(decision_fingerprint);
        gate.version.0 = gate
            .version
            .0
            .checked_add(1)
            .ok_or(StoreError::ArithmeticOverflow)?;
        state.gates.insert(gate_key, gate.clone());
        let run = state
            .runs
            .get_mut(&(scope.clone(), command.run_id.clone()))
            .expect("run");
        set_run_mutated(run, now)?;
        append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Host,
            command.principal.principal_id().to_owned(),
            specs,
        )?;
        Ok(gate)
        })
    }
    async fn expire_approval(
        &self,
        scope: &ExecutionScope,
        command: ExpireApproval,
    ) -> Result<ApprovalGate, StoreError> {
        self.transaction(|mut state, now| {
        running_fence(&state, scope, &command.run_id, Some(&command.permit), now)?;
        let gate_key = (
            scope.clone(),
            command.run_id.clone(),
            command.gate_id.clone(),
        );
        let mut gate = state
            .gates
            .get(&gate_key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if now < gate.expires_at {
            return Err(StoreError::ExpiryNotDue {
                database_now: now,
                expires_at: gate.expires_at,
            });
        }
        if gate.status != crate::run::GateState::Pending {
            return Err(StoreError::ApprovalRaceLost);
        }
        let node_key = (
            scope.clone(),
            command.run_id.clone(),
            gate.node_instance_id.clone(),
        );
        let mut node = state
            .nodes
            .get(&node_key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let mut specs = Vec::new();
        match gate.on_expiry {
            crate::approval::ApprovalExpiryPolicy::Approve => {
                let output = command
                    .approval_output
                    .as_ref()
                    .ok_or(StoreError::ObjectNotVerified)?;
                verified(&mut state, scope, output, now)?;
                let expected = crate::approval::canonical_expiry_approval_result();
                if output.media_type() != "application/json"
                    || output.digest() != &digest(&expected)
                    || output.size_bytes() != expected.len() as u64
                {
                    return Err(StoreError::ContractValidationApplied {
                        code: "ApprovalPayloadInvalid".to_owned(),
                    });
                }
                gate.status = crate::run::GateState::ExpiredApproved;
                gate.resolution_source = Some(ApprovalResolutionSource::Expiry);
                node.status = NodeState::Succeeded;
                node.result_ref = Some(json_ref(
                &mut state,
                    scope,
                    output,
                    ArtifactKind::NodeOutput,
                    Some(&command.run_id),
                    Some(&node.node_instance_id),
                    None,
                    0,
                    now,
                )?);
                set_node_mutated(&mut node, now)?;
                let node_id = node.node_instance_id.clone();
                state.nodes.insert(node_key, node);
                specs.push(event_spec(
                    EventType::ApprovalGateExpiredApproved,
                    "G04",
                    Some(&node_id),
                    None,
                    Some(&gate.gate_id),
                    json!({"expires_at": gate.expires_at, "database_now": now, "approval_output_digest": output.digest()}),
                ));
                specs.push(event_spec(
                    EventType::ApprovalExpiredApproved,
                    "N14",
                    Some(&node_id),
                    None,
                    Some(&gate.gate_id),
                    json!({"gate_id": gate.gate_id, "expires_at": gate.expires_at, "approval_output_digest": output.digest()}),
                ));
                frontier_reduce(
                    &mut state,
                    scope,
                    &command.run_id,
                    &node_id,
                    None,
                    now,
                    &mut specs,
                )?;
            }
            crate::approval::ApprovalExpiryPolicy::Reject => {
                gate.status = crate::run::GateState::ExpiredRejected;
                gate.resolution_source = Some(ApprovalResolutionSource::Expiry);
                node.status = NodeState::Failed;
                node.failure_kind = Some(NodeFailureKind::ApprovalExpiredRejected);
                set_node_mutated(&mut node, now)?;
                let node_id = node.node_instance_id.clone();
                state.nodes.insert(node_key, node);
                specs.push(event_spec(
                    EventType::ApprovalGateExpiredRejected,
                    "G05",
                    Some(&node_id),
                    None,
                    Some(&gate.gate_id),
                    json!({"expires_at": gate.expires_at, "database_now": now}),
                ));
                specs.push(event_spec(
                    EventType::ApprovalExpiredRejected,
                    "N15",
                    Some(&node_id),
                    None,
                    Some(&gate.gate_id),
                    json!({"gate_id": gate.gate_id, "expires_at": gate.expires_at}),
                ));
                terminalize_run(
                    &mut state,
                    scope,
                    &command.run_id,
                    RunState::Failed,
                    Some(RunFailureKind::ApprovalExpiredRejected),
                    "ApprovalExpiredRejected",
                    now,
                    &mut specs,
                )?;
                specs.push(event_spec(
                    EventType::RunFailed,
                    "R07",
                    None,
                    None,
                    None,
                    json!({"failure_kind": "ApprovalExpiredRejected"}),
                ));
            }
        }
        gate.decided_at = Some(now);
        gate.version.0 = gate
            .version
            .0
            .checked_add(1)
            .ok_or(StoreError::ArithmeticOverflow)?;
        state.gates.insert(gate_key, gate.clone());
        let run = state
            .runs
            .get_mut(&(scope.clone(), command.run_id.clone()))
            .expect("run");
        set_run_mutated(run, now)?;
        append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Clock,
            command.permit.instance_id().as_str().to_owned(),
            specs,
        )?;
        Ok(gate)
        })
    }
    async fn resolve_terminal_node(
        &self,
        scope: &ExecutionScope,
        command: ResolveTerminalNode,
    ) -> Result<WorkflowRun, StoreError> {
        self.transaction(|mut state, now| {
        running_fence(&state, scope, &command.run_id, Some(&command.permit), now)?;
        let node_key = (
            scope.clone(),
            command.run_id.clone(),
            command.node_id.clone(),
        );
        let mut node = state
            .nodes
            .get(&node_key)
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if node.version != command.expected_node_version || node.status != NodeState::Ready {
            return Err(StoreError::CasConflict);
        }
        let mut specs = Vec::new();
        let run = match node.kind {
            NodeKind::Succeed => {
                let output = command
                    .output
                    .as_ref()
                    .ok_or(StoreError::ObjectNotVerified)?;
                node.status = NodeState::Succeeded;
                node.result_ref = Some(json_ref(
                &mut state,
                    scope,
                    output,
                    ArtifactKind::NodeOutput,
                    Some(&command.run_id),
                    Some(&command.node_id),
                    None,
                    0,
                    now,
                )?);
                set_node_mutated(&mut node, now)?;
                state.nodes.insert(node_key, node);
                specs.push(event_spec(
                    EventType::SucceedNodeReached,
                    "N16",
                    Some(&command.node_id),
                    None,
                    None,
                    json!({"output_digest": output.digest()}),
                ));
                let active = state.nodes.values().any(|candidate| {
                    candidate.scope == *scope
                        && candidate.run_id == command.run_id
                        && !candidate.status.is_terminal()
                });
                if active {
                    return Err(StoreError::IllegalTransition);
                }
                let result_ref = state
                    .nodes
                    .get(&(
                        scope.clone(),
                        command.run_id.clone(),
                        command.node_id.clone(),
                    ))
                    .expect("node")
                    .result_ref
                    .clone();
                let run = state
                    .runs
                    .get_mut(&(scope.clone(), command.run_id.clone()))
                    .expect("run");
                run.status = RunState::Succeeded;
                run.output_ref = result_ref;
                run.finished_at = Some(now);
                set_run_mutated(run, now)?;
                specs.push(event_spec(EventType::RunSucceeded, "R06", None, None, None, json!({"output_digest": output.digest(), "consumed_cost_units": run.budget_consumed})));
                run.clone()
            }
            NodeKind::Fail => {
                node.status = NodeState::Failed;
                node.failure_kind = Some(NodeFailureKind::ExplicitFailNode);
                set_node_mutated(&mut node, now)?;
                state.nodes.insert(node_key, node);
                specs.push(event_spec(
                    EventType::FailNodeReached,
                    "N17",
                    Some(&command.node_id),
                    None,
                    None,
                    json!({"code": "explicit_fail", "message_digest": digest(b"explicit fail")}),
                ));
                let run = terminalize_run(
                    &mut state,
                    scope,
                    &command.run_id,
                    RunState::Failed,
                    Some(RunFailureKind::ExplicitFailNode),
                    "ExplicitFailNode",
                    now,
                    &mut specs,
                )?;
                specs.push(event_spec(
                    EventType::RunFailed,
                    "R07",
                    None,
                    None,
                    None,
                    json!({"failure_kind": "ExplicitFailNode"}),
                ));
                run
            }
            _ => return Err(StoreError::IllegalTransition),
        };
        append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Engine,
            command.permit.instance_id().as_str().to_owned(),
            specs,
        )?;
        Ok(run)
        })
    }
    async fn fail_contract(
        &self,
        scope: &ExecutionScope,
        command: FailContract,
    ) -> Result<WorkflowRun, StoreError> {
        self.transaction(|mut state, now| {
            running_fence(&state, scope, &command.run_id, Some(&command.permit), now)?;
            let key = (
                scope.clone(),
                command.run_id.clone(),
                command.node_id.clone(),
            );
            let node = state.nodes.get_mut(&key).ok_or(StoreError::NotFound)?;
            if node.version != command.expected_node_version || node.status != NodeState::Ready {
                return Err(StoreError::CasConflict);
            }
            node.status = NodeState::ContractFailed;
            node.failure_kind = Some(command.closed_failure_kind);
            set_node_mutated(node, now)?;
            let run_kind = run_failure(command.closed_failure_kind);
            let mut specs = vec![event_spec(
                EventType::NodeContractFailed,
                "N46",
                Some(&command.node_id),
                None,
                None,
                json!({"failure_kind": command.closed_failure_kind}),
            )];
            let run = terminalize_run(
                &mut state,
                scope,
                &command.run_id,
                RunState::ContractFailed,
                Some(run_kind),
                "ContractFailed",
                now,
                &mut specs,
            )?;
            specs.push(event_spec(
                EventType::RunContractFailed,
                "R08",
                None,
                None,
                None,
                json!({"failure_kind": run_kind}),
            ));
            append_batch(
                &mut state,
                scope,
                &command.run_id,
                now,
                EventActorKind::Engine,
                command.permit.instance_id().as_str().to_owned(),
                specs,
            )?;
            Ok(run)
        })
    }
    async fn cancel_run(
        &self,
        scope: &ExecutionScope,
        command: CancelRun,
    ) -> Result<CommandReceipt, StoreError> {
        self.transaction(|mut state, now| {
        if command.principal.scope() != scope
            || command.reason_code.is_empty()
            || command.idempotency_token.len() < 16
        {
            return Err(StoreError::InvalidField);
        }
        let expected_gate_versions = command
            .expected_pending_gate_versions
            .iter()
            .map(|gate| (gate.gate_id.as_str(), gate.version.0))
            .collect::<Vec<_>>();
        let request_fingerprint = fingerprint(
            "cancel-v1",
            scope,
            &(
                &command.run_id,
                command.expected_run_version,
                &expected_gate_versions,
                command.principal.principal_id(),
                &command.reason_code,
            ),
        );
        let receipt_key = (
            scope.clone(),
            CommandKindKey::Cancel,
            command.idempotency_token.clone(),
        );
        if let Some(receipt) = state.receipts.get(&receipt_key) {
            return if receipt.request_fingerprint == request_fingerprint {
                Ok(receipt.clone())
            } else {
                Err(StoreError::IdempotencyConflict)
            };
        }
        command_fence(&state, scope, &command.run_id, None, now, true)?;
        let prior = state
            .runs
            .get(&(scope.clone(), command.run_id.clone()))
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if prior.version != command.expected_run_version || prior.status.is_terminal() {
            return Err(StoreError::CancellationRaceLost);
        }
        let mut actual_gate_versions = state
            .gates
            .values()
            .filter(|gate| {
                gate.scope == *scope
                    && gate.run_id == command.run_id
                    && gate.status == crate::run::GateState::Pending
            })
            .map(|gate| (gate.gate_id.as_str(), gate.version.0))
            .collect::<Vec<_>>();
        actual_gate_versions.sort_unstable();
        let mut supplied_gate_versions = expected_gate_versions;
        supplied_gate_versions.sort_unstable();
        if actual_gate_versions != supplied_gate_versions {
            return Err(StoreError::CancellationRaceLost);
        }
        let mut specs = Vec::new();
        let run = terminalize_run(
            &mut state,
            scope,
            &command.run_id,
            RunState::Cancelled,
            None,
            &command.reason_code,
            now,
            &mut specs,
        )?;
        specs.push(event_spec(EventType::RunCancelled, match prior.status { RunState::Pending => "R11", RunState::Running => "R12", _ => "R13" }, None, None, None, json!({"principal": command.principal.principal_id(), "reason_code": command.reason_code, "prior_status": prior.status})));
        let (batch_id, first, last) = append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Host,
            command.principal.principal_id().to_owned(),
            specs,
        )?;
        let receipt = CommandReceipt {
            scope: scope.clone(),
            command_kind: CommandKind::CancelRun,
            idempotency_token: command.idempotency_token,
            request_fingerprint,
            run_id: command.run_id.clone(),
            outcome: CommandReceiptOutcome::CancelRunCommitted {
                run_id: command.run_id.clone(),
                prior_status: prior.status,
                status: RunState::Cancelled,
                run_version: run.version,
                batch_id: batch_id.clone(),
                first_event_seq: first,
                last_event_seq: last,
            },
            batch_id,
            committed_at: now,
        };
        state.receipts.insert(receipt_key, receipt.clone());
        Ok(receipt)
        })
    }
    async fn expire_run_lifetime(
        &self,
        scope: &ExecutionScope,
        command: ExpireRunLifetime,
    ) -> Result<WorkflowRun, StoreError> {
        self.transaction(|mut state, now| {
            command_fence(
                &state,
                scope,
                &command.run_id,
                Some(&command.permit),
                now,
                true,
            )?;
            let prior = state
                .runs
                .get(&(scope.clone(), command.run_id.clone()))
                .cloned()
                .ok_or(StoreError::NotFound)?;
            if prior.status.is_terminal() {
                return Ok(prior);
            }
            if now < prior.lifetime_deadline_at {
                return Err(StoreError::LifetimeNotDue {
                    database_now: now,
                    lifetime_deadline_at: prior.lifetime_deadline_at,
                });
            }
            let mut specs = Vec::new();
            let run = terminalize_run(
                &mut state,
                scope,
                &command.run_id,
                RunState::Cancelled,
                None,
                "RunLifetimeExceeded",
                now,
                &mut specs,
            )?;
            specs.push(event_spec(
                EventType::RunCancelled,
                match prior.status {
                    RunState::Pending => "R11",
                    RunState::Running => "R12",
                    _ => "R13",
                },
                None,
                None,
                None,
                json!({"reason_code": "RunLifetimeExceeded", "prior_status": prior.status}),
            ));
            append_batch(
                &mut state,
                scope,
                &command.run_id,
                now,
                EventActorKind::Clock,
                command.permit.instance_id().as_str().to_owned(),
                specs,
            )?;
            Ok(run)
        })
    }
    async fn mark_corrupt_storage(
        &self,
        scope: &ExecutionScope,
        command: MarkCorruptStorage,
    ) -> Result<WorkflowRun, StoreError> {
        self.transaction(|mut state, now| {
        if command.proof.scope() != scope
            || command.proof.requested_digest() != &command.bad_ref.digest
            || command.bad_ref.scope != *scope
            || state.object_store_nonce.as_deref()
                != Some(command.proof.store_instance_nonce())
            || state
                .artifact_refs
                .get(&(scope.clone(), command.bad_ref.artifact_ref_id.clone()))
                != Some(&command.bad_ref)
        {
            return Err(StoreError::InvalidFailedReadProof);
        }
        let prior = state
            .runs
            .get(&(scope.clone(), command.run_id.clone()))
            .cloned()
            .ok_or(StoreError::NotFound)?;
        if prior.status == RunState::CorruptStorage {
            return Ok(prior);
        }
        let proof_fingerprint = digest(&command.proof.fingerprint_material());
        let mut specs = Vec::new();
        if let Some(owner_node_id) = &command.owner_node_id {
            if command.bad_ref.producer_node_id.as_ref() != Some(owner_node_id) {
                return Err(StoreError::InvalidFailedReadProof);
            }
            let node_key = (
                scope.clone(),
                command.run_id.clone(),
                owner_node_id.clone(),
            );
            let mut node = state
                .nodes
                .get(&node_key)
                .cloned()
                .ok_or(StoreError::NotFound)?;
            let prior_status = node.status;
            let transition = match prior_status {
                NodeState::Ready => "N47",
                NodeState::RetryWaiting => "N48",
                NodeState::WaitingApproval => "N49",
                NodeState::WaitingChildren => "N50",
                NodeState::BlockedIncompatible => "N51",
                NodeState::Succeeded => "N52",
                NodeState::Failed => "N53",
                NodeState::ContractFailed => "N54",
                NodeState::RetriesExhausted => "N55",
                NodeState::BudgetExhausted => "N56",
                NodeState::Cancelled => "N57",
                _ => return Err(StoreError::IllegalTransition),
            };
            node.status = NodeState::CorruptStorage;
            node.active_attempt_id = None;
            node.next_eligible_at = None;
            node.budget_wait_amount = None;
            set_node_mutated(&mut node, now)?;
            state.nodes.insert(node_key, node);
            specs.push(event_spec(
                EventType::NodeCorruptStorage,
                transition,
                Some(owner_node_id),
                None,
                None,
                json!({"bad_artifact_ref_id": command.bad_ref.artifact_ref_id, "bad_digest": command.bad_ref.digest, "error_class": command.proof.error_class(), "corrupt_proof_fingerprint": proof_fingerprint, "prior_status": prior_status}),
            ));
        }
        cancellation_cascade(
            &mut state,
            scope,
            &command.run_id,
            RunState::CorruptStorage,
            "CorruptStorage",
            now,
            &mut specs,
        )?;
        let run = state
            .runs
            .get_mut(&(scope.clone(), command.run_id.clone()))
            .expect("run");
        run.status = RunState::CorruptStorage;
        run.output_ref = None;
        run.corrupt_bad_artifact_ref_id = Some(command.bad_ref.artifact_ref_id.clone());
        run.corrupt_owner_node_id = command.owner_node_id;
        run.corrupt_error_class = Some(command.proof.error_class());
        run.corrupt_proof_fingerprint = Some(proof_fingerprint);
        run.finished_at = Some(now);
        set_run_mutated(run, now)?;
        let returned = run.clone();
        let store_instance_nonce_digest = digest(command.proof.store_instance_nonce());
        specs.push(event_spec(EventType::RunCorruptStorage, "R15", None, None, None, json!({"bad_artifact_ref_id": command.bad_ref.artifact_ref_id, "bad_digest": command.bad_ref.digest, "error_class": command.proof.error_class(), "corrupt_proof_fingerprint": returned.corrupt_proof_fingerprint, "store_instance_nonce_digest": store_instance_nonce_digest, "prior_status": prior.status, "owner_node_id": returned.corrupt_owner_node_id})));
        append_batch(
            &mut state,
            scope,
            &command.run_id,
            now,
            EventActorKind::Recovery,
            "object-read".to_owned(),
            specs,
        )?;
        Ok(returned)
        })
    }
    async fn get_definition(
        &self,
        scope: &ExecutionScope,
        definition_id: &Id,
    ) -> Result<DefinitionRecord, StoreError> {
        self.state
            .lock()
            .expect("store lock poisoned")
            .definitions
            .get(&(scope.clone(), definition_id.clone()))
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_revision(
        &self,
        scope: &ExecutionScope,
        definition_id: &Id,
        revision_hash: &Digest,
    ) -> Result<WorkflowRevision, StoreError> {
        self.state
            .lock()
            .expect("store lock poisoned")
            .revisions
            .get(&(scope.clone(), definition_id.clone(), revision_hash.clone()))
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_run(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
    ) -> Result<WorkflowRunView, StoreError> {
        let state = self.state.lock().expect("store lock poisoned");
        let run = state
            .runs
            .get(&(scope.clone(), run_id.clone()))
            .cloned()
            .ok_or(StoreError::NotFound)?;
        let operational = if run.status == RunState::Running {
            let nodes = state
                .nodes
                .values()
                .filter(|node| node.scope == *scope && node.run_id == *run_id)
                .collect::<Vec<_>>();
            let counts = RunOperationalCounts {
                ready: nodes
                    .iter()
                    .filter(|node| node.status == NodeState::Ready)
                    .count() as u64,
                running_attempts: nodes
                    .iter()
                    .filter(|node| node.status == NodeState::Running)
                    .count() as u64,
                budget_waiting: nodes
                    .iter()
                    .filter(|node| node.status == NodeState::BudgetWaiting)
                    .count() as u64,
                pending_approvals: nodes
                    .iter()
                    .filter(|node| node.status == NodeState::WaitingApproval)
                    .count() as u64,
                retry_waiting: nodes
                    .iter()
                    .filter(|node| node.status == NodeState::RetryWaiting)
                    .count() as u64,
                maps_waiting_children: nodes
                    .iter()
                    .filter(|node| node.status == NodeState::WaitingChildren)
                    .count() as u64,
            };
            let phase = counts.phase().ok_or(StoreError::CorruptControlPlane)?;
            let mut due = vec![run.lifetime_deadline_at];
            due.extend(nodes.iter().filter_map(|node| node.next_eligible_at));
            due.extend(
                state
                    .attempts
                    .values()
                    .filter(|attempt| {
                        attempt.scope == *scope
                            && attempt.run_id == *run_id
                            && attempt.status == AttemptState::Started
                    })
                    .map(|attempt| attempt.deadline_at),
            );
            due.extend(
                state
                    .gates
                    .values()
                    .filter(|gate| {
                        gate.scope == *scope
                            && gate.run_id == *run_id
                            && gate.status == crate::run::GateState::Pending
                    })
                    .map(|gate| gate.expires_at),
            );
            Some(RunOperationalView {
                phase,
                counts,
                next_due_at: due.into_iter().min(),
            })
        } else {
            None
        };
        Ok(WorkflowRunView { run, operational })
    }

    async fn get_node(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        node_id: &NodeInstanceId,
    ) -> Result<NodeRun, StoreError> {
        self.state
            .lock()
            .expect("store lock poisoned")
            .nodes
            .get(&(scope.clone(), run_id.clone(), node_id.clone()))
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_attempt(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        attempt_id: &Id,
    ) -> Result<NodeAttempt, StoreError> {
        self.state
            .lock()
            .expect("store lock poisoned")
            .attempts
            .get(&(scope.clone(), run_id.clone(), attempt_id.clone()))
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn get_gate(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        gate_id: &Id,
    ) -> Result<ApprovalGate, StoreError> {
        self.state
            .lock()
            .expect("store lock poisoned")
            .gates
            .get(&(scope.clone(), run_id.clone(), gate_id.clone()))
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    async fn list_runs(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError> {
        let state = self.state.lock().expect("store lock poisoned");
        let query = "list_runs";
        let (limit, cutoff, last) = scan_page_context(&page, scope, query, self.now())?;
        let mut items = state
            .runs
            .values()
            .filter(|run| {
                run.scope == *scope
                    && run.created_at <= cutoff
                    && (last.is_empty() || vec![run.run_id.as_str().to_owned()] > last)
            })
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by_key(|run| run.run_id.clone());
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = if has_more {
            items
                .last()
                .map(|run| {
                    next_scan_cursor(scope, query, cutoff, vec![run.run_id.as_str().to_owned()])
                })
                .transpose()?
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    async fn list_nodes(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError> {
        let state = self.state.lock().expect("store lock poisoned");
        let query = format!("list_nodes:{}", run_id.as_str());
        let (limit, cutoff, last) = scan_page_context(&page, scope, &query, self.now())?;
        if !state.runs.contains_key(&(scope.clone(), run_id.clone())) {
            return Err(StoreError::NotFound);
        }
        let mut items = state
            .nodes
            .values()
            .filter(|node| {
                node.scope == *scope
                    && node.run_id == *run_id
                    && node.created_at <= cutoff
                    && (last.is_empty() || vec![node.node_instance_id.as_str().to_owned()] > last)
            })
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by_key(|node| node.node_instance_id.clone());
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = if has_more {
            items
                .last()
                .map(|node| {
                    next_scan_cursor(
                        scope,
                        &query,
                        cutoff,
                        vec![node.node_instance_id.as_str().to_owned()],
                    )
                })
                .transpose()?
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }

    async fn list_events_after(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        page: EventPageRequest,
    ) -> Result<Vec<WorkflowEvent>, StoreError> {
        if page.page_size == 0 || page.page_size > 1000 {
            return Err(StoreError::InvalidField);
        }
        let state = self.state.lock().expect("store lock poisoned");
        let events = state
            .events
            .get(&(scope.clone(), run_id.clone()))
            .ok_or(StoreError::NotFound)?;
        let mut result = events
            .iter()
            .filter(|event| event.event_seq > page.after_event_seq)
            .take(page.page_size as usize)
            .cloned()
            .collect::<Vec<_>>();
        if let Some((last_seq, batch)) = result
            .last()
            .map(|last| (last.event_seq, last.batch_id.clone()))
        {
            result.extend(
                events
                    .iter()
                    .filter(|event| event.event_seq > last_seq && event.batch_id == batch)
                    .cloned(),
            );
        }
        let bytes = serde_json::to_vec(&result)
            .map_err(|_| StoreError::TransactionFailed)?
            .len() as u64;
        if bytes > page.hard_response_byte_limit {
            return Err(StoreError::BatchTooLarge);
        }
        Ok(result)
    }

    async fn scan_ready_nodes(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError> {
        scan_nodes(&self.state, &self.clock, scope, page, "ready", |node| {
            node.status == NodeState::Ready
        })
    }

    async fn scan_budget_waiters(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError> {
        scan_nodes(
            &self.state,
            &self.clock,
            scope,
            page,
            "budget_waiters",
            |node| node.status == NodeState::BudgetWaiting,
        )
    }

    async fn scan_due_deadlines(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeAttempt>, StoreError> {
        let state = self.state.lock().expect("store lock poisoned");
        let query = "due_deadlines";
        let (limit, cutoff, last) = scan_page_context(&page, scope, query, self.now())?;
        let mut items = state
            .attempts
            .values()
            .filter(|attempt| {
                attempt.scope == *scope
                    && attempt.status == AttemptState::Started
                    && attempt.started_at <= cutoff
                    && attempt.deadline_at <= cutoff
            })
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by_key(|attempt| {
            (
                attempt.deadline_at,
                attempt.run_id.clone(),
                attempt.node_instance_id.clone(),
                attempt.attempt_id.clone(),
            )
        });
        let key = |attempt: &NodeAttempt| {
            vec![
                timestamp_cursor_key(attempt.deadline_at),
                attempt.run_id.as_str().to_owned(),
                attempt.node_instance_id.as_str().to_owned(),
                attempt.attempt_id.as_str().to_owned(),
            ]
        };
        if !last.is_empty() {
            items.retain(|attempt| key(attempt) > last);
        }
        finish_scan_page(items, limit, scope, query, cutoff, key)
    }

    async fn scan_due_retries(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError> {
        scan_nodes(
            &self.state,
            &self.clock,
            scope,
            page,
            "due_retries",
            |node| node.status == NodeState::RetryWaiting && node.next_eligible_at.is_some(),
        )
    }

    async fn scan_recovery_runs(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError> {
        let state = self.state.lock().expect("store lock poisoned");
        let query = "recovery_runs";
        let (limit, cutoff, last) = scan_page_context(&page, scope, query, self.now())?;
        let generation = state
            .claims
            .get(scope)
            .map_or(0, |claim| claim.claim.generation);
        let ids = state
            .attempts
            .values()
            .filter(|attempt| {
                attempt.scope == *scope
                    && attempt.status == AttemptState::Started
                    && attempt.engine_generation < generation
                    && attempt.started_at <= cutoff
            })
            .map(|attempt| attempt.run_id.clone())
            .collect::<BTreeSet<_>>();
        let mut items = ids
            .into_iter()
            .filter_map(|run_id| state.runs.get(&(scope.clone(), run_id)).cloned())
            .filter(|run| last.is_empty() || vec![run.run_id.as_str().to_owned()] > last)
            .collect::<Vec<_>>();
        items.sort_by_key(|run| run.run_id.clone());
        finish_scan_page(items, limit, scope, query, cutoff, |run| {
            vec![run.run_id.as_str().to_owned()]
        })
    }

    async fn scan_compatibility_rechecks(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError> {
        let state = self.state.lock().expect("store lock poisoned");
        let query = "compatibility_rechecks";
        let (limit, cutoff, last) = scan_page_context(&page, scope, query, self.now())?;
        let mut items = state
            .runs
            .values()
            .filter(|run| {
                run.scope == *scope
                    && run.updated_at <= cutoff
                    && matches!(
                        run.status,
                        RunState::Pending | RunState::Running | RunState::BlockedIncompatible
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by_key(|run| (run.updated_at, run.run_id.clone()));
        let key = |run: &WorkflowRun| {
            vec![
                timestamp_cursor_key(run.updated_at),
                run.run_id.as_str().to_owned(),
            ]
        };
        if !last.is_empty() {
            items.retain(|run| key(run) > last);
        }
        finish_scan_page(items, limit, scope, query, cutoff, key)
    }

    async fn scan_due_gates(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<ApprovalGate>, StoreError> {
        let state = self.state.lock().expect("store lock poisoned");
        let query = "due_gates";
        let (limit, cutoff, last) = scan_page_context(&page, scope, query, self.now())?;
        let mut items = state
            .gates
            .values()
            .filter(|gate| {
                gate.scope == *scope
                    && gate.status == crate::run::GateState::Pending
                    && gate.expires_at <= cutoff
            })
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by_key(|gate| (gate.expires_at, gate.run_id.clone(), gate.gate_id.clone()));
        let key = |gate: &ApprovalGate| {
            vec![
                timestamp_cursor_key(gate.expires_at),
                gate.run_id.as_str().to_owned(),
                gate.gate_id.as_str().to_owned(),
            ]
        };
        if !last.is_empty() {
            items.retain(|gate| key(gate) > last);
        }
        finish_scan_page(items, limit, scope, query, cutoff, key)
    }

    async fn scan_due_run_lifetimes(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError> {
        let state = self.state.lock().expect("store lock poisoned");
        let query = "due_run_lifetimes";
        let (limit, cutoff, last) = scan_page_context(&page, scope, query, self.now())?;
        let mut items = state
            .runs
            .values()
            .filter(|run| {
                run.scope == *scope
                    && !run.status.is_terminal()
                    && run.lifetime_deadline_at <= cutoff
            })
            .cloned()
            .collect::<Vec<_>>();
        items.sort_by_key(|run| (run.lifetime_deadline_at, run.run_id.clone()));
        let key = |run: &WorkflowRun| {
            vec![
                timestamp_cursor_key(run.lifetime_deadline_at),
                run.run_id.as_str().to_owned(),
            ]
        };
        if !last.is_empty() {
            items.retain(|run| key(run) > last);
        }
        finish_scan_page(items, limit, scope, query, cutoff, key)
    }
}

fn scan_nodes<C: Clock>(
    mutex: &Mutex<MemoryState>,
    clock: &Arc<C>,
    scope: &ExecutionScope,
    page: PageRequest,
    query: &str,
    predicate: impl Fn(&NodeRun) -> bool,
) -> Result<Page<NodeRun>, StoreError> {
    let state = mutex.lock().expect("store lock poisoned");
    let (limit, cutoff, last) = scan_page_context(&page, scope, query, clock.now())?;
    let mut items = state
        .nodes
        .values()
        .filter(|node| {
            node.scope == *scope
                && predicate(node)
                && node.updated_at <= cutoff
                && (query != "due_retries"
                    || node.next_eligible_at.is_some_and(|due| due <= cutoff))
                && state
                    .runs
                    .get(&(scope.clone(), node.run_id.clone()))
                    .is_some_and(|run| run.status == RunState::Running)
        })
        .cloned()
        .collect::<Vec<_>>();
    items.sort_by_key(|node| (node.run_id.clone(), node.node_instance_id.clone()));
    if !last.is_empty() {
        items.retain(|node| {
            vec![
                node.run_id.as_str().to_owned(),
                node.node_instance_id.as_str().to_owned(),
            ] > last
        });
    }
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if has_more {
        items
            .last()
            .map(|node| {
                next_scan_cursor(
                    scope,
                    query,
                    cutoff,
                    vec![
                        node.run_id.as_str().to_owned(),
                        node.node_instance_id.as_str().to_owned(),
                    ],
                )
            })
            .transpose()?
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}

fn outcome_category(outcome: &ActionOutcome) -> &'static str {
    match outcome {
        ActionOutcome::Success { .. } => "success",
        ActionOutcome::Retryable { .. } => "retryable",
        ActionOutcome::Permanent { .. } => "permanent",
    }
}

fn outcome_digest(outcome: &ActionOutcome) -> Digest {
    let value = match outcome {
        ActionOutcome::Success {
            output,
            actual_cost_units,
            ..
        } => json!({"kind": "success", "output": output, "cost": actual_cost_units}),
        ActionOutcome::Retryable {
            code,
            message,
            actual_cost_units,
            ..
        } => {
            json!({"kind": "retryable", "code": code, "message": message, "cost": actual_cost_units})
        }
        ActionOutcome::Permanent {
            code,
            message,
            actual_cost_units,
            ..
        } => {
            json!({"kind": "permanent", "code": code, "message": message, "cost": actual_cost_units})
        }
    };
    digest(&serde_jcs::to_vec(&value).expect("outcome projection serializes"))
}

fn outcome_cost_reason(outcome: &ActionOutcome) -> (CostUnits, BudgetLedgerReason) {
    match outcome {
        ActionOutcome::Success {
            actual_cost_units, ..
        } => (*actual_cost_units, BudgetLedgerReason::Succeeded),
        ActionOutcome::Retryable {
            actual_cost_units, ..
        } => (*actual_cost_units, BudgetLedgerReason::Retryable),
        ActionOutcome::Permanent {
            actual_cost_units, ..
        } => (*actual_cost_units, BudgetLedgerReason::Permanent),
    }
}

fn budget_settled_spec(
    state: &MemoryState,
    scope: &ExecutionScope,
    run_id: &Id,
    attempt: &NodeAttempt,
) -> Result<EventSpec, StoreError> {
    let ledger = state
        .ledger
        .get(&(scope.clone(), run_id.clone()))
        .and_then(|entries| entries.last())
        .filter(|entry| entry.attempt_id == attempt.attempt_id)
        .ok_or(StoreError::CorruptControlPlane)?;
    let run = state
        .runs
        .get(&(scope.clone(), run_id.clone()))
        .ok_or(StoreError::NotFound)?;
    let consumed = attempt
        .settled_cost
        .ok_or(StoreError::CorruptControlPlane)?;
    let released = attempt
        .reserved_cost
        .0
        .checked_sub(consumed.0)
        .ok_or(StoreError::ArithmeticOverflow)?;
    let available = run
        .budget_limit
        .0
        .checked_sub(run.budget_consumed.0)
        .and_then(|value| value.checked_sub(run.budget_reserved.0))
        .ok_or(StoreError::ArithmeticOverflow)?;
    Ok(event_spec(
        EventType::BudgetSettled,
        match attempt.status {
            AttemptState::Succeeded => "A02",
            AttemptState::RetryableFailed => "A03",
            AttemptState::PermanentFailed => "A04",
            AttemptState::ContractFailed => "A05",
            AttemptState::TimedOut => "A06",
            AttemptState::UnknownOutcome => "A07",
            AttemptState::Cancelled => "A08",
            AttemptState::Stale => "A09",
            AttemptState::Started => return Err(StoreError::IllegalTransition),
        },
        Some(&attempt.node_instance_id),
        Some(&attempt.attempt_id),
        None,
        json!({"ledger_seq": ledger.ledger_seq, "reservation_amount": attempt.reserved_cost, "consumed_amount": consumed, "released_amount": released.to_string(), "reason": ledger.reason, "available_after": available.to_string()}),
    ))
}

fn timeout_active(
    state: &mut MemoryState,
    scope: &ExecutionScope,
    run_id: &Id,
    node: &mut NodeRun,
    mut attempt: NodeAttempt,
    now: Timestamp,
) -> Result<(NodeAttempt, bool), StoreError> {
    let reservation = attempt.reserved_cost;
    settle(
        state,
        scope,
        run_id,
        &mut attempt,
        reservation,
        BudgetLedgerReason::TimedOut,
        now,
    )?;
    attempt.status = AttemptState::TimedOut;
    attempt.finished_at = Some(now);
    let definition = state
        .run_definitions
        .get(&(scope.clone(), run_id.clone()))
        .expect("definition");
    let (_, retry, _, _) =
        action_config(definition, node).ok_or(StoreError::CorruptControlPlane)?;
    let exhausted = attempt.attempt_number >= retry.max_attempts;
    node.status = if exhausted {
        NodeState::RetriesExhausted
    } else {
        NodeState::RetryWaiting
    };
    node.active_attempt_id = None;
    node.next_eligible_at = if exhausted {
        None
    } else {
        Some(retry_at(now, retry, attempt.attempt_number)?)
    };
    set_node_mutated(node, now)?;
    Ok((attempt, exhausted))
}

fn timeout_specs(
    state: &MemoryState,
    scope: &ExecutionScope,
    run_id: &Id,
    attempt: &NodeAttempt,
    reservation: CostUnits,
    now: Timestamp,
    exhausted: bool,
    next_eligible_at: Option<Timestamp>,
) -> Result<Vec<EventSpec>, StoreError> {
    Ok(vec![
        event_spec(
            EventType::AttemptTimedOut,
            "A06",
            Some(&attempt.node_instance_id),
            Some(&attempt.attempt_id),
            None,
            json!({"deadline_at": attempt.deadline_at, "database_now": now, "charged_cost_units": reservation}),
        ),
        event_spec(
            if exhausted {
                EventType::NodeRetriesExhausted
            } else {
                EventType::NodeRetryScheduled
            },
            if exhausted { "N25" } else { "N22" },
            Some(&attempt.node_instance_id),
            Some(&attempt.attempt_id),
            None,
            if exhausted {
                json!({"attempt_id": attempt.attempt_id, "attempt_number": attempt.attempt_number, "max_attempts": attempt.attempt_number, "cause": "timeout"})
            } else {
                json!({"attempt_id": attempt.attempt_id, "attempt_number": attempt.attempt_number, "next_eligible_at": next_eligible_at.ok_or(StoreError::CorruptControlPlane)?, "cause": "timeout"})
            },
        ),
        budget_settled_spec(state, scope, run_id, attempt)?,
    ])
}

fn run_failure(kind: NodeFailureKind) -> RunFailureKind {
    match kind {
        NodeFailureKind::ActionPermanent => RunFailureKind::ActionPermanent,
        NodeFailureKind::ExplicitFailNode => RunFailureKind::ExplicitFailNode,
        NodeFailureKind::MapChildFailed => RunFailureKind::MapChildFailed,
        NodeFailureKind::ApprovalRejected => RunFailureKind::ApprovalRejected,
        NodeFailureKind::ApprovalExpiredRejected => RunFailureKind::ApprovalExpiredRejected,
        NodeFailureKind::RunDynamicNodeLimitExceeded => RunFailureKind::RunDynamicNodeLimitExceeded,
        NodeFailureKind::RunAttemptLimitExceeded => RunFailureKind::RunAttemptLimitExceeded,
        NodeFailureKind::InlineJsonLimitExceeded => RunFailureKind::InlineJsonLimitExceeded,
        NodeFailureKind::ArtifactsPerAttemptLimitExceeded => {
            RunFailureKind::ArtifactsPerAttemptLimitExceeded
        }
        NodeFailureKind::AggregateObjectLimitExceeded => {
            RunFailureKind::AggregateObjectLimitExceeded
        }
        NodeFailureKind::RunOutputSchemaMismatch => RunFailureKind::RunOutputSchemaMismatch,
        NodeFailureKind::BindingSourceUnavailable => RunFailureKind::BindingSourceUnavailable,
        NodeFailureKind::BindingPointerMissing => RunFailureKind::BindingPointerMissing,
        NodeFailureKind::BindingTypeMismatch => RunFailureKind::BindingTypeMismatch,
        NodeFailureKind::ActionOutputSchemaMismatch => RunFailureKind::ActionOutputSchemaMismatch,
        NodeFailureKind::ChoiceInputInvalid => RunFailureKind::ChoiceInputInvalid,
        NodeFailureKind::MapInputInvalid => RunFailureKind::MapInputInvalid,
        NodeFailureKind::MapBoundExceeded => RunFailureKind::MapBoundExceeded,
        NodeFailureKind::ApprovalPayloadInvalid => RunFailureKind::ApprovalPayloadInvalid,
        NodeFailureKind::ActionCostProtocolViolation => RunFailureKind::ActionCostProtocolViolation,
    }
}
