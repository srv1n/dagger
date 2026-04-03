use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use smallvec::SmallVec;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

// Type aliases
pub type TaskId = u64;
pub type JobId = u64;
pub type AgentId = u16;

/// Task durability mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Durability {
    BestEffort, // Idempotent - safe to rerun
    AtMostOnce, // Must not rerun automatically
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
    Cancelling = 9,
    Cancelled = 10,
    Deleted = 11,
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
            9 => Some(TaskStatus::Cancelling),
            10 => Some(TaskStatus::Cancelled),
            11 => Some(TaskStatus::Deleted),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Ready => "ready",
            TaskStatus::Running => "running",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Paused => "paused",
            TaskStatus::Rejected => "rejected",
            TaskStatus::Accepted => "accepted",
            TaskStatus::Cancelling => "cancelling",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Deleted => "deleted",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(TaskStatus::Pending),
            "blocked" => Some(TaskStatus::Blocked),
            "ready" => Some(TaskStatus::Ready),
            "running" => Some(TaskStatus::Running),
            "completed" => Some(TaskStatus::Completed),
            "failed" => Some(TaskStatus::Failed),
            "paused" => Some(TaskStatus::Paused),
            "rejected" => Some(TaskStatus::Rejected),
            "accepted" => Some(TaskStatus::Accepted),
            "cancelling" => Some(TaskStatus::Cancelling),
            "cancelled" => Some(TaskStatus::Cancelled),
            "deleted" => Some(TaskStatus::Deleted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublicTaskStatus {
    Queued,
    Blocked,
    Running,
    Paused,
    Succeeded,
    Failed,
    Rejected,
    Stopping,
    Cancelled,
    Deleted,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSourceMetadata {
    pub surface: Option<String>,
    pub tool_name: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub run_id: Option<String>,
    pub call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutputRecord {
    pub sequence: u64,
    pub output: Bytes,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewTaskEvent {
    pub event_type: String,
    pub status: Option<TaskStatus>,
    pub reason: Option<String>,
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEventRecord {
    pub sequence: u64,
    pub event_type: String,
    pub status: Option<TaskStatus>,
    pub reason: Option<String>,
    pub payload: Option<Value>,
    pub created_at: DateTime<Utc>,
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
#[derive(Debug)]
pub struct Task {
    pub id: TaskId,
    pub public_id: Arc<str>,
    pub job: JobId,
    pub agent: AgentId,
    pub status: AtomicU8,
    pub durability: Durability,
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

    // Host-facing metadata
    pub thread_id: Option<Arc<str>>,
    pub subject: Option<Arc<str>>,
    pub description: Arc<str>,
    pub owner: Option<Arc<str>>,
    pub metadata: Value,
    pub source: Option<TaskSourceMetadata>,
    pub acceptance_criteria: Option<Arc<str>>,
    pub status_reason: Option<Arc<str>>,
    pub summary: Option<Arc<str>>,
    pub stop_reason: Option<Arc<str>>,
    pub stop_requested_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub deleted_reason: Option<Arc<str>>,

    // Error storage
    pub error: Option<Bytes>,
}

/// Task payload - rarely accessed fields
#[derive(Debug)]
pub struct TaskPayload {
    pub input: Bytes,
    pub output: tokio::sync::RwLock<Option<Bytes>>,
}

/// Specification for creating new tasks
#[derive(Debug, Clone)]
pub struct NewTaskSpec {
    pub job: Option<JobId>,
    pub agent: AgentId,
    pub public_id: Option<Arc<str>>,
    pub thread_id: Option<Arc<str>>,
    pub subject: Option<Arc<str>>,
    pub description: Arc<str>,
    pub owner: Option<Arc<str>>,
    pub metadata: Value,
    pub source: Option<TaskSourceMetadata>,
    pub acceptance_criteria: Option<Arc<str>>,
    pub input: Bytes,
    pub dependencies: SmallVec<[TaskId; 4]>,
    pub durability: Durability,
    pub task_type: TaskType,
    pub timeout: Option<Duration>,
    pub max_retries: Option<u8>,
    pub parent: Option<TaskId>,
}

/// Job metadata
#[derive(Debug)]
pub struct Job {
    pub id: JobId,
    pub public_id: Arc<str>,
    pub root_task: TaskId,
    pub status: AtomicU8,
    pub summary: tokio::sync::RwLock<Option<String>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Agent error types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentError {
    User(String),      // Serializable user error
    System(String),    // System error (from anyhow)
    Timeout(Duration), // Timeout error
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
        let description = spec.description;
        let public_id = spec
            .public_id
            .unwrap_or_else(|| Arc::<str>::from(format!("task-{}", id)));
        let subject = spec.subject.or_else(|| Some(description.clone()));
        Self {
            id,
            public_id,
            job: spec.job.unwrap_or(id),
            agent: spec.agent,
            status: AtomicU8::new(TaskStatus::Pending as u8),
            durability: spec.durability,
            retry_count: AtomicU8::new(0),
            dependencies: spec.dependencies,
            payload: Arc::new(TaskPayload {
                input: spec.input,
                output: tokio::sync::RwLock::new(None),
            }),
            parent: spec.parent,
            task_type: spec.task_type,
            max_retries: spec.max_retries.unwrap_or(3),
            timeout: spec.timeout,
            created_at: now,
            updated_at: now,
            thread_id: spec.thread_id,
            subject,
            description,
            owner: spec.owner,
            metadata: spec.metadata,
            source: spec.source,
            acceptance_criteria: spec.acceptance_criteria,
            status_reason: None,
            summary: None,
            stop_reason: None,
            stop_requested_at: None,
            deleted_at: None,
            deleted_reason: None,
            error: None,
        }
    }

    pub fn status(&self) -> TaskStatus {
        TaskStatus::from_u8(self.status.load(Ordering::Relaxed)).unwrap_or(TaskStatus::Pending)
    }

    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some() || self.status() == TaskStatus::Deleted
    }

    pub fn public_status(&self) -> PublicTaskStatus {
        if self.is_deleted() {
            return PublicTaskStatus::Deleted;
        }

        if self.stop_requested_at.is_some() && self.status() == TaskStatus::Running {
            return PublicTaskStatus::Stopping;
        }

        match self.status() {
            TaskStatus::Pending | TaskStatus::Ready | TaskStatus::Accepted => {
                PublicTaskStatus::Queued
            }
            TaskStatus::Blocked => PublicTaskStatus::Blocked,
            TaskStatus::Running => PublicTaskStatus::Running,
            TaskStatus::Paused => PublicTaskStatus::Paused,
            TaskStatus::Completed => PublicTaskStatus::Succeeded,
            TaskStatus::Failed => PublicTaskStatus::Failed,
            TaskStatus::Rejected => PublicTaskStatus::Rejected,
            TaskStatus::Cancelling => PublicTaskStatus::Stopping,
            TaskStatus::Cancelled => PublicTaskStatus::Cancelled,
            TaskStatus::Deleted => PublicTaskStatus::Deleted,
        }
    }

    /// Record an error
    pub fn record_error(&mut self, err: &AgentError) {
        let error_bytes = match err {
            AgentError::User(s) => Bytes::from(s.clone()),
            AgentError::System(e) => Bytes::from(e.clone()),
            AgentError::Timeout(d) => Bytes::from(format!("Timeout: {:?}", d)),
        };
        self.error = Some(error_bytes);
        self.status_reason = Some(Arc::from(err.to_string()));
    }
}

impl Clone for Task {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            public_id: self.public_id.clone(),
            job: self.job,
            agent: self.agent,
            status: AtomicU8::new(self.status.load(Ordering::Relaxed)),
            durability: self.durability,
            retry_count: AtomicU8::new(self.retry_count.load(Ordering::Relaxed)),
            dependencies: self.dependencies.clone(),
            payload: Arc::clone(&self.payload),
            parent: self.parent,
            task_type: self.task_type,
            max_retries: self.max_retries,
            timeout: self.timeout,
            created_at: self.created_at,
            updated_at: self.updated_at,
            thread_id: self.thread_id.clone(),
            subject: self.subject.clone(),
            description: self.description.clone(),
            owner: self.owner.clone(),
            metadata: self.metadata.clone(),
            source: self.source.clone(),
            acceptance_criteria: self.acceptance_criteria.clone(),
            status_reason: self.status_reason.clone(),
            summary: self.summary.clone(),
            stop_reason: self.stop_reason.clone(),
            stop_requested_at: self.stop_requested_at,
            deleted_at: self.deleted_at,
            deleted_reason: self.deleted_reason.clone(),
            error: self.error.clone(),
        }
    }
}
