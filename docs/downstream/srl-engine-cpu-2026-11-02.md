# Dagger engine CPU: response to SRL's 2026-11-02 report

For: the Song Rendition Lab (SRL) team. From: Dagger. Date: 2026-11-02.
Your report: `docs/upstream/dagger-engine-cpu-2026-11-02.md` in the SRL repo (Dagger `4b7f768`).
Dagger branch: `perf/engine-sqlite-overhead`, on top of `4b7f768`. Not merged yet.

## Summary

All five problems in your report are fixed or reduced inside Dagger. On your v7b song
(86 nodes, Mac, release builds of `srl_host`, one run each, all runs
`delivery_candidate_ready`), measured at the branch head:

| | Dagger `4b7f768` | Branch | Change |
|---|---|---|---|
| Cold song, `srl_host` CPU | 142.9 s | 22.1 s | -85% |
| Cold song, wall | 203.7 s | 131.3 s | -36% |
| Fully cached rerun, wall | 29.7 s | 15.3 s | -48% |
| Fully cached rerun, `srl_host` CPU | 21.9 s | 7.8 s | -64% |

The Mac was shared and loaded (load average 7 to 80 during the day), so treat single runs
as rough. An earlier run on this branch at `df43e5b` measured 82.4 s vs 25.8 s cold host CPU
and 56.7 s vs 16.4 s cached wall. Each pair ran back to back under the same load. On
both days the branch cut host CPU by more than two thirds. The cold wall-time gain
comes from the engine no longer competing with your actions for CPU.

## Your five points

1. **`monitor_attempt` read the full run 4 times a second per attempt.** Fixed. It now
   calls `WorkflowStore::get_run_record`, a one-row read (0.14 ms vs 7.3 ms for `get_run` on
   an 85-node run). `start`, the per-claim definition lookup, pinning and approvals use it too.
2. **Every `tick()` wrote the engine claim.** Fixed. `tick()` refreshes the claim only when
   one is due, at most every 5 s per scope, shared with the attempt monitors.
3. **Scheduling ticks were expensive.** Reduced. Root causes, all fixed:
   - Each SQLite command re-encoded the whole loaded run as canonical JSON twice to find the
     changed rows. It now encodes once and reuses stored text for unchanged rows.
   - Each command loaded the run's full event log and budget ledger. Commands now skip them.
   - Each claim re-read and re-parsed the workflow revision (about 435 KB of JSON on your
     song). The engine now keeps the parsed definition per revision.
   - Writers in one process slept in SQLite's busy handler. They now queue in-process.
   - Each object read fsynced two directories. Synced objects now skip it. On Linux with a
     real disk this saves milliseconds per read; under `eatmydata` you will not see it.
4. **No public way to wait for engine activity.** Added `WorkflowEngine::activity()`, a
   `tokio::sync::watch::Receiver<u64>` that changes after each local attempt finishes.
5. **Cache hits resolved serially at about 1 s each.** Fixed. A cache hit (or pinned
   output) no longer spawns an attempt task. The scheduler claims it and accepts its outcome
   in one store command (`claim_and_complete_reused`, one SQLite transaction), then rescans
   in the same `tick`. A fully cached run now finishes in one `tick` call. Re-publishing an
   object that already exists also no longer fsyncs its directories.

## What SRL should change

These are your calls. Dagger does not depend on any of them.

- **Bump the pin** in `native/srl_host/Cargo.toml` to the merge commit once the branch
  lands. No Dagger API you use was removed.
- **Replace the host loop's sleep and polling with `activity()`.** Your loop waits 150 ms
  after a subprocess exit for the outcome to commit. `activity()` changes only after the
  commit, so the wait and the guess go away:

  ```rust
  let mut activity = engine.activity(); // subscribe before the first tick
  loop {
      engine.tick(&scope).await?;
      if store.get_run_record(&scope, &run_id).await?.status.is_terminal() {
          break;
      }
      // The timeout covers retry backoff and deadlines, which no attempt signals.
      let _ = tokio::time::timeout(Duration::from_secs(1), activity.changed()).await;
  }
  ```

- **Use `get_run_record` instead of `get_run` for status checks.** `get_run` builds the full
  operational view and costs about 50 times more.
- **Your per-action `Notify` on subprocess exit can go.** `activity()` covers it.

## Compatibility

- **Existing stores keep working.** No SQLite schema change. Some stored fields and event
  payload fields are no longer written (digests that nothing read). Old rows still load,
  because row decoding ignores unknown fields, and events are only validated when written.
- **Event payloads changed.** These fields are gone if anything of yours reads events:
  `binding_derivation_digest`, `corrupt_proof_fingerprint`, `store_instance_nonce_digest`,
  `authorization_policy_digest`, `compatibility_evidence_digest`,
  `required_semantic_digest`, `available_semantic_digest`, and `submitted_payload_digest`
  on stale-completion events. Nothing in `native/srl_host` reads them.
- **`InMemoryStore` is now the same reducer the SQLite store uses.** If you use it in tests,
  claim renewal now keeps the lifetime agreed at claim time, as SQLite already did.

## Not done yet

- **Per-run state cache.** Every SQLite command still loads and decodes the run's working
  rows. After the cache-hit changes it is about half of the scheduler's remaining CPU on a
  cached rerun. The design is in Dagger's
  `docs/system/proposals/sqlite-run-state-cache.md`. It needs a schema migration, so it is
  waiting on a decision.
- **Cache hits wait for a free slot to be looked up.** A cache hit holds a concurrency slot
  only while the scheduler resolves it. When every slot runs a long action, ready cache hits
  wait until one frees.

## How we measured

- Engine only: `cargo run -p dagger-workflow-core --release --features sqlite --example
  engine_cpu` (85-node fan-in DAG with SRL's shape; actions only sleep).
  `scripts/ab_engine_cpu.sh OLD NEW` alternates two builds for a fair A/B.
- Your song: `scripts/render_speed/dagger_host_ab.sh` in the SRL repo builds nothing; it runs
  one plan through two `srl_host` binaries, cold and fully cached, and records wall time
  and `srl_host` CPU. We added this file to your repo while measuring. It is untracked; keep
  it or delete it.
- The patched `srl_host` was built without editing your files, with
  `cargo build --config "patch.'https://github.com/srv1n/dagger'.dagger-workflow-core.path='<dagger>/dagger-workflow-core'"`.
  That command rewrites `native/srl_host/Cargo.lock`; we restored it after each build.
