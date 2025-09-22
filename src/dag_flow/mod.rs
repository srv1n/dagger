// Core modules
pub mod dag_flow;
pub mod dag_flow_parallel;
pub mod sqlite_cache;

// Support modules
pub mod any;
pub mod dag_builder;
pub mod supervisor;
pub mod planning;
pub mod events;
pub mod branch;

// Compatibility modules for legacy code
pub mod legacy_compat;
pub mod function_action;

// Main exports
pub use dag_flow::*;

// Builder exports
pub use dag_builder::{
    BackoffStrategy,
    DagExecutionContext,
    DagExecutionMetrics,
    DagExecutionState,
    DagFlowBuilder,
    DagStatus,
    ErrorHandling,
    NodeBuilder,
    NodeDefinition,
    NodeExecutionState,
    NodeStatus,
    RetryPolicy,
    RetryPolicyBuilder,
};

// Module-specific exports
pub use self::supervisor::{SupervisorHook, LoggingSupervisor, CompositeSupervisor};
pub use self::planning::{NodeSpec, Plan, plan_from_llm_output};
pub use self::events::{RuntimeEvent, RuntimeEventEnvelope, EventSink, LoggingEventSink, BufferingEventSink};
pub use self::branch::{BranchStatus, BranchState, BranchRegistry};
