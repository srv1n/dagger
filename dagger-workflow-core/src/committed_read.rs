//! Crate-owned reader for committed objects held by an existing run.
//!
//!.

use crate::artifact::{ArtifactRef, FailedReadProof, ObjectReadError, ObjectStore, VerifiedObject};
use crate::ids::{Id, NodeInstanceId};
use crate::scope::ExecutionScope;
use crate::store::{MarkCorruptStorage, StoreError, WorkflowStore};
use std::sync::Arc;

/// The four distinguishable results of reading one committed object.
///
/// The variants are not interchangeable, and in particular the two failure
/// variants that both originate from `ObjectReadError::Corrupt` differ in
/// whether the control plane has already been told.
#[derive(Debug)]
pub enum CommittedReadOutcome {
    /// Read completed and the bytes were verified against the committed digest.
    Verified(VerifiedObject),
    /// The store could not complete a verification. No proof, no durable change.
    ///
    /// Nothing was mutated, so a later operational retry may repeat the read.
    StorageUnavailable,
    /// Integrity failure whose `mark_corrupt_storage` command is already committed.
    ///
    /// The run has been durably moved to CorruptStorage before this value was
    /// produced, so a caller may treat the run output as invalidated.
    CorruptionApplied {
        /// The proof minted by the failed read, retained for diagnostics.
        proof: FailedReadProof,
    },
    /// Integrity failure whose `mark_corrupt_storage` command did NOT commit.
    ///
    /// The object is corrupt but the control plane still says otherwise. This is
    /// deliberately distinct from `CorruptionApplied`: reporting the integrity
    /// result here would falsely imply the run output had already been
    /// invalidated. Callers must surface the mark failure, not the integrity
    /// failure.
    CorruptionMarkFailed {
        /// The proof minted by the failed read, still unconsumed.
        proof: FailedReadProof,
        /// Why the control-plane command failed.
        error: StoreError,
    },
}

/// Reads committed objects on behalf of an existing run.
///
/// Low-level `ObjectStore::get` is NOT sufficient by itself for a host read
/// performed on behalf of a run: on corruption it mints a proof and returns,
/// leaving the run claiming a healthy output that no longer exists. Every host
/// read of a committed ref belonging to a run must go through this type, which
/// commits `mark_corrupt_storage` before it reports the integrity failure.
/// A host read needs no action registry, scheduler claim, or scheduling
/// lifecycle, so this reader deliberately depends on nothing but the two
/// stores.
pub struct CommittedObjectReader<S, O>
where
    S: WorkflowStore,
    O: ObjectStore,
{
    store: Arc<S>,
    object_store: Arc<O>,
}

impl<S, O> CommittedObjectReader<S, O>
where
    S: WorkflowStore,
    O: ObjectStore,
{
    /// Constructs a reader over one control plane and its object store.
    pub fn new(store: Arc<S>, object_store: Arc<O>) -> Self {
        Self {
            store,
            object_store,
        }
    }

    /// Reads one committed ref of an existing run in the fixed contract order.
    ///
    /// The order is: read; return verified bytes; propagate unavailability
    /// having mutated nothing; on corruption submit `mark_corrupt_storage` with
    /// the exact committed ref and the original proof, await the control-plane
    /// commit, and only then report the integrity failure. `owner_node_id` is
    /// the node the reducer accepts for this ref, which is the producing node
    /// unless aggregation supplies the waiting Map parent explicitly.
    pub async fn read(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
        committed: &ArtifactRef,
        owner_node_id: Option<&NodeInstanceId>,
    ) -> CommittedReadOutcome {
        let proof = match self.object_store.get(scope, &committed.digest).await {
            Ok(object) => return CommittedReadOutcome::Verified(object),
            Err(ObjectReadError::StorageUnavailable) => {
                return CommittedReadOutcome::StorageUnavailable
            }
            Err(ObjectReadError::Corrupt(proof)) => proof,
        };
        match self
            .store
            .mark_corrupt_storage(
                scope,
                MarkCorruptStorage {
                    run_id: run_id.clone(),
                    bad_ref: committed.clone(),
                    proof: proof.clone(),
                    owner_node_id: owner_node_id
                        .cloned()
                        .or_else(|| committed.producer_node_id.clone()),
                },
            )
            .await
        {
            Ok(_) => CommittedReadOutcome::CorruptionApplied { proof },
            Err(error) => CommittedReadOutcome::CorruptionMarkFailed { proof, error },
        }
    }
}
