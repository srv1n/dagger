//! Coordinator-based parallel execution system
//! 
//! This module implements Sam's recommended architecture for safe parallel
//! DAG execution with dynamic graph growth.

pub mod types;
pub mod hooks;
pub mod coordinator;
pub mod action;
pub mod registry;

pub use types::*;
pub use hooks::*;
pub use coordinator::*;
pub use action::*;
pub use registry::*;