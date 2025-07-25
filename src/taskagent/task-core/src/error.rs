use thiserror::Error;
use std::time::Duration;
use std::io;

/// Task system errors with comprehensive error handling
#[derive(Error, Debug)]
pub enum TaskError {
    // Storage errors
    #[error("Storage error: {0}")]
    Storage(String),
    
    #[error("Database error: {0}")]
    Database(#[from] sled::Error),
    
    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),
    
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    
    // Task lifecycle errors
    #[error("Task not found: {task_id}")]
    TaskNotFound { task_id: String },
    
    #[error("Task already exists: {task_id}")]
    TaskAlreadyExists { task_id: String },
    
    #[error("Task execution failed: {task_id} - {reason}")]
    TaskExecutionFailed { task_id: String, reason: String },
    
    #[error("Task timeout: {task_id} (duration: {duration:?})")]
    TaskTimeout { task_id: String, duration: Duration },
    
    #[error("Task cancelled: {task_id}")]
    TaskCancelled { task_id: String },
    
    #[error("Task dependency failed: {task_id} depends on {dependency_id}")]
    DependencyFailed { task_id: String, dependency_id: String },
    
    #[error("Circular dependency detected: {cycle}")]
    CircularDependency { cycle: String },
    
    // Job errors
    #[error("Job not found: {job_id}")]
    JobNotFound { job_id: String },
    
    #[error("Job already exists: {job_id}")]
    JobAlreadyExists { job_id: String },
    
    #[error("Job execution failed: {job_id} - {reason}")]
    JobExecutionFailed { job_id: String, reason: String },
    
    #[error("Job stalled: {job_id} (last activity: {duration:?} ago)")]
    JobStalled { job_id: String, duration: Duration },
    
    // Agent errors
    #[error("Agent not found: {agent_name}")]
    AgentNotFound { agent_name: String },
    
    #[error("Agent already registered: {agent_name}")]
    AgentAlreadyRegistered { agent_name: String },
    
    #[error("Agent execution error: {agent_name} - {error}")]
    AgentExecutionError { agent_name: String, error: String },
    
    #[error("Agent initialization failed: {agent_name} - {reason}")]
    AgentInitializationFailed { agent_name: String, reason: String },
    
    // Queue errors
    #[error("Queue full: capacity {capacity} reached")]
    QueueFull { capacity: usize },
    
    #[error("Queue empty")]
    QueueEmpty,
    
    #[error("Priority out of range: {priority} (max: {max})")]
    InvalidPriority { priority: usize, max: usize },
    
    // Scheduler errors
    #[error("Scheduler error: {0}")]
    Scheduler(String),
    
    #[error("Task not ready: {task_id} - waiting for {waiting_for}")]
    TaskNotReady { task_id: String, waiting_for: String },
    
    #[error("Resource limit exceeded: {resource} - {details}")]
    ResourceLimitExceeded { resource: String, details: String },
    
    // Recovery errors
    #[error("Recovery failed: {reason}")]
    RecoveryFailed { reason: String },
    
    #[error("Checkpoint creation failed: {0}")]
    CheckpointFailed(String),
    
    #[error("Restore failed: {0}")]
    RestoreFailed(String),
    
    // Configuration errors
    #[error("Configuration invalid: {0}")]
    InvalidConfiguration(String),
    
    #[error("Missing required configuration: {field}")]
    MissingConfiguration { field: String },
    
    // Validation errors
    #[error("Invalid input for task {task_id}: {reason}")]
    InvalidInput { task_id: String, reason: String },
    
    #[error("Invalid output from task {task_id}: {reason}")]
    InvalidOutput { task_id: String, reason: String },
    
    #[error("Schema validation failed: {0}")]
    SchemaValidation(String),
    
    // System errors
    #[error("Worker pool exhausted: all {count} workers are busy")]
    WorkerPoolExhausted { count: usize },
    
    #[error("System shutting down")]
    SystemShuttingDown,
    
    #[error("Operation cancelled")]
    OperationCancelled,
    
    // IO errors
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    
    // Channel errors
    #[error("Channel send error: {0}")]
    ChannelSend(String),
    
    #[error("Channel receive error: {0}")]
    ChannelReceive(String),
    
    // Additional errors for new design
    #[error("System shutdown in progress")]
    SystemShutdown,
    
    #[error("Invalid task status")]
    InvalidStatus,
    
    #[error("Status mismatch: expected {expected:?}, found {found:?}")]
    StatusMismatch {
        expected: crate::model::TaskStatus,
        found: crate::model::TaskStatus,
    },
    
    #[error("Channel closed")]
    ChannelClosed,
    
