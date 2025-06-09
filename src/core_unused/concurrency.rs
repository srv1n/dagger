use crate::core::errors::{DaggerError, Result};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Notify, RwLock, Semaphore};
use tokio::time::timeout;
use tracing::{debug, warn};

/// Lock acquisition ordering to prevent deadlocks
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LockOrder {
    Config = 0,
    Registry = 1,
    Tasks = 2,
    Cache = 3,
    Channels = 4,
    ExecutionTree = 5,
}

/// Lock manager to enforce acquisition ordering
#[derive(Debug)]
pub struct LockManager {
    acquired_locks: Arc<RwLock<Vec<LockOrder>>>,
}

impl LockManager {
    pub fn new() -> Self {
        Self {
            acquired_locks: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn can_acquire(&self, lock_order: LockOrder) -> Result<()> {
        let acquired = self.acquired_locks.read().await;
        
        // Check if we're trying to acquire a lock in the wrong order
        if let Some(&last_acquired) = acquired.last() {
            if lock_order <= last_acquired {
                return Err(DaggerError::concurrency(format!(
                    "Lock ordering violation: trying to acquire {:?} after {:?}",
                    lock_order, last_acquired
                )));
            }
        }
        
        Ok(())
    }

    pub async fn acquire(&self, lock_order: LockOrder) -> Result<()> {
        self.can_acquire(lock_order).await?;
        
        let mut acquired = self.acquired_locks.write().await;
        acquired.push(lock_order);
        Ok(())
    }

    pub async fn release(&self, lock_order: LockOrder) {
        let mut acquired = self.acquired_locks.write().await;
        if let Some(pos) = acquired.iter().position(|&x| x == lock_order) {
            acquired.remove(pos);
        }
    }
}

/// Atomic task status for lock-free operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum TaskStatus {
    Pending = 0,
    InProgress = 1,
    Completed = 2,
    Failed = 3,
    Blocked = 4,
    Accepted = 5,
    Rejected = 6,
}

impl From<u8> for TaskStatus {
    fn from(value: u8) -> Self {
        match value {
            0 => TaskStatus::Pending,
            1 => TaskStatus::InProgress,
            2 => TaskStatus::Completed,
            3 => TaskStatus::Failed,
            4 => TaskStatus::Blocked,
            5 => TaskStatus::Accepted,
            6 => TaskStatus::Rejected,
            _ => TaskStatus::Failed, // Default to failed for invalid states
        }
    }
}

/// Atomic task state for concurrent access
#[derive(Debug)]
pub struct AtomicTaskState {
    pub id: String,
    pub status: AtomicU8,
    pub last_updated: AtomicU64,
    pub attempts: AtomicU32,
    pub agent: RwLock<String>,
    pub job_id: RwLock<String>,
}

impl AtomicTaskState {
    pub fn new(id: String, job_id: String, agent: String) -> Self {
        Self {
            id,
            status: AtomicU8::new(TaskStatus::Pending as u8),
            last_updated: AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            ),
            attempts: AtomicU32::new(0),
            agent: RwLock::new(agent),
            job_id: RwLock::new(job_id),
        }
    }

