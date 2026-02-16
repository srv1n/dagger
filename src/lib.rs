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
pub mod work_queue; // Work Queue primitives (Lobster-like)

// Storage layer
pub mod storage; // SQLite storage backend

// Core exports
pub use anyhow;
pub use core::errors::{DaggerError, Result};
pub use core::limits::{ResourceLimits, ResourceTracker};
pub use dagger_macros::{action, pubsub_agent, task_agent};
pub use serde_json;

// Re-export task-core for macro expansion convenience
pub use task_core;

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
    Agent, AgentError, AgentId, AgentRegistry, Durability, JobId, NewTaskSpec, Task, TaskConfig,
    TaskConfigBuilder, TaskContext, TaskHandle, TaskId, TaskStatus, TaskSystem, TaskSystemBuilder,
    TaskType,
};

// Pub/Sub exports
pub use pubsub::{
    Message, PubSubAgent, PubSubConfig, PubSubContext, PubSubExecutor, PubSubWorkflowSpec,
};

// Work Queue exports
pub use work_queue::{
    ExecAction, ExecError, ExecHost, ExecKind, ExecResult, ExecServices, ExecSpec, LocalExecHost,
};
pub use work_queue::{
    build_batch_plan, execute_batch, BatchExecution, BatchFanoutSpec, BatchInput, BatchPlan,
    BatchStepRef, PerItemStepTemplate,
};

// Export storage functionality
pub use storage::{
    Artifact, ArtifactStorage, DatabaseStats, FlowRun, FlowRunStatus, NodeRun, NodeRunStatus,
    OutboxEvent, SharedState, SqliteStorage, StorageError, TaskDependency,
};
