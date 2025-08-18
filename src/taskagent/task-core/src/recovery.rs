use anyhow::Result;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::model::{Task, TaskStatus};
use crate::ready_queue::ReadyQueue;
use crate::scheduler::Scheduler;
use crate::storage::Storage;

/// Durability mode for task recovery
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurabilityMode {
    /// Tasks are automatically retried on recovery (default)
    BestEffort,
    /// Tasks are paused on recovery and require manual intervention
    AtMostOnce,
}

/// Recovery configuration
pub struct RecoveryConfig {
    /// Default durability mode for tasks without explicit configuration
    pub default_durability: DurabilityMode,
    /// Whether to automatically start recovery on initialization
    pub auto_recover: bool,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            default_durability: DurabilityMode::BestEffort,
            auto_recover: true,
        }
    }
}

/// Handles recovery of tasks after a restart or crash
pub struct Recovery {
    storage: Arc<dyn Storage>,
    ready_queue: Arc<ReadyQueue<String>>,
    scheduler: Arc<Scheduler>,
    config: RecoveryConfig,
}

impl Recovery {
    /// Create a new Recovery instance
    pub fn new(
        storage: Arc<dyn Storage>,
        ready_queue: Arc<ReadyQueue<String>>,
        scheduler: Arc<Scheduler>,
        config: RecoveryConfig,
    ) -> Self {
        Self {
            storage,
            ready_queue,
            scheduler,
            config,
        }
    }

    /// Recover tasks that were running when the system stopped
    ///
    /// This function:
    /// 1. Scans for all tasks with InProgress status
    /// 2. Based on durability mode:
    ///    - BestEffort: Resets status to Pending and re-enqueues
    ///    - AtMostOnce: Updates status to Blocked with reason
    /// 3. Re-enqueues recoverable tasks to the ready queue if dependencies are met
    pub async fn recover(&self) -> Result<RecoveryStats> {
        info!("Starting task recovery process");
        let mut stats = RecoveryStats::default();

        // Get all running tasks
        let running_tasks = self.storage.list_running().await?;
        stats.total_running = running_tasks.len();

        info!("Found {} tasks in InProgress state", running_tasks.len());

        for task in running_tasks {
            let task_id = task.task_id.clone();
            match self.recover_task(task).await {
                Ok(outcome) => match outcome {
                    RecoveryOutcome::Retried => stats.retried += 1,
                    RecoveryOutcome::Paused => stats.paused += 1,
                    RecoveryOutcome::Failed => stats.failed += 1,
                },
                Err(e) => {
                    error!("Failed to recover task {}: {}", task_id, e);
                    stats.failed += 1;
                }
            }
        }

        info!(
            "Recovery completed: {} retried, {} paused, {} failed out of {} total",
            stats.retried, stats.paused, stats.failed, stats.total_running
        );

        Ok(stats)
    }

    /// Recover a single task based on its durability mode
    async fn recover_task(&self, task: Task) -> Result<RecoveryOutcome> {
        let task_id = &task.task_id;
        let durability = self.get_task_durability(&task);

        info!(
            "Recovering task {} with durability mode {:?}",
            task_id, durability
        );

        match durability {
            DurabilityMode::BestEffort => self.recover_best_effort(task).await,
            DurabilityMode::AtMostOnce => self.recover_at_most_once(task).await,
        }
    }

    /// Recover a task with BestEffort durability - retry automatically
    async fn recover_best_effort(&self, task: Task) -> Result<RecoveryOutcome> {
        let task_id = task.task_id.clone();

        debug!(
            "Recovering task {} with BestEffort mode - will retry",
            task_id
        );

        // Update task status back to Pending
        self.storage
            .update_status(&task_id, TaskStatus::Pending)
            .await?;

        info!("Reset task {} status from InProgress to Pending", task_id);

        // Check if dependencies are met and enqueue if ready
        if self.should_enqueue_task(&task).await? {
            if self.ready_queue.push(task_id.clone()) {
                info!("Successfully enqueued task {} for retry", task_id);
            } else {
                warn!("Failed to enqueue task {} - ready queue is full", task_id);
                // Task remains Pending and will be picked up later
            }
        } else {
            debug!("Task {} has unmet dependencies, will wait", task_id);
        }

        Ok(RecoveryOutcome::Retried)
    }

