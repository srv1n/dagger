use anyhow::Result;
use dashmap::{DashMap, DashSet};
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::storage::Storage;
use crate::ready_queue::ReadyQueue;
use crate::model::{Task, TaskStatus};

/// Scheduler manages task dependencies and scheduling
pub struct Scheduler {
    /// Storage backend for task persistence
    storage: Arc<dyn Storage>,
    
    /// Ready queue for tasks that are ready to execute
    ready_queue: Arc<ReadyQueue<String>>,
    
    /// Index mapping task_id to dependent task_ids
    /// If task A depends on task B, then dependency_index[B] contains A
    dependency_index: Arc<DashMap<String, DashSet<String>>>,
    
    /// Reverse index: mapping task_id to its dependencies
    /// If task A depends on task B, then task_dependencies[A] contains B
    task_dependencies: Arc<DashMap<String, DashSet<String>>>,
}

impl Scheduler {
    /// Create a new Scheduler instance
    pub fn new(storage: Arc<dyn Storage>, ready_queue: Arc<ReadyQueue<String>>) -> Self {
        Self {
            storage,
            ready_queue,
            dependency_index: Arc::new(DashMap::new()),
            task_dependencies: Arc::new(DashMap::new()),
        }
    }

    /// Handle task status changes and check if dependent tasks can be scheduled
    pub async fn on_status_change(&self, task_id: &str, new_status: TaskStatus) -> Result<()> {
        debug!("Processing status change for task {} to {:?}", task_id, new_status);

        // Only process completed or failed tasks
        match new_status {
            TaskStatus::Completed | TaskStatus::Failed => {
                // Get all tasks that depend on this task
                if let Some(dependent_tasks) = self.dependency_index.get(task_id) {
                    let dependents: Vec<String> = dependent_tasks.iter().map(|s| s.clone()).collect();
                    
                    debug!("Task {} has {} dependent tasks", task_id, dependents.len());
                    
                    // Check each dependent task
                    for dependent_task_id in dependents {
                        self.check_and_schedule_task(&dependent_task_id).await?;
                    }
                }
                
                // Clean up the completed/failed task from indices
                self.remove_task_from_indices(task_id);
            }
            _ => {
                // For other status changes, we don't need to check dependencies
                debug!("Status change to {:?} for task {} - no dependency checks needed", new_status, task_id);
            }
        }

        Ok(())
    }

    /// Check if a task's dependencies are met and schedule it if ready
    async fn check_and_schedule_task(&self, task_id: &str) -> Result<()> {
        // Get the task from storage
        let task = match self.storage.get(task_id).await? {
            Some(task) => task,
            None => {
                warn!("Task {} not found in storage", task_id);
                return Ok(());
            }
        };

        // Skip if task is not pending
        if task.status != TaskStatus::Pending {
            debug!("Task {} is not pending (status: {:?}), skipping", task_id, task.status);
            return Ok(());
        }

        // Check if all dependencies are met
        if self.dependencies_met(&task).await? {
            info!("All dependencies met for task {}, pushing to ready queue", task_id);
            
            // Try to push to ready queue
            if self.ready_queue.push(task_id.to_string()) {
                debug!("Successfully pushed task {} to ready queue", task_id);
            } else {
                warn!("Failed to push task {} to ready queue (queue full)", task_id);
                // The task remains pending and will be retried later
            }
        } else {
            debug!("Dependencies not yet met for task {}", task_id);
        }

        Ok(())
    }

    /// Check if all dependencies for a task are satisfied
    pub async fn dependencies_met(&self, task: &Task) -> Result<bool> {
        if task.dependencies.is_empty() {
            return Ok(true);
        }

        debug!("Checking {} dependencies for task {}", task.dependencies.len(), task.task_id);

        for dep_id in &task.dependencies {
            // Get dependency task status
            let dep_status = match self.storage.get_status(dep_id).await? {
                Some(status) => status,
                None => {
                    warn!("Dependency {} not found for task {}", dep_id, task.task_id);
                    return Ok(false);
                }
            };

            // Check if dependency is satisfied
            match dep_status {
                TaskStatus::Completed | TaskStatus::Accepted => {
                    debug!("Dependency {} is satisfied (status: {:?})", dep_id, dep_status);
                }
                _ => {
                    debug!("Dependency {} is not satisfied (status: {:?})", dep_id, dep_status);
                    return Ok(false);
                }
            }
        }

        Ok(true)
    }

