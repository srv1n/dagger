---
subject: Hosting long-running actions
keywords: [host, actions, cancellation, SQLite, reattach]
part_of: Workflow engine
describes: [dagger-workflow-core/src/engine.rs, dagger-workflow-core/src/action/mod.rs, dagger-workflow-core/src/sqlite/mod.rs]
status: canonical
---

# Hosting long-running actions

`WorkflowEngine` is suitable for a hosted-agent action that owns one long-lived
guest session. The workflow engine owns workflow state; the action owns the provider session,
including creating it, reattaching to it, and stopping it on cancellation.

## Driver loop

Acquire one scope before doing scheduler work. Call `tick` continuously from a Tokio task; it
claims work and starts action tasks without waiting for them to finish. Use `run_until_idle` only
when a caller actually wants to wait for the current frontier to settle.

```rust
engine.acquire_scope(&scope).await?;
loop {
    engine.tick(&scope).await?;
    engine.heartbeat_scope(&scope).await?;
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
}
```

Keep the driver alive for as long as it owns the scope. The supervisor heartbeats automatically
while an action is in flight. The explicit `heartbeat_scope` in the driver preserves a live claim
between actions, while the driver is intentionally idle, or while it is doing host work outside
the engine. Treat a heartbeat failure as loss of scheduling authority: stop dispatching and
reacquire before continuing.

`EngineConfig::max_concurrency` is process-local. Give each independently running host process
its own `EngineConfig::instance_id`; the durable scope claim selects the active generation.

## Cancellation and cleanup

`WorkflowEngine::cancel` first commits cancellation, then signals a local action through
`ActionContext::cancellation_token`. An action may poll `CancellationToken::is_cancelled` or wait
on `CancellationToken::cancelled`. The supervisor also notices a durable cancellation made by a
different process.

Set `EngineConfig::cancellation_grace` to the longest acceptable external-cleanup window. After
that duration, Dagger drops a non-cooperative action future. That only stops Rust execution: the
action must use its drop or cancellation path to terminate the E2B sandbox, subprocess, request,
or other paid external work. A late action result is not live workflow state.

## Progress budget

Use `ActionContext::report_progress` only for low-frequency milestones. Its accepted
`ProgressRecord` variants are `Checkpoint`, `Phase`, and `ExternalHandle`. The default limit is
64 records per attempt; use `WorkflowEngine::with_max_progress_events_per_attempt` before sharing
the engine to set another positive limit. Reports are rate-limited to one per second and return
`StoreError::ProgressRateLimited` or `StoreError::ProgressEventLimitExceeded` rather than being
silently dropped.

Do not send guest logs, token traces, or per-second status through Dagger progress. Store those in
the harness's own log or trace system and use a checkpoint or phase record to link the durable
workflow to that system. High-volume trace streaming is explicitly out of scope.

## External-session reattach

An `ActionContext::idempotency_key` is stable across retries for the same logical action. Use this
order before doing expensive work:

1. Call `ActionContext::lookup_external_handle`.
2. If the desired provider kind exists, reattach to that external ID.
3. Otherwise create the provider session, then immediately call
   `ActionContext::register_external_handle` before starting the guest work.

The handle registry is fenced to the active attempt and idempotent for each
`(idempotency_key, kind)`. Hosts can inspect recovered handles through
`WorkflowStore::get_run(...).external_handles`.

## SQLite deployment

For the embedded deployment shape, construct `SqliteWorkflowStore` with `SqliteWorkflowStore::open`
and retain `FsObjectStore` on durable local storage. `SqliteWorkflowStore::open` configures WAL,
foreign keys, full synchronous writes, a busy timeout, and a bounded retry around `BEGIN IMMEDIATE`.
If the host supplies its own `SqlitePool`, use `SqliteWorkflowStore::from_pool`; it normalizes the
connection settings on every transaction boundary.

Run `make sqlite-write-load` before increasing writer count or changing SQLite deployment details.
It is the 16-writer, held-contention proof and is intentionally separate from the fast branch gate.

## Non-goals

Dagger does not own provider credentials, external-resource cleanup, a secret store, a high-volume
guest trace pipeline, or a distributed multi-writer scheduler for one `ExecutionScope`. Keep those
responsibilities in the embedding service.
