# Task Agent Architecture

Task Agent mode is powered by the `task-core` engine. It executes dynamic task graphs with persistent storage, dependency scheduling, and crash recovery.

## Core Components

- **TaskSystem**: Orchestrates storage, scheduler, executor, and recovery.
- **Storage (SqliteStorage)**: Persists tasks, outputs, and shared state with CAS updates.
- **Scheduler**: Tracks dependencies and moves ready tasks into the queue.
- **Executor**: Worker pool that executes agents with timeouts and retries.
- **AgentRegistry / Agent**: Pluggable task executors (auto-registered via macros).
- **Recovery**: Requeues in-flight tasks at startup (BestEffort) or pauses them (AtMostOnce).

## Data Model

- **Task**: A single unit of work with dependencies, status, durability, and optional parent.
- **Job**: A logical grouping of tasks (future-facing; IDs are stored but orchestration is per task).
- **Shared State**: A small key/value store for inter-task coordination.

## Execution Flow

1. Submit a task via `TaskSystem::submit_task`.
2. Scheduler indexes dependencies and enqueues ready tasks.
3. Executor pulls tasks and calls the registered agent.
4. Outputs are persisted; dependents are re-evaluated.
5. Recovery runs on startup to requeue interrupted tasks.

## Macro Support

Use the macro to create an agent with automatic registration:

```rust
#[dagger::task_agent(name = "analyze")]
async fn analyze(task: Task, ctx: std::sync::Arc<TaskContext>) -> anyhow::Result<serde_json::Value> {
    Ok(serde_json::json!({ "status": "processing", "task": task.id }))
}
```

The macro registers the agent with the global `AGENTS` slice; `TaskSystemBuilder::build(...)` loads them automatically.

## Minimal Setup

```rust
use bytes::Bytes;
use std::sync::Arc;
use dagger::{AgentRegistry, NewTaskSpec, TaskSystemBuilder, TaskType, Durability};

let registry = Arc::new(AgentRegistry::new());
let system = TaskSystemBuilder::new().build(registry).await?;

let _task_id = system
    .submit_task(NewTaskSpec {
        agent: system.agent_id("analyze").expect("agent registered"),
        input: Bytes::from("payload"),
        dependencies: Vec::new().into(),
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("example"),
        timeout: None,
        max_retries: Some(3),
        parent: None,
    })
    .await?;
```

## Notes

- Agent outputs are persisted as `Bytes`. Return any type implementing `IntoBytes` (e.g. `serde_json::Value`, `Bytes`, `Vec<u8>`, `String`).
- Use `TaskContext::dependency_output(...)` to read dependency outputs or shared state (`ctx.shared`).
- For non-idempotent tasks, choose `Durability::AtMostOnce` to avoid automatic retries.