    /// Add a task to the dependency tracking system
    pub async fn add_task(&self, task: &Task) -> Result<()> {
        debug!("Adding task {} to scheduler with {} dependencies", 
               task.task_id, task.dependencies.len());

        // If task has no dependencies and is pending, add to ready queue immediately
        if task.dependencies.is_empty() && task.status == TaskStatus::Pending {
            if self.ready_queue.push(task.task_id.clone()) {
                info!("Task {} has no dependencies, pushed to ready queue", task.task_id);
            } else {
                warn!("Failed to push task {} to ready queue (queue full)", task.task_id);
            }
            return Ok(());
        }

        // Add task dependencies to the task_dependencies index
        if !task.dependencies.is_empty() {
            let deps = self.task_dependencies
                .entry(task.task_id.clone())
                .or_insert_with(DashSet::new);
            
            for dep_id in &task.dependencies {
                deps.insert(dep_id.clone());
                
                // Add to dependency index (reverse mapping)
                let dependents = self.dependency_index
                    .entry(dep_id.clone())
                    .or_insert_with(DashSet::new);
                dependents.insert(task.task_id.clone());
            }
        }

        Ok(())
    }

    /// Remove a task from all dependency indices
    pub fn remove_task_from_indices(&self, task_id: &str) {
        debug!("Removing task {} from dependency indices", task_id);

        // Remove from dependency index (tasks that depend on this one)
        self.dependency_index.remove(task_id);

        // Remove from task_dependencies
        if let Some((_, deps)) = self.task_dependencies.remove(task_id) {
            // Also remove this task from the dependency_index for each of its dependencies
            for dep_id in deps.iter() {
                if let Some(dependents) = self.dependency_index.get_mut(&dep_id.clone()) {
                    dependents.remove(task_id);
                }
            }
        }
    }

    /// Batch check dependencies for multiple tasks
    /// Returns a vector of task IDs that have all dependencies met
    pub async fn batch_check_dependencies(&self, task_ids: &[String]) -> Result<Vec<String>> {
        let mut ready_tasks = Vec::new();

        for task_id in task_ids {
            // Get the task
            let task = match self.storage.get(task_id).await? {
                Some(task) => task,
                None => continue,
            };

            // Skip non-pending tasks
            if task.status != TaskStatus::Pending {
                continue;
            }

            // Check dependencies
            if self.dependencies_met(&task).await? {
                ready_tasks.push(task_id.clone());
            }
        }

        debug!("Batch dependency check: {} of {} tasks are ready", 
               ready_tasks.len(), task_ids.len());

        Ok(ready_tasks)
    }

    /// Get all tasks that depend on a given task
    pub fn get_dependent_tasks(&self, task_id: &str) -> Vec<String> {
        self.dependency_index
            .get(task_id)
            .map(|deps| deps.iter().map(|s| s.clone()).collect())
            .unwrap_or_default()
    }

    /// Get all dependencies for a given task
    pub fn get_task_dependencies(&self, task_id: &str) -> Vec<String> {
        self.task_dependencies
            .get(task_id)
            .map(|deps| deps.iter().map(|s| s.clone()).collect())
            .unwrap_or_default()
    }

    /// Clear all dependency indices
    pub fn clear_indices(&self) {
        self.dependency_index.clear();
        self.task_dependencies.clear();
    }

