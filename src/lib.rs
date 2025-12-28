// Core infrastructure modules
pub mod core {
    pub mod errors;
    pub mod limits;
    pub mod memory;
}

// Three main execution paradigms
pub mod coord; // Coordinator-based parallel execution (NEW)
pub mod dag_flow; // DAG-based workflow execution
pub mod pubsub; // Pub/Sub messaging execution
pub mod taskagent; // Task-based agent execution

// Storage layer
pub mod storage; // SQLite storage backend

// Core exports
pub use core::errors::{DaggerError, Result};
pub use core::limits::{ResourceLimits, ResourceTracker};

// DAG Flow exports
pub use dag_flow::{
    append_global_value, get_global_input, get_input, insert_global_value, insert_value,
    parse_input_from_name, serialize_cache_to_json, serialize_cache_to_prettyjson, Cache,
    DagConfig, DagExecutionReport, DagExecutor, ExecutionObserver, Graph, Node,
    NodeExecutionOutcome, NodeSpec,
};

// Coordinator exports
pub use coord::{Coordinator, ExecutorCommand, NodeAction};

// Task Agent exports
pub use taskagent::{
    JobHandle, JobStatus, Task, TaskAgent, TaskExecutionReport, TaskManager, TaskOutcome,
    TaskStatus,
};

// Pub/Sub exports
pub use pubsub::{Message, PubSubAgent, PubSubConfig, PubSubExecutor};

// Export storage functionality
pub use storage::{
    Artifact, ArtifactStorage, DatabaseStats, FlowRun, FlowRunStatus, NodeRun, NodeRunStatus,
    OutboxEvent, SharedState, SqliteStorage, StorageError, TaskDependency,
};
