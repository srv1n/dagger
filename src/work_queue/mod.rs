//! Work Queue primitives
//!
//! This module contains "Lobster-like" primitives intended to be embedded into
//! host applications (e.g. RZN) that need deterministic, auditable execution.

pub mod exec;
pub mod batch;

pub use exec::{
    ExecAction, ExecError, ExecHost, ExecKind, ExecResult, ExecServices, ExecSpec, LocalExecHost,
    ResolvedExec,
};

pub use batch::{
    build_batch_plan, execute_batch, BatchExecution, BatchFanoutSpec, BatchInput, BatchPlan,
    BatchStepRef, PerItemStepTemplate,
};
