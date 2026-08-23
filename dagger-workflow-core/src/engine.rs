//! Minimal engine construction surface reserved for W3.

use crate::action::{
    ActionContext, ActionOutcome, ActionRegistry, BudgetHandle, CancellationSource,
};
use crate::artifact::{
    ArtifactRef, ArtifactRefValue, FailedReadProof, ObjectReadError, ObjectStore, VerifiedObject,
};
use crate::committed_read::{CommittedObjectReader, CommittedReadOutcome};
use crate::definition::{
    ArtifactLocator, Binding, BindingSource, ChoiceCase, MapBinding, MapBindingSource,
    NodeDefinition, WorkflowDefinition,
};
use crate::ids::{edge_id, map_child_id, map_expansion_digest, Digest, Id, MapChildIdentity};
use crate::run::{NodeKind, NodeRun};
use crate::scope::ExecutionScope;
use crate::store::{
    CancelRun, ClaimNodeAttempt, ClaimNodeAttemptResult, CommandReceipt, CompleteAttempt,
    CompleteMap, CompletionObjects, ExpandMap, ExpireApproval, ExpireRunLifetime,
    MarkCorruptStorage, OrderedMapItem, PageRequest, RecordChoice, RecoverAbandonedAttemptsForRun,
    ReleaseRetry, RequestApproval, ResolveTerminalNode, ResumeCompatible, StartRun, StoreError,
    SuspendIncompatible, TimeoutAttempt, WorkflowStore,
};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::sync::{atomic::AtomicU64, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;

use crate::ids::Timestamp;

#[derive(serde::Serialize)]
struct IncompatibilityEvidenceDocument<'a> {
    evidence_digest: &'a Digest,
    incompatible_reference_locations: &'a [String],
}

/// Store-observed clock used for every durable time comparison.
pub trait Clock: Send + Sync {
    /// Returns the current database-equivalent timestamp.
    fn now(&self) -> Timestamp;
}

/// Manually advanced deterministic clock for stores and engine tests.
#[derive(Debug)]
pub struct TestClock {
    now_ms: AtomicI64,
}

impl TestClock {
    /// Creates a clock at an explicit Unix epoch millisecond.
    pub fn new(now: Timestamp) -> Self {
        Self {
            now_ms: AtomicI64::new(now.0),
        }
    }

    /// Advances the clock using checked arithmetic.
    pub fn advance_ms(&self, milliseconds: i64) -> Result<Timestamp, ClockError> {
        let result = self
            .now_ms
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(milliseconds)
            })
            .map_err(|_| ClockError::Overflow)?
            .checked_add(milliseconds)
            .ok_or(ClockError::Overflow)?;
        Ok(Timestamp(result))
    }

    /// Sets an explicit time, including a regressed time for fail-closed tests.
    pub fn set(&self, now: Timestamp) {
        self.now_ms.store(now.0, Ordering::Release);
    }
}

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.now_ms.load(Ordering::Acquire))
    }
}

/// A virtual-clock arithmetic failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ClockError {
    /// Timestamp arithmetic overflowed.
    #[error("clock arithmetic overflow")]
    Overflow,
}

/// Scheduler-wide non-durable execution configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineConfig {
    /// Engine instance label used by the singleton claim.
    pub instance_id: Id,
    /// Maximum concurrently invoked actions in this process.
    pub max_concurrency: usize,
}

/// Engine construction failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EngineBuildError {
    /// The configured concurrency is zero.
    #[error("engine concurrency must be positive")]
    InvalidConcurrency,
}

/// Durable workflow scheduler shell implemented by W3.
#[allow(dead_code)]
pub struct WorkflowEngine<S, O, R>
where
    S: WorkflowStore,
    O: ObjectStore,
{
    store: Arc<S>,
    object_store: Arc<O>,
    reader: CommittedObjectReader<S, O>,
    registry: Arc<R>,
    config: EngineConfig,
    permits: Mutex<BTreeMap<ExecutionScope, crate::store::EnginePermit>>,
    cancellation: Mutex<BTreeMap<(ExecutionScope, Id, Id), CancellationSource>>,
    id_counter: AtomicU64,
}

