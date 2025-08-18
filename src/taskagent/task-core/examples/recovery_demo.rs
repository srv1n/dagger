use anyhow::Result;
use std::sync::Arc;
use tempfile::TempDir;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use chrono::Utc;
use serde_json::json;
use task_core::model::{Task, TaskOutput, TaskStatus, TaskType};
use task_core::{
    DurabilityMode, ReadyQueue, Recovery, RecoveryConfig, Scheduler, SqliteStorage, Storage,
};

#[tokio::main]
async fn main() -> Result<()> {
    // Set up logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    // Example 1: BestEffort recovery (default)
    info!("=== Example 1: BestEffort Recovery ===");
    demo_best_effort_recovery().await?;

    // Example 2: AtMostOnce recovery
    info!("\n=== Example 2: AtMostOnce Recovery ===");
    demo_at_most_once_recovery().await?;

    // Example 3: Recovery with dependencies
    info!("\n=== Example 3: Recovery with Dependencies ===");
    demo_recovery_with_dependencies().await?;

    Ok(())
}

async fn demo_best_effort_recovery() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let db_path = temp_dir.path().join("recovery.db");
    let storage = Arc::new(SqliteStorage::open(db_path).await?) as Arc<dyn Storage>;
    let ready_queue = Arc::new(ReadyQueue::new(100));
    let scheduler = Arc::new(Scheduler::new(storage.clone(), ready_queue.clone()));

    // Simulate tasks that were running when system crashed
    create_and_store_running_task(&storage, "task1", "job1").await?;
    create_and_store_running_task(&storage, "task2", "job1").await?;
    create_and_store_running_task(&storage, "task3", "job2").await?;

    info!("Created 3 tasks in InProgress state to simulate a crash");

    // Create recovery with BestEffort mode (default)
    let recovery = Recovery::new(
        storage.clone(),
        ready_queue.clone(),
        scheduler,
        RecoveryConfig::default(), // BestEffort by default
    );

    // Run recovery
    let stats = recovery.recover().await?;

    info!("Recovery stats: {:?}", stats);
    info!("Ready queue length: {}", ready_queue.len());

    // Verify all tasks were reset to Pending and enqueued
    let task1 = storage.get("task1").await?.unwrap();
    assert_eq!(task1.status, TaskStatus::Pending);

    Ok(())
}

async fn demo_at_most_once_recovery() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let db_path = temp_dir.path().join("recovery.db");
    let storage = Arc::new(SqliteStorage::open(db_path).await?) as Arc<dyn Storage>;
    let ready_queue = Arc::new(ReadyQueue::new(100));
    let scheduler = Arc::new(Scheduler::new(storage.clone(), ready_queue.clone()));

    // Simulate tasks that were running
    create_and_store_running_task(&storage, "critical_task1", "job1").await?;
    create_and_store_running_task(&storage, "critical_task2", "job1").await?;

    info!("Created 2 critical tasks in InProgress state");

    // Create recovery with AtMostOnce mode
    let recovery = Recovery::new(
        storage.clone(),
        ready_queue.clone(),
        scheduler,
        RecoveryConfig {
            default_durability: DurabilityMode::AtMostOnce,
            auto_recover: true,
        },
    );

    // Run recovery
    let stats = recovery.recover().await?;

    info!("Recovery stats: {:?}", stats);
    info!("Ready queue length: {} (should be 0)", ready_queue.len());

    // Verify tasks were blocked and not enqueued
    let task = storage.get("critical_task1").await?.unwrap();
    assert_eq!(task.status, TaskStatus::Blocked);
    assert!(task.status_reason.is_some());
    info!("Task status reason: {}", task.status_reason.unwrap());

    Ok(())
}

async fn demo_recovery_with_dependencies() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let db_path = temp_dir.path().join("recovery.db");
    let storage = Arc::new(SqliteStorage::open(db_path).await?) as Arc<dyn Storage>;
    let ready_queue = Arc::new(ReadyQueue::new(100));
    let scheduler = Arc::new(Scheduler::new(storage.clone(), ready_queue.clone()));

    // Create a completed dependency
    let mut dep_task = create_test_task("dep1", "job1");
    dep_task.status = TaskStatus::Completed;
    storage.put(&dep_task).await?;

    // Create a pending dependency
    let mut pending_dep = create_test_task("dep2", "job1");
    pending_dep.status = TaskStatus::Pending;
    storage.put(&pending_dep).await?;

    // Create running tasks with different dependency scenarios
    let mut task_with_met_deps = create_test_task("task1", "job1");
    task_with_met_deps.status = TaskStatus::InProgress;
    task_with_met_deps.dependencies = vec!["dep1".to_string()];
    storage.put(&task_with_met_deps).await?;

    let mut task_with_unmet_deps = create_test_task("task2", "job1");
    task_with_unmet_deps.status = TaskStatus::InProgress;
    task_with_unmet_deps.dependencies = vec!["dep2".to_string()];
    storage.put(&task_with_unmet_deps).await?;

    info!("Created tasks with various dependency states");

    // Run recovery
    let recovery = Recovery::new(
        storage.clone(),
        ready_queue.clone(),
        scheduler,
        RecoveryConfig::default(),
    );

    let stats = recovery.recover().await?;

    info!("Recovery stats: {:?}", stats);
    info!(
        "Ready queue length: {} (only task1 should be enqueued)",
        ready_queue.len()
    );

    // Verify only task with met dependencies was enqueued
    assert_eq!(ready_queue.len(), 1);
    let queued_task_id = ready_queue.pop().unwrap();
    assert_eq!(queued_task_id, "task1");

    // Both tasks should be Pending
    let task1 = storage.get("task1").await?.unwrap();
    let task2 = storage.get("task2").await?.unwrap();
    assert_eq!(task1.status, TaskStatus::Pending);
    assert_eq!(task2.status, TaskStatus::Pending);

    info!("Task1 (met deps) was enqueued, Task2 (unmet deps) remains pending");

    Ok(())
}

// Helper functions
async fn create_and_store_running_task(
    storage: &Arc<dyn Storage>,
    task_id: &str,
    job_id: &str,
) -> Result<()> {
    let mut task = create_test_task(task_id, job_id);
    task.status = TaskStatus::InProgress;
    storage.put(&task).await?;
    Ok(())
}

fn create_test_task(task_id: &str, job_id: &str) -> Task {
    Task {
        job_id: job_id.to_string(),
        task_id: task_id.to_string(),
        parent_task_id: None,
        acceptance_criteria: None,
        description: format!("Test task {}", task_id),
        status: TaskStatus::Pending,
        status_reason: None,
        agent: "test_agent".to_string(),
        dependencies: vec![],
        input: json!({"test": true}),
        output: TaskOutput {
            success: false,
            data: None,
            error: None,
        },
        created_at: Utc::now().naive_utc(),
        updated_at: Utc::now().naive_utc(),
        timeout: None,
        max_retries: 3,
        retry_count: 0,
        task_type: TaskType::Task,
        summary: None,
    }
}
