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

// Re-exports for convenience
pub use core::errors::{DaggerError, Result};
// Export the original Cache for backward compatibility
pub use dag_flow::{insert_value, Cache};
// Export the enhanced cache with a different name
pub use core::limits::{ResourceLimits, ResourceTracker};
pub use core::memory::Cache as EnhancedCache;

// Re-export existing modules for backward compatibility
pub use dag_flow::*;
pub use taskagent::*;

// Export storage functionality
pub use storage::{
    SqliteStorage, StorageError, ArtifactStorage, DatabaseStats,
    FlowRun, NodeRun, Artifact, OutboxEvent, Task, TaskDependency, SharedState,
    FlowRunStatus, NodeRunStatus
};

