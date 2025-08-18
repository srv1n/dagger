use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use task_core::model::{Task, TaskOutput, TaskStatus, TaskType};
use task_core::{
    Agent, AgentMetadata, AgentRegistry, Executor, ExecutorConfig, JsonAgent, SqliteStorage, Storage,
    TaskContext,
};

/// Example agent that processes messages
#[derive(Clone)]
struct MessageProcessor {
    name: String,
}

#[async_trait]
impl JsonAgent for MessageProcessor {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn description(&self) -> String {
        "Processes messages and returns transformed results".to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" },
                "transform": {
                    "type": "string",
                    "enum": ["uppercase", "lowercase", "reverse"]
                }
            },
            "required": ["message", "transform"]
        })
    }

    fn output_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "original": { "type": "string" },
                "transformed": { "type": "string" },
                "agent": { "type": "string" }
            },
            "required": ["original", "transformed", "agent"]
        })
    }

    async fn execute(&self, context: &TaskContext) -> Result<TaskOutput> {
        info!("MessageProcessor executing task {}", context.task_id);

        // Extract input
        let message = context
            .input
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing message"))?;

        let transform = context
            .input
            .get("transform")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing transform"))?;

        // Apply transformation
        let transformed = match transform {
            "uppercase" => message.to_uppercase(),
            "lowercase" => message.to_lowercase(),
            "reverse" => message.chars().rev().collect(),
            _ => return Err(anyhow::anyhow!("Invalid transform")),
        };

        // Store result in cache
        context.store_cache_value("last_transform", &transformed)?;

        // Store in shared state
        context.set_shared_value(
            format!("processed_{}", context.task_id),
            json!({ "message": transformed.clone() }),
        );

        Ok(TaskOutput {
            success: true,
            data: Some(json!({
                "original": message,
                "transformed": transformed,
                "agent": self.name()
            })),
            error: None,
        })
    }
}

/// Example agent that creates child tasks
#[derive(Clone)]
struct TaskCreator {
    name: String,
}