impl<S, O, R> WorkflowEngine<S, O, R>
where
    S: WorkflowStore,
    O: ObjectStore,
    R: ActionRegistry,
{
    /// Constructs an engine without acquiring its scoped claim.
    pub fn new(
        store: Arc<S>,
        object_store: Arc<O>,
        registry: Arc<R>,
        config: EngineConfig,
    ) -> Result<Self, EngineBuildError> {
        if config.max_concurrency == 0 {
            return Err(EngineBuildError::InvalidConcurrency);
        }
        Ok(Self {
            reader: CommittedObjectReader::new(store.clone(), object_store.clone()),
            store,
            object_store,
            registry,
            config,
            permits: Mutex::new(BTreeMap::new()),
            cancellation: Mutex::new(BTreeMap::new()),
            id_counter: AtomicU64::new(0),
        })
    }

    /// Acquires the singleton scheduler generation for one execution scope.
    pub async fn acquire_scope(
        &self,
        scope: &ExecutionScope,
    ) -> Result<crate::store::EngineClaim, EngineError> {
        let acquired = self
            .store
            .acquire_engine_claim(scope, self.config.instance_id.clone())
            .await?;
        self.permits
            .lock()
            .expect("permit lock poisoned")
            .insert(scope.clone(), acquired.permit);
        self.run_maintenance_scans(scope).await?;
        Ok(acquired.claim)
    }

    /// Heartbeats the live generation for one scope.
    pub async fn heartbeat_scope(
        &self,
        scope: &ExecutionScope,
    ) -> Result<crate::store::EngineClaim, EngineError> {
        let permit = self.permit(scope)?;
        Ok(self.store.heartbeat_engine_claim(scope, &permit).await?)
    }

    /// Releases the live generation for one scope.
    pub async fn release_scope(&self, scope: &ExecutionScope) -> Result<(), EngineError> {
        let permit = self.permit(scope)?;
        self.store.release_engine_claim(scope, &permit).await?;
        self.permits
            .lock()
            .expect("permit lock poisoned")
            .remove(scope);
        Ok(())
    }

    /// Starts one Pending run after checking every exact action pin.
    pub async fn start(&self, scope: &ExecutionScope, run_id: &Id) -> Result<(), EngineError> {
        let view = self.store.get_run(scope, run_id).await?;
        let revision = self
            .store
            .get_revision(scope, &view.run.definition_id, &view.run.revision_hash)
            .await?;
        let evidence = self.registry.check_pins(&revision.action_pins);
        match self
            .store
            .start_run(
                scope,
                StartRun {
                    permit: self.permit(scope)?,
                    run_id: run_id.clone(),
                    compatibility_evidence: evidence,
                },
            )
            .await
        {
            Ok(_) => Ok(()),
            // The run already exists, so hydration corruption is marked before
            // the integrity failure is surfaced.
            Err(StoreError::CommittedObjectCorrupt { bad_ref, proof }) => Err(self
                .apply_committed_corruption(scope, run_id, bad_ref, proof, None)
                .await),
            Err(error) => Err(error.into()),
        }
    }

    /// Executes one deterministic scheduler pass and returns committed work count.
    pub async fn tick(&self, scope: &ExecutionScope) -> Result<usize, EngineError> {
        let permit = self.permit(scope)?;
        let mut changed = self.run_maintenance_scans(scope).await?;
        let retries = self
            .store
            .scan_due_retries(scope, first_page())
            .await?
            .items;
        for node in retries {
            if self
                .store
                .release_retry(
                    scope,
                    ReleaseRetry {
                        permit: permit.clone(),
                        run_id: node.run_id,
                        node_id: node.node_instance_id,
                        expected_node_version: node.version,
                    },
                )
                .await
                .is_ok()
            {
                changed += 1;
            }
        }
        let deadlines = self
            .store
            .scan_due_deadlines(scope, first_page())
            .await?
            .items;
        for attempt in deadlines {
            if self
                .store
                .timeout_attempt(
                    scope,
                    TimeoutAttempt {
                        permit: permit.clone(),
                        run_id: attempt.run_id,
                        node_id: attempt.node_instance_id,
                        attempt_id: attempt.attempt_id,
                    },
                )
                .await
                .is_ok()
            {
                changed += 1;
            }
        }

        let mut ready = self
            .store
            .scan_ready_nodes(scope, first_page())
            .await?
            .items;
        ready.extend(
            self.store
                .scan_budget_waiters(scope, first_page())
                .await?
                .items,
        );
        let mut action_nodes = Vec::new();
        for node in ready {
            match node.kind {
                NodeKind::Action if action_nodes.len() < self.config.max_concurrency => {
                    action_nodes.push(node)
                }
                NodeKind::Choice => {
                    self.execute_choice(scope, &permit, node).await?;
                    changed += 1;
                }
                NodeKind::Map => {
                    self.execute_map(scope, &permit, node).await?;
                    changed += 1;
                }
                NodeKind::Approval => {
                    self.execute_approval(scope, &permit, node).await?;
                    changed += 1;
                }
                NodeKind::Succeed | NodeKind::Fail => {
                    self.execute_terminal(scope, &permit, node).await?;
                    changed += 1;
                }
                _ => {}
            }
        }
        let mut claimed = Vec::new();
        for node in action_nodes {
            if let Some(invocation) = self.claim_action(scope, &permit, node).await? {
                claimed.push(invocation);
            }
        }
        if !claimed.is_empty() {
            let completions = thread::scope(|thread_scope| {
                let handles = claimed
                    .into_iter()
                    .map(|claimed| {
                        thread_scope.spawn(move || {
                            let outcome = block_on(
                                claimed
                                    .action
                                    .invoke(claimed.context.clone(), &claimed.input_bytes),
                            );
                            (claimed, outcome)
                        })
                    })
                    .collect::<Vec<_>>();
                handles
                    .into_iter()
                    .map(|handle| handle.join().expect("action thread panicked"))
                    .collect::<Vec<_>>()
            });
            for (claimed, outcome) in completions {
                let objects = self
                    .completion_objects(scope, &claimed.context, &outcome)
                    .await?;
                self.store
                    .complete_attempt(
                        scope,
                        CompleteAttempt {
                            completion_credential: claimed.context.completion_credential.clone(),
                            run_id: claimed.context.run_id.clone(),
                            node_id: claimed.context.node_instance_id.clone(),
                            attempt_id: claimed.context.attempt_id.clone(),
                            submitted_outcome: outcome,
                            objects,
                        },
                    )
                    .await?;
                self.cancellation
                    .lock()
                    .expect("cancellation lock poisoned")
                    .remove(&(
                        scope.clone(),
                        claimed.context.run_id,
                        claimed.context.attempt_id,
                    ));
                changed += 1;
            }
        }
        changed += self.complete_waiting_maps(scope, &permit).await?;
        Ok(changed)
    }

    /// Drives passes until the scoped frontier is quiescent at the current clock.
    pub async fn run_until_idle(
        &self,
        scope: &ExecutionScope,
        max_passes: usize,
    ) -> Result<usize, EngineError> {
        let mut total = 0;
        for _ in 0..max_passes {
            let changed = self.tick(scope).await?;
            total += changed;
            if changed == 0 {
                break;
            }
        }
        Ok(total)
    }

    /// Signals all locally running attempts for an already-terminalizing run.
    pub fn signal_run_cancellation(&self, scope: &ExecutionScope, run_id: &Id) {
        for ((token_scope, token_run, _), source) in self
            .cancellation
            .lock()
            .expect("cancellation lock poisoned")
            .iter()
        {
            if token_scope == scope && token_run == run_id {
                source.cancel();
            }
        }
    }

    /// Commits authenticated cancellation, then signals locally running actions.
    pub async fn cancel(
        &self,
        scope: &ExecutionScope,
        command: CancelRun,
    ) -> Result<CommandReceipt, EngineError> {
        let run_id = command.run_id.clone();
        let receipt = self.store.cancel_run(scope, command).await?;
        self.signal_run_cancellation(scope, &run_id);
        Ok(receipt)
    }

    fn permit(&self, scope: &ExecutionScope) -> Result<crate::store::EnginePermit, EngineError> {
        self.permits
            .lock()
            .expect("permit lock poisoned")
            .get(scope)
            .cloned()
            .ok_or(EngineError::ScopeNotAcquired)
    }

    fn next_id(&self, prefix: &str) -> Id {
        let number = self.id_counter.fetch_add(1, Ordering::AcqRel) + 1;
        Id::new(format!("{prefix}_{number:016x}")).expect("generated ID is valid")
    }

    async fn run_maintenance_scans(&self, scope: &ExecutionScope) -> Result<usize, EngineError> {
        let permit = self.permit(scope)?;
        let mut changed = 0;
        for run in self
            .store
            .scan_recovery_runs(scope, first_page())
            .await?
            .items
        {
            let recovered = self
                .store
                .recover_abandoned_attempts_for_run(
                    scope,
                    RecoverAbandonedAttemptsForRun {
                        permit: permit.clone(),
                        run_id: run.run_id,
                    },
                )
                .await?;
            changed += usize::from(!recovered.is_empty());
        }
        for run in self
            .store
            .scan_compatibility_rechecks(scope, first_page())
            .await?
            .items
        {
            let revision = self
                .store
                .get_revision(scope, &run.definition_id, &run.revision_hash)
                .await?;
            let evidence = self.registry.check_pins(&revision.action_pins);
            match run.status {
                crate::run::RunState::BlockedIncompatible
                    if evidence.incompatible_reference_locations.is_empty() =>
                {
                    self.store
                        .resume_compatible(
                            scope,
                            ResumeCompatible {
                                permit: permit.clone(),
                                run_id: run.run_id,
                                availability_evidence: evidence,
                            },
                        )
                        .await?;
                    changed += 1;
                }
                crate::run::RunState::Pending | crate::run::RunState::Running
                    if !evidence.incompatible_reference_locations.is_empty() =>
                {
                    let bytes = serde_jcs::to_vec(&IncompatibilityEvidenceDocument {
                        evidence_digest: &evidence.evidence_digest,
                        incompatible_reference_locations: &evidence
                            .incompatible_reference_locations,
                    })
                    .map_err(|_| EngineError::ObjectWrite)?;
                    let incompatibilities = self
                        .object_store
                        .put(scope, &bytes, "application/json")
                        .await
                        .map_err(|_| EngineError::ObjectWrite)?;
                    self.store
                        .suspend_incompatible(
                            scope,
                            SuspendIncompatible {
                                permit: permit.clone(),
                                run_id: run.run_id,
                                incompatibilities,
                                evidence,
                            },
                        )
                        .await?;
                    changed += 1;
                }
                _ => {}
            }
        }
        for gate in self.store.scan_due_gates(scope, first_page()).await?.items {
            let approval_output =
                if gate.on_expiry == crate::approval::ApprovalExpiryPolicy::Approve {
                    let bytes = crate::approval::canonical_expiry_approval_result();
                    Some(
                        self.object_store
                            .put(scope, &bytes, "application/json")
                            .await
                            .map_err(|_| EngineError::ObjectWrite)?,
                    )
                } else {
                    None
                };
            match self
                .store
                .expire_approval(
                    scope,
                    ExpireApproval {
                        permit: permit.clone(),
                        run_id: gate.run_id,
                        gate_id: gate.gate_id,
                        approval_output,
                    },
                )
                .await
            {
                Ok(_) => changed += 1,
                Err(StoreError::ApprovalRaceLost) => {}
                Err(error) => return Err(error.into()),
            }
        }
        for run in self
            .store
            .scan_due_run_lifetimes(scope, first_page())
            .await?
            .items
        {
            self.store
                .expire_run_lifetime(
                    scope,
                    ExpireRunLifetime {
                        permit: permit.clone(),
                        run_id: run.run_id,
                    },
                )
                .await?;
            changed += 1;
        }
        Ok(changed)
    }

    /// Reads one committed object through the crate-owned committed reader.
    ///
    /// The ordering rules live in `CommittedObjectReader`; this is only the
    /// engine's mapping of its four outcomes onto `EngineError`. A failed
    /// `mark_corrupt_storage` surfaces as the store failure, never as the
    /// integrity failure, because the run has not been invalidated in that
    /// case.
    async fn read_committed(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        committed: &ArtifactRef,
        owner_node_id: Option<&Id>,
    ) -> Result<VerifiedObject, EngineError> {
        // The reducer accepts only the node that produced the bad ref (or the
        // waiting Map parent, passed explicitly by aggregation).
        match self
            .reader
            .read(scope, run_id, committed, owner_node_id)
            .await
        {
            CommittedReadOutcome::Verified(object) => Ok(object),
            CommittedReadOutcome::StorageUnavailable => Err(EngineError::StorageUnavailable),
            CommittedReadOutcome::CorruptionApplied { .. } => Err(EngineError::ObjectRead),
            CommittedReadOutcome::CorruptionMarkFailed { error, .. } => Err(error.into()),
        }
    }

    /// Applies a store-reported committed-object corruption to an existing run.
    ///
    /// A store command may reject with `CommittedObjectCorrupt` after hydrating
    /// a committed prerequisite. The proof it carries is the same capability a
    /// direct read would have minted, so the run is marked here before the
    /// integrity failure is surfaced. A failed mark surfaces as the store
    /// failure.
    async fn apply_committed_corruption(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        bad_ref: ArtifactRef,
        proof: FailedReadProof,
        owner_node_id: Option<&Id>,
    ) -> EngineError {
        let owner_node_id = owner_node_id
            .cloned()
            .or_else(|| bad_ref.producer_node_id.clone());
        match self
            .store
            .mark_corrupt_storage(
                scope,
                MarkCorruptStorage {
                    run_id: run_id.clone(),
                    bad_ref,
                    proof,
                    owner_node_id,
                },
            )
            .await
        {
            Ok(_) => EngineError::ObjectRead,
            Err(error) => error.into(),
        }
    }

    /// Reads an object addressed by digest alone, with no committed ref to mark.
    ///
    /// Artifact locators carry a digest, not an `ArtifactRef`, so no valid
    /// `mark_corrupt_storage` command can be formed for them; corruption surfaces
    /// as an integrity error only. The availability split is still preserved.
    // ponytail: no artifact-ref lookup exists on WorkflowStore. If one is added,
    // route these two reads through read_committed as well.
    async fn read_by_digest(
        &self,
        scope: &ExecutionScope,
        digest: &Digest,
    ) -> Result<VerifiedObject, EngineError> {
        match self.object_store.get(scope, digest).await {
            Ok(object) => Ok(object),
            Err(ObjectReadError::StorageUnavailable) => Err(EngineError::StorageUnavailable),
            Err(ObjectReadError::Corrupt(_)) => Err(EngineError::ObjectRead),
        }
    }

    async fn definition_for(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
    ) -> Result<(crate::run::WorkflowRun, WorkflowDefinition), EngineError> {
        let run = self.store.get_run(scope, run_id).await?.run;
        let revision = self
            .store
            .get_revision(scope, &run.definition_id, &run.revision_hash)
            .await?;
        let object = self
            .read_committed(scope, run_id, &revision.canonical_definition_ref.0, None)
            .await?;
        let definition = crate::definition::parse_json_definition(
            std::str::from_utf8(&object.bytes).map_err(|_| EngineError::DefinitionDecode)?,
        )
        .map_err(|_| EngineError::DefinitionDecode)?;
        Ok((run, definition))
    }

    async fn claim_action(
        &self,
        scope: &ExecutionScope,
        permit: &crate::store::EnginePermit,
        node: NodeRun,
    ) -> Result<Option<ClaimedAction>, EngineError> {
        let (run, definition) = self.definition_for(scope, &node.run_id).await?;
        let definition_node = find_node(&definition, &node.definition_node_id)?;
        let action_ref = match definition_node {
            NodeDefinition::Action { action, .. } => action,
            NodeDefinition::Map { action, .. } if node.parent_map_instance_id.is_some() => action,
            _ => return Err(EngineError::DefinitionDecode),
        };
        let bound = match definition_node {
            NodeDefinition::Action { bindings, .. } => {
                self.bind_object(scope, &run, bindings).await?
            }
            NodeDefinition::Map { bindings, .. } => {
                self.bind_map_object(scope, &run, &node, bindings).await?
            }
            _ => return Err(EngineError::DefinitionDecode),
        };
        let bound_object = self
            .object_store
            .put(scope, &bound, "application/json")
            .await
            .map_err(|_| EngineError::ObjectWrite)?;
        let attempt_id = self.next_id(&format!("attempt_{}", self.config.instance_id.as_str()));
        let result = self
            .store
            .claim_node_attempt(
                scope,
                ClaimNodeAttempt {
                    permit: permit.clone(),
                    run_id: node.run_id.clone(),
                    node_id: node.node_instance_id.clone(),
                    expected_node_version: node.version,
                    attempt_id: attempt_id.clone(),
                    worker_id: self.config.instance_id.clone(),
                    bound_input: bound_object,
                    binding_derivation_digest: sha(&bound),
                },
            )
            .await?;
        let ClaimNodeAttemptResult::Claimed {
            invocation,
            completion_credential,
        } = result
        else {
            return Ok(None);
        };
        let attempt = self
            .store
            .get_attempt(scope, &node.run_id, &attempt_id)
            .await?;
        let source = CancellationSource::new();
        self.cancellation
            .lock()
            .expect("cancellation lock poisoned")
            .insert(
                (scope.clone(), node.run_id.clone(), attempt_id.clone()),
                source.clone(),
            );
        let context = ActionContext::new(
            scope.clone(),
            node.run_id,
            run.revision_hash,
            node.node_instance_id,
            attempt_id,
            attempt.attempt_number,
            completion_credential,
            attempt.deadline_at,
            source.token(),
            BudgetHandle {
                declared_max_cost_units: attempt.declared_max_cost,
            },
        );
        let action = self
            .registry
            .resolve(&action_ref.name)
            .ok_or(EngineError::ActionMissing)?;
        Ok(Some(ClaimedAction {
            action,
            context,
            input_bytes: bound,
            _invocation: invocation,
        }))
    }

    async fn bind_object(
        &self,
        scope: &ExecutionScope,
        run: &crate::run::WorkflowRun,
        bindings: &[Binding],
    ) -> Result<Vec<u8>, EngineError> {
        let mut target = Value::Object(Map::new());
        for binding in bindings {
            let value = self.resolve_source(scope, run, &binding.source).await?;
            set_pointer(&mut target, &binding.target, value)?;
        }
        serde_jcs::to_vec(&target).map_err(|_| EngineError::Binding)
    }

    async fn bind_map_object(
        &self,
        scope: &ExecutionScope,
        run: &crate::run::WorkflowRun,
        child: &NodeRun,
        bindings: &[MapBinding],
    ) -> Result<Vec<u8>, EngineError> {
        let parent_id = child
            .parent_map_instance_id
            .as_ref()
            .ok_or(EngineError::Binding)?;
        let parent = self.store.get_node(scope, &run.run_id, parent_id).await?;
        let map_input = parent.map_input_ref.ok_or(EngineError::Binding)?;
        let input = self
            .read_committed(scope, &run.run_id, &map_input.0, None)
            .await?;
        let items: Value =
            serde_json::from_slice(&input.bytes).map_err(|_| EngineError::Binding)?;
        let index = child.map_item_index.ok_or(EngineError::Binding)?;
        let item = items
            .as_array()
            .and_then(|values| values.get(index as usize))
            .cloned()
            .ok_or(EngineError::Binding)?;
        let mut target = Value::Object(Map::new());
        for binding in bindings {
            let value = match &binding.source {
                MapBindingSource::Constant { value } => value.clone(),
                MapBindingSource::RunInput { pointer } => {
                    self.resolve_source(
                        scope,
                        run,
                        &BindingSource::RunInput {
                            pointer: pointer.clone(),
                        },
                    )
                    .await?
                }
                MapBindingSource::NodeOutput { node_id, pointer } => {
                    self.resolve_source(
                        scope,
                        run,
                        &BindingSource::NodeOutput {
                            node_id: node_id.clone(),
                            pointer: pointer.clone(),
                        },
                    )
                    .await?
                }
                MapBindingSource::MapItem { pointer } => {
                    if pointer.is_empty() {
                        item.clone()
                    } else {
                        item.pointer(pointer).cloned().ok_or(EngineError::Binding)?
                    }
                }
                MapBindingSource::MapIndex => Value::from(index),
                MapBindingSource::ArtifactRef { source } => {
                    self.resolve_artifact_locator(scope, run, source).await?
                }
            };
            set_pointer(&mut target, &binding.target, value)?;
        }
        serde_jcs::to_vec(&target).map_err(|_| EngineError::Binding)
    }

    async fn resolve_source(
        &self,
        scope: &ExecutionScope,
        run: &crate::run::WorkflowRun,
        source: &BindingSource,
    ) -> Result<Value, EngineError> {
        match source {
            BindingSource::Constant { value } => Ok(value.clone()),
            BindingSource::RunInput { pointer } => {
                let object = self
                    .read_committed(scope, &run.run_id, &run.input_ref.0, None)
                    .await?;
                select_json(&object.bytes, pointer)
            }
            BindingSource::NodeOutput { node_id, pointer } => {
                let node = self.store.get_node(scope, &run.run_id, node_id).await?;
                let output = node.result_ref.ok_or(EngineError::Binding)?;
                let object = self
                    .read_committed(scope, &run.run_id, &output.0, None)
                    .await?;
                select_json(&object.bytes, pointer)
            }
            BindingSource::ArtifactRef { source } => {
                self.resolve_artifact_locator(scope, run, source).await
            }
        }
    }

    async fn resolve_artifact_locator(
        &self,
        scope: &ExecutionScope,
        run: &crate::run::WorkflowRun,
        source: &ArtifactLocator,
    ) -> Result<Value, EngineError> {
        let value = match source {
            ArtifactLocator::Literal {
                artifact_ref_id,
                digest,
                media_type,
            } => {
                let object = self.read_by_digest(scope, digest).await?;
                if object.reference.media_type() != media_type {
                    return Err(EngineError::Binding);
                }
                ArtifactRefValue {
                    artifact_ref_id: artifact_ref_id.clone(),
                    digest: digest.clone(),
                    size_bytes: object.reference.size_bytes().to_string(),
                    media_type: media_type.clone(),
                }
            }
            ArtifactLocator::RunInput { pointer } => {
                let object = self
                    .read_committed(scope, &run.run_id, &run.input_ref.0, None)
                    .await?;
                serde_json::from_value(select_json(&object.bytes, pointer)?)
                    .map_err(|_| EngineError::Binding)?
            }
            ArtifactLocator::NodeOutput { node_id, pointer } => {
                let node = self.store.get_node(scope, &run.run_id, node_id).await?;
                let output = node.result_ref.ok_or(EngineError::Binding)?;
                let object = self
                    .read_committed(scope, &run.run_id, &output.0, None)
                    .await?;
                serde_json::from_value(select_json(&object.bytes, pointer)?)
                    .map_err(|_| EngineError::Binding)?
            }
        };
        let object = self.read_by_digest(scope, &value.digest).await?;
        if object.reference.size_bytes().to_string() != value.size_bytes
            || object.reference.media_type() != value.media_type
        {
            return Err(EngineError::Binding);
        }
        serde_json::to_value(value).map_err(|_| EngineError::Binding)
    }

    async fn execute_choice(
        &self,
        scope: &ExecutionScope,
        permit: &crate::store::EnginePermit,
        node: NodeRun,
    ) -> Result<(), EngineError> {
        let (run, definition) = self.definition_for(scope, &node.run_id).await?;
        let NodeDefinition::Choice {
            input,
            selector,
            cases,
            default,
            ..
        } = find_node(&definition, &node.definition_node_id)?
        else {
            return Err(EngineError::DefinitionDecode);
        };
        let input_value = self.resolve_source(scope, &run, input).await?;
        let input_bytes = serde_jcs::to_vec(&input_value).map_err(|_| EngineError::Binding)?;
        let selector_value = input_value.pointer(selector).ok_or(EngineError::Binding)?;
        let mut selected = None;
        for (index, case) in cases.iter().enumerate() {
            let (matches, target) = match case {
                ChoiceCase::Equals { equals, next } => (selector_value == equals, next),
                ChoiceCase::In { r#in, next } => {
                    (r#in.iter().any(|value| value == selector_value), next)
                }
            };
            if matches {
                selected = Some(crate::run::ChoiceSelection::Case {
                    case_index: index as u32,
                    edge_id: edge_id(
                        &run.revision_hash,
                        &node.definition_node_id,
                        &format!("case/{index}"),
                        target,
                    ),
                });
                break;
            }
        }
        let selection = selected.unwrap_or_else(|| crate::run::ChoiceSelection::Default {
            edge_id: edge_id(
                &run.revision_hash,
                &node.definition_node_id,
                "default",
                default,
            ),
        });
        let object = self
            .object_store
            .put(scope, &input_bytes, "application/json")
            .await
            .map_err(|_| EngineError::ObjectWrite)?;
        self.store
            .record_choice(
                scope,
                RecordChoice {
                    permit: permit.clone(),
                    run_id: node.run_id,
                    node_id: node.node_instance_id,
                    expected_node_version: node.version,
                    input: object,
                    evaluated_selector_digest: sha(
                        &serde_jcs::to_vec(selector_value).map_err(|_| EngineError::Binding)?
                    ),
                    selection,
                },
            )
            .await?;
        Ok(())
    }

    async fn execute_map(
        &self,
        scope: &ExecutionScope,
        permit: &crate::store::EnginePermit,
        node: NodeRun,
    ) -> Result<(), EngineError> {
        let (run, definition) = self.definition_for(scope, &node.run_id).await?;
        let NodeDefinition::Map { items, .. } = find_node(&definition, &node.definition_node_id)?
        else {
            return Err(EngineError::DefinitionDecode);
        };
        let value = self.resolve_source(scope, &run, items).await?;
        let values = value.as_array().ok_or(EngineError::Binding)?;
        let bytes = serde_jcs::to_vec(&value).map_err(|_| EngineError::Binding)?;
        let input = self
            .object_store
            .put(scope, &bytes, "application/json")
            .await
            .map_err(|_| EngineError::ObjectWrite)?;
        let mut identities = Vec::with_capacity(values.len());
        let mut ordered_items = Vec::with_capacity(values.len());
        for (index, item) in values.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| EngineError::Binding)?;
            let item_digest = sha(&serde_jcs::to_vec(item).map_err(|_| EngineError::Binding)?);
            let child_id = map_child_id(&node.run_id, &node.node_instance_id, index, &item_digest);
            identities.push(MapChildIdentity {
                item_index: index,
                item_digest: item_digest.clone(),
                child_id: child_id.clone(),
            });
            ordered_items.push(OrderedMapItem {
                index,
                item_digest,
                child_id,
            });
        }
        match self
            .store
            .expand_map(
                scope,
                ExpandMap {
                    permit: permit.clone(),
                    run_id: node.run_id.clone(),
                    map_node_id: node.node_instance_id.clone(),
                    expected_node_version: node.version,
                    input,
                    ordered_items,
                    expansion_digest: map_expansion_digest(&identities),
                },
            )
            .await
        {
            Ok(_) => {}
            // Section 5.5: both variants are applied outcomes. `expand_map` already
            // committed N46/R08 for the still-Ready Map node and terminalized the run in
            // the same transaction, so there is nothing left for the scheduler to do and
            // nothing to report as a scheduler error. Section 5.3 lists exactly these two
            // as the command's runtime failure outcomes (`MapInputInvalid`,
            // `MapBoundExceeded`, `RunDynamicNodeLimitExceeded`,
            // `AggregateObjectLimitExceeded`).
            Err(
                StoreError::ContractValidationApplied { .. } | StoreError::RunLimitApplied { .. },
            ) => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    async fn execute_approval(
        &self,
        scope: &ExecutionScope,
        permit: &crate::store::EnginePermit,
        node: NodeRun,
    ) -> Result<(), EngineError> {
        let (run, definition) = self.definition_for(scope, &node.run_id).await?;
        let NodeDefinition::Approval { request, .. } =
            find_node(&definition, &node.definition_node_id)?
        else {
            return Err(EngineError::DefinitionDecode);
        };
        let request = self.resolve_source(scope, &run, request).await?;
        let bytes = serde_jcs::to_vec(&request).map_err(|_| EngineError::Binding)?;
        let request = self
            .object_store
            .put(scope, &bytes, "application/json")
            .await
            .map_err(|_| EngineError::ObjectWrite)?;
        self.store
            .request_approval(
                scope,
                RequestApproval {
                    permit: permit.clone(),
                    run_id: node.run_id.clone(),
                    node_id: node.node_instance_id.clone(),
                    expected_node_version: node.version,
                    gate_id: approval_gate_id(&node.run_id, &node.node_instance_id),
                    request,
                },
            )
            .await?;
        Ok(())
    }

    async fn complete_waiting_maps(
        &self,
        scope: &ExecutionScope,
        permit: &crate::store::EnginePermit,
    ) -> Result<usize, EngineError> {
        let mut changed = 0;
        let runs = self
            .store
            .scan_compatibility_rechecks(scope, first_page())
            .await?
            .items;
        for run in runs
            .into_iter()
            .filter(|run| run.status == crate::run::RunState::Running)
        {
            let mut cursor = None;
            let mut nodes = Vec::new();
            loop {
                let page = self
                    .store
                    .list_nodes(
                        scope,
                        &run.run_id,
                        PageRequest {
                            cursor: cursor.clone(),
                            page_size: 1000,
                        },
                    )
                    .await?;
                nodes.extend(page.items);
                cursor = page.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            for parent in nodes.iter().filter(|node| {
                node.kind == NodeKind::Map && node.status == crate::run::NodeState::WaitingChildren
            }) {
                let mut children = nodes
                    .iter()
                    .filter(|node| {
                        node.parent_map_instance_id.as_ref() == Some(&parent.node_instance_id)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if children.len() != parent.map_child_count.unwrap_or(0) as usize
                    || children
                        .iter()
                        .any(|child| child.status != crate::run::NodeState::Succeeded)
                {
                    continue;
                }
                children.sort_by_key(|child| child.map_item_index);
                let mut values: Vec<Value> = Vec::with_capacity(children.len());
                for child in &children {
                    let output = child.result_ref.clone().ok_or(EngineError::Binding)?;
                    let object = match self
                        .read_committed(
                            scope,
                            &run.run_id,
                            &output.0,
                            Some(&parent.node_instance_id),
                        )
                        .await
                    {
                        Ok(object) => object,
                        // The helper already committed mark_corrupt_storage for the
                        // waiting Map parent; stop aggregating this run's map.
                        Err(EngineError::ObjectRead) => {
                            changed += 1;
                            break;
                        }
                        // Unavailability commits nothing and aborts the pass.
                        Err(error) => return Err(error),
                    };
                    values.push(
                        serde_json::from_slice(&object.bytes).map_err(|_| EngineError::Binding)?,
                    );
                }
                if values.len() != children.len() {
                    continue;
                }
                let bytes = serde_jcs::to_vec(&values).map_err(|_| EngineError::Binding)?;
                let aggregate = self
                    .object_store
                    .put(scope, &bytes, "application/json")
                    .await
                    .map_err(|_| EngineError::ObjectWrite)?;
                let current_parent = self
                    .store
                    .get_node(scope, &run.run_id, &parent.node_instance_id)
                    .await?;
                if current_parent.status != crate::run::NodeState::WaitingChildren {
                    continue;
                }
                match self
                    .store
                    .complete_map(
                        scope,
                        CompleteMap {
                            permit: permit.clone(),
                            run_id: run.run_id.clone(),
                            map_node_id: parent.node_instance_id.clone(),
                            expected_node_version: current_parent.version,
                            aggregate,
                        },
                    )
                    .await
                {
                    Ok(_) => changed += 1,
                    Err(StoreError::CasConflict | StoreError::ChildrenIncomplete) => {}
                    // The store hydrated a committed child object and found it
                    // corrupt. The run exists, so it is marked before the
                    // integrity failure is surfaced.
                    Err(StoreError::CommittedObjectCorrupt { bad_ref, proof }) => {
                        return Err(self
                            .apply_committed_corruption(
                                scope,
                                &run.run_id,
                                bad_ref,
                                proof,
                                Some(&parent.node_instance_id),
                            )
                            .await)
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(changed)
    }

    async fn execute_terminal(
        &self,
        scope: &ExecutionScope,
        permit: &crate::store::EnginePermit,
        node: NodeRun,
    ) -> Result<(), EngineError> {
        let (run, definition) = self.definition_for(scope, &node.run_id).await?;
        let output = match find_node(&definition, &node.definition_node_id)? {
            NodeDefinition::Succeed { output, .. } => {
                let value = self.resolve_source(scope, &run, output).await?;
                let bytes = serde_jcs::to_vec(&value).map_err(|_| EngineError::Binding)?;
                Some(
                    self.object_store
                        .put(scope, &bytes, "application/json")
                        .await
                        .map_err(|_| EngineError::ObjectWrite)?,
                )
            }
            NodeDefinition::Fail { .. } => None,
            _ => return Err(EngineError::DefinitionDecode),
        };
        self.store
            .resolve_terminal_node(
                scope,
                ResolveTerminalNode {
                    permit: permit.clone(),
                    run_id: node.run_id,
                    node_id: node.node_instance_id,
                    expected_node_version: node.version,
                    output,
                },
            )
            .await?;
        Ok(())
    }

    async fn completion_objects(
        &self,
        scope: &ExecutionScope,
        context: &ActionContext,
        outcome: &ActionOutcome,
    ) -> Result<CompletionObjects, EngineError> {
        let output = match outcome {
            ActionOutcome::Success { output, .. } => {
                let bytes = serde_jcs::to_vec(output).map_err(|_| EngineError::ObjectWrite)?;
                Some(
                    self.object_store
                        .put(scope, &bytes, "application/json")
                        .await
                        .map_err(|_| EngineError::ObjectWrite)?,
                )
            }
            _ => None,
        };
        let artifacts = match outcome {
            ActionOutcome::Success { artifacts, .. } => artifacts
                .iter()
                .map(|artifact| artifact.object.clone())
                .collect(),
            _ => Vec::new(),
        };
        let diagnostics = match outcome {
            ActionOutcome::Success { diagnostics, .. }
            | ActionOutcome::Retryable { diagnostics, .. }
            | ActionOutcome::Permanent { diagnostics, .. } => {
                if let Some(diagnostics) = diagnostics {
                    let bytes =
                        serde_jcs::to_vec(diagnostics).map_err(|_| EngineError::ObjectWrite)?;
                    Some(
                        self.object_store
                            .put(scope, &bytes, "application/json")
                            .await
                            .map_err(|_| EngineError::ObjectWrite)?,
                    )
                } else {
                    None
                }
            }
        };
        let _ = context;
        Ok(CompletionObjects {
            output,
            artifacts,
            diagnostics,
        })
    }
}

struct ClaimedAction {
    action: Arc<dyn crate::action::WorkflowAction>,
    context: ActionContext,
    input_bytes: Vec<u8>,
    _invocation: crate::action::ActionInvocation,
}

/// Engine scheduling and binding failure.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// A durable store command failed.
    ///
    /// A pre-run `StoreError::CommittedObjectCorrupt` propagates through here
    /// unchanged. `create_run` is a no-write pre-run failure: no run exists, so
    /// no `mark_corrupt_storage` is attempted, no run and no control-plane state
    /// are created, and the carried proof stays available for diagnostics.
    ///.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The scope has no acquired local scheduler permit.
    #[error("engine scope is not acquired")]
    ScopeNotAcquired,
    /// A committed object failed verification. Proof-backed integrity failure.
    #[error("committed object read failed")]
    ObjectRead,
    /// The object store could not complete a read. No proof, no state change.
    #[error("object storage unavailable")]
    StorageUnavailable,
    /// An object could not be published.
    #[error("object publication failed")]
    ObjectWrite,
    /// The canonical definition object was invalid.
    #[error("definition object could not be decoded")]
    DefinitionDecode,
    /// A dataflow binding could not be resolved.
    #[error("dataflow binding failed")]
    Binding,
    /// A pinned action disappeared after compatibility checking.
    #[error("pinned action implementation missing")]
    ActionMissing,
}

fn first_page() -> PageRequest {
    PageRequest {
        cursor: None,
        page_size: 1000,
    }
}

fn find_node<'a>(
    definition: &'a WorkflowDefinition,
    id: &Id,
) -> Result<&'a NodeDefinition, EngineError> {
    definition
        .nodes
        .iter()
        .find(|node| match node {
            NodeDefinition::Action { id: node_id, .. }
            | NodeDefinition::Map { id: node_id, .. }
            | NodeDefinition::Choice { id: node_id, .. }
            | NodeDefinition::Approval { id: node_id, .. }
            | NodeDefinition::Succeed { id: node_id, .. }
            | NodeDefinition::Fail { id: node_id, .. } => node_id == id,
        })
        .ok_or(EngineError::DefinitionDecode)
}

fn select_json(bytes: &[u8], pointer: &str) -> Result<Value, EngineError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| EngineError::Binding)?;
    value.pointer(pointer).cloned().ok_or(EngineError::Binding)
}

fn set_pointer(target: &mut Value, pointer: &str, value: Value) -> Result<(), EngineError> {
    if pointer.is_empty() {
        *target = value;
        return Ok(());
    }
    let segments = pointer
        .strip_prefix('/')
        .ok_or(EngineError::Binding)?
        .split('/')
        .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
        .collect::<Vec<_>>();
    let mut cursor = target;
    for segment in &segments[..segments.len() - 1] {
        let object = cursor.as_object_mut().ok_or(EngineError::Binding)?;
        cursor = object
            .entry(segment.clone())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    cursor
        .as_object_mut()
        .ok_or(EngineError::Binding)?
        .insert(segments.last().expect("non-empty").clone(), value);
    Ok(())
}

fn sha(bytes: &[u8]) -> Digest {
    let hex = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Digest::new(format!("sha256:{hex}")).expect("SHA-256 output is valid")
}

fn approval_gate_id(run_id: &Id, node_id: &Id) -> Id {
    fn length_prefixed(value: &[u8]) -> Vec<u8> {
        let mut encoded = Vec::with_capacity(8 + value.len());
        encoded.extend((value.len() as u64).to_be_bytes());
        encoded.extend(value);
        encoded
    }
    let mut bytes = length_prefixed(b"dagger-approval-v1");
    bytes.extend(length_prefixed(run_id.as_str().as_bytes()));
    bytes.extend(length_prefixed(node_id.as_str().as_bytes()));
    let hex = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Id::new(format!("approval_{hex}")).expect("SHA-256 gate ID is valid")
}

struct ThreadWake(thread::Thread);

impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on<T>(future: impl Future<Output = T>) -> T {
    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match Pin::new(&mut future).poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => thread::park(),
        }
    }
}
