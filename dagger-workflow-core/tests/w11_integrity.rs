//! W11-A integrity propagation and W11-B host boundary acceptance specs.
//!
//! Normative source: docs/WORKFLOW_CORE_CONTRACT_ERRATUM_0_1_1.md sections A
//! and B. Two properties are proved here that a green pass alone never shows:
//! the proof and the typed use that reach the caller are the ones the failing
//! read actually produced, and the host reader's four outcomes are ordered
//! against durable control-plane state rather than against a return value.

use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::{
    ArtifactRef, FailedReadClass, ObjectReadError, ObjectStore, ObjectStoreError, VerifiedObject,
    VerifiedObjectRef,
};
use dagger_workflow_core::committed_read::{CommittedObjectReader, CommittedReadOutcome};
use dagger_workflow_core::definition::{
    ActionReference, BackoffPolicy, Binding, BindingSource, NodeDefinition, PublishableDefinition,
    RetryPolicy, TimeoutPolicy, WorkflowDefinition,
};
use dagger_workflow_core::engine::TestClock;
use dagger_workflow_core::ids::{CostUnits, Digest, Id, Timestamp, TopologicalRank, Version};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::run::{RunLimits, RunState, WorkflowRun};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::store::{
    CreateDefinition, CreateRun, PublishRevision, ResolvedActionSchemas, StoreError, WorkflowStore,
};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;

const SCHEMA: &[u8] = br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#;

fn block_on<T>(future: impl Future<Output = T>) -> T {
    struct ThreadWake(thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
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

fn scope(tenant: &str) -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new(tenant).unwrap(),
        namespace: ScopeAtom::new("integrity").unwrap(),
    }
}

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}

/// Injected object-store behaviour, armed per digest.
#[derive(Clone, Copy, Debug)]
enum Fault {
    /// Every read reports unavailability, mints no proof, and touches nothing.
    Unavailable,
    /// The nth read since arming fails verification; every other read succeeds.
    ///
    /// The corruption is transient on purpose. A caller that discards the first
    /// proof and reads again observes healthy bytes, which is exactly the
    /// unrecoverable case erratum 0.1.1 section A.2 forbids.
    CorruptOnRead(usize),
}

/// Minimal fault-injecting `ObjectStore` wrapper. Test-local by design.
struct FaultObjectStore {
    inner: Arc<InMemoryObjectStore<TestClock>>,
    faults: Mutex<BTreeMap<Digest, (Fault, usize)>>,
}

impl FaultObjectStore {
    fn new(inner: Arc<InMemoryObjectStore<TestClock>>) -> Self {
        Self {
            inner,
            faults: Mutex::new(BTreeMap::new()),
        }
    }

    /// Arms one digest and resets its read counter.
    fn arm(&self, digest: &Digest, fault: Fault) {
        self.faults
            .lock()
            .expect("fault lock poisoned")
            .insert(digest.clone(), (fault, 0));
    }

    /// Counts reads of an armed digest since it was armed.
    fn reads(&self, digest: &Digest) -> usize {
        self.faults
            .lock()
            .expect("fault lock poisoned")
            .get(digest)
            .map(|(_, count)| *count)
            .unwrap_or_default()
    }
}

impl ObjectStore for FaultObjectStore {
    async fn put(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        self.inner.put(scope, bytes, media_type).await
    }

    async fn publish_if_absent(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        self.inner.publish_if_absent(scope, bytes, media_type).await
    }

    async fn get(
        &self,
        scope: &ExecutionScope,
        requested: &Digest,
    ) -> Result<VerifiedObject, ObjectReadError> {
        let armed = {
            let mut faults = self.faults.lock().expect("fault lock poisoned");
            faults.get_mut(requested).map(|entry| {
                entry.1 += 1;
                (entry.0, entry.1)
            })
        };
        match armed {
            None => self.inner.get(scope, requested).await,
            Some((Fault::Unavailable, _)) => Err(ObjectReadError::StorageUnavailable),
            Some((Fault::CorruptOnRead(target), count)) if count == target => {
                let healthy = self
                    .inner
                    .get(scope, requested)
                    .await
                    .expect("armed digest is committed")
                    .bytes;
                assert!(self
                    .inner
                    .corrupt_bytes(scope, requested, b"corrupted".to_vec()));
                let error = self
                    .inner
                    .get(scope, requested)
                    .await
                    .expect_err("corrupted bytes fail verification");
                assert!(self.inner.corrupt_bytes(scope, requested, healthy));
                Err(error)
            }
            Some((Fault::CorruptOnRead(_), _)) => self.inner.get(scope, requested).await,
        }
    }
}