#[async_trait]
impl JsonAgent for TaskCreator {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn description(&self) -> String {
        "Creates child tasks based on input".to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "messages": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "transform": {
                    "type": "string",
                    "enum": ["uppercase", "lowercase", "reverse"]
                }
            },
            "required": ["messages", "transform"]
        })
    }

    fn output_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "created_tasks": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "required": ["created_tasks"]
        })
    }

    async fn execute(&self, context: &TaskContext) -> Result<TaskOutput> {
        info!("TaskCreator executing task {}", context.task_id);

        let messages = context
            .input
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("Missing messages array"))?;

        let transform = context
            .input
            .get("transform")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing transform"))?;

        let mut created_tasks = Vec::new();

        // Create child tasks for each message
        for (idx, message) in messages.iter().enumerate() {
            if let Some(msg) = message.as_str() {
                let child_task = Task {
                    job_id: context.job_id.clone(),
                    task_id: format!("{}_child_{}", context.task_id, idx),
                    parent_task_id: Some(context.task_id.clone()),
                    acceptance_criteria: None,
                    description: format!("Process message: {}", msg),
                    status: TaskStatus::Pending,
                    status_reason: None,
                    agent: "message_processor".to_string(),
                    dependencies: vec![],
                    input: json!({
                        "message": msg,
                        "transform": transform
                    }),
                    output: TaskOutput {
                        success: false,
                        data: None,
                        error: None,
                    },
                    created_at: chrono::Utc::now().naive_utc(),
                    updated_at: chrono::Utc::now().naive_utc(),
                    timeout: Some(Duration::from_secs(30)),
                    max_retries: 2,
                    retry_count: 0,
                    task_type: TaskType::Subtask,
                    summary: None,
                };

                let task_id = context.create_child_task(child_task).await?;
                created_tasks.push(task_id);
            }
        }

        Ok(TaskOutput {
            success: true,
            data: Some(json!({
                "created_tasks": created_tasks
            })),
            error: None,
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("Starting Executor Demo");

    // Create temporary storage
    let temp_dir = TempDir::new()?;
    let db_path = temp_dir.path().join("demo.db");
    let storage = Arc::new(SqliteStorage::open(db_path).await?) as Arc<dyn Storage>;

    // Create agent registry
    let registry = Arc::new(AgentRegistry::new());

    // Register agents with metadata
    let message_processor = Arc::new(MessageProcessor {
        name: "message_processor".to_string(),
    });
    registry.register_with_metadata(
        message_processor,
        AgentMetadata {
            name: "message_processor".to_string(),
            description: "Processes and transforms messages".to_string(),
            version: "1.0.0".to_string(),
            author: "Demo Author".to_string(),
            tags: vec!["transform".to_string(), "message".to_string()],
        },
    );

    let task_creator = Arc::new(TaskCreator {
        name: "task_creator".to_string(),
    });
    registry.register_with_metadata(
        task_creator,
        AgentMetadata {
            name: "task_creator".to_string(),
            description: "Creates child tasks dynamically".to_string(),
            version: "1.0.0".to_string(),
            author: "Demo Author".to_string(),
            tags: vec!["orchestration".to_string(), "dynamic".to_string()],
        },
    );

    // Create cache
    let cache = Arc::new(Cache::new());

    // Create executor configuration
    let config = ExecutorConfig {
        max_workers: 4,
        queue_capacity: 100,
        task_timeout: Some(Duration::from_secs(60)),
        retry_delay: Duration::from_secs(2),
    };

    // Create executor
    let executor = Arc::new(Executor::new(storage.clone(), registry, cache, config));

    // Create initial tasks
    let task1 = Task {
        job_id: "demo_job".to_string(),
        task_id: "task_1".to_string(),
        parent_task_id: None,
        acceptance_criteria: None,
        description: "Single message transformation".to_string(),
        status: TaskStatus::Pending,
        status_reason: None,
        agent: "message_processor".to_string(),
        dependencies: vec![],
        input: json!({
            "message": "Hello, World!",
            "transform": "uppercase"
        }),
        output: TaskOutput {
            success: false,
            data: None,
            error: None,
        },
        created_at: chrono::Utc::now().naive_utc(),
        updated_at: chrono::Utc::now().naive_utc(),
        timeout: None,
        max_retries: 1,
        retry_count: 0,
        task_type: TaskType::Task,
        summary: None,
    };

    let task2 = Task {
        job_id: "demo_job".to_string(),
        task_id: "task_2".to_string(),
        parent_task_id: None,
        acceptance_criteria: None,
        description: "Create multiple child tasks".to_string(),
        status: TaskStatus::Pending,
        status_reason: None,
        agent: "task_creator".to_string(),
        dependencies: vec!["task_1".to_string()], // Depends on task_1
        input: json!({
            "messages": ["first message", "second message", "third message"],
            "transform": "reverse"
        }),
        output: TaskOutput {
            success: false,
            data: None,
            error: None,
        },
        created_at: chrono::Utc::now().naive_utc(),
        updated_at: chrono::Utc::now().naive_utc(),
        timeout: None,
        max_retries: 0,
        retry_count: 0,
        task_type: TaskType::Task,
        summary: None,
    };

    // Store tasks
    storage.put(&task1).await?;
    storage.put(&task2).await?;

    // Add task1 to ready queue (no dependencies)
    executor.enqueue_task("task_1".to_string())?;

    // Start executor
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let executor_clone = executor.clone();
    let executor_handle = tokio::spawn(async move { executor_clone.run(shutdown_rx).await });

    // Start a scheduler that checks for ready tasks
    let scheduler_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;

            // Check task statuses and dependencies
            let tasks = storage
                .list_tasks_by_status(TaskStatus::Pending)
                .await
                .unwrap();
            for task in tasks {
                // Check if all dependencies are completed
                let mut all_deps_completed = true;
                for dep_id in &task.dependencies {
                    if let Some(dep_task) = storage.get(dep_id).await.unwrap() {
                        if dep_task.status != TaskStatus::Completed {
                            all_deps_completed = false;
                            break;
                        }
                    }
                }

                if all_deps_completed && task.dependencies.is_empty() == false {
                    info!("Task {} is ready (dependencies completed)", task.task_id);
                    let _ = executor.enqueue_task(task.task_id);
                }
            }

            // Check if all tasks are done
            let pending = storage
                .list_tasks_by_status(TaskStatus::Pending)
                .await
                .unwrap();
            let in_progress = storage
                .list_tasks_by_status(TaskStatus::InProgress)
                .await
                .unwrap();

            if pending.is_empty() && in_progress.is_empty() {
                info!("All tasks completed!");
                break;
            }

            // Print queue stats
            let (current, capacity) = executor.queue_stats();
            info!("Queue stats: {}/{}", current, capacity);
        }
    });

    // Wait for scheduler to finish
    let _ = scheduler_handle.await;

    // Shutdown executor
    shutdown_tx.send(()).unwrap();
    let _ = executor_handle.await;

    // Print results
    info!("\n=== Execution Results ===");

    // Get all tasks and print their status
    let all_tasks = storage.list_tasks_by_job("demo_job").await?;
    for task in all_tasks {
        info!(
            "Task {}: {} (Agent: {})",
            task.task_id,
            match task.status {
                TaskStatus::Completed => "✅ Completed",
                TaskStatus::Failed => "❌ Failed",
                TaskStatus::Pending => "⏳ Pending",
                TaskStatus::InProgress => "🔄 In Progress",
                _ => "❓ Other",
            },
            task.agent
        );

        if task.status == TaskStatus::Completed {
            if let Some(data) = task.output.data {
                info!("  Output: {}", serde_json::to_string_pretty(&data)?);
            }
        } else if task.status == TaskStatus::Failed {
            if let Some(error) = task.output.error {
                info!("  Error: {}", error);
            }
        }
    }

    info!("\n=== Demo Complete ===");

    Ok(())
}
