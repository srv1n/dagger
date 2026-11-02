# Engine persistence prior art

Date: 2026-11-02. Question: how do mature durable workflow engines avoid
reloading full run state on every state transition?

Evidence labels: **[src]** read in source or official doc this session.
**[doc]** official docs, high level. **[unverified]** secondary source or not found.

## Summary (what transfers to Dagger)

- Temporal splits state in two. Mutable state is a small summary row. History is append-only
  and is written but not read on the hot path. This is plan item (a), proven at scale.
- Temporal caches decoded mutable state in-process per run. The cache is valid because one
  process owns the shard (fenced by rangeID) and every write is conditional on a per-run
  counter. Plan item (b) matches this. Dagger's engine claim plays the role of the shard lock.
- On ANY write error Temporal drops the cached state and reloads. It also panics if the
  cached state is dirty at release. Do the same: clear the cache entry on every error, not
  only on CAS failure.
- Temporal holds a per-run in-process lock around load, apply and persist. Writers to one
  run queue in memory, not in the database. This supports plan item (c), scoped per run.
- Temporal writes history BEFORE the conditional state update, outside the state transaction.
  Orphan history is harmless because state decides what is visible. Dagger could append events
  after the state commit only if it accepts a gap on crash, so keep events in the same tx.
- Hatchet batches writes behind a 10 ms buffer. Throughput comes from batching many
  transitions per transaction, not from making one transition cheaper. Supports (d).
- Restate treats its state store as a rebuildable cache of a log, with single-leader epoch
  fencing. Same shape as our authority rows plus cache.
- Inngest replays with the full prior state on every step. That is the O(n^2) pattern and it
  is capped by hard limits (1,000 steps, 32 MB, unverified). Do not copy it.
- SQLite: use BEGIN IMMEDIATE and a single in-process writer. busy_timeout does not cover
  lock upgrade from a read transaction.

## Temporal (and Cadence)

### Mutable state vs history

- History events are an append-only record per run. Mutable state is the current summary:
  pending activities, timers, child workflows, and the id of the latest event it reflects
  (`NextEventID`). [src: history-service.md]
- History is not replayed to rebuild state per request. Mutable state is loaded from
  persistence when needed. [src: history-service.md]
- Mutable state is stored in the `executions` row (blob plus `next_event_id` plus
  `db_record_version`). [src: common/persistence/sql/execution.go]

### Cache and validity

- `workflow/cache/cache.go` is a host-level LRU keyed by (namespace, workflow, run).
  Size and TTL come from config. Default `history.cacheTTL` is 1 hour. [src: dynamicconfig/constants.go]
- Each cache entry is a `WorkflowContext` holding the decoded `MutableState` and a lock.
  `LoadMutableState` hits the DB only when `c.MutableState == nil`. [src: context.go L426-L470]
- The caller takes the per-run lock (`lockWorkflowExecution`), then loads, applies, persists,
  then releases. [src: cache.go L332-L360]
- Release function (`makeReleaseFunc`): on error or panic it calls `wfContext.Clear()`, which
  drops the cached state, so the next caller reloads. If state is still dirty at release it
  clears and panics. [src: cache.go L364-L405]
- `updateWorkflowExecutionWithNew` has `defer { if err != nil { c.Clear() } }`. [src: context.go L911-L915]
- Docs state the same rule: on failure to persist, reload from persistence. [src: history-service.md]

### Ownership fencing

- A history host owns shards (Ringpop membership). The shard has a `RangeID`, a monotonically
  increasing generation used for fencing. [src: history-service.md]
- In the SQL store every mutation runs `txExecuteShardLocked`. It reads the shard row with a
  shared lock and compares `range_id` to the owner's copy. Mismatch returns
  `ShardOwnershipLostError`. [src: sql/execution.go L40-L58, sql/shard.go `readLockShard`]
- Inside the same tx, `lockAndCheckExecution` locks the run row and checks
  `db_record_version` (or `next_event_id` when version is 0). Mismatch returns
  `WorkflowConditionFailedError`. [src: sql/execution_util.go L606-L640]
- So validity = (a) single owner via rangeID, (b) per-run in-process lock, (c) DB
  conditional update as the last line of defense. The cache never trusts itself.
- There is no per-transaction "is my cache stale" read. The write CAS is the check. A stale
  cache costs one failed write plus a reload.

### History write path

- Update flow: `AppendHistoryNodes` runs first for each new event batch, then
  `txExecuteShardLocked` runs the state update. [src: sql/execution.go L340-L462]
