pub mod dag_flow;
pub mod dag_flow_parallel;
// pub mod enhanced_dag_flow;
pub mod any;
pub mod dag_builder;
pub mod sqlite_cache;
pub mod supervisor;
pub mod planning;
pub mod events;
pub mod branch;

pub use dag_flow::*;
// Re-export builder types with explicit names to avoid conflicts
pub use dag_builder::{
    BackoffStrategy,
    DagExecutionContext, // Note: DagConfig is not re-exported to avoid conflict with dag_flow::DagConfig
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

// Re-export new modules
pub use self::supervisor::{SupervisorHook, LoggingSupervisor, CompositeSupervisor};
pub use self::planning::{NodeSpec, Plan, plan_from_llm_output};
pub use self::events::{RuntimeEvent, RuntimeEventEnvelope, EventSink, LoggingEventSink, BufferingEventSink};
pub use self::branch::{BranchStatus, BranchState, BranchRegistry};
// pub use enhanced_dag_flow::{
//     EnhancedDagExecutor, EnhancedNodeAction, EnhancedExecutionResult,
//     NodeExecutionMetrics, ExampleEnhancedAction
// };
