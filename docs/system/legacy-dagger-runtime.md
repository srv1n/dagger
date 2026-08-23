---
subject: Legacy Dagger runtime
keywords: [dag flow, task agent, pubsub, work queue]
part_of: overview
describes: [src, examples, tests, dagger-macros]
status: canonical
created: 2026-08-23
last_verified: "2026-08-23 @ bdd41fa"
read_when: "You work on the root dagger package."
skip_when: "You work only on dagger-workflow-core."
---

# Legacy Dagger runtime

This engine runs existing workflows in four ways.

The root package contains the implementation.

## DAG Flow

DAG Flow loads a graph from YAML. A node names an action. Dependencies control the execution order.

`DagExecutor` runs ready nodes. Independent nodes can run in parallel. `Cache` passes JSON values between nodes.

Use `#[action]` to register an async Rust function as an action. Start with `examples/dag_flow_basic.rs`.

## Task Agent

Task Agent creates durable tasks at run time. A task can depend on other tasks. An agent implementation handles a task.

`TaskSystemBuilder` accepts a SQLite path or a caller-owned `SqlitePool`. The storage uses tables with the `dagger_` prefix.

Use `#[task_agent]` to register an agent function. Start with `examples/task_agent_basic.rs`.

## Pub/Sub

Pub/Sub sends a `Message` to a named channel. Registered agents subscribe to channels. An agent can publish another message.

Use `#[pubsub_agent]` to register an agent function. Start with `examples/pubsub_basic.rs`.

## Work Queue

Work Queue builds deterministic DAGs for batch and pipeline work.

`ExecSpec` is structured data. It does not use shell interpolation. The host must allow each system executable or register it as a sidecar.

The exec layer applies a timeout. It also limits stdout and stderr. The host owns approval policy.

Start with `examples/work_queue_batch_exec.rs`.

## Limits

`ResourceLimits` controls memory, tasks, graph size, cache entries, file handles, network connections, time, retries, message size, queue size, and CPU percentage.

Call `ResourceLimits::validate` after you set custom values.

## Important boundary

This runtime is not `dagger-workflow-core`. The types, stores, run states, and workflow formats are different.