    /// Atomically compare and set status
    pub fn compare_and_set_status(
        &self,
        expected: TaskStatus,
        new: TaskStatus,
    ) -> std::result::Result<TaskStatus, TaskStatus> {
        let expected_u8 = expected as u8;
        let new_u8 = new as u8;
        
        match self.status.compare_exchange(
            expected_u8,
            new_u8,
            Ordering::SeqCst,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                self.touch();
                Ok(new)
            }
            Err(actual) => Err(TaskStatus::from(actual)),
        }
    }

    /// Get current status
    pub fn get_status(&self) -> TaskStatus {
        TaskStatus::from(self.status.load(Ordering::Relaxed))
    }

    /// Set status unconditionally
    pub fn set_status(&self, status: TaskStatus) -> TaskStatus {
        let old = self.status.swap(status as u8, Ordering::SeqCst);
        self.touch();
        TaskStatus::from(old)
    }

    /// Increment attempt counter
    pub fn increment_attempts(&self) -> u32 {
        self.attempts.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Get attempt count
    pub fn get_attempts(&self) -> u32 {
        self.attempts.load(Ordering::Relaxed)
    }

    /// Update last modified timestamp
    pub fn touch(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        self.last_updated.store(now, Ordering::Relaxed);
    }

    /// Get last updated timestamp
    pub fn get_last_updated(&self) -> u64 {
        self.last_updated.load(Ordering::Relaxed)
    }

    /// Check if task is in a terminal state
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.get_status(),
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Rejected
        )
    }

    /// Check if task can be claimed by an agent
    pub fn can_claim(&self) -> bool {
        matches!(self.get_status(), TaskStatus::Pending)
    }

    /// Attempt to claim the task
    pub fn try_claim(&self) -> bool {
        self.compare_and_set_status(TaskStatus::Pending, TaskStatus::InProgress).is_ok()
    }
}

/// Event-driven notification system for task scheduling
#[derive(Debug)]
pub struct TaskNotificationSystem {
    /// Notifies when new tasks are ready
    ready_tasks: Arc<Notify>,
    /// Notifies when tasks complete
    completed_tasks: Arc<Notify>,
    /// Notifies when tasks fail
    failed_tasks: Arc<Notify>,
    /// Limits concurrent task executions
    execution_semaphore: Arc<Semaphore>,
    /// Statistics
    notifications_sent: AtomicU64,
}

impl TaskNotificationSystem {
    pub fn new(max_concurrent_tasks: usize) -> Self {
        Self {
            ready_tasks: Arc::new(Notify::new()),
            completed_tasks: Arc::new(Notify::new()),
            failed_tasks: Arc::new(Notify::new()),
            execution_semaphore: Arc::new(Semaphore::new(max_concurrent_tasks)),
            notifications_sent: AtomicU64::new(0),
        }
    }

    /// Notify that a task is ready for execution
    pub fn notify_task_ready(&self) {
        self.ready_tasks.notify_one();
        self.notifications_sent.fetch_add(1, Ordering::Relaxed);
        debug!("Notified: task ready");
    }

    /// Notify that multiple tasks are ready
    pub fn notify_tasks_ready(&self, count: usize) {
        self.ready_tasks.notify_waiters();
        self.notifications_sent.fetch_add(count as u64, Ordering::Relaxed);
        debug!("Notified: {} tasks ready", count);
    }

    /// Wait for ready tasks with timeout
    pub async fn wait_for_ready_tasks(&self, timeout_duration: Duration) -> Result<()> {
        timeout(timeout_duration, self.ready_tasks.notified())
            .await
            .map_err(|_| DaggerError::timeout("wait_for_ready_tasks", timeout_duration.as_millis() as u64))?;
        Ok(())
    }

    /// Notify that a task completed
    pub fn notify_task_completed(&self) {
        self.completed_tasks.notify_one();
        self.notifications_sent.fetch_add(1, Ordering::Relaxed);
        debug!("Notified: task completed");
    }

    /// Wait for task completion
    pub async fn wait_for_task_completion(&self, timeout_duration: Duration) -> Result<()> {
        timeout(timeout_duration, self.completed_tasks.notified())
            .await
            .map_err(|_| DaggerError::timeout("wait_for_task_completion", timeout_duration.as_millis() as u64))?;
        Ok(())
    }

    /// Notify that a task failed
    pub fn notify_task_failed(&self) {
        self.failed_tasks.notify_one();
        self.notifications_sent.fetch_add(1, Ordering::Relaxed);
        debug!("Notified: task failed");
    }