- History nodes are keyed (tree_id, branch_id, node_id). One node holds a batch of events from
  one transition. Insert is a plain INSERT. Duplicate key becomes `ConditionFailedError`.
  [src: sql/history_store.go]
- Branches (`tree_id` plus `branch_id`) exist for reset and fork. A reset creates a new branch
  sharing the ancestor prefix. This is not needed for Dagger. [src]
- History append is outside the state tx. A crash leaves orphan nodes that the state row never
  references. Temporal's own comment on the ordering was not located. [unverified rationale]

### One workflow-task transition

From `workflow-lifecycle.md` [src]:

- Reads: cached mutable state (DB read only on miss). The worker may read history events via
  `GetHistoryEvents` when its own sticky cache misses.
- Writes in one DB transaction: updated mutable state row (conditional), new history node
  (just before), plus new transfer or timer tasks (outbox rows).
- Tasks are processed later by queue processors. The transition itself does not wait on them.

### Cadence

- Same ancestry and same shape: `execution/context.go` in uber/cadence has
  `LoadWorkflowExecution`, `UpdateWorkflowExecutionAsActive`, `Clear`, and a conditional
  update (`ConditionFailedError`, `request.UpdateWorkflowMutation.Condition`). [src, skimmed]
- Cadence cache TTL, lock details and rangeID code were not read. [unverified]

## Restate

- Each step appends a journal entry to a replicated log. The step is durable once the log
  quorum acks. [src: docs.restate.dev/references/architecture]
- Each partition has one active leader. Attempts carry increasing epochs. A new leader
  appends an epoch-bump record and late appends from old epochs are ignored. [src: same]
- The processor keeps a materialized state in embedded RocksDB for random reads and updates.
  The doc says it is "derivative", rebuildable from the log, not a second source of truth.
  [src: same]
- Recovery replays from the latest snapshot. [src: same]
- Not found: append batching, whether state is also held decoded in memory. [unverified]
- Relevance: single-writer by epoch, state is an updatable index, never re-read as a whole
  document per step. Dagger's per-entity rows already resemble this. The cache idea (b)
  is the same trade at a different layer.

## Hatchet

- v1 engine runs on Postgres only. [doc]
- Items from internal queues are buffered and flushed every 10 ms in batches. Batch inserts
  are used on almost every table. [doc: hatchet.run/launch-week-01/v1-engine, HN launch post]
- Task status updates are deferred and processed in bulk. State transitions run in Postgres
  triggers, close to the data. Tables are hash-partitioned by run id. [doc: same]
- The stated reason: each task makes several writes and not all are latency sensitive.
  [doc: HN thread]
- Tuning knobs are flush period and buffer size. Default pool is 50 connections. [doc:
  docs.hatchet.run self-hosting performance]
- A separate blog reports 2k to 80k writes/s from batched inserts. This is a generic Postgres
  benchmark, not the engine. [doc, caveat]
- Relevance: batching is the throughput lever once per-command cost is flat. Matches (d).
  Hatchet moves logic into SQL, which Dagger cannot do while keeping the shared pure reducer.

## Inngest

- Runs are re-executed with the event payload and the prior step state as JSON. The SDK finds
  a step result by hashing the step name. [doc: how-functions-are-executed]
- The state store persists triggering event, step output and errors, written incrementally.
  [doc]
- Reported limits: 1,000 steps per function, 4 MB per step, 32 MB total run state, from a
  third-party page. [unverified]
- Redis/Lua state-loading internals were not found. [unverified]
- Relevance: a negative example. Full state is passed on every step, so cost per step grows
  with run size. They bound it with hard limits instead of fixing it.

## Windmill and Conductor

- Windmill: flow progress lives in a `flow_status` JSON in `v2_job_status`, which grows with
  modules and iterations. Whether it is rewritten on every step, and the cost, were not
  found. [unverified] Large for-loops avoid loading every iteration in the UI. [doc: changelog]
- Conductor: public architecture page has nothing on per-update load or locking. The Redis
  default store and the decider were not inspected. [unverified]
- Skipped: nothing verified to add.

## SQLite single-writer guidance

- WAL allows one writer at a time. Readers do not block it. [src: sqlite.org/wal.html]
- `BEGIN DEFERRED` followed by a write upgrades the read tx. If another connection has
  modified or is modifying the db, the upgrade fails with SQLITE_BUSY. [src:
  sqlite.org/lang_transaction.html]
- `BEGIN IMMEDIATE` takes the write lock at start. It may fail with SQLITE_BUSY if another
  writer is active. [src: same] We already use it.