    /// Recover a task with AtMostOnce durability - pause and require manual intervention
    async fn recover_at_most_once(&self, task: Task) -> Result<RecoveryOutcome> {
        let task_id = task.task_id.clone();

        debug!(
            "Recovering task {} with AtMostOnce mode - will pause",
            task_id
        );

        // Update task status to Blocked with recovery reason
        self.storage
            .update_status(&task_id, TaskStatus::Blocked)
            .await?;

        // Update the task with a status reason
        // Note: This would ideally be done atomically with the status update
        // For now, we'll do a separate update
        if let Some(mut updated_task) = self.storage.get(&task_id).await? {
            updated_task.status_reason = Some(
                "Task was in progress during system restart. Manual intervention required due to AtMostOnce durability.".to_string()
            );
            self.storage.put(&updated_task).await?;
        }

        warn!(
            "Task {} paused due to AtMostOnce durability - manual intervention required",
            task_id
        );

        Ok(RecoveryOutcome::Paused)
    }

    /// Determine the durability mode for a task
    /// In the future, this could read from task metadata or configuration
    fn get_task_durability(&self, _task: &Task) -> DurabilityMode {
        // For now, use the default durability mode
        // In the future, this could check task.metadata for a durability field
        self.config.default_durability
    }

    /// Check if a task should be enqueued based on its dependencies
    async fn should_enqueue_task(&self, task: &Task) -> Result<bool> {
        if task.dependencies.is_empty() {
            return Ok(true);
        }

        // Check if all dependencies are completed
        for dep_id in &task.dependencies {
            match self.storage.get_status(dep_id).await? {
                Some(TaskStatus::Completed) => continue,
                Some(status) => {
                    debug!(
                        "Task {} dependency {} is not completed (status: {:?})",
                        task.task_id, dep_id, status
                    );
                    return Ok(false);
                }
                None => {
                    warn!(
                        "Task {} dependency {} not found in storage",
                        task.task_id, dep_id
                    );
                    return Ok(false);
                }
            }
        }

        Ok(true)
    }
}

/// Statistics from the recovery process
#[derive(Debug, Default)]
pub struct RecoveryStats {
    /// Total number of tasks that were running
    pub total_running: usize,
    /// Number of tasks retried (BestEffort)
    pub retried: usize,
    /// Number of tasks paused (AtMostOnce)
    pub paused: usize,
    /// Number of tasks that failed to recover
    pub failed: usize,
}

/// Outcome of recovering a single task
#[derive(Debug)]
enum RecoveryOutcome {
    /// Task was retried (BestEffort)
    Retried,
    /// Task was paused (AtMostOnce)
    Paused,
    /// Recovery failed
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{TaskOutput, TaskType};
    use crate::sqlite_storage::SqliteStorage;
    use chrono::Utc;
    use serde_json::json;
    use tempfile::TempDir;

    async fn create_test_recovery() -> (Recovery, Arc<dyn Storage>, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let storage = Arc::new(SqliteStorage::open(db_path).await.unwrap()) as Arc<dyn Storage>;
        let ready_queue = Arc::new(ReadyQueue::new(100));
        let scheduler = Arc::new(Scheduler::new(storage.clone(), ready_queue.clone()));

        let recovery = Recovery::new(
            storage.clone(),
            ready_queue,
            scheduler,
            RecoveryConfig::default(),
        );

        (recovery, storage, temp_dir)
    }

