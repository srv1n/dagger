//! Coordinator-based parallel execution system
//!
//! This module implements Sam's recommended architecture for safe parallel
//! DAG execution with dynamic graph growth.

pub mod action;
pub mod coordinator;
pub mod hooks;
pub mod registry;
pub mod types;

pub use action::*;
pub use coordinator::*;
pub use hooks::*;
pub use registry::*;
pub use types::*;
