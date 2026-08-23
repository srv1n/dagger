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

## Run lifecycle

1. The host creates `WorkflowEngine` with a store, an object store, and an action registry.
2. The host gets the scheduler claim for one scope.
3. The host creates a run with immutable input, limits, and budget.
4. `start` checks the exact action pins and starts the run.
5. `tick` performs maintenance and advances ready nodes.
6. `run_until_idle` calls `tick` until one pass makes no change.
7. The host reads committed output through `CommittedObjectReader`.

`EngineConfig.max_concurrency` limits action calls in one process. It must be greater than zero.

## Node types

| Node | Behavior |
| --- | --- |
| `Action` | Calls one registered action. |
| `Map` | Creates one bounded action child for each array item. |
| `Choice` | Selects the first matching case or the required default. |
| `Approval` | Waits for an allowed decision or an expiry result. |
| `Succeed` | Commits the root output. |
| `Fail` | Commits an explicit domain failure. |

A valid definition has exactly one reachable `Succeed` node. It can have more than one `Fail` node.

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

## Scheduler claim and recovery

Each scope has one live engine claim. The claim and completion credentials fence an old generation. One engine process can hold claims for more than one scope.

After takeover, the engine finds attempts from an older generation. It marks their outcome as unknown. It then applies the retry rule. It does not treat an unknown action call as success.

The action gets a retry-stable idempotency key. An external action can use this key to detect a repeated request.

## Host duties

The host supplies authenticated principals for approval decisions.

The host keeps pinned action implementations available for active runs. Dagger blocks an incompatible run. It does not select a different implementation.

The host sets every run limit and the run budget.

The host keeps secrets outside definitions, action input, diagnostics, events, and ordinary artifacts. Dagger does not provide a secret store.
