# Dagger engine performance changes, November 2026

For: every team that embeds `dagger-workflow-core`. From: Dagger. Date: 2026-11-03.
Merged to `main` from `perf/engine-sqlite-overhead` (base `4b7f768`).

## Summary

The engine now uses much less CPU per run, mostly in the SQLite store and the scheduler.
Cache hits finish in the scheduler pass instead of each spawning a task. You do not have
to change any code to get this. Bump your pin to `main`.

One real workload (an 86-node DAG on a Mac, release builds, one run each):

| | Before | After | Change |
|---|---|---|---|
| Cold run, host process CPU | 142.9 s | 22.1 s | -85% |
| Cold run, wall | 203.7 s | 131.3 s | -36% |
| Fully cached rerun, wall | 29.7 s | 15.3 s | -48% |
| Fully cached rerun, host process CPU | 21.9 s | 7.8 s | -64% |

The machine was loaded, so treat single runs as rough. Your gain depends on how much of
your run time was engine overhead. DAGs with many nodes, short actions or many cache hits
gain the most. Cold wall time improves because the engine no longer competes with your
actions for CPU.

## What changed

- **Cache hits and pinned outputs run no task.** The scheduler claims the node and accepts
  the stored outcome in one store command (`claim_and_complete_reused`, one SQLite
  transaction), then rescans in the same `tick`. A fully cached run finishes in one `tick`.
- **Cheaper SQLite commands.** Each command encodes the run once instead of twice, skips
  the event log and budget ledger it does not need, and reuses stored text for unchanged
  rows.
- **Workflow definitions are parsed once per revision**, not on every claim.
- **Writers in one process queue in-process** instead of sleeping in SQLite's busy handler.
- **Attempt monitors read one row.** They call `get_run_record` instead of loading the
  whole run four times a second.
- **`tick()` refreshes the engine claim only when one is due**, at most every 5 s per
  scope.
- **The filesystem object store syncs each object's directories once per process.**
  Reading or re-publishing an object that is already durable skips the fsync.

## What you can change

All optional. Your code keeps working without them.

- **Wait on `activity()` instead of sleeping between ticks.** `WorkflowEngine::activity()`
  returns a `tokio::sync::watch::Receiver<u64>` that changes after each local attempt's
  outcome commits. Subscribe before the first `tick`:

  ```rust
  let mut activity = engine.activity();
  loop {
      engine.tick(&scope).await?;
      if store.get_run_record(&scope, &run_id).await?.status.is_terminal() {
          break;
      }
      // The timeout covers retry backoff and deadlines, which no attempt signals.
      let _ = tokio::time::timeout(Duration::from_secs(1), activity.changed()).await;
  }
  ```

  This replaces fixed sleeps and any per-action "done" signals you built yourself.
- **Use `get_run_record` for status checks.** `get_run` builds the full operational view
  and costs about 50 times more on an 85-node run (7.3 ms vs 0.14 ms).

## If you implement `WorkflowStore` yourself

Two new trait methods have defaults, so your store still compiles and behaves the same:

- `get_run_record` defaults to `get_run(...).run`. Override it with a one-row read; the
  engine now calls it often.
- `claim_and_complete_reused` defaults to `claim_reusable_node_attempt` followed by
  `complete_attempt`. Override it to run both in one transaction if your store has them.

## Compatibility

- **No SQLite schema change.** Existing stores open and run as before.
- **Some event payload fields are gone.** They were digests nothing read:
  `binding_derivation_digest`, `corrupt_proof_fingerprint`, `store_instance_nonce_digest`,
  `authorization_policy_digest`, `compatibility_evidence_digest`,
  `required_semantic_digest`, `available_semantic_digest`, and `submitted_payload_digest` on
  stale-completion events. Old rows still load. Check your code only if you parse events.
- **`InMemoryStore` now runs the same reducer as the SQLite store.** In tests, claim
  renewal keeps the lifetime agreed at claim time, as SQLite already did.
- **Cache hits finish inside `tick`.** Your action code still does not run on a hit, as
  before, but a hit no longer goes through a spawned attempt task. If a test waited for
  cache hits to finish after `tick` returned, they are now already done.

## Known limits

- **Every SQLite command still loads the run's working rows.** That is about half of the
  remaining scheduler CPU on a cached rerun. A per-run state cache would remove it but
  needs a schema migration. The design is in `docs/system/proposals/sqlite-run-state-cache.md`.
- **Cache hits need a free concurrency slot to be resolved.** The slot is held only for
  the lookup. When every slot runs a long action, ready cache hits wait until one frees.

## Measuring your own workload

- `cargo run -p dagger-workflow-core --release --features sqlite --example engine_cpu`
  runs an 85-node fan-in DAG whose actions only sleep, so it measures engine overhead.
- `scripts/ab_engine_cpu.sh OLD NEW` alternates two builds of that example for a fair A/B.
- To try a Dagger checkout in your project, build with a path patch instead of
  editing your manifest:
  `cargo build --config "patch.'https://github.com/srv1n/dagger'.dagger-workflow-core.path='<dagger>/dagger-workflow-core'"`.
  This rewrites your `Cargo.lock`; restore it afterwards.
