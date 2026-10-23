#![cfg(feature = "sqlite")]

use dagger_workflow_core::engine::TestClock;
use dagger_workflow_core::ids::{Id, Timestamp};
use dagger_workflow_core::memory::InMemoryObjectStore;
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::sqlite::SqliteWorkflowStore;
use dagger_workflow_core::store::{StoreError, WorkflowStore};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn configured_lease_renews_persisted_window_and_preserves_takeover_fencing() {
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = SqliteWorkflowStore::open_url("sqlite::memory:", clock.clone(), objects.clone())
        .await
        .unwrap();
    let scope = ExecutionScope {
        tenant_id: ScopeAtom::new("host-lease").unwrap(),
        namespace: ScopeAtom::new("demo").unwrap(),
    };
    let initial = store
        .acquire_engine_claim(&scope, Id::new("default").unwrap())
        .await
        .unwrap();
    assert_eq!(
        initial.claim.expires_at.0 - initial.claim.heartbeat_at.0,
        20_000
    );
    store
        .release_engine_claim(&scope, &initial.permit)
        .await
        .unwrap();

    let store = store
        .with_engine_claim_lifetime(Duration::from_secs(120))
        .unwrap();
    let acquired = store
        .acquire_engine_claim(&scope, Id::new("slow-disk-host").unwrap())
        .await
        .unwrap();
    assert_eq!(
        acquired.claim.expires_at.0 - acquired.claim.heartbeat_at.0,
        120_000
    );
    // Reopening through the old default must not shorten an accepted lease.
    let reopened = SqliteWorkflowStore::from_pool(store.pool().clone(), clock, objects)
        .await
        .unwrap();
    reopened.advance_database_clock_ms(30_001).await.unwrap();
    let heartbeat = reopened
        .heartbeat_engine_claim(&scope, &acquired.permit)
        .await
        .unwrap();
    assert_eq!(heartbeat.expires_at.0 - heartbeat.heartbeat_at.0, 120_000);
    assert!(matches!(
        reopened
            .acquire_engine_claim(&scope, Id::new("too-early").unwrap())
            .await,
        Err(StoreError::EngineAlreadyLive { .. })
    ));

    reopened.advance_database_clock_ms(120_001).await.unwrap();
    assert!(matches!(
        reopened
            .heartbeat_engine_claim(&scope, &acquired.permit)
            .await,
        Err(StoreError::EngineClaimExpired)
    ));
    let takeover = reopened
        .acquire_engine_claim(&scope, Id::new("replacement").unwrap())
        .await
        .unwrap();
    assert_eq!(takeover.claim.generation, acquired.claim.generation + 1);
    assert_eq!(
        takeover.claim.expires_at.0 - takeover.claim.heartbeat_at.0,
        20_000
    );
    assert!(matches!(
        reopened
            .heartbeat_engine_claim(&scope, &acquired.permit)
            .await,
        Err(StoreError::EngineClaimLost)
    ));
    assert!(matches!(
        reopened.with_engine_claim_lifetime(Duration::ZERO),
        Err(StoreError::InvalidField)
    ));
}