    /// Acquire execution permit (rate limiting)
    pub async fn acquire_execution_permit(&self) -> Result<tokio::sync::SemaphorePermit<'_>> {
        self.execution_semaphore
            .acquire()
            .await
            .map_err(|e| DaggerError::concurrency(format!("Failed to acquire execution permit: {}", e)))
    }

    /// Try to acquire execution permit without waiting
    pub fn try_acquire_execution_permit(&self) -> Option<tokio::sync::SemaphorePermit<'_>> {
        self.execution_semaphore.try_acquire().ok()
    }

    /// Get available execution permits
    pub fn available_permits(&self) -> usize {
        self.execution_semaphore.available_permits()
    }

    /// Get notification statistics
    pub fn get_notifications_sent(&self) -> u64 {
        self.notifications_sent.load(Ordering::Relaxed)
    }
}

/// Concurrent task registry with atomic operations
#[derive(Debug)]
pub struct ConcurrentTaskRegistry {
    tasks: Arc<DashMap<String, Arc<AtomicTaskState>>>,
    tasks_by_job: Arc<DashMap<String, Vec<String>>>,
    tasks_by_status: Arc<DashMap<u8, Vec<String>>>,
    notification_system: Arc<TaskNotificationSystem>,
    task_count: AtomicU64,
}

impl ConcurrentTaskRegistry {
    pub fn new(max_concurrent_tasks: usize) -> Self {
        Self {
            tasks: Arc::new(DashMap::new()),
            tasks_by_job: Arc::new(DashMap::new()),
            tasks_by_status: Arc::new(DashMap::new()),
            notification_system: Arc::new(TaskNotificationSystem::new(max_concurrent_tasks)),
            task_count: AtomicU64::new(0),
        }
    }

    /// Add a new task atomically
    pub async fn add_task(&self, task: Arc<AtomicTaskState>) -> Result<()> {
        let task_id = task.id.clone();
        
        // Insert task
        if self.tasks.insert(task_id.clone(), task.clone()).is_some() {
            return Err(DaggerError::validation(format!("Task {} already exists", task_id)));
        }

        // Update job index
        {
            let job_id = task.job_id.read().await.clone();
            
            self.tasks_by_job
                .entry(job_id)
                .or_insert_with(Vec::new)
                .push(task_id.clone());
        }

        // Update status index
        let status = task.get_status() as u8;
        self.tasks_by_status
            .entry(status)
            .or_insert_with(Vec::new)
            .push(task_id.clone());

        self.task_count.fetch_add(1, Ordering::Relaxed);

        // Notify if task is ready
        if task.can_claim() {
            self.notification_system.notify_task_ready();
        }

        Ok(())
    }

    /// Get task by ID
    pub fn get_task(&self, task_id: &str) -> Option<Arc<AtomicTaskState>> {
        self.tasks.get(task_id).map(|entry| entry.value().clone())
    }

    /// Update task status atomically
    pub fn update_task_status(
        &self,
        task_id: &str,
        expected: TaskStatus,
        new: TaskStatus,
    ) -> Result<TaskStatus> {
        let task = self.get_task(task_id)
            .ok_or_else(|| DaggerError::task(task_id, "Task not found"))?;

        let result = task.compare_and_set_status(expected, new);
        
        match result {
            Ok(new_status) => {
                // Update status index
                self.update_status_index(task_id, expected as u8, new_status as u8);
                
                // Send notifications
                match new_status {
                    TaskStatus::Pending => self.notification_system.notify_task_ready(),
                    TaskStatus::Completed => self.notification_system.notify_task_completed(),
                    TaskStatus::Failed | TaskStatus::Rejected => self.notification_system.notify_task_failed(),
                    _ => {}
                }
                
                Ok(new_status)
            }
            Err(actual) => Err(DaggerError::concurrency(format!(
                "Task {} status update failed: expected {:?}, actual {:?}",
                task_id, expected, actual
            ))),
        }
    }

    fn update_status_index(&self, task_id: &str, old_status: u8, new_status: u8) {
        // Remove from old status
        if let Some(mut tasks) = self.tasks_by_status.get_mut(&old_status) {
            if let Some(pos) = tasks.iter().position(|id| id == task_id) {
                tasks.remove(pos);
            }
        }

        // Add to new status
        self.tasks_by_status
            .entry(new_status)
            .or_insert_with(Vec::new)
            .push(task_id.to_string());
    }

