//! Test suite for Task-Core agent system

use dagger::taskagent::task_core::{
    TaskSystem, TaskSystemBuilder, TaskConfig, 
    model::{NewTaskSpec, TaskId, AgentId, Durability, TaskType, TaskStatus, AgentError},
    executor::{Agent, TaskContext, AgentRegistry},
    storage::Storage,
    error::{TaskError, Result},
};
use dagger_macros::task_agent;
use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;
use smallvec::smallvec;
use tokio::sync::Semaphore;
use std::sync::atomic::{AtomicU32, AtomicBool, Ordering};

/// Test agent that echoes input
#[task_agent(
    name = "echo",
    description = "Echoes input back"
)]
async fn echo_agent(input: Bytes, _ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    Ok(input)
}

/// Test agent that adds two numbers
#[task_agent(
    name = "adder",
    description = "Adds two numbers from JSON input"
)]
async fn adder_agent(input: Bytes, _ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    let json_str = String::from_utf8(input.to_vec())
        .map_err(|e| AgentError::User(format!("Invalid UTF-8: {}", e)))?;
    
    let data: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| AgentError::User(format!("Invalid JSON: {}", e)))?;
    
    let a = data["a"].as_f64()
        .ok_or_else(|| AgentError::User("Missing field 'a'".into()))?;
    let b = data["b"].as_f64()
        .ok_or_else(|| AgentError::User("Missing field 'b'".into()))?;
    
    let result = serde_json::json!({ "sum": a + b });
    Ok(Bytes::from(result.to_string()))
}

/// Test agent that fails with retryable error
static FAIL_COUNT: AtomicU32 = AtomicU32::new(0);

#[task_agent(
    name = "retry_fail",
    description = "Fails first N times then succeeds"
)]
async fn retry_fail_agent(input: Bytes, _ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    let count = FAIL_COUNT.fetch_add(1, Ordering::SeqCst);
    if count < 2 {
        Err(AgentError::System("Temporary failure".into()))
    } else {
        Ok(Bytes::from("Success after retries"))
    }
}

/// Test agent that always fails with user error
#[task_agent(
    name = "always_fail",
    description = "Always fails with non-retryable error"
)]
async fn always_fail_agent(_input: Bytes, _ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    Err(AgentError::User("Permanent failure".into()))
}

/// Test agent that creates subtasks
#[task_agent(
    name = "spawner",
    description = "Spawns subtasks"
)]
async fn spawner_agent(input: Bytes, ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    let count = String::from_utf8(input.to_vec())
        .map_err(|e| AgentError::User(e.to_string()))?
        .parse::<u32>()
        .map_err(|e| AgentError::User(e.to_string()))?;
    
    let mut subtask_ids = Vec::new();
    
    for i in 0..count {
        let subtask_id = ctx.handle.spawn_task(NewTaskSpec {
            agent: echoAgent::AGENT_ID,
            input: Bytes::from(format!("Subtask {}", i)),
            dependencies: smallvec![],
            durability: Durability::BestEffort,
            task_type: TaskType::Subtask,
            description: Arc::from(format!("Subtask {}", i)),
            timeout: Some(Duration::from_secs(5)),
            max_retries: Some(3),
            parent: ctx.parent,
        }).await?;
        
        subtask_ids.push(subtask_id);
    }
    
    let result = format!("Spawned {} subtasks: {:?}", count, subtask_ids);
    Ok(Bytes::from(result))
}

/// Test agent that waits for dependencies
#[task_agent(
    name = "dependent",
    description = "Processes dependency results"
)]
async fn dependent_agent(_input: Bytes, ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    // Get outputs from all dependencies
    let mut results = Vec::new();
    
    for &dep_id in &ctx.dependencies {
        if let Some(output) = ctx.dependency_output(dep_id).await? {
            let result = String::from_utf8(output.to_vec())
                .unwrap_or_else(|_| "<binary>".to_string());
            results.push(format!("{}: {}", dep_id, result));
        }
    }
    
    Ok(Bytes::from(format!("Processed {} dependencies: {}", 
        results.len(), results.join(", "))))
}

