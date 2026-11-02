# Proposal: per-run state cache and one writer per process

Status: proposal. Date: 2026-11-02. Prior art: `engine-persistence-prior-art.md`.

## Problem

Each SQLite command opens `BEGIN IMMEDIATE`, loads every working row of the run, decodes it,
applies the reducer, diffs and writes the changed rows. Commands no longer load event or
ledger history, but they still load and decode all nodes, edges, attempts, invocations,
artifacts and the revision. On an 86-node run that is about 1 MB of JSON per command. The
write lock is held for the whole load. Writers in the same process wait in SQLite's busy
handler, which sleeps in steps of up to 100 ms.

## What Temporal does

- One host owns a shard. Ownership is fenced by a generation (`rangeID`).
- The host keeps decoded mutable state per run in an LRU cache.
- A per-run lock covers load, apply and persist.
- The write is conditional on a per-run version. The write is the check; there is no
  "is my cache fresh" read.
- On any write error the cache entry is cleared (`Context.Clear()`), and the next caller
  reloads from the database. A dirty entry at release clears the cache and panics.

Dagger already has the matching pieces: the engine claim is the shard lock with a
generation, and authority rows already carry per-row versions.

## Design

### 1. One writer per store (done in `cffaa96`)

`SqliteWorkflowStore` has a `tokio::sync::Mutex<()>`. Every write transaction takes it
before `BEGIN IMMEDIATE` and holds it until commit or rollback. Writers in this process queue
in order instead of polling SQLite's lock. `busy_timeout` stays for other processes.

### 2. Per-run state cache (not built yet)

Measured value: after the other changes on this branch, `load_state` is about 39% of the
engine's remaining CPU on the 85-node synthetic run (`examples/engine_cpu.rs`).

- Key: `(scope, run_id)`. Value: the decoded run-scoped part of `LoadedAuthority` (state
  plus stored row text and versions) and the run generation it was built from.
- Validity needs a value that changes on every write to the run, from any process. Two
  options were checked:
  - `PRAGMA data_version` does not work here. It is per connection, and the cancel-safe
    `BEGIN` (`begin_owned`) moves an owned pooled connection into a task, so writes do not
    stay on one connection.
  - A run generation does. Add `dagger_workflow_run_generations(tenant_id, namespace,
    run_id, generation)` (schema v4). `persist_all` increments it in the same transaction for
    every run whose rows changed. A command reads it with one point query under the writer
    lock and uses the cache only when it matches.
- Claims, the scope counter, receipts, and per-command object and artifact rows stay
  uncached and load as they do now; they are small.
- Hit: skip loading and decoding. Apply the reducer to a clone of the cached state.
- Commit succeeds: replace the entry with the new state and versions.
- Any error, rollback, CAS conflict, claim loss or panic: remove the entry. The next command
  loads from SQLite.
- Bound: LRU by run count and approximate bytes, so a many-tenant server cannot grow it
  without limit.
- Reads (`get_run`, `list_events_after`) do not use the cache. They stay plain SQL or full
  loads.

### 3. Immutable revisions

The engine already caches parsed definitions per revision (`cffaa96`), so claims no longer
read the revision row. Commands that load a run still decode it. With the run cache, keep the
decoded revision with the entry; it never changes.

## Correctness checks

- The conformance, SQLite, recovery and crash-model suites stay green.
- A differential test runs random command sequences against a store with the cache and one
  without, and requires identical authority rows byte for byte after each command.
- A fault test drops the cache entry mid-sequence, forces CAS conflicts, and changes the claim
  generation; every case must reload and produce the same rows.
- SRL A/B, cold and cached, with `scripts/render_speed/dagger_host_ab.sh`.

## Not doing

- Writing events outside the state transaction, as Temporal does. Keeping one transaction is
  simpler and crash-safe here.
- Moving rules into SQL. That would split the reducer into two sources of truth.
- Batching several commands into one transaction. Revisit after this lands; Hatchet's 10 ms
  write buffer shows it is the next throughput lever.
