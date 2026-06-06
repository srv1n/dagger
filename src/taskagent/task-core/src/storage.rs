use crate::{
    error::Result,
    model::{JobId, NewTaskEvent, Task, TaskEventRecord, TaskId, TaskOutputRecord, TaskStatus},
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

    /// Get a task by public ID
    async fn get_by_public_id(&self, public_id: &str) -> Result<Option<Task>>;

    /// Update task status with CAS
    async fn update_status(&self, id: TaskId, old: TaskStatus, new: TaskStatus) -> Result<()>;

    /// List all running tasks
    async fn list_running(&self) -> Result<Vec<TaskId>>;

    /// List all tasks
    async fn list_tasks(&self) -> Result<Vec<Task>>;

    /// List tasks for a specific job
    async fn list_tasks_by_job(&self, job_id: JobId) -> Result<Vec<Task>>;

    /// List tasks for a thread
    async fn list_tasks_by_thread(&self, thread_id: &str) -> Result<Vec<Task>>;

    /// List child tasks for a task
    async fn list_child_tasks(&self, parent_id: TaskId) -> Result<Vec<Task>>;

    /// List direct dependencies for a task
    async fn list_task_dependencies(&self, task_id: TaskId) -> Result<Vec<Task>>;

    /// List direct dependents for a task
    async fn list_task_dependents(&self, task_id: TaskId) -> Result<Vec<Task>>;

    /// Get task output
    async fn get_output(&self, id: TaskId) -> Result<Option<Bytes>>;

    /// Append a task output to ordered history
    async fn append_output(&self, id: TaskId, output: Bytes) -> Result<TaskOutputRecord>;

    /// List ordered task output history
    async fn list_outputs(&self, id: TaskId) -> Result<Vec<TaskOutputRecord>>;

    /// Update task output
    async fn update_output(&self, id: TaskId, output: Bytes) -> Result<()>;

    /// Append a lifecycle event
    async fn append_event(&self, task_id: TaskId, event: NewTaskEvent) -> Result<TaskEventRecord>;

    /// List lifecycle events
    async fn list_events(&self, task_id: TaskId) -> Result<Vec<TaskEventRecord>>;

    /// Request a task stop or cancellation
    async fn request_stop(&self, id: TaskId, reason: Option<&str>) -> Result<Task>;

    /// Soft delete a task without removing history
    async fn soft_delete(&self, id: TaskId, reason: Option<&str>) -> Result<Task>;

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
