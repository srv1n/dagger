//! Work Queue primitives
//!
//! This module contains "Lobster-like" primitives intended to be embedded into
//! host applications (e.g. RZN) that need deterministic, auditable execution.

pub mod batch;
pub mod exec;
pub mod hitl;
pub mod pipeline;

pub use exec::{
    ExecAction, ExecError, ExecHost, ExecKind, ExecResult, ExecServices, ExecSpec, LocalExecHost,
    ResolvedExec,
};

pub use batch::{
    build_batch_plan, execute_batch, BatchExecution, BatchFanoutSpec, BatchInput, BatchPlan,
    BatchStepRef, PerItemStepTemplate,
};

pub use pipeline::{
    build_pipeline_plan, execute_pipeline, PipeMode, PipeSpec, PipelineExecution, PipelinePlan,
    PipelineSpec, PipelineStep, PipelineStepRef,
};

pub use hitl::{
    HitlApprovalRequest, HitlCheckpointAction, HitlCheckpointSpec, HitlDecision, HitlError,
    HitlResumeToken, HitlRuntime,
};
