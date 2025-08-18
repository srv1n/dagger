//! Event hook system for coordinator-based execution
//! 
//! Hooks process events and return commands, never directly mutating state.

use async_trait::async_trait;
use super::types::{ExecutionEvent, ExecutorCommand};
use std::sync::Arc;
use std::any::Any;

/// Context provided to hooks for processing events
pub struct HookContext {
    /// Run ID for this execution
    pub run_id: String,
    /// DAG name being executed
    pub dag_name: String,
    /// Optional application-specific data (for RZN team to use)
    pub app_data: Option<Arc<dyn Any + Send + Sync>>,
}

impl HookContext {
    /// Create a new hook context
    pub fn new(run_id: impl Into<String>, dag_name: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            dag_name: dag_name.into(),
            app_data: None,
        }
    }
    
    /// Set application data
    pub fn with_app_data(mut self, data: Arc<dyn Any + Send + Sync>) -> Self {
        self.app_data = Some(data);
        self
    }
}

/// Event hook trait - processes events and returns commands
/// 
/// This is the key abstraction that enables safe parallel execution.
/// Hooks cannot mutate state directly, only return commands.
#[async_trait]
pub trait EventHook: Send + Sync {
    /// Handle an execution event and return commands to apply
    async fn handle(&self, ctx: &HookContext, event: &ExecutionEvent) -> Vec<ExecutorCommand>;
    
    /// Optional: Called when the DAG execution starts
    async fn on_start(&self, _ctx: &HookContext) -> Vec<ExecutorCommand> {
        Vec::new()
    }
    
    /// Optional: Called when the DAG execution completes
    async fn on_complete(&self, _ctx: &HookContext, _success: bool) -> Vec<ExecutorCommand> {
        Vec::new()
    }
}

/// Composite hook that chains multiple hooks
pub struct CompositeHook {
    hooks: Vec<Arc<dyn EventHook>>,
}

impl CompositeHook {
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }
    
    pub fn add_hook(&mut self, hook: Arc<dyn EventHook>) {
        self.hooks.push(hook);
    }
}

#[async_trait]
impl EventHook for CompositeHook {
    async fn handle(&self, ctx: &HookContext, event: &ExecutionEvent) -> Vec<ExecutorCommand> {
        let mut commands = Vec::new();
        for hook in &self.hooks {
            commands.extend(hook.handle(ctx, event).await);
        }
        commands
    }
    
    async fn on_start(&self, ctx: &HookContext) -> Vec<ExecutorCommand> {
        let mut commands = Vec::new();
        for hook in &self.hooks {
            commands.extend(hook.on_start(ctx).await);
        }
        commands
    }
    
    async fn on_complete(&self, ctx: &HookContext, success: bool) -> Vec<ExecutorCommand> {
        let mut commands = Vec::new();
        for hook in &self.hooks {
            commands.extend(hook.on_complete(ctx, success).await);
        }
        commands
    }
}

/// Example: Logging hook
pub struct LoggingHook;

#[async_trait]
impl EventHook for LoggingHook {
    async fn handle(&self, _ctx: &HookContext, event: &ExecutionEvent) -> Vec<ExecutorCommand> {
        match event {
            ExecutionEvent::NodeStarted { node } => {
                tracing::info!("Node started: {}/{}", node.dag_name, node.node_id);
            }
            ExecutionEvent::NodeCompleted { node, outcome } => {
                tracing::info!("Node completed: {}/{} - {:?}", node.dag_name, node.node_id, outcome);
            }
            ExecutionEvent::NodeFailed { node, error } => {
                tracing::error!("Node failed: {}/{} - {}", node.dag_name, node.node_id, error);
            }
        }
        Vec::new() // Logging hook doesn't produce commands
    }
}