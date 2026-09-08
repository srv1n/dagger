//! Release-mode microbenchmarks for object-reference clones and stable IDs.
//!
//! Run the same file, toolchain, dependency lockfile, and machine on both refs:
//! cargo run -p dagger-workflow-core --release --example perf_core
//! These timings are not end-to-end workflow throughput measurements.

use dagger_workflow_core::artifact::{ArtifactKind, ObjectStore, VerifiedObjectRef};
use dagger_workflow_core::engine::TestClock;
use dagger_workflow_core::ids::{
    artifact_ref_id, edge_id, idempotency_key, map_child_id, map_child_idempotency_key,
    map_expansion_digest, ArtifactRefIdentity, Digest, Id, MapChildIdentity, Timestamp,
};
use dagger_workflow_core::memory::InMemoryObjectStore;
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use std::error::Error;
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn batch(iterations: u64, operation: &mut impl FnMut()) -> Duration {
    let start = Instant::now();
    for _ in 0..iterations {
        operation();
    }
    start.elapsed()
}

fn measure(label: &str, mut operation: impl FnMut()) {
    // Calibrate outside the measured samples. Small inputs need more iterations
    // to avoid timer noise. The same calibration is used on both code versions.
    let mut iterations = 1_u64;
    while batch(iterations, &mut operation) < Duration::from_millis(50) {
        if iterations >= 1 << 24 {
            break;
        }
        iterations *= 2;
    }
    let mut samples = [0.0_f64; 7];
    for sample in &mut samples {
        *sample = batch(iterations, &mut operation).as_secs_f64() * 1e9 / iterations as f64;
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "{label}: median={:.1} ns/op min={:.1} max={:.1} iterations={iterations}",
        samples[3], samples[0], samples[6]
    );
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    if cfg!(debug_assertions) {
        return Err("use --release for comparable timings".into());
    }
    let scope = ExecutionScope {
        tenant_id: ScopeAtom::new("bench-tenant")?,
        namespace: ScopeAtom::new("bench")?,
    };
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let store = InMemoryObjectStore::new(clock);
    for size in [0, 64, 1024, 1024 * 1024, 4 * 1024 * 1024] {
        let bytes = vec![42; size];
        let reference = store
            .put(&scope, &bytes, "application/octet-stream")
            .await?;
        measure(&format!("reference_clone/{size}_bytes"), || {
            black_box(VerifiedObjectRef::clone(black_box(&reference)));
        });
    }

    let run = Id::new("bench-run")?;
    let parent = Id::new("bench-map")?;
    let node = Id::new("bench-node")?;
    let attempt = Id::new("bench-attempt")?;
    let digest = Digest::new(format!("sha256:{}", "ab".repeat(32)))?;
    measure("edge_id", || {
        black_box(edge_id(black_box(&digest), &parent, "next/0", &node));
    });
    measure("artifact_ref_id", || {
        black_box(artifact_ref_id(black_box(ArtifactRefIdentity {
            scope: &scope,
            digest: &digest,
            kind: ArtifactKind::ActionArtifact,
            producer_run_id: Some(&run),
            producer_node_id: Some(&node),
            producer_attempt_id: Some(&attempt),
            ordinal: 7,
        })));
    });
    measure("idempotency_key", || {
        black_box(idempotency_key(black_box(&scope), &run, &node));
    });
    measure("map_child_id", || {
        black_box(map_child_id(black_box(&run), &parent, 7, &digest));
    });
    measure("map_child_idempotency_key", || {
        black_box(map_child_idempotency_key(
            black_box(&scope),
            &run,
            &node,
            &parent,
            7,
            &digest,
        ));
    });
    for count in [0, 1, 100, 1000, 10_000] {
        let children: Vec<_> = (0..count)
            .map(|index| MapChildIdentity {
                item_index: index,
                item_digest: digest.clone(),
                child_id: map_child_id(&run, &parent, index, &digest),
            })
            .collect();
        measure(&format!("map_expansion/{count}_children"), || {
            black_box(map_expansion_digest(black_box(&children)));
        });
    }
    Ok(())
}
