//! Storage layer for Dagger
//! 
//! This module provides database storage capabilities for the Dagger workflow engine.
//! It includes SQLite-based storage for flow runs, node runs, artifacts, and shared state.

pub mod sqlite_storage;

pub use sqlite_storage::{SqliteStorage, StorageError, ArtifactStorage, DatabaseStats};

/// Common storage traits and types
use serde_json::Value;
use std::collections::HashMap;
use chrono::{DateTime, Utc};

/// Represents the status of a flow run
#[derive(Debug, Clone, PartialEq)]
pub enum FlowRunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl FlowRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            FlowRunStatus::Pending => "pending",
            FlowRunStatus::Running => "running", 
            FlowRunStatus::Completed => "completed",
            FlowRunStatus::Failed => "failed",
            FlowRunStatus::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "pending" => Ok(FlowRunStatus::Pending),
            "running" => Ok(FlowRunStatus::Running),
            "completed" => Ok(FlowRunStatus::Completed),
            "failed" => Ok(FlowRunStatus::Failed),
            "cancelled" => Ok(FlowRunStatus::Cancelled),
            _ => Err(format!("Unknown flow run status: {}", s)),
        }
    }
}

/// Represents the status of a node run
#[derive(Debug, Clone, PartialEq)]
pub enum NodeRunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
    Cancelled,
}

impl NodeRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeRunStatus::Pending => "pending",
            NodeRunStatus::Running => "running",
            NodeRunStatus::Completed => "completed", 
            NodeRunStatus::Failed => "failed",
            NodeRunStatus::Skipped => "skipped",
            NodeRunStatus::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "pending" => Ok(NodeRunStatus::Pending),
            "running" => Ok(NodeRunStatus::Running),
            "completed" => Ok(NodeRunStatus::Completed),
            "failed" => Ok(NodeRunStatus::Failed),
            "skipped" => Ok(NodeRunStatus::Skipped),
            "cancelled" => Ok(NodeRunStatus::Cancelled),
            _ => Err(format!("Unknown node run status: {}", s)),
        }
    }
}

/// Flow run record
#[derive(Debug, Clone)]
pub struct FlowRun {
    pub id: String,
    pub name: String,
    pub status: FlowRunStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub parameters: Option<Value>,
    pub error_message: Option<String>,
}

/// Node run record
#[derive(Debug, Clone)]
pub struct NodeRun {
    pub id: String,
    pub flow_run_id: String,
    pub node_name: String,
    pub status: NodeRunStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub inputs: Option<Value>,
    pub outputs: Option<Value>,
    pub error_message: Option<String>,
    pub retry_count: i32,
}

/// Artifact record
#[derive(Debug, Clone)]
pub struct Artifact {
    pub id: String,
    pub flow_run_id: Option<String>,
    pub node_run_id: Option<String>,
    pub key: String,
    pub artifact_type: String,
    pub created_at: DateTime<Utc>,
    pub data: Option<Value>,
    pub file_path: Option<String>,
    pub metadata: Option<Value>,
}

/// Outbox event record for event sourcing
#[derive(Debug, Clone)]
pub struct OutboxEvent {
    pub id: String,
    pub event_type: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub processed: bool,
    pub processed_at: Option<DateTime<Utc>>,
}

/// Task record
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub flow_run_id: Option<String>,
    pub name: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub priority: i32,
    pub max_retries: i32,
    pub retry_count: i32,
    pub payload: Option<Value>,
    pub result: Option<Value>,
    pub error_message: Option<String>,
}

/// Task dependency record
#[derive(Debug, Clone)]
pub struct TaskDependency {
    pub task_id: String,
    pub depends_on_task_id: String,
    pub created_at: DateTime<Utc>,
}

/// Shared state record
#[derive(Debug, Clone)]
pub struct SharedState {
    pub key: String,
    pub value: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i32,
    pub ttl: Option<DateTime<Utc>>,
}