use crate::{
    error::{Result, TaskError},
    model::{Task, TaskId, TaskStatus},
};
use async_trait::async_trait;
use bytes::Bytes;

/// Storage trait for task persistence
#[async_trait]
pub trait Storage: Send + Sync {
    /// Store a task
    async fn put(&self, task: &Task) -> Result<()>;

    /// Get a task by ID
    async fn get(&self, id: TaskId) -> Result<Option<Task>>;

    /// Update task status with CAS
    async fn update_status(&self, id: TaskId, old: TaskStatus, new: TaskStatus) -> Result<()>;

    /// List all running tasks
    async fn list_running(&self) -> Result<Vec<TaskId>>;

    /// Get task output
    async fn get_output(&self, id: TaskId) -> Result<Option<Bytes>>;

    /// Update task output
    async fn update_output(&self, id: TaskId, output: Bytes) -> Result<()>;

    /// Get task status
    async fn get_status(&self, id: TaskId) -> Result<TaskStatus>;

    /// Flush pending writes
    async fn flush(&self) -> Result<()>;

    /// Get next task ID
    async fn next_task_id(&self) -> Result<TaskId>;

    /// Get shared state value
    async fn get_shared_state(&self, key: &str) -> Result<Option<Bytes>>;
    
    /// Set shared state value
    async fn set_shared_state(&self, key: &str, value: Bytes) -> Result<()>;
    
    /// Delete shared state value
    async fn delete_shared_state(&self, key: &str) -> Result<bool>;
}


