use dagger_workflow_core::artifact::{FailedReadClass, ObjectStore};
use dagger_workflow_core::engine::{Clock, TestClock};
use dagger_workflow_core::ids::{Id, Timestamp};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::run::{OperationalPhase, RunOperationalCounts};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::store::{StoreError, WorkflowStore};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread;

fn scope(tenant: &str) -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new(tenant).unwrap(),
        namespace: ScopeAtom::new("workflow").unwrap(),
    }
}

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
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

#[test]
fn object_store_is_content_addressed_and_scope_confined() {
    block_on(async {
        let clock = Arc::new(TestClock::new(Timestamp(100)));
        let store = InMemoryObjectStore::new(clock);
        let a = scope("tenant-a");
        let b = scope("tenant-b");
        let first = store.put(&a, b"same", "application/json").await.unwrap();
        let replay = store
            .publish_if_absent(&a, b"same", "application/json")
            .await
            .unwrap();
        let other_scope = store.put(&b, b"same", "application/json").await.unwrap();
        assert_eq!(first, replay);
        assert_eq!(first.digest(), other_scope.digest());
        assert_ne!(first.scope(), other_scope.scope());
        assert_eq!(store.get(&a, first.digest()).await.unwrap().bytes, b"same");
        let foreign = InMemoryObjectStore::new(Arc::new(TestClock::new(Timestamp(100))));
        let foreign_ref = foreign.put(&a, b"same", "application/json").await.unwrap();
        assert_ne!(first, foreign_ref);
    });
}

#[test]
fn object_store_mints_closed_failed_read_proofs() {
    block_on(async {
        let clock = Arc::new(TestClock::new(Timestamp(200)));
        let store = InMemoryObjectStore::new(clock);
        let execution_scope = scope("tenant-a");
        let missing =
            dagger_workflow_core::ids::Digest::new(format!("sha256:{}", "0".repeat(64))).unwrap();
        let error = store.get(&execution_scope, &missing).await.unwrap_err();
        assert_eq!(error.proof.error_class(), FailedReadClass::Missing);
    });
}

#[test]
fn singleton_claim_rejects_live_peer_and_increments_takeover_generation() {
    block_on(async {
        let clock = Arc::new(TestClock::new(Timestamp(1_000)));
        let store = InMemoryStore::new(clock.clone());
        let execution_scope = scope("tenant-a");
        let first = store
            .acquire_engine_claim(&execution_scope, id("engine-a"))
            .await
            .unwrap();
        assert_eq!(first.claim.control_plane_id, "scheduler");
        assert!(matches!(
            store
                .acquire_engine_claim(&execution_scope, id("engine-b"))
                .await,
            Err(StoreError::EngineAlreadyLive { .. })
        ));
        clock.advance_ms(20_000).unwrap();
        let takeover = store
            .acquire_engine_claim(&execution_scope, id("engine-b"))
            .await
            .unwrap();
        assert_eq!(takeover.claim.generation, first.claim.generation + 1);
        assert!(matches!(
            store
                .heartbeat_engine_claim(&execution_scope, &first.permit)
                .await,
            Err(StoreError::EngineClaimLost)
        ));
    });
}

#[test]
fn operational_phase_distinguishes_budget_waiting_and_mixed() {
    let budget_only = RunOperationalCounts {
        ready: 0,
        running_attempts: 0,
        budget_waiting: 2,
        pending_approvals: 0,
        retry_waiting: 0,
        maps_waiting_children: 0,
    };
    assert_eq!(budget_only.phase(), Some(OperationalPhase::AwaitingBudget));
    let mixed = RunOperationalCounts {
        ready: 1,
        retry_waiting: 1,
        ..budget_only
    };
    assert_eq!(mixed.phase(), Some(OperationalPhase::Mixed));
}

#[test]
fn virtual_clock_has_no_wall_time_dependency() {
    let clock = TestClock::new(Timestamp(-10));
    assert_eq!(clock.now(), Timestamp(-10));
    assert_eq!(clock.advance_ms(25).unwrap(), Timestamp(15));
    clock.set(Timestamp(7));
    assert_eq!(clock.now(), Timestamp(7));
    clock.set(Timestamp(-20));
    assert_eq!(clock.now(), Timestamp(-20));
}

#[cfg(feature = "conformance")]
#[test]
fn reusable_conformance_suite_runs_against_memory_adapter() {
    struct Adapter {
        clock: Arc<TestClock>,
        store: InMemoryStore<TestClock>,
        objects: InMemoryObjectStore<TestClock>,
    }

    impl dagger_workflow_core::conformance::ConformanceAdapter for Adapter {
        type Store = InMemoryStore<TestClock>;
        type Objects = InMemoryObjectStore<TestClock>;

        fn store(&self) -> &Self::Store {
            &self.store
        }

        fn objects(&self) -> &Self::Objects {
            &self.objects
        }

        fn advance_clock_ms(&self, milliseconds: i64) {
            self.clock.advance_ms(milliseconds).unwrap();
        }
    }

    block_on(async {
        let clock = Arc::new(TestClock::new(Timestamp(0)));
        let adapter = Adapter {
            store: InMemoryStore::new(clock.clone()),
            objects: InMemoryObjectStore::new(clock.clone()),
            clock,
        };
        assert_eq!(
            dagger_workflow_core::conformance::run_conformance(
                &adapter,
                &scope("tenant-a"),
                &scope("tenant-b"),
            )
            .await
            .unwrap(),
            dagger_workflow_core::conformance::CASE_COUNT
        );
        assert_eq!(
            dagger_workflow_core::conformance::CASE_NAMES
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            dagger_workflow_core::conformance::CASE_COUNT
        );
    });
}
