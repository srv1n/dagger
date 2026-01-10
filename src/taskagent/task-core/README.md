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
            agent: 1,
            input: Bytes::from("hello"),
            dependencies: Vec::new().into(),
            durability: task_core::Durability::BestEffort,
            task_type: task_core::TaskType::Task,
            description: Arc::from("echo"),
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