    /// Initialize scheduler with existing tasks from storage
    pub async fn initialize_from_storage(&self, job_id: &str) -> Result<()> {
        info!("Initializing scheduler for job {}", job_id);

        // Get all tasks for the job
        let tasks = self.storage.list_tasks_by_job(job_id).await?;
        
        debug!("Found {} tasks for job {}", tasks.len(), job_id);

        // Add all tasks to the scheduler
        for task in &tasks {
            self.add_task(task).await?;
        }

        // Check all pending tasks for readiness
        let pending_tasks: Vec<String> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Pending)
            .map(|t| t.task_id.clone())
            .collect();

        if !pending_tasks.is_empty() {
            let ready_tasks = self.batch_check_dependencies(&pending_tasks).await?;
            
            // Push ready tasks to the queue
            for task_id in ready_tasks {
                if !self.ready_queue.push(task_id.clone()) {
                    warn!("Failed to push task {} to ready queue during initialization", task_id);
                }
            }
        }

        info!("Scheduler initialization complete for job {}", job_id);
        Ok(())
    }

    /// Get statistics about the scheduler state
    pub fn get_stats(&self) -> SchedulerStats {
        SchedulerStats {
            dependency_index_size: self.dependency_index.len(),
            task_dependencies_size: self.task_dependencies.len(),
            ready_queue_size: self.ready_queue.len(),
            ready_queue_capacity: self.ready_queue.capacity(),
        }
    }
}

/// Statistics about the scheduler state
#[derive(Debug, Clone)]
pub struct SchedulerStats {
    pub dependency_index_size: usize,
    pub task_dependencies_size: usize,
    pub ready_queue_size: usize,
    pub ready_queue_capacity: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use chrono::Utc;
    use serde_json::json;
    use crate::model::{TaskOutput, TaskType};
    use crate::storage::SledStorage;

    async fn create_test_scheduler() -> (Scheduler, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(SledStorage::new(temp_dir.path()).unwrap()) as Arc<dyn Storage>;
        let ready_queue = Arc::new(ReadyQueue::new(100));
        let scheduler = Scheduler::new(storage, ready_queue);
        (scheduler, temp_dir)
    }

    fn create_test_task(task_id: &str, dependencies: Vec<String>, status: TaskStatus) -> Task {
        Task {
            job_id: "test_job".to_string(),
            task_id: task_id.to_string(),
            parent_task_id: None,
            acceptance_criteria: None,
            description: "Test task".to_string(),
            status,
            status_reason: None,
            agent: "test_agent".to_string(),
            dependencies,
            input: json!({}),
            output: TaskOutput {
                success: false,
                data: None,
                error: None,
            },
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            timeout: None,
            max_retries: 0,
            retry_count: 0,
            task_type: TaskType::Task,
            summary: None,
        }
    }

    #[tokio::test]
    async fn test_task_with_no_dependencies() {
        let (scheduler, _temp_dir) = create_test_scheduler().await;
        
        // Create a task with no dependencies
        let task = create_test_task("task1", vec![], TaskStatus::Pending);
        
        // Store the task
        scheduler.storage.put(&task).await.unwrap();
        
        // Add task to scheduler
        scheduler.add_task(&task).await.unwrap();
        
        // Task should be in ready queue
        assert_eq!(scheduler.ready_queue.len(), 1);
        assert_eq!(scheduler.ready_queue.pop(), Some("task1".to_string()));
    }

    #[tokio::test]
    async fn test_dependency_tracking() {
        let (scheduler, _temp_dir) = create_test_scheduler().await;
        
        // Create tasks with dependencies
        let task_a = create_test_task("task_a", vec![], TaskStatus::Pending);
        let task_b = create_test_task("task_b", vec!["task_a".to_string()], TaskStatus::Pending);
        let task_c = create_test_task("task_c", vec!["task_b".to_string()], TaskStatus::Pending);
        
        // Store tasks
        scheduler.storage.put(&task_a).await.unwrap();
        scheduler.storage.put(&task_b).await.unwrap();
        scheduler.storage.put(&task_c).await.unwrap();
        
        // Add tasks to scheduler
        scheduler.add_task(&task_a).await.unwrap();
        scheduler.add_task(&task_b).await.unwrap();
        scheduler.add_task(&task_c).await.unwrap();
        
        // Only task_a should be in ready queue
        assert_eq!(scheduler.ready_queue.len(), 1);
        assert_eq!(scheduler.ready_queue.pop(), Some("task_a".to_string()));
        
        // Check dependency indices
        assert_eq!(scheduler.get_dependent_tasks("task_a"), vec!["task_b".to_string()]);
        assert_eq!(scheduler.get_dependent_tasks("task_b"), vec!["task_c".to_string()]);
        assert_eq!(scheduler.get_task_dependencies("task_b"), vec!["task_a".to_string()]);
        assert_eq!(scheduler.get_task_dependencies("task_c"), vec!["task_b".to_string()]);
    }

