---
subject: Workflow engine
keywords: [engine, scheduler, actions, state]
part_of: System overview
describes: [dagger-workflow-core/src/engine.rs, dagger-workflow-core/src/action, dagger-workflow-core/src/run.rs, dagger-workflow-core/src/store.rs]
status: canonical
created: 2026-08-23
last_verified: 2026-08-23 @ 94e8bc4543ccf2c8f57c30715071e7c0b9352b57
read_when: "You build or review the Dagger engine."
skip_when: "You only need commands; read Getting started."
---

# Workflow engine

The Dagger engine executes one published workflow revision.

The main Rust type is `WorkflowEngine`. The revision type is `WorkflowRevision`.

Each durable key belongs to one `ExecutionScope`. A scope contains a tenant ID and a namespace.

`WorkflowRevision.root_node_ids` stores the lexical root set derived at publication. A root has
no incoming `node_output` reference and no incoming control activation. The engine creates those
authored nodes as ready. It creates no synthetic start node. Output-reference edges and lexical
Kahn ranks keep joins deterministic across start, tick, resume, and recovery.

## Run lifecycle

1. The host creates `WorkflowEngine` with a store, an object store, and an action registry.
2. The host gets the scheduler claim for one scope.
3. The host creates a run with immutable input, limits, and budget.
4. `start` checks the exact action pins and starts every derived root once.
5. `tick` performs maintenance and advances ready nodes.
6. `run_until_idle` calls `tick` until one pass makes no change.
7. The host reads committed output through `CommittedObjectReader`.

`EngineConfig.max_concurrency` limits action calls in one process. It must be greater than zero.

## Long-running action executor design

`tick` claims ready attempts only while the engine-wide semaphore has capacity. It starts each
claimed action as a Tokio task on the host runtime and returns without waiting for the action.
Each task publishes and commits its own result as soon as it finishes. `run_until_idle` waits for
supervisor activity before it decides that the current frontier is idle.

The supervisor heartbeats a scope while that scope has an action in flight. Attempt IDs contain
the durable engine generation and random bytes. A completion is valid only while its attempt's
engine generation still owns the live scope claim. A takeover therefore rejects an old
generation's completion before recovery changes the old attempt state.

Cancellation has two layers. An action can poll `is_cancelled` or await `cancelled`. The
supervisor also polls durable run state, so cancellation from another process reaches a local
task. After `EngineConfig.cancellation_grace`, the supervisor drops a non-cooperative action
future. Dropping the future is the engine guarantee. The action's drop or abort path must stop
subprocesses, sandboxes, requests, and other external resources.

## Node types

| Node | Behavior |
| --- | --- |
| `Action` | Calls one registered action. |
| `Map` | Creates one bounded action child for each array item. |
| `Choice` | Selects the first matching case or the required default. A case/default may target a node or explicitly skip. |
| `Approval` | Waits for an allowed decision or an expiry result. |
| `Succeed` | Commits the root output. |
| `Fail` | Commits an explicit domain failure. |

A valid definition has exactly one reachable `Succeed` node. It can have more than one `Fail` node.

A permanent Action failure skips nodes that need its output. It does not stop an independent
branch. If another branch reaches `Succeed`, the run succeeds; otherwise the run fails after no
success output remains possible.

## Action contract

An `ActionDescriptor` contains these exact pins:

- Action name.
- Contract version.
- Input schema digest.
- Output schema digest.
- Implementation compatibility digest.

The engine gives canonical input bytes to `WorkflowAction`.

The action returns `Success`, `Retryable`, or `Permanent`. A success result can include output, artifacts, cost, and diagnostics.

The engine checks size, digest, deadline, registration, and action pins before it commits a result.

## Action progress for embedders

Actions may `await context.report_progress(...)` for low-frequency durable checkpoints and
phase labels. Each record is appended to the ordinary run event stream as an
action-attempt-correlated event; it is not a harness trace channel.

The engine permits 64 progress records per attempt by default. An embedder can set a different
positive cap with `WorkflowEngine::with_max_progress_events_per_attempt` before sharing the
engine. Records are limited to one per second, and over-cap or over-rate reports return typed
errors; nothing is silently dropped. A report is accepted only while its credentialed attempt is
Started, active, and owned by the current engine generation. Event-cap exhaustion follows the
normal `RunEventLimitExceeded` terminalization path.

Each store applies rate limiting and attempt fencing with its authoritative clock. `InMemoryStore`
uses its injected `Clock`. `SqliteWorkflowStore` uses SQLite time inside the command transaction,
including the durable test offset, so every process observes one clock for leases and progress.

## Scheduler claim and recovery

Each scope has one live engine claim. The claim and completion credentials fence an old generation. One engine process can hold claims for more than one scope.

After takeover, the engine finds attempts from an older generation. It marks their outcome as unknown. It then applies the retry rule. It does not treat an unknown action call as success.

The action gets a retry-stable idempotency key. For paid or otherwise expensive external work,
use the durable registry in this order: `lookup_external_handle()`; reattach when a matching
kind exists or create the provider operation when absent; immediately
`register_external_handle(kind, external_id, metadata)`; then execute. Registration is
active-attempt-fenced and idempotent per `(idempotency_key, kind)`, emits the existing P03
external-handle progress event, and is visible in `get_run(...).external_handles`. Registry rows
are run-scoped and follow run retention.

## Host duties

The host supplies authenticated principals for approval decisions.

The host keeps pinned action implementations available for active runs. Dagger blocks an incompatible run. It does not select a different implementation.

The host sets every run limit and the run budget.

The host keeps secrets outside definitions, action input, diagnostics, events, and ordinary artifacts. Dagger does not provide a secret store.
