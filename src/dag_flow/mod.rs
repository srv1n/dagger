#![allow(clippy::module_inception)]

// Core modules
pub mod dag_flow;
pub mod dag_flow_parallel;
pub mod sqlite_cache;

// Support modules
pub mod any;
pub mod branch;
pub mod dag_builder;
pub mod events;
pub mod planning;
pub mod services;
pub mod supervisor;

// Main exports
pub use dag_flow::*;

// Builder exports
pub use dag_builder::{
    BackoffStrategy, DagExecutionContext, DagExecutionMetrics, DagExecutionState, DagFlowBuilder,
    DagStatus, ErrorHandling, NodeBuilder, NodeDefinition, NodeExecutionState, NodeStatus,
    RetryPolicy, RetryPolicyBuilder,
};

// Module-specific exports
pub use self::branch::{BranchRegistry, BranchState, BranchStatus};
pub use self::events::{
    BufferingEventSink, EventSink, LoggingEventSink, RuntimeEvent, RuntimeEventEnvelope,
};
pub use self::planning::{plan_from_llm_output, NodeSpec, Plan};
pub use self::services::ServiceRegistry;
pub use self::supervisor::{CompositeSupervisor, LoggingSupervisor, SupervisorHook};