    #[tokio::test]
    async fn test_on_status_change() {
        let (scheduler, _temp_dir) = create_test_scheduler().await;
        
        // Create dependency chain: A -> B -> C
        let task_a = create_test_task("task_a", vec![], TaskStatus::InProgress);
        let task_b = create_test_task("task_b", vec!["task_a".to_string()], TaskStatus::Pending);
        let task_c = create_test_task("task_c", vec!["task_b".to_string()], TaskStatus::Pending);
        
        // Store tasks
        scheduler.storage.put(&task_a).await.unwrap();
        scheduler.storage.put(&task_b).await.unwrap();
        scheduler.storage.put(&task_c).await.unwrap();
        
        // Add tasks to scheduler
        scheduler.add_task(&task_a).await.unwrap();
        scheduler.add_task(&task_b).await.unwrap();
        scheduler.add_task(&task_c).await.unwrap();
        
        // Initially, ready queue should be empty (task_a is already InProgress)
        assert_eq!(scheduler.ready_queue.len(), 0);
        
        // Complete task_a
        scheduler.storage.update_status("task_a", TaskStatus::Completed).await.unwrap();
        scheduler.on_status_change("task_a", TaskStatus::Completed).await.unwrap();
        
        // task_b should now be in ready queue
        assert_eq!(scheduler.ready_queue.len(), 1);
        assert_eq!(scheduler.ready_queue.pop(), Some("task_b".to_string()));
        
        // Complete task_b
        scheduler.storage.update_status("task_b", TaskStatus::Completed).await.unwrap();
        scheduler.on_status_change("task_b", TaskStatus::Completed).await.unwrap();
        
        // task_c should now be in ready queue
        assert_eq!(scheduler.ready_queue.len(), 1);
        assert_eq!(scheduler.ready_queue.pop(), Some("task_c".to_string()));
    }

    #[tokio::test]
    async fn test_multiple_dependencies() {
        let (scheduler, _temp_dir) = create_test_scheduler().await;
        
        // Create tasks where C depends on both A and B
        let task_a = create_test_task("task_a", vec![], TaskStatus::Pending);
        let task_b = create_test_task("task_b", vec![], TaskStatus::Pending);
        let task_c = create_test_task("task_c", vec!["task_a".to_string(), "task_b".to_string()], TaskStatus::Pending);
        
        // Store tasks
        scheduler.storage.put(&task_a).await.unwrap();
        scheduler.storage.put(&task_b).await.unwrap();
        scheduler.storage.put(&task_c).await.unwrap();
        
        // Add tasks to scheduler
        scheduler.add_task(&task_a).await.unwrap();
        scheduler.add_task(&task_b).await.unwrap();
        scheduler.add_task(&task_c).await.unwrap();
        
        // Both A and B should be in ready queue
        assert_eq!(scheduler.ready_queue.len(), 2);
        
        // Complete only task_a
        scheduler.storage.update_status("task_a", TaskStatus::Completed).await.unwrap();
        scheduler.on_status_change("task_a", TaskStatus::Completed).await.unwrap();
        
        // task_c should not be ready yet (still waiting for task_b)
        let ready_tasks: Vec<String> = (0..scheduler.ready_queue.len())
            .filter_map(|_| scheduler.ready_queue.pop())
            .collect();
        assert!(!ready_tasks.contains(&"task_c".to_string()));
        
        // Complete task_b
        scheduler.storage.update_status("task_b", TaskStatus::Completed).await.unwrap();
        scheduler.on_status_change("task_b", TaskStatus::Completed).await.unwrap();
        
        // Now task_c should be ready
        assert_eq!(scheduler.ready_queue.pop(), Some("task_c".to_string()));
    }