    fn create_running_task(task_id: &str, job_id: &str) -> Task {
        Task {
            job_id: job_id.to_string(),
            task_id: task_id.to_string(),
            parent_task_id: None,
            acceptance_criteria: None,
            description: "Test task".to_string(),
            status: TaskStatus::InProgress,
            status_reason: None,
            agent: "test_agent".to_string(),
            dependencies: vec![],
            input: json!({}),
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

    #[tokio::test]
    async fn test_recover_with_best_effort() {
        let (recovery, storage, _temp_dir) = create_test_recovery().await;

        // Create and store a running task
        let task = create_running_task("task1", "job1");
        storage.put(&task).await.unwrap();

        // Run recovery
        let stats = recovery.recover().await.unwrap();

        // Verify stats
        assert_eq!(stats.total_running, 1);
        assert_eq!(stats.retried, 1);
        assert_eq!(stats.paused, 0);
        assert_eq!(stats.failed, 0);

        // Verify task status was reset to Pending
        let recovered_task = storage.get("task1").await.unwrap().unwrap();
        assert_eq!(recovered_task.status, TaskStatus::Pending);

        // Verify task was enqueued
        assert_eq!(recovery.ready_queue.len(), 1);
    }

    #[tokio::test]
    async fn test_recover_with_at_most_once() {
        let (mut recovery, storage, _temp_dir) = create_test_recovery().await;

        // Change config to AtMostOnce
        recovery.config.default_durability = DurabilityMode::AtMostOnce;

        // Create and store a running task
        let task = create_running_task("task1", "job1");
        storage.put(&task).await.unwrap();

        // Run recovery
        let stats = recovery.recover().await.unwrap();

        // Verify stats
        assert_eq!(stats.total_running, 1);
        assert_eq!(stats.retried, 0);
        assert_eq!(stats.paused, 1);
        assert_eq!(stats.failed, 0);

        // Verify task status was set to Blocked
        let recovered_task = storage.get("task1").await.unwrap().unwrap();
        assert_eq!(recovered_task.status, TaskStatus::Blocked);
        assert!(recovered_task.status_reason.is_some());

        // Verify task was NOT enqueued
        assert_eq!(recovery.ready_queue.len(), 0);
    }

    #[tokio::test]
    async fn test_recover_with_dependencies() {
        let (recovery, storage, _temp_dir) = create_test_recovery().await;

        // Create a completed dependency
        let mut dep_task = create_running_task("dep1", "job1");
        dep_task.status = TaskStatus::Completed;
        storage.put(&dep_task).await.unwrap();

        // Create a running task with dependency
        let mut task = create_running_task("task1", "job1");
        task.dependencies = vec!["dep1".to_string()];
        storage.put(&task).await.unwrap();

        // Run recovery
        let stats = recovery.recover().await.unwrap();

        // Verify task was retried since dependency is met
        assert_eq!(stats.retried, 1);
        assert_eq!(recovery.ready_queue.len(), 1);
    }

    #[tokio::test]
    async fn test_recover_with_unmet_dependencies() {
        let (recovery, storage, _temp_dir) = create_test_recovery().await;

        // Create a pending dependency
        let mut dep_task = create_running_task("dep1", "job1");
        dep_task.status = TaskStatus::Pending;
        storage.put(&dep_task).await.unwrap();

        // Create a running task with dependency
        let mut task = create_running_task("task1", "job1");
        task.dependencies = vec!["dep1".to_string()];
        storage.put(&task).await.unwrap();

        // Run recovery
        let stats = recovery.recover().await.unwrap();

        // Verify task was retried but not enqueued due to unmet dependency
        assert_eq!(stats.retried, 1);
        assert_eq!(recovery.ready_queue.len(), 0);

        // Task should be Pending
        let recovered_task = storage.get("task1").await.unwrap().unwrap();
        assert_eq!(recovered_task.status, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn test_recover_multiple_tasks() {
        let (recovery, storage, _temp_dir) = create_test_recovery().await;

        // Create multiple running tasks
        for i in 1..=5 {
            let task = create_running_task(&format!("task{}", i), "job1");
            storage.put(&task).await.unwrap();
        }

        // Run recovery
        let stats = recovery.recover().await.unwrap();

        // Verify all tasks were recovered
        assert_eq!(stats.total_running, 5);
        assert_eq!(stats.retried, 5);
        assert_eq!(stats.failed, 0);

        // All tasks should be enqueued
        assert_eq!(recovery.ready_queue.len(), 5);
    }

    #[tokio::test]
    async fn test_recovery_with_no_running_tasks() {
        let (recovery, _storage, _temp_dir) = create_test_recovery().await;

        // Run recovery with no running tasks
        let stats = recovery.recover().await.unwrap();

        // Verify no tasks were processed
        assert_eq!(stats.total_running, 0);
        assert_eq!(stats.retried, 0);
        assert_eq!(stats.paused, 0);
        assert_eq!(stats.failed, 0);
    }
}