fn definition(definition_id: &Id, schema: &Digest) -> WorkflowDefinition {
    WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: definition_id.clone(),
        name: "integrity fixture".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.clone(),
        run_output_schema_digest: schema.clone(),
        entry_node_id: id("action"),
        nodes: vec![
            NodeDefinition::Action {
                id: id("action"),
                action: ActionReference {
                    name: "integrity.action".to_owned(),
                    contract_version: "1".to_owned(),
                    input_schema_digest: schema.clone(),
                    output_schema_digest: schema.clone(),
                    compatible_implementation_requirement: schema.clone(),
                },
                bindings: vec![Binding {
                    target: "/value".to_owned(),
                    source: BindingSource::RunInput {
                        pointer: "/value".to_owned(),
                    },
                }],
                retry: RetryPolicy {
                    max_attempts: 2,
                    backoff: BackoffPolicy::Fixed { delay_ms: 0 },
                },
                timeout: TimeoutPolicy { timeout_ms: 60_000 },
                declared_max_cost_units: CostUnits(1),
                next: vec![id("succeed")],
            },
            NodeDefinition::Succeed {
                id: id("succeed"),
                output: BindingSource::NodeOutput {
                    node_id: id("action"),
                    pointer: String::new(),
                },
            },
        ],
    }
}

/// A published revision whose run input schema and action pin schemas are one
/// shared digest under two distinct typed uses (ordinal 0 and ordinal 1).
struct Fixture {
    scope: ExecutionScope,
    definition_id: Id,
    revision_hash: Digest,
    schema_digest: Digest,
    principal: AuthenticatedPrincipal,
}