/// Test agent that simulates long-running work
#[task_agent(
    name = "slow_worker",
    description = "Simulates slow work"
)]
async fn slow_worker_agent(input: Bytes, _ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    let duration_ms = String::from_utf8(input.to_vec())
        .map_err(|e| AgentError::User(e.to_string()))?
        .parse::<u64>()
        .map_err(|e| AgentError::User(e.to_string()))?;
    
    tokio::time::sleep(Duration::from_millis(duration_ms)).await;
    Ok(Bytes::from(format!("Worked for {}ms", duration_ms)))
}

fn setup_system(name: &str) -> Result<(TaskSystem, Arc<AgentRegistry>)> {
    let config = TaskConfig {
        max_workers: 4,
        queue_capacity: 1000,
        max_retries: 3,
        retry_delay: Duration::from_millis(100),
        worker_keepalive: Duration::from_secs(60),
        enable_metrics: true,
        checkpoint_interval: Duration::from_secs(30),
    };
    
    let mut registry = AgentRegistry::new();
    
    // Register all test agents
    echoAgent::register(&mut registry);
    adderAgent::register(&mut registry);
    retry_failAgent::register(&mut registry);
    always_failAgent::register(&mut registry);
    spawnerAgent::register(&mut registry);
    dependentAgent::register(&mut registry);
    slow_workerAgent::register(&mut registry);
    
    let registry = Arc::new(registry);
    let db_path = format!("test_{}_db", name);
    
    // Clean up any existing database
    let _ = std::fs::remove_dir_all(&db_path);
    
    let system = TaskSystemBuilder::new()
        .with_config(config)
        .with_registry(registry.clone())
        .with_storage_path(&db_path)
        .build()?;
    
    Ok((system, registry))
}

