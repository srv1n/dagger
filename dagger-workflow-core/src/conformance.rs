//! Adapter-neutral black-box checks shared by volatile and durable stores.

use crate::artifact::{ObjectStore, ObjectStoreError};
use crate::ids::{Digest, Id};
use crate::scope::ExecutionScope;
use crate::store::{StoreError, WorkflowStore};

/// Number of independent assertions in the current adapter-neutral suite.
pub const CASE_COUNT: usize = 12;

/// Supplies one isolated store pair and control over its database clock.
pub trait ConformanceAdapter {
    /// Workflow store implementation under test.
    type Store: WorkflowStore;
    /// Object store implementation under test.
    type Objects: ObjectStore;

    /// Returns the workflow store.
    fn store(&self) -> &Self::Store;
    /// Returns the object store.
    fn objects(&self) -> &Self::Objects;
    /// Advances the adapter's database-equivalent clock.
    fn advance_clock_ms(&self, milliseconds: i64);
}

/// A conformance-suite failure naming the first violated case.
#[derive(Debug, thiserror::Error)]
#[error("conformance case {case} failed: {detail}")]
pub struct ConformanceFailure {
    /// Stable one-based case number.
    pub case: usize,
    /// Compact failure detail.
    pub detail: &'static str,
}

/// Runs the adapter-neutral object and singleton-claim cases.
///
/// Runtime command cases are intentionally exercised by adapter-specific
/// fixtures built through the same public `WorkflowStore` trait; this entry
/// point covers the prerequisites that require no published workflow fixture.
pub async fn run_conformance<A: ConformanceAdapter>(
    adapter: &A,
    scope_a: &ExecutionScope,
    scope_b: &ExecutionScope,
) -> Result<usize, ConformanceFailure> {
    let bytes = br#"{"same":true}"#;
    let a = adapter
        .objects()
        .put(scope_a, bytes, "application/json")
        .await
        .map_err(|_| failure(1, "object publication failed"))?;
    let b = adapter
        .objects()
        .put(scope_b, bytes, "application/json")
        .await
        .map_err(|_| failure(2, "second-scope publication failed"))?;
    if a.digest() != b.digest() {
        return Err(failure(3, "equal bytes did not share a digest"));
    }
    if a.scope() == b.scope() {
        return Err(failure(4, "verified refs were not scope-bound"));
    }
    let read_a = adapter
        .objects()
        .get(scope_a, a.digest())
        .await
        .map_err(|_| failure(5, "verified read failed"))?;
    if read_a.bytes != bytes {
        return Err(failure(6, "verified read changed bytes"));
    }
    let missing = Digest::new(format!("sha256:{}", "0".repeat(64))).expect("fixture digest");
    if adapter.objects().get(scope_a, &missing).await.is_ok() {
        return Err(failure(7, "missing read did not mint a failure"));
    }
    let first = adapter
        .store()
        .acquire_engine_claim(scope_a, id("engine-a"))
        .await
        .map_err(|_| failure(8, "first engine claim failed"))?;
    if !matches!(
        adapter
            .store()
            .acquire_engine_claim(scope_a, id("engine-b"))
            .await,
        Err(StoreError::EngineAlreadyLive { .. })
    ) {
        return Err(failure(9, "second live engine was accepted"));
    }
    adapter
        .store()
        .acquire_engine_claim(scope_b, id("engine-b"))
        .await
        .map_err(|_| failure(10, "claim leaked across scopes"))?;
    adapter.advance_clock_ms(20_000);
    let takeover = adapter
        .store()
        .acquire_engine_claim(scope_a, id("engine-c"))
        .await
        .map_err(|_| failure(11, "expired takeover failed"))?;
    if takeover.claim.generation != first.claim.generation + 1 {
        return Err(failure(12, "takeover generation did not increment"));
    }
    Ok(CASE_COUNT)
}

fn id(value: &str) -> Id {
    Id::new(value).expect("conformance IDs are valid")
}

fn failure(case: usize, detail: &'static str) -> ConformanceFailure {
    ConformanceFailure { case, detail }
}

#[allow(dead_code)]
fn _closed_object_error(error: ObjectStoreError) -> ObjectStoreError {
    error
}
