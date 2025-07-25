use bytes::Bytes;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use chrono::{DateTime, Utc};

// Type aliases
pub type TaskId = u64;
pub type JobId = u64;
pub type AgentId = u16;

/// Task durability mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Durability {
    BestEffort,   // Idempotent - safe to rerun
    AtMostOnce,   // Must not rerun automatically
}

/// Task status with atomic representation
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending = 0,
    Blocked = 1,
    Ready = 2,
    Running = 3,
    Completed = 4,
    Failed = 5,
    Paused = 6,
    Rejected = 7,
    Accepted = 8,
}

impl TaskStatus {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(TaskStatus::Pending),
            1 => Some(TaskStatus::Blocked),
            2 => Some(TaskStatus::Ready),
            3 => Some(TaskStatus::Running),
            4 => Some(TaskStatus::Completed),
            5 => Some(TaskStatus::Failed),
            6 => Some(TaskStatus::Paused),
            7 => Some(TaskStatus::Rejected),
            8 => Some(TaskStatus::Accepted),
            _ => None,
        }
    }
}

/// Task type hierarchy
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskType {
    Objective = 0,
    Story = 1,
    Task = 2,
    Subtask = 3,
}

/// Core task structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub job: JobId,
    pub agent: AgentId,
    #[serde(with = "atomic_u8")]
    pub status: AtomicU8,
    pub durability: Durability,
    #[serde(with = "atomic_u8")]
    pub retry_count: AtomicU8,
    pub dependencies: SmallVec<[TaskId; 4]>,
    pub payload: Arc<TaskPayload>,
    
    // Additional fields
    pub parent: Option<TaskId>,
    pub task_type: TaskType,
    pub max_retries: u8,
    pub timeout: Option<Duration>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    
    // Optional fields from architect's extended design
    pub acceptance_criteria: Option<Arc<str>>,
    pub status_reason: Option<Arc<str>>,
    pub summary: Option<Arc<str>>,
    
    // Error storage
    pub error: Option<Bytes>,
}

/// Task payload - rarely accessed fields
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPayload {
    pub input: Bytes,
    pub output: tokio::sync::RwLock<Option<Bytes>>,
    pub description: Arc<str>,
}

/// Specification for creating new tasks
#[derive(Debug, Clone)]
pub struct NewTaskSpec {
    pub agent: AgentId,
    pub input: Bytes,
    pub dependencies: SmallVec<[TaskId; 4]>,
    pub durability: Durability,
    pub task_type: TaskType,
    pub description: Arc<str>,
    pub timeout: Option<Duration>,
    pub max_retries: Option<u8>,
    pub parent: Option<TaskId>,
}

/// Job metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub root_task: TaskId,
    #[serde(with = "atomic_u8")]
    pub status: AtomicU8,
    pub summary: tokio::sync::RwLock<Option<String>>,
}

/// Agent error types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentError {
    User(String),          // Serializable user error
    System(String),        // System error (from anyhow)
    Timeout(Duration),     // Timeout error
}

impl AgentError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, AgentError::System(_) | AgentError::Timeout(_))
    }
}

impl From<anyhow::Error> for AgentError {
    fn from(err: anyhow::Error) -> Self {
        AgentError::System(err.to_string())
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::User(msg) => write!(f, "User error: {}", msg),
            AgentError::System(msg) => write!(f, "System error: {}", msg),
            AgentError::Timeout(duration) => write!(f, "Timeout after {:?}", duration),
        }
    }
}

impl std::error::Error for AgentError {}

// Task implementation
impl Task {
    /// Create task from specification
    pub fn from_spec(id: TaskId, spec: NewTaskSpec) -> Self {
        let now = Utc::now();
        Self {
            id,
            job: 0, // Will be set by job manager
            agent: spec.agent,
            status: AtomicU8::new(TaskStatus::Pending as u8),
            durability: spec.durability,
            retry_count: AtomicU8::new(0),
            dependencies: spec.dependencies,
            payload: Arc::new(TaskPayload {
                input: spec.input,
                output: tokio::sync::RwLock::new(None),
                description: spec.description,
            }),
            parent: spec.parent,
            task_type: spec.task_type,
            max_retries: spec.max_retries.unwrap_or(3),
            timeout: spec.timeout,
            created_at: now,
            updated_at: now,
            acceptance_criteria: None,
            status_reason: None,
            summary: None,
            error: None,
        }
    }
    
    /// Record an error
    pub fn record_error(&self, err: &AgentError) {
        let error_bytes = match err {
            AgentError::User(s) => s.as_bytes(),
            AgentError::System(e) => e.as_bytes(),
            AgentError::Timeout(d) => format!("Timeout: {:?}", d).as_bytes(),
        };
        // Note: In real implementation, this would need mutable access
        // self.error = Some(Bytes::copy_from_slice(error_bytes));
    }
}

// Serialization helpers for atomic types
mod atomic_u8 {
    use super::*;
    use serde::{Deserializer, Serializer};
    
    pub fn serialize<S>(atomic: &AtomicU8, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(atomic.load(Ordering::Relaxed))
    }
    
    pub fn deserialize<'de, D>(deserializer: D) -> Result<AtomicU8, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u8::deserialize(deserializer)?;
        Ok(AtomicU8::new(value))
    }
}