    /// Get ready tasks for execution
    pub fn get_ready_tasks(&self) -> Vec<Arc<AtomicTaskState>> {
        let pending_status = TaskStatus::Pending as u8;
        
        self.tasks_by_status
            .get(&pending_status)
            .map(|task_ids| {
                task_ids
                    .iter()
                    .filter_map(|id| self.get_task(id))
                    .filter(|task| task.can_claim())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get ready tasks for a specific job
    pub fn get_ready_tasks_for_job(&self, job_id: &str) -> Vec<Arc<AtomicTaskState>> {
        self.tasks_by_job
            .get(job_id)
            .map(|task_ids| {
                task_ids
                    .iter()
                    .filter_map(|id| self.get_task(id))
                    .filter(|task| task.can_claim())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Try to claim a task for execution
    pub fn try_claim_task(&self, task_id: &str) -> Result<bool> {
        let task = self.get_task(task_id)
            .ok_or_else(|| DaggerError::task(task_id, "Task not found"))?;

        let claimed = task.try_claim();
        
        if claimed {
            // Update status index
            self.update_status_index(task_id, TaskStatus::Pending as u8, TaskStatus::InProgress as u8);
            debug!("Task {} claimed successfully", task_id);
        }

        Ok(claimed)
    }

    /// Wait for ready tasks with timeout
    pub async fn wait_for_ready_tasks(&self, timeout_duration: Duration) -> Result<()> {
        self.notification_system.wait_for_ready_tasks(timeout_duration).await
    }

    /// Get notification system
    pub fn get_notification_system(&self) -> &TaskNotificationSystem {
        &self.notification_system
    }

    /// Get task count
    pub fn task_count(&self) -> u64 {
        self.task_count.load(Ordering::Relaxed)
    }

    /// Get task counts by status
    pub fn get_status_counts(&self) -> std::collections::HashMap<TaskStatus, usize> {
        let mut counts = std::collections::HashMap::new();
        
        for entry in self.tasks_by_status.iter() {
            let status = TaskStatus::from(*entry.key());
            let count = entry.value().len();
            counts.insert(status, count);
        }
        
        counts
    }
}

/// Resource limiter with backpressure
#[derive(Debug)]
pub struct ResourceLimiter {
    /// Maximum concurrent operations
    semaphore: Arc<Semaphore>,
    /// Current resource usage
    current_usage: Arc<AtomicU64>,
    /// Maximum resource usage
    max_usage: u64,
    /// Resource type name for error messages
    resource_name: String,
}

impl ResourceLimiter {
    pub fn new(max_concurrent: usize, max_usage: u64, resource_name: String) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            current_usage: Arc::new(AtomicU64::new(0)),
            max_usage,
            resource_name,
        }
    }

    /// Acquire a resource permit
    pub async fn acquire(&self, usage: u64) -> Result<ResourcePermit> {
        // Check resource usage limit
        let current = self.current_usage.load(Ordering::Relaxed);
        if current + usage > self.max_usage {
            return Err(DaggerError::resource_exhausted(
                &self.resource_name,
                current + usage,
                self.max_usage,
            ));
        }

        // Acquire semaphore permit
        let permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| DaggerError::concurrency(format!("Failed to acquire semaphore: {}", e)))?;

        // Update usage
        self.current_usage.fetch_add(usage, Ordering::Relaxed);

        Ok(ResourcePermit {
            _permit: permit,
            usage,
            current_usage: self.current_usage.clone(),
        })
    }

    /// Try to acquire without waiting
    pub fn try_acquire(&self, usage: u64) -> Result<Option<ResourcePermit>> {
        // Check resource usage limit
        let current = self.current_usage.load(Ordering::Relaxed);
        if current + usage > self.max_usage {
            return Err(DaggerError::resource_exhausted(
                &self.resource_name,
                current + usage,
                self.max_usage,
            ));
        }

        // Try to acquire semaphore permit
        if let Ok(permit) = self.semaphore.try_acquire() {
            self.current_usage.fetch_add(usage, Ordering::Relaxed);
            Ok(Some(ResourcePermit {
                _permit: permit,
                usage,
                current_usage: self.current_usage.clone(),
            }))
        } else {
            Ok(None)
        }
    }

    /// Get current usage
    pub fn current_usage(&self) -> u64 {
        self.current_usage.load(Ordering::Relaxed)
    }

    /// Get available permits
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }
}

/// RAII resource permit
pub struct ResourcePermit {
    _permit: tokio::sync::SemaphorePermit<'static>,
    usage: u64,
    current_usage: Arc<AtomicU64>,
}

impl Drop for ResourcePermit {
    fn drop(&mut self) {
        self.current_usage.fetch_sub(self.usage, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_atomic_task_state() {
        let task = AtomicTaskState::new(
            "test_task".to_string(),
            "test_job".to_string(),
            "test_agent".to_string(),
        );

        // Test status operations
        assert_eq!(task.get_status(), TaskStatus::Pending);
        assert!(task.can_claim());
        assert!(task.try_claim());
        assert_eq!(task.get_status(), TaskStatus::InProgress);
        assert!(!task.can_claim());

        // Test compare and set
        let result = task.compare_and_set_status(TaskStatus::InProgress, TaskStatus::Completed);
        assert!(result.is_ok());
        assert_eq!(task.get_status(), TaskStatus::Completed);
        assert!(task.is_terminal());
    }

    #[tokio::test]
    async fn test_concurrent_task_registry() {
        let registry = ConcurrentTaskRegistry::new(10);
        
        let task = Arc::new(AtomicTaskState::new(
            "test_task".to_string(),
            "test_job".to_string(),
            "test_agent".to_string(),
        ));

        // Add task
        registry.add_task(task.clone()).unwrap();
        assert_eq!(registry.task_count(), 1);

        // Get ready tasks
        let ready_tasks = registry.get_ready_tasks();
        assert_eq!(ready_tasks.len(), 1);

        // Claim task
        assert!(registry.try_claim_task("test_task").unwrap());
        let ready_tasks = registry.get_ready_tasks();
        assert_eq!(ready_tasks.len(), 0);
    }

    #[tokio::test]
    async fn test_notification_system() {
        let system = TaskNotificationSystem::new(5);
        
        // Test acquire permit
        let permit = system.acquire_execution_permit().await.unwrap();
        assert_eq!(system.available_permits(), 4);
        drop(permit);
        assert_eq!(system.available_permits(), 5);

        // Test notifications
        let notify_count_before = system.get_notifications_sent();
        system.notify_task_ready();
        let notify_count_after = system.get_notifications_sent();
        assert_eq!(notify_count_after, notify_count_before + 1);
    }

    #[tokio::test]
    async fn test_resource_limiter() {
        let limiter = ResourceLimiter::new(2, 100, "memory".to_string());

        // Acquire resource
        let permit1 = limiter.acquire(30).await.unwrap();
        assert_eq!(limiter.current_usage(), 30);

        let permit2 = limiter.acquire(40).await.unwrap();
        assert_eq!(limiter.current_usage(), 70);

        // Try to exceed limit
        let result = limiter.acquire(50).await;
        assert!(result.is_err());

        // Release resources
        drop(permit1);
        assert_eq!(limiter.current_usage(), 40);
        drop(permit2);
        assert_eq!(limiter.current_usage(), 0);
    }

    #[tokio::test]
    async fn test_lock_ordering() {
        let lock_manager = LockManager::new();

        // Test correct ordering
        lock_manager.acquire(LockOrder::Config).await.unwrap();
        lock_manager.acquire(LockOrder::Registry).await.unwrap();
        lock_manager.acquire(LockOrder::Tasks).await.unwrap();

        // Test incorrect ordering
        let result = lock_manager.acquire(LockOrder::Config).await;
        assert!(result.is_err());

        // Release locks
        lock_manager.release(LockOrder::Tasks).await;
        lock_manager.release(LockOrder::Registry).await;
        lock_manager.release(LockOrder::Config).await;
    }
}