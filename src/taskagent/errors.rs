use thiserror::Error;

use super::taskagent::TaskStatus;

#[derive(Error, Debug)]
pub enum TaskError {
    #[error("Agent not found: {0}")]
    AgentNotFound(String),
    #[error("Agent already exists: {0}")]
    AgentAlreadyExists(String),
    #[error("Agent registration failed: {0}")]
    AgentRegistrationFailed(String), // Keep this, could be used in the registry
    #[error("Agent creation failed: {0}")]
    AgentCreationFailed(String), // Keep, could be used in the registry
    #[error("Agent execution failed: {0}")]
    AgentExecutionFailed(String), // Keep, general execution error
    #[error("Agent timeout: {0}")]
    AgentTimeout(String), // Keep

    // Task-Specific Errors (from TaskManager)
    #[error("Task not found: {0}")]
    TaskNotFound(String),
    #[error("Task already exists: {0}")]
    TaskAlreadyExists(String),
    #[error("Task is not claimable (current status: {0:?})")]
    TaskNotClaimable(TaskStatus),
    #[error("Task is blocked: {0}")]
    TaskBlocked(String), // Reason for blocking
    #[error("Task dependency cycle detected")]
    DependencyCycle,
    #[error("Task ID or Job ID cannot be empty")]
    EmptyId,
    #[error("Invalid task input: {0}")]
    InvalidTaskInput(String), // More specific input validation
    #[error("Invalid task output: {0}")]
    InvalidTaskOutput(String),  // More specific output validation

    // Job-Specific Errors
    #[error("Job stalled: {0}")]
    JobStalled(String),
    #[error("Job cancelled: {0}")]
    JobCancelled(String), // Could include a reason
    #[error("Job not found: {0}")] //If we add get_job_status or similar functions
    JobNotFound(String),

    // Registry Errors
    #[error("Failed to serialize/deserialize: {0}")]
    SerdeError(#[from] serde_json::Error), // Capture serde errors directly

    // General Errors (consider more specific variants as needed)
    #[error("Internal error: {0}")]
    InternalError(String),

    // Wrapped anyhow::Error (for flexibility)
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}