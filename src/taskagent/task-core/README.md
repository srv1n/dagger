# Task Core

Task-core is the high-performance task execution engine behind Dagger's Task Agent mode. It provides persistent storage, dependency scheduling, crash recovery, and a worker pool for concurrent task execution.

## Key Components

- **TaskSystem**: Orchestrates storage, scheduler, executor, and recovery.
- **Storage**: `SqliteStorage` persists tasks and outputs with CAS-style updates.
- **Scheduler**: Tracks dependencies and moves ready tasks into the queue.
- **Executor**: Worker pool that runs agents and records outputs.
- **AgentRegistry / Agent**: Pluggable task execution.

## Minimal Usage

```rust
use async_trait::async_trait;
use bytes::Bytes;
use std::sync::Arc;
use task_core::{Agent, AgentError, AgentRegistry, NewTaskSpec, Task, TaskContext, TaskSystemBuilder};

struct EchoAgent;

#[async_trait]
impl Agent for EchoAgent {
    async fn execute(
        &self,
        task: Task,
        _ctx: Arc<TaskContext>,
    ) -> Result<Bytes, AgentError> {
        Ok(task.payload.input.clone())
    }
}

#[tokio::main]
async fn main() -> Result<(), task_core::TaskError> {
    let mut registry = AgentRegistry::new();
    registry.register(1, "echo", Arc::new(EchoAgent))?;

    let system = TaskSystemBuilder::new().build(Arc::new(registry)).await?;

    let _task_id = system
        .submit_task(NewTaskSpec {
            job: None,
            agent: 1,
            public_id: None,
            thread_id: Some(Arc::from("thread-1")),
            subject: Some(Arc::from("echo")),
            description: Arc::from("echo"),
            owner: Some(Arc::from("example")),
            metadata: serde_json::json!({ "surface": "example" }),
            source: None,
            acceptance_criteria: None,
            input: Bytes::from("hello"),
            dependencies: Vec::new().into(),
            durability: task_core::Durability::BestEffort,
            task_type: task_core::TaskType::Task,
            timeout: None,
            max_retries: Some(3),
            parent: None,
        })
        .await?;

    system.run().await
}
```

## Notes

- Use `TaskContext` to access dependency outputs and shared state.
- The scheduler automatically evaluates dependencies and enqueues tasks.
- Recovery runs at startup to requeue running tasks (BestEffort) or pause them (AtMostOnce).

## Storage Modes

- `with_storage_path(...)` keeps task-core standalone and is the right fit for tests, examples, and isolated tooling.
- `with_sqlite_pool(...)` embeds task-core into a caller-owned app database and initializes the canonical host-facing `dagger_*` tables idempotently.

## Durable Task Surface

Task records now carry both execution data and host-facing metadata:

- Engine/runtime: status, retries, parent id, dependency edges, payload bytes, output summary, error bytes
- Host/public: `public_id`, `thread_id`, `subject`, `description`, `owner`, metadata JSON, and source metadata

The SQLite storage backend persists:

- `dagger_jobs`
- `dagger_tasks`
- `dagger_task_dependencies`
- `dagger_task_outputs`
- `dagger_task_events`
- `dagger_shared_state`

Host code can:

- fetch tasks by `public_id`
- list tasks by thread
- list child tasks, dependencies, and dependents
- append and read ordered output history
- append and read lifecycle event history
- request stop/cancel and soft-delete tasks without losing history

## Status Mapping

Public/host status projection is derived from engine state:

| Engine status | Host/public status |
| --- | --- |
| `Pending`, `Ready`, `Accepted` | `Queued` |
| `Blocked` | `Blocked` |
| `Running` | `Running` |
| `Paused` | `Paused` |
| `Completed` | `Succeeded` |
| `Failed` | `Failed` |
| `Rejected` | `Rejected` |
| `Running` with a persisted `stop_requested` lifecycle event | `Stopping` |
| `Cancelled` | `Cancelled` |
| `Deleted` or any task with `deleted_at` set | `Deleted` |