#[tokio::test]
async fn test_simple_task_execution() {
    let (system, _) = setup_system("simple_execution").unwrap();
    
    // Submit echo task
    let task_id = system.submit_task(NewTaskSpec {
        agent: echoAgent::AGENT_ID,
        input: Bytes::from("Hello, Task Core!"),
        dependencies: smallvec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Test echo"),
        timeout: Some(Duration::from_secs(5)),
        max_retries: Some(3),
        parent: None,
    }).await.unwrap();
    
    // Wait for completion
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    // Check task completed
    let task = system.get_task(task_id).await.unwrap();
    assert_eq!(task.status.load(Ordering::Relaxed), TaskStatus::Completed as u8);
    
    // Check output
    let output = task.payload.output.read().await;
    assert_eq!(output.as_ref().unwrap().as_ref(), b"Hello, Task Core!");
    
    system.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_task_with_dependencies() {
    let (system, _) = setup_system("dependencies").unwrap();
    
    // Create two independent tasks
    let task1 = system.submit_task(NewTaskSpec {
        agent: echoAgent::AGENT_ID,
        input: Bytes::from("Task 1"),
        dependencies: smallvec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Task 1"),
        timeout: None,
        max_retries: Some(3),
        parent: None,
    }).await.unwrap();
    
    let task2 = system.submit_task(NewTaskSpec {
        agent: echoAgent::AGENT_ID,
        input: Bytes::from("Task 2"),
        dependencies: smallvec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Task 2"),
        timeout: None,
        max_retries: Some(3),
        parent: None,
    }).await.unwrap();
    
    // Create dependent task
    let dependent = system.submit_task(NewTaskSpec {
        agent: dependentAgent::AGENT_ID,
        input: Bytes::from("Process dependencies"),
        dependencies: smallvec![task1, task2],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Dependent task"),
        timeout: None,
        max_retries: Some(3),
        parent: None,
    }).await.unwrap();
    
    // Wait for completion
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    // Check dependent task completed
    let task = system.get_task(dependent).await.unwrap();
    assert_eq!(task.status.load(Ordering::Relaxed), TaskStatus::Completed as u8);
    
    // Check output mentions both dependencies
    let output = task.payload.output.read().await;
    let output_str = String::from_utf8(output.as_ref().unwrap().to_vec()).unwrap();
    assert!(output_str.contains("2 dependencies"));
    
    system.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_retry_mechanism() {
    let (system, _) = setup_system("retry").unwrap();
    
    // Reset retry counter
    FAIL_COUNT.store(0, Ordering::SeqCst);
    
    // Submit task that will fail twice then succeed
    let task_id = system.submit_task(NewTaskSpec {
        agent: retry_failAgent::AGENT_ID,
        input: Bytes::from("Test retry"),
        dependencies: smallvec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Retry test"),
        timeout: Some(Duration::from_secs(5)),
        max_retries: Some(3),
        parent: None,
    }).await.unwrap();
    
    // Wait for retries and completion
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    // Check task eventually succeeded
    let task = system.get_task(task_id).await.unwrap();
    assert_eq!(task.status.load(Ordering::Relaxed), TaskStatus::Completed as u8);
    assert_eq!(task.retry_count.load(Ordering::Relaxed), 2);
    
    system.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_non_retryable_failure() {
    let (system, _) = setup_system("non_retryable").unwrap();
    
    // Submit task that will fail with user error (non-retryable)
    let task_id = system.submit_task(NewTaskSpec {
        agent: always_failAgent::AGENT_ID,
        input: Bytes::from("Will fail"),
        dependencies: smallvec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Permanent failure test"),
        timeout: Some(Duration::from_secs(5)),
        max_retries: Some(3),
        parent: None,
    }).await.unwrap();
    
    // Wait for execution
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    // Check task failed without retries
    let task = system.get_task(task_id).await.unwrap();
    assert_eq!(task.status.load(Ordering::Relaxed), TaskStatus::Failed as u8);
    assert_eq!(task.retry_count.load(Ordering::Relaxed), 0);
    
    system.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_subtask_spawning() {
    let (system, _) = setup_system("subtasks").unwrap();
    
    // Submit spawner task that creates 3 subtasks
    let parent_id = system.submit_task(NewTaskSpec {
        agent: spawnerAgent::AGENT_ID,
        input: Bytes::from("3"),
        dependencies: smallvec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Parent spawner"),
        timeout: Some(Duration::from_secs(10)),
        max_retries: Some(1),
        parent: None,
    }).await.unwrap();
    
    // Wait for execution
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    // Check parent completed
    let parent = system.get_task(parent_id).await.unwrap();
    assert_eq!(parent.status.load(Ordering::Relaxed), TaskStatus::Completed as u8);
    
    // Check output mentions 3 subtasks
    let output = parent.payload.output.read().await;
    let output_str = String::from_utf8(output.as_ref().unwrap().to_vec()).unwrap();
    assert!(output_str.contains("Spawned 3 subtasks"));
    
    system.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_concurrent_execution() {
    let (system, _) = setup_system("concurrent").unwrap();
    
    let start = std::time::Instant::now();
    
    // Submit multiple slow tasks
    let mut task_ids = Vec::new();
    for i in 0..4 {
        let task_id = system.submit_task(NewTaskSpec {
            agent: slow_workerAgent::AGENT_ID,
            input: Bytes::from("200"), // 200ms each
            dependencies: smallvec![],
            durability: Durability::BestEffort,
            task_type: TaskType::Task,
            description: Arc::from(format!("Slow task {}", i)),
            timeout: Some(Duration::from_secs(5)),
            max_retries: Some(1),
            parent: None,
        }).await.unwrap();
        task_ids.push(task_id);
    }
    
    // Wait for all to complete
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    let elapsed = start.elapsed();
    
    // Check all completed
    for task_id in task_ids {
        let task = system.get_task(task_id).await.unwrap();
        assert_eq!(task.status.load(Ordering::Relaxed), TaskStatus::Completed as u8);
    }
    
    // With 4 workers, 4 tasks of 200ms each should complete in ~200ms, not 800ms
    assert!(elapsed < Duration::from_millis(400), "Tasks didn't run concurrently");
    
    system.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_json_processing() {
    let (system, _) = setup_system("json").unwrap();
    
    // Submit adder task
    let input = serde_json::json!({
        "a": 10.5,
        "b": 20.3
    });
    
    let task_id = system.submit_task(NewTaskSpec {
        agent: adderAgent::AGENT_ID,
        input: Bytes::from(input.to_string()),
        dependencies: smallvec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Add numbers"),
        timeout: Some(Duration::from_secs(5)),
        max_retries: Some(1),
        parent: None,
    }).await.unwrap();
    
    // Wait for completion
    tokio::time::sleep(Duration::from_millis(200)).await;
    
    // Check result
    let task = system.get_task(task_id).await.unwrap();
    assert_eq!(task.status.load(Ordering::Relaxed), TaskStatus::Completed as u8);
    
    let output = task.payload.output.read().await;
    let result: serde_json::Value = serde_json::from_slice(output.as_ref().unwrap())
        .unwrap();
    assert_eq!(result["sum"], 30.8);
    
    system.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_system_persistence() {
    let db_path = "test_persistence_db";
    let _ = std::fs::remove_dir_all(db_path);
    
    let task_id = {
        // Create system and submit task
        let (system, _) = setup_system("persistence").unwrap();
        
        let task_id = system.submit_task(NewTaskSpec {
            agent: echoAgent::AGENT_ID,
            input: Bytes::from("Persistent task"),
            dependencies: smallvec![],
            durability: Durability::AtMostOnce,
            task_type: TaskType::Task,
            description: Arc::from("Test persistence"),
            timeout: None,
            max_retries: Some(3),
            parent: None,
        }).await.unwrap();
        
        // Don't wait for completion - simulate crash
        system.shutdown().await.unwrap();
        task_id
    };
    
    // Restart system
    let (system, _) = setup_system("persistence").unwrap();
    
    // Task should still exist
    let task = system.get_task(task_id).await.unwrap();
    assert_eq!(task.durability, Durability::AtMostOnce);
    
    // Wait for recovery and execution
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    // Should eventually complete
    let task = system.get_task(task_id).await.unwrap();
    assert_eq!(task.status.load(Ordering::Relaxed), TaskStatus::Completed as u8);
    
    system.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_task_timeout() {
    let (system, _) = setup_system("timeout").unwrap();
    
    // Submit task that will timeout
    let task_id = system.submit_task(NewTaskSpec {
        agent: slow_workerAgent::AGENT_ID,
        input: Bytes::from("5000"), // 5 seconds
        dependencies: smallvec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Timeout test"),
        timeout: Some(Duration::from_millis(500)), // 500ms timeout
        max_retries: Some(0),
        parent: None,
    }).await.unwrap();
    
    // Wait for timeout
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    // Check task failed due to timeout
    let task = system.get_task(task_id).await.unwrap();
    assert_eq!(task.status.load(Ordering::Relaxed), TaskStatus::Failed as u8);
    
    system.shutdown().await.unwrap();
}

/// Cleanup test databases
fn cleanup_test_dbs() {
    let test_dbs = [
        "test_simple_execution_db",
        "test_dependencies_db", 
        "test_retry_db",
        "test_non_retryable_db",
        "test_subtasks_db",
        "test_concurrent_db",
        "test_json_db",
        "test_persistence_db",
        "test_timeout_db",
    ];
    
    for db in &test_dbs {
        let _ = std::fs::remove_dir_all(db);
    }
}