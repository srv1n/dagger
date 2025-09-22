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
    Cache, DagExecutor, DagConfig, Node, Graph, ExecutionObserver,
    DagExecutionReport, NodeExecutionOutcome, NodeSpec,
    insert_value, get_input, get_global_input, parse_input_from_name,
    insert_global_value, append_global_value,
    serialize_cache_to_json, serialize_cache_to_prettyjson,
};

// Coordinator exports
pub use coord::{Coordinator, ExecutorCommand, NodeAction};

// Task Agent exports  
pub use taskagent::{
    TaskManager, TaskAgent, Task, TaskStatus, TaskOutcome,
    TaskExecutionReport, JobHandle, JobStatus,
};

// Pub/Sub exports
pub use pubsub::{
    PubSubExecutor, PubSubAgent, Message, PubSubConfig,
};

// Export storage functionality
pub use storage::{
    SqliteStorage, StorageError, ArtifactStorage, DatabaseStats,
    FlowRun, NodeRun, Artifact, OutboxEvent, TaskDependency, SharedState,
    FlowRunStatus, NodeRunStatus
};

