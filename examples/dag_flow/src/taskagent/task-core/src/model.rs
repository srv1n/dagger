// Data model definitions for task-core

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::RwLock;

// Type aliases
pub type TaskId = u64;
pub type JobId = u64;
pub type AgentId = u16;

// Durability enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Durability {
    BestEffort,
    AtMostOnce,
}

// TaskStatus enum with all 9 statuses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    Blocked,
    Ready,
    Running,
    Completed,
    Failed,
    Paused,
    Rejected,
    Accepted,
}

// TaskType enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskType {
    Objective,
    Story,
    Task,
    Subtask,
}

// TaskPayload struct
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPayload {
    pub action: String,
    pub input: serde_json::Value,
    pub metadata: HashMap<String, serde_json::Value>,
}

// Task struct with all fields
#[derive(Debug)]
pub struct Task {
    // Core identifiers
    pub id: TaskId,
    pub job_id: JobId,
    pub task_type: TaskType,
    
    // Task metadata
    pub name: String,
    pub description: Option<String>,
    pub payload: TaskPayload,
    
    // Status and state
    pub status: TaskStatus,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    
    // Agent assignment
    pub assigned_agent: Option<AgentId>,
    pub required_agent_type: Option<String>,
    
    // Dependencies
    pub dependencies: HashSet<TaskId>,
    pub parent_task: Option<TaskId>,
    pub child_tasks: HashSet<TaskId>,
    
    // Timing
    pub created_at: SystemTime,
    pub started_at: Option<SystemTime>,
    pub completed_at: Option<SystemTime>,
    pub deadline: Option<SystemTime>,
    pub timeout: Option<Duration>,
    
    // Retry configuration
    pub retry_count: u32,
    pub max_retries: u32,
    pub retry_delay: Option<Duration>,
    
    // Priority and ordering
    pub priority: i32,
    pub sequence_number: Option<u64>,
    
    // Durability
    pub durability: Durability,
    
    // Atomic fields for thread-safe updates
    pub is_cancelled: Arc<AtomicBool>,
    pub progress: Arc<AtomicU64>,
    
    // Arc fields for shared state
    pub shared_state: Arc<RwLock<HashMap<String, serde_json::Value>>>,
    pub tags: Arc<RwLock<HashSet<String>>>,
    
    // New fields from architect
    pub estimated_duration: Option<Duration>,
    pub actual_duration: Option<Duration>,
    pub resource_requirements: HashMap<String, serde_json::Value>,
    pub execution_context: HashMap<String, serde_json::Value>,
}

// Job struct
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub name: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub created_at: SystemTime,
    pub started_at: Option<SystemTime>,
    pub completed_at: Option<SystemTime>,
    pub task_ids: HashSet<TaskId>,
    pub root_task_ids: HashSet<TaskId>,
    pub metadata: HashMap<String, serde_json::Value>,
    pub priority: i32,
    pub tags: HashSet<String>,
}

// AgentError enum
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentError {
    TaskNotFound(TaskId),
    TaskAlreadyRunning(TaskId),
    TaskFailed { id: TaskId, error: String },
    InvalidTaskType { expected: String, actual: String },
    DependencyFailed(TaskId),
    Timeout(TaskId),
    Cancelled(TaskId),
    ResourceUnavailable(String),
    InvalidInput(String),
    InternalError(String),
}

// NewTaskSpec struct for creating new tasks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTaskSpec {
    pub job_id: JobId,
    pub task_type: TaskType,
    pub name: String,
    pub description: Option<String>,
    pub payload: TaskPayload,
    pub dependencies: HashSet<TaskId>,
    pub parent_task: Option<TaskId>,
    pub required_agent_type: Option<String>,
    pub priority: i32,
    pub deadline: Option<SystemTime>,
    pub timeout: Option<Duration>,
    pub max_retries: u32,
    pub retry_delay: Option<Duration>,
    pub durability: Durability,
    pub tags: HashSet<String>,
    pub estimated_duration: Option<Duration>,
    pub resource_requirements: HashMap<String, serde_json::Value>,
    pub execution_context: HashMap<String, serde_json::Value>,
}

// Implement Display for better logging
impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "Pending"),
            TaskStatus::Blocked => write!(f, "Blocked"),
            TaskStatus::Ready => write!(f, "Ready"),
            TaskStatus::Running => write!(f, "Running"),
            TaskStatus::Completed => write!(f, "Completed"),
            TaskStatus::Failed => write!(f, "Failed"),
            TaskStatus::Paused => write!(f, "Paused"),
            TaskStatus::Rejected => write!(f, "Rejected"),
            TaskStatus::Accepted => write!(f, "Accepted"),
        }
    }
}

impl std::fmt::Display for TaskType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskType::Objective => write!(f, "Objective"),
            TaskType::Story => write!(f, "Story"),
            TaskType::Task => write!(f, "Task"),
            TaskType::Subtask => write!(f, "Subtask"),
        }
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::TaskNotFound(id) => write!(f, "Task not found: {}", id),
            AgentError::TaskAlreadyRunning(id) => write!(f, "Task already running: {}", id),
            AgentError::TaskFailed { id, error } => write!(f, "Task {} failed: {}", id, error),
            AgentError::InvalidTaskType { expected, actual } => {
                write!(f, "Invalid task type: expected {}, got {}", expected, actual)
            }
            AgentError::DependencyFailed(id) => write!(f, "Dependency failed: {}", id),
            AgentError::Timeout(id) => write!(f, "Task timed out: {}", id),
            AgentError::Cancelled(id) => write!(f, "Task cancelled: {}", id),
            AgentError::ResourceUnavailable(res) => write!(f, "Resource unavailable: {}", res),
            AgentError::InvalidInput(msg) => write!(f, "Invalid input: {}", msg),
            AgentError::InternalError(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl std::error::Error for AgentError {}

// Default implementations
impl Default for Durability {
    fn default() -> Self {
        Durability::BestEffort
    }
}

impl Default for TaskType {
    fn default() -> Self {
        TaskType::Task
    }
}

impl Default for TaskStatus {
    fn default() -> Self {
        TaskStatus::Pending
    }
}