- Blog consensus: the busy handler is not invoked for the upgrade case, so `busy_timeout`
  helps only with IMMEDIATE. Common pattern is N read connections plus one write connection
  or queue, serialized in the app. [secondary: Simon Willison / Stephen Margheim summary,
  HN thread, oneuptime blog] I did not find an official SQLite doc that says "use an
  in-process queue". Treat as convention, not specification.
- A queue gives fair ordering, no spin or timeout tuning, and lets you batch commands into
  one tx. `busy_timeout` stays as a backstop for other processes.
- Long readers can starve WAL checkpoints. Keep read transactions short. [src: wal.html]

## Plan item verdicts

| Item | Verdict | Reason |
| --- | --- | --- |
| (a) Stop loading append-only tables for commands | Do it | Temporal never reads history on the transition path. Mutable state carries the latest event id. Check that no reducer rule reads old events or ledger rows. If one does, keep a summary field in the run row. |
| (b) Per-run decoded-state cache, validated by per-run version at tx start | Do it, with two changes | Temporal validates with the write CAS, not a read check. Our version check at BEGIN IMMEDIATE is equivalent and cheap, keep it. Add: drop the entry on every error and on claim loss. Add a per-run in-process lock around read-apply-write. Tie validity to engine-claim generation, as Temporal ties it to rangeID. |
| (c) In-process writer queue before BEGIN IMMEDIATE | Do it, but queue per process, and consider group commit | SQLite has one writer anyway. Temporal serializes per run in memory. A single write queue avoids BUSY upgrade failures. Do not rely on `busy_timeout`. Keep CAS as backstop for other processes. |
| (d) Batch claims and resolve cache hits in one pass | Do it later, as designed | Hatchet's 10 ms buffer shows batching is the main throughput lever. Do it after (a)-(c) make per-command cost flat. Keep the reducer pure by folding a batch of commands over one in-memory state, then one diff and one write. |
| Not in plan: event write ordering | Skip changing | Temporal writes history before state, outside the state tx. We keep events in the same tx. Simpler and crash-safe. Only revisit if event writes dominate. |
| Not in plan: moving logic into SQL | Skip | Hatchet does it. It would break the single shared reducer. |
| Not in plan: history branches | Skip | Only for reset and fork. |

## Sources

- Temporal history service doc: https://github.com/temporalio/temporal/blob/main/docs/architecture/history-service.md
- Temporal workflow lifecycle doc: https://github.com/temporalio/temporal/blob/main/docs/architecture/workflow-lifecycle.md
- Temporal cache: https://github.com/temporalio/temporal/blob/main/service/history/workflow/cache/cache.go
- Temporal workflow context: https://github.com/temporalio/temporal/blob/main/service/history/workflow/context.go
- Temporal SQL execution store: https://github.com/temporalio/temporal/blob/main/common/persistence/sql/execution.go
- Temporal SQL execution util: https://github.com/temporalio/temporal/blob/main/common/persistence/sql/execution_util.go
- Temporal SQL shard (readLockShard): https://github.com/temporalio/temporal/blob/main/common/persistence/sql/shard.go
- Temporal SQL history store: https://github.com/temporalio/temporal/blob/main/common/persistence/sql/history_store.go
- Temporal dynamic config: https://github.com/temporalio/temporal/blob/main/common/dynamicconfig/constants.go
- Cadence execution context: https://github.com/uber/cadence/blob/master/service/history/execution/context.go
- Restate architecture: https://docs.restate.dev/references/architecture
- Hatchet v1 launch: https://hatchet.run/launch-week-01/v1-engine
- Hatchet HN thread: https://hn.svelte.dev/item/43572733
- Hatchet performance tuning: https://docs.hatchet.run/llms/self-hosting/improving-performance.md
- Inngest durable execution: https://www.inngest.com/docs/learn/how-functions-are-executed
- Windmill flow status changelog: https://www.windmill.dev/changelog/flow-status-viewer
- SQLite WAL: https://sqlite.org/wal.html
- SQLite transactions: https://www.sqlite.org/lang_transaction.html
- SQLite busy discussion (secondary): https://simonwillison.net/tags/sqlite-busy
- SQLite busy discussion (secondary): https://hn.svelte.dev/item/45781298
- SQLite forum: https://sqlite.org/forum/forumpost/c8e6f96d76

Not reached: DeepWiki (HTTP 429), Windmill architecture page (empty), Conductor internals,
Inngest server source. Line numbers cited are from `main` as fetched 2026-11-02 and may drift.