async fn seed_revision<S: WorkflowStore>(
    store: &S,
    objects: &InMemoryObjectStore<TestClock>,
    tenant: &str,
) -> Fixture {
    let execution_scope = scope(tenant);
    let schema = objects
        .put(&execution_scope, SCHEMA, "application/json")
        .await
        .unwrap();
    let principal = AuthenticatedPrincipal::mint(
        execution_scope.clone(),
        format!("{tenant}-host"),
        Vec::new(),
        schema.digest().clone(),
    )
    .unwrap();
    let definition_id = id("definition");
    store
        .create_definition(
            &execution_scope,
            CreateDefinition {
                definition_id: definition_id.clone(),
                display_name: "integrity fixture".to_owned(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let parsed = definition(&definition_id, schema.digest());
    let canonical = objects
        .put(
            &execution_scope,
            &serde_jcs::to_vec(&parsed).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    store
        .publish_revision(
            &execution_scope,
            PublishRevision {
                definition_id: definition_id.clone(),
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema.clone(),
                resolved_action_schema_objects: BTreeMap::from([(
                    "action".to_owned(),
                    ResolvedActionSchemas {
                        input_schema: schema.clone(),
                        output_schema: schema.clone(),
                    },
                )]),
                parsed_revision: PublishableDefinition {
                    definition: parsed,
                    topological_ranks: BTreeMap::from([
                        (id("action"), TopologicalRank(0)),
                        (id("succeed"), TopologicalRank(1)),
                    ]),
                },
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    Fixture {
        scope: execution_scope,
        definition_id,
        revision_hash: canonical.digest().clone(),
        schema_digest: schema.digest().clone(),
        principal,
    }
}

async fn create_run<S: WorkflowStore>(
    store: &S,
    objects: &InMemoryObjectStore<TestClock>,
    fixture: &Fixture,
    run_id: &str,
    input_bytes: &[u8],
) -> Result<WorkflowRun, StoreError> {
    let input = objects
        .put(&fixture.scope, input_bytes, "application/json")
        .await
        .unwrap();
    store
        .create_run(
            &fixture.scope,
            CreateRun {
                run_id: id(run_id),
                definition_id: fixture.definition_id.clone(),
                revision_hash: fixture.revision_hash.clone(),
                input,
                budget_limit: CostUnits(10),
                limits: RunLimits {
                    max_dynamic_node_instances: 10,
                    max_total_attempts: 10,
                    max_total_events: 1_000,
                    max_inline_json_bytes_per_value: 10_000,
                    max_artifacts_per_attempt: 10,
                    max_aggregate_object_bytes_per_run: 100_000,
                    max_run_lifetime_ms: 100_000,
                },
                principal: fixture.principal.clone(),
                idempotency_token: format!("{run_id}-create-token-long-enough"),
            },
        )
        .await?;
    Ok(store
        .get_run(&fixture.scope, &id(run_id))
        .await
        .unwrap()
        .run)
}

// ---------------------------------------------------------------------------
// W11-B: host boundary. Erratum 0.1.1 section B.
// ---------------------------------------------------------------------------

/// The corruption command is durable before the integrity outcome exists.
///
/// Asserted against the control plane, not against the returned value: at the
/// first instant the caller can observe `CorruptionApplied`, `get_run` must
/// already report CorruptStorage. Erratum 0.1.1 sections B.2 and B.3.
#[test]
fn host_read_commits_corruption_before_reporting_it() {
    block_on(async {
        let clock = Arc::new(TestClock::new(Timestamp(1_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        let fixture = seed_revision(store.as_ref(), objects.as_ref(), "host-corrupt").await;
        let run = create_run(
            store.as_ref(),
            objects.as_ref(),
            &fixture,
            "run",
            br#"{"value":1}"#,
        )
        .await
        .unwrap();
        let bad_ref = run.input_ref.0.clone();
        assert!(objects.corrupt_bytes(&fixture.scope, &bad_ref.digest, b"tampered".to_vec()));

        let reader = CommittedObjectReader::new(store.clone(), objects.clone());
        let outcome = reader
            .read(&fixture.scope, &run.run_id, &bad_ref, None)
            .await;
        let observed = store
            .get_run(&fixture.scope, &run.run_id)
            .await
            .unwrap()
            .run;

        let CommittedReadOutcome::CorruptionApplied { proof } = outcome else {
            panic!("a committed corrupt object with a live run applies corruption");
        };
        assert_eq!(proof.error_class(), FailedReadClass::DigestInvalid);
        assert_eq!(observed.status, RunState::CorruptStorage);
        assert_eq!(
            observed.corrupt_bad_artifact_ref_id,
            Some(bad_ref.artifact_ref_id)
        );
        assert_eq!(
            observed.corrupt_error_class,
            Some(FailedReadClass::DigestInvalid)
        );
    });
}

/// Unavailability mints no proof and mutates nothing at all.
///
/// The whole run entity is compared before and after, so an event append, a
/// version bump, or a timestamp touch fails this. Erratum 0.1.1 sections A.4
/// and B.5.
#[test]
fn host_read_storage_unavailable_leaves_the_run_untouched() {
    block_on(async {
        let clock = Arc::new(TestClock::new(Timestamp(2_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        let fixture = seed_revision(store.as_ref(), objects.as_ref(), "host-unavailable").await;
        let run = create_run(
            store.as_ref(),
            objects.as_ref(),
            &fixture,
            "run",
            br#"{"value":1}"#,
        )
        .await
        .unwrap();
        let bad_ref = run.input_ref.0.clone();
        let faulty = Arc::new(FaultObjectStore::new(objects.clone()));
        faulty.arm(&bad_ref.digest, Fault::Unavailable);

        let reader = CommittedObjectReader::new(store.clone(), faulty.clone());
        let outcome = reader
            .read(&fixture.scope, &run.run_id, &bad_ref, None)
            .await;
        let observed = store
            .get_run(&fixture.scope, &run.run_id)
            .await
            .unwrap()
            .run;

        assert!(
            matches!(outcome, CommittedReadOutcome::StorageUnavailable),
            "{outcome:?}"
        );
        assert_eq!(observed, run);
        assert_eq!(observed.status, RunState::Pending);
    });
}

/// A failed corruption command is reported instead of the integrity result.
///
/// Reporting `CorruptionApplied` here would tell the host the run output was
/// already invalidated while the control plane still says otherwise. Erratum
/// 0.1.1 section B.2, mark-failure precedence.
#[test]
fn mark_failure_takes_precedence_over_the_integrity_result() {
    block_on(async {
        let clock = Arc::new(TestClock::new(Timestamp(3_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock));
        let fixture = seed_revision(store.as_ref(), objects.as_ref(), "mark-failure").await;
        let target = create_run(
            store.as_ref(),
            objects.as_ref(),
            &fixture,
            "target-run",
            br#"{"value":1}"#,
        )
        .await
        .unwrap();
        let other = create_run(
            store.as_ref(),
            objects.as_ref(),
            &fixture,
            "other-run",
            br#"{"value":2}"#,
        )
        .await
        .unwrap();
        // A genuinely corrupt committed object whose typed use belongs to a
        // different run: the read fails, and the corruption command must reject.
        let bad_ref = other.input_ref.0.clone();
        assert!(objects.corrupt_bytes(&fixture.scope, &bad_ref.digest, b"tampered".to_vec()));

        let reader = CommittedObjectReader::new(store.clone(), objects.clone());
        let outcome = reader
            .read(&fixture.scope, &target.run_id, &bad_ref, None)
            .await;
        let observed_target = store
            .get_run(&fixture.scope, &target.run_id)
            .await
            .unwrap()
            .run;
        let observed_other = store
            .get_run(&fixture.scope, &other.run_id)
            .await
            .unwrap()
            .run;

        let CommittedReadOutcome::CorruptionMarkFailed { proof, error } = outcome else {
            panic!("a rejected corruption command is never applied corruption");
        };
        assert_eq!(proof.error_class(), FailedReadClass::DigestInvalid);
        assert!(
            matches!(error, StoreError::InvalidFailedReadProof),
            "{error:?}"
        );
        assert_eq!(observed_target, target);
        assert_eq!(observed_other, other);
        assert_ne!(observed_target.status, RunState::CorruptStorage);
    });
}

// ---------------------------------------------------------------------------
// W11-A: integrity propagation through hydration, and the infrastructure-class
// mark failure. Both need the durable adapter, which is the only implementation
// that hydrates committed prerequisite objects before opening a transaction.
// ---------------------------------------------------------------------------

#[cfg(feature = "sqlite")]
mod durable {
    use super::*;
    use dagger_workflow_core::sqlite::SqliteWorkflowStore;
    use dagger_workflow_core::store::CompleteMap;

    async fn open(
        clock: Arc<TestClock>,
        objects: Arc<FaultObjectStore>,
    ) -> SqliteWorkflowStore<TestClock, FaultObjectStore> {
        SqliteWorkflowStore::open_url("sqlite::memory:", clock, objects)
            .await
            .unwrap()
    }

    /// The exact committed typed use and the first proof reach the caller.
    ///
    /// The armed digest fails verification once and then reads healthy, so a
    /// re-minted proof is not merely different, it is impossible: a second read
    /// succeeds. Erratum 0.1.1 sections A.1, A.2 and A.4.
    #[tokio::test]
    async fn hydration_corruption_delivers_the_first_proof_and_the_exact_ref() {
        let clock = Arc::new(TestClock::new(Timestamp(4_000)));
        let inner = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let objects = Arc::new(FaultObjectStore::new(inner.clone()));
        let store = open(clock, objects.clone()).await;
        let fixture = seed_revision(&store, inner.as_ref(), "hydration-corrupt").await;
        let revision = store
            .get_revision(
                &fixture.scope,
                &fixture.definition_id,
                &fixture.revision_hash,
            )
            .await
            .unwrap();
        objects.arm(&fixture.schema_digest, Fault::CorruptOnRead(1));

        let error = create_run(&store, inner.as_ref(), &fixture, "run", br#"{"value":1}"#)
            .await
            .expect_err("hydrating a corrupt pinned schema fails the command");

        let StoreError::CommittedObjectCorrupt { bad_ref, proof } = error else {
            panic!("object corruption is CommittedObjectCorrupt and nothing else: {error:?}");
        };
        // Both root schema refs carry the same bytes in this fixture, so hydration
        // has two equally valid typed uses of the armed digest and which one it
        // reaches first is incidental ordering, not a property. This case owns the
        // first-proof and no-run-created guarantees; discriminating which typed use
        // gets named belongs to hydration_names_the_typed_use_whose_read_actually_failed.
        assert!(
            [
                &revision.run_input_schema_ref.0,
                &revision.run_output_schema_ref.0,
            ]
            .contains(&&bad_ref),
            "the named ref must be a pinned root schema use: {bad_ref:?}"
        );
        assert_eq!(proof.error_class(), FailedReadClass::DigestInvalid);
        // One read: the proof that arrived is the one the failure minted, not a
        // proof recovered by reading again.
        assert_eq!(objects.reads(&fixture.schema_digest), 1);
        assert!(inner
            .get(&fixture.scope, &fixture.schema_digest)
            .await
            .is_ok());
        // A.4: a pre-run failure creates no run and applies no transition.
        assert!(matches!(
            store.get_run(&fixture.scope, &id("run")).await,
            Err(StoreError::NotFound)
        ));
    }

    /// Hydration keeps typed-use identity instead of collapsing to a digest.
    ///
    /// The revision's run input schema and its action pin output schema are the
    /// same bytes under two different `ArtifactRef`s. Hydration therefore
    /// verifies the digest twice, once per typed use, and the ref it reports is
    /// the use whose own read failed: corrupting the first read and corrupting
    /// the second read must name different refs. Erratum 0.1.1 section A.2.
    #[tokio::test]
    async fn hydration_names_the_typed_use_whose_read_actually_failed() {
        async fn hydrate(
            fault: Option<Fault>,
        ) -> (Result<(), StoreError>, usize, Vec<ArtifactRef>) {
            let clock = Arc::new(TestClock::new(Timestamp(5_000)));
            let inner = Arc::new(InMemoryObjectStore::new(clock.clone()));
            let objects = Arc::new(FaultObjectStore::new(inner.clone()));
            let store = open(clock, objects.clone()).await;
            let fixture = seed_revision(&store, inner.as_ref(), "typed-use").await;
            let revision = store
                .get_revision(
                    &fixture.scope,
                    &fixture.definition_id,
                    &fixture.revision_hash,
                )
                .await
                .unwrap();
            create_run(&store, inner.as_ref(), &fixture, "run", br#"{"value":1}"#)
                .await
                .unwrap();
            let permit = store
                .acquire_engine_claim(&fixture.scope, id("engine"))
                .await
                .unwrap()
                .permit;
            let aggregate = inner
                .put(&fixture.scope, b"[]", "application/json")
                .await
                .unwrap();
            // Arm after run creation so the counter covers exactly the reads
            // this one command performs.
            objects.arm(
                &fixture.schema_digest,
                fault.unwrap_or(Fault::CorruptOnRead(0)),
            );
            // complete_map hydrates the run's pinned action schemas before it
            // opens the transaction, so the command never has to be otherwise
            // valid to exercise hydration.
            let result = store
                .complete_map(
                    &fixture.scope,
                    CompleteMap {
                        permit,
                        run_id: id("run"),
                        map_node_id: id("action"),
                        expected_node_version: Version(1),
                        aggregate,
                    },
                )
                .await
                .map(|_| ());
            let refs = vec![
                revision.run_input_schema_ref.0.clone(),
                revision.action_pins[0].output_schema_ref.0.clone(),
            ];
            (result, objects.reads(&fixture.schema_digest), refs)
        }

        let (healthy, reads, refs) = hydrate(None).await;
        assert_ne!(
            refs[0], refs[1],
            "the fixture must pin one digest under two distinct typed uses"
        );
        assert_eq!(refs[0].digest, refs[1].digest);
        assert!(
            !matches!(healthy, Err(StoreError::CommittedObjectCorrupt { .. })),
            "an intact object is never a corruption"
        );
        assert_eq!(
            reads, 2,
            "each typed use is verified on its own; deduplicating to the bare digest reads once"
        );

        let (first, _, _) = hydrate(Some(Fault::CorruptOnRead(1))).await;
        let (second, _, _) = hydrate(Some(Fault::CorruptOnRead(2))).await;
        let first = match first {
            Err(StoreError::CommittedObjectCorrupt { bad_ref, .. }) => bad_ref,
            other => panic!("the first hydrated typed use failed: {other:?}"),
        };
        let second = match second {
            Err(StoreError::CommittedObjectCorrupt { bad_ref, .. }) => bad_ref,
            other => panic!("the second hydrated typed use failed: {other:?}"),
        };
        assert_ne!(
            first.artifact_ref_id, second.artifact_ref_id,
            "the reported ref tracks the read that failed, not the shared digest"
        );
        assert_eq!(first.digest, second.digest);
        // Compared by typed-use identity: each variant runs its own fresh store,
        // so only the derived identity is stable across them.
        let mut reported = [first.artifact_ref_id, second.artifact_ref_id];
        reported.sort();
        let mut expected = [
            refs[0].artifact_ref_id.clone(),
            refs[1].artifact_ref_id.clone(),
        ];
        expected.sort();
        assert_eq!(reported, expected);
    }

    /// Unavailable storage during hydration is never corruption.
    ///
    /// It mints no proof, it is not `CommittedObjectCorrupt`, and it authorizes
    /// no transition. Erratum 0.1.1 sections A.4 and C.1.
    #[tokio::test]
    async fn hydration_storage_unavailability_stays_unavailable() {
        let clock = Arc::new(TestClock::new(Timestamp(6_000)));
        let inner = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let objects = Arc::new(FaultObjectStore::new(inner.clone()));
        let store = open(clock, objects.clone()).await;
        let fixture = seed_revision(&store, inner.as_ref(), "hydration-unavailable").await;
        objects.arm(&fixture.schema_digest, Fault::Unavailable);

        let error = create_run(&store, inner.as_ref(), &fixture, "run", br#"{"value":1}"#)
            .await
            .expect_err("hydration cannot complete while storage is unavailable");

        assert!(matches!(error, StoreError::StorageUnavailable), "{error:?}");
        assert!(matches!(
            store.get_run(&fixture.scope, &id("run")).await,
            Err(StoreError::NotFound)
        ));
    }

    /// An infrastructure-class mark failure is also not applied corruption.
    ///
    /// The natural rejection path is covered on the reference store; this pins
    /// the transaction-failure path, where the command was legal and simply did
    /// not commit. Erratum 0.1.1 section B.2.
    #[cfg(feature = "conformance")]
    #[tokio::test]
    async fn transaction_failure_on_the_mark_is_not_applied_corruption() {
        use dagger_workflow_core::sqlite::SqliteCommitFault;

        let clock = Arc::new(TestClock::new(Timestamp(7_000)));
        let inner = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let objects = Arc::new(FaultObjectStore::new(inner.clone()));
        let store = Arc::new(open(clock, objects.clone()).await);
        let fixture = seed_revision(store.as_ref(), inner.as_ref(), "mark-transaction").await;
        let run = create_run(
            store.as_ref(),
            inner.as_ref(),
            &fixture,
            "run",
            br#"{"value":1}"#,
        )
        .await
        .unwrap();
        let bad_ref = run.input_ref.0.clone();
        assert!(inner.corrupt_bytes(&fixture.scope, &bad_ref.digest, b"tampered".to_vec()));
        store.inject_commit_fault_once(SqliteCommitFault::BeforeCommit);

        let reader = CommittedObjectReader::new(store.clone(), objects.clone());
        let outcome = reader
            .read(&fixture.scope, &run.run_id, &bad_ref, None)
            .await;
        let observed = store
            .get_run(&fixture.scope, &run.run_id)
            .await
            .unwrap()
            .run;

        let CommittedReadOutcome::CorruptionMarkFailed { error, .. } = outcome else {
            panic!("an uncommitted corruption command is never applied corruption");
        };
        assert!(matches!(error, StoreError::TransactionFailed), "{error:?}");
        assert_eq!(observed, run);
        assert_ne!(observed.status, RunState::CorruptStorage);
    }
}
