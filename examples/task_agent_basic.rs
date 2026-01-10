//! Minimal Task Agent example using task-core

use bytes::Bytes;
use dagger::{
    task_agent, AgentRegistry, Durability, NewTaskSpec, Task, TaskContext, TaskSystemBuilder,
    TaskType,
};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

#[task_agent(name = "echo")]
async fn echo(task: Task, _ctx: Arc<TaskContext>) -> anyhow::Result<serde_json::Value> {
    let text = String::from_utf8_lossy(&task.payload.input).to_string();
    Ok(serde_json::json!({"echo": text}))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let registry = Arc::new(AgentRegistry::new());
    let db_path = std::env::temp_dir().join(format!("task_agent_basic_{}.db", Uuid::new_v4()));
    let system = TaskSystemBuilder::new()
        .with_storage_path(&db_path)
        .build(registry)
        .await?;

    let agent_id = system.agent_id("echo").expect("echo agent registered");

    system
        .submit_task(NewTaskSpec {
            agent: agent_id,
            input: Bytes::from("hello"),
            dependencies: Vec::new().into(),
            durability: Durability::BestEffort,
            task_type: TaskType::Task,
            description: Arc::from("echo"),
            timeout: None,
            max_retries: Some(3),
            parent: None,
        })
        .await?;

    let runner = tokio::spawn(system.clone().run());
    tokio::time::sleep(Duration::from_millis(250)).await;
    system.shutdown().await?;
    let _ = runner.await;
    let _ = std::fs::remove_file(db_path);

    Ok(())
}