    #[tokio::test]
    async fn test_failed_dependency() {
        let (scheduler, _temp_dir) = create_test_scheduler().await;
        
        // Create tasks with dependencies
        let task_a = create_test_task("task_a", vec![], TaskStatus::InProgress);
        let task_b = create_test_task("task_b", vec!["task_a".to_string()], TaskStatus::Pending);
        
        // Store tasks
        scheduler.storage.put(&task_a).await.unwrap();
        scheduler.storage.put(&task_b).await.unwrap();
        
        // Add tasks to scheduler
        scheduler.add_task(&task_a).await.unwrap();
        scheduler.add_task(&task_b).await.unwrap();
        
        // Fail task_a
        scheduler.storage.update_status("task_a", TaskStatus::Failed).await.unwrap();
        scheduler.on_status_change("task_a", TaskStatus::Failed).await.unwrap();
        
        // task_b should not be in ready queue (dependency failed)
        assert_eq!(scheduler.ready_queue.len(), 0);
    }

    #[tokio::test]
    async fn test_batch_dependency_check() {
        let (scheduler, _temp_dir) = create_test_scheduler().await;
        
        // Create multiple independent tasks
        let tasks: Vec<Task> = (0..5)
            .map(|i| create_test_task(&format!("task_{}", i), vec![], TaskStatus::Pending))
            .collect();
        
        // Store and add tasks
        for task in &tasks {
            scheduler.storage.put(task).await.unwrap();
            // Don't add to scheduler yet to test batch check
        }
        
        // Batch check should find all tasks ready
        let task_ids: Vec<String> = tasks.iter().map(|t| t.task_id.clone()).collect();
        let ready_tasks = scheduler.batch_check_dependencies(&task_ids).await.unwrap();
        
        assert_eq!(ready_tasks.len(), 5);
    }

    #[tokio::test]
    async fn test_initialize_from_storage() {
        let (scheduler, _temp_dir) = create_test_scheduler().await;
        
        // Create and store tasks
        let task_a = create_test_task("task_a", vec![], TaskStatus::Completed);
        let task_b = create_test_task("task_b", vec!["task_a".to_string()], TaskStatus::Pending);
        let task_c = create_test_task("task_c", vec!["task_b".to_string()], TaskStatus::Pending);
        
        scheduler.storage.put(&task_a).await.unwrap();
        scheduler.storage.put(&task_b).await.unwrap();
        scheduler.storage.put(&task_c).await.unwrap();
        
        // Initialize scheduler from storage
        scheduler.initialize_from_storage("test_job").await.unwrap();
        
        // task_b should be ready (task_a is already completed)
        assert_eq!(scheduler.ready_queue.len(), 1);
        assert_eq!(scheduler.ready_queue.pop(), Some("task_b".to_string()));
    }

    #[tokio::test]
    async fn test_scheduler_stats() {
        let (scheduler, _temp_dir) = create_test_scheduler().await;
        
        // Add some tasks
        let task_a = create_test_task("task_a", vec![], TaskStatus::Pending);
        let task_b = create_test_task("task_b", vec!["task_a".to_string()], TaskStatus::Pending);
        
        scheduler.storage.put(&task_a).await.unwrap();
        scheduler.storage.put(&task_b).await.unwrap();
        
        scheduler.add_task(&task_a).await.unwrap();
        scheduler.add_task(&task_b).await.unwrap();
        
        let stats = scheduler.get_stats();
        assert_eq!(stats.dependency_index_size, 1); // task_a has task_b as dependent
        assert_eq!(stats.task_dependencies_size, 1); // task_b has dependencies
        assert_eq!(stats.ready_queue_size, 1); // task_a is ready
        assert_eq!(stats.ready_queue_capacity, 100);
    }
}