    // Generic errors
    #[error("Internal error: {0}")]
    Internal(String),
    
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl TaskError {
    /// Create a storage error
    pub fn storage(msg: impl Into<String>) -> Self {
        Self::Storage(msg.into())
    }
    
    /// Create a scheduler error
    pub fn scheduler(msg: impl Into<String>) -> Self {
        Self::Scheduler(msg.into())
    }
    
    /// Create an internal error
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
    
    /// Check if this error is retryable
    pub fn is_retryable(&self) -> bool {
        match self {
            // Non-retryable errors
            Self::TaskNotFound { .. } |
            Self::JobNotFound { .. } |
            Self::AgentNotFound { .. } |
            Self::CircularDependency { .. } |
            Self::InvalidConfiguration(_) |
            Self::MissingConfiguration { .. } |
            Self::SchemaValidation(_) |
            Self::SystemShuttingDown |
            Self::InvalidInput { .. } => false,
            
            // Potentially retryable errors
            Self::Database(_) |
            Self::Storage(_) |
            Self::TaskExecutionFailed { .. } |
            Self::AgentExecutionError { .. } |
            Self::QueueFull { .. } |
            Self::WorkerPoolExhausted { .. } |
            Self::Io(_) |
            Self::ChannelSend(_) |
            Self::ChannelReceive(_) => true,
            
            // Timeout might be retryable with longer timeout
            Self::TaskTimeout { .. } => true,
            
            // Check wrapped errors
            Self::Other(e) => {
                // Could implement more sophisticated logic here
                !e.to_string().contains("fatal")
            }
            
            _ => false,
        }
    }
    
    /// Check if this error indicates a transient condition
    pub fn is_transient(&self) -> bool {
        match self {
            Self::QueueFull { .. } |
            Self::WorkerPoolExhausted { .. } |
            Self::TaskNotReady { .. } => true,
            _ => false,
        }
    }
    
    /// Get suggested retry delay for this error
    pub fn suggested_retry_delay(&self) -> Option<Duration> {
        match self {
            Self::QueueFull { .. } => Some(Duration::from_millis(100)),
            Self::WorkerPoolExhausted { .. } => Some(Duration::from_secs(1)),
            Self::TaskNotReady { .. } => Some(Duration::from_secs(5)),
            Self::Database(_) | Self::Storage(_) => Some(Duration::from_secs(2)),
            Self::Io(_) => Some(Duration::from_secs(1)),
            _ if self.is_retryable() => Some(Duration::from_secs(3)),
            _ => None,
        }
    }
}

/// Result type alias for TaskError
pub type Result<T> = std::result::Result<T, TaskError>;

/// Extension trait for converting between error types
pub trait ErrorExt<T> {
    /// Convert to TaskError with context
    fn context(self, msg: &str) -> Result<T>;
    
    /// Convert to TaskError for a specific task
    fn for_task(self, task_id: &str) -> Result<T>;
}

impl<T, E> ErrorExt<T> for std::result::Result<T, E>
where
    E: Into<TaskError>,
{
    fn context(self, msg: &str) -> Result<T> {
        self.map_err(|e| {
            let err: TaskError = e.into();
            TaskError::Internal(format!("{}: {}", msg, err))
        })
    }
    
    fn for_task(self, task_id: &str) -> Result<T> {
        self.map_err(|e| {
            let err: TaskError = e.into();
            TaskError::TaskExecutionFailed {
                task_id: task_id.to_string(),
                reason: err.to_string(),
            }
        })
    }
}

// Implement From for common error types
impl From<String> for TaskError {
    fn from(s: String) -> Self {
        TaskError::Internal(s)
    }
}

impl From<&str> for TaskError {
    fn from(s: &str) -> Self {
        TaskError::Internal(s.to_string())
    }
}

impl<T> From<tokio::sync::mpsc::error::SendError<T>> for TaskError {
    fn from(e: tokio::sync::mpsc::error::SendError<T>) -> Self {
        TaskError::ChannelSend(e.to_string())
    }
}

impl From<tokio::sync::oneshot::error::RecvError> for TaskError {
    fn from(_: tokio::sync::oneshot::error::RecvError) -> Self {
        TaskError::ChannelClosed
    }
}

impl<T> From<tokio::sync::watch::error::SendError<T>> for TaskError {
    fn from(e: tokio::sync::watch::error::SendError<T>) -> Self {
        TaskError::ChannelSend(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_error_retryable() {
        let storage_err = TaskError::storage("disk full");
        assert!(storage_err.is_retryable());
        
        let not_found = TaskError::TaskNotFound { 
            task_id: "test".to_string() 
        };
        assert!(!not_found.is_retryable());
        
        let timeout = TaskError::TaskTimeout {
            task_id: "test".to_string(),
            duration: Duration::from_secs(30),
        };
        assert!(timeout.is_retryable());
    }
    
    #[test]
    fn test_error_transient() {
        let queue_full = TaskError::QueueFull { capacity: 1000 };
        assert!(queue_full.is_transient());
        
        let worker_busy = TaskError::WorkerPoolExhausted { count: 10 };
        assert!(worker_busy.is_transient());
        
        let permanent = TaskError::InvalidConfiguration("bad config".to_string());
        assert!(!permanent.is_transient());
    }
    
    #[test]
    fn test_retry_delays() {
        let queue_full = TaskError::QueueFull { capacity: 1000 };
        assert_eq!(
            queue_full.suggested_retry_delay(),
            Some(Duration::from_millis(100))
        );
        
        let io_err = TaskError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "timeout"
        ));
        assert_eq!(
            io_err.suggested_retry_delay(),
            Some(Duration::from_secs(1))
        );
    }
    
    #[test]
    fn test_error_context() {
        let result: std::result::Result<i32, String> = Err("base error".to_string());
        let with_context = result.context("operation failed");
        
        match with_context {
            Err(TaskError::Internal(msg)) => {
                assert!(msg.contains("operation failed"));
                assert!(msg.contains("base error"));
            }
            _ => panic!("Expected Internal error with context"),
        }
    }
    
    #[test]
    fn test_error_display() {
        let err = TaskError::TaskExecutionFailed {
            task_id: "task-123".to_string(),
            reason: "out of memory".to_string(),
        };
        
        let display = err.to_string();
        assert!(display.contains("task-123"));
        assert!(display.contains("out of memory"));
    }
}