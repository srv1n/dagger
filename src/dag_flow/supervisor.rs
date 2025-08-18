//! Supervisor hook system for DAG Flow
//! 
//! Provides a formalized hook system for handling node completions
//! and orchestrating dynamic workflow growth.

use crate::dag_flow::{Cache, DagExecutor, Node};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// Supervisor hook trait for handling node completions
/// 
/// This trait allows external code to react to node completions
/// and potentially add new nodes to the DAG dynamically.
#[async_trait]
pub trait SupervisorHook: Send + Sync {
    /// Called after a node completes successfully
    /// 
    /// # Arguments
    /// * `executor` - The DAG executor (mutable for adding nodes)
    /// * `node` - The node that just completed
    /// * `cache` - The cache containing node outputs
    async fn on_node_complete(
        &self,
        executor: &mut DagExecutor,
        node: &Node,
        cache: &Cache,
    ) -> Result<()>;
    
    /// Optional: Called when a node fails
    /// 
    /// Default implementation does nothing
    async fn on_node_failure(
        &self,
        _executor: &mut DagExecutor,
        _node: &Node,
        _cache: &Cache,
        _error: &anyhow::Error,
    ) -> Result<()> {
        Ok(())
    }
    
    /// Optional: Called before a node starts execution
    /// 
    /// Default implementation does nothing
    async fn before_node_start(
        &self,
        _executor: &mut DagExecutor,
        _node: &Node,
        _cache: &Cache,
    ) -> Result<()> {
        Ok(())
    }
}

/// A simple supervisor that logs node completions
pub struct LoggingSupervisor;

#[async_trait]
impl SupervisorHook for LoggingSupervisor {
    async fn on_node_complete(
        &self,
        _executor: &mut DagExecutor,
        node: &Node,
        _cache: &Cache,
    ) -> Result<()> {
        tracing::info!("Node {} completed successfully", node.id);
        Ok(())
    }
    
    async fn on_node_failure(
        &self,
        _executor: &mut DagExecutor,
        node: &Node,
        _cache: &Cache,
        error: &anyhow::Error,
    ) -> Result<()> {
        tracing::error!("Node {} failed: {}", node.id, error);
        Ok(())
    }
}

/// A supervisor that can chain multiple hooks
pub struct CompositeSupervisor {
    hooks: Vec<Arc<dyn SupervisorHook>>,
}

impl CompositeSupervisor {
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }
    
    pub fn add_hook(&mut self, hook: Arc<dyn SupervisorHook>) {
        self.hooks.push(hook);
    }
}

#[async_trait]
impl SupervisorHook for CompositeSupervisor {
    async fn on_node_complete(
        &self,
        executor: &mut DagExecutor,
        node: &Node,
        cache: &Cache,
    ) -> Result<()> {
        for hook in &self.hooks {
            hook.on_node_complete(executor, node, cache).await?;
        }
        Ok(())
    }
    
    async fn on_node_failure(
        &self,
        executor: &mut DagExecutor,
        node: &Node,
        cache: &Cache,
        error: &anyhow::Error,
    ) -> Result<()> {
        for hook in &self.hooks {
            hook.on_node_failure(executor, node, cache, error).await?;
        }
        Ok(())
    }
    
    async fn before_node_start(
        &self,
        executor: &mut DagExecutor,
        node: &Node,
        cache: &Cache,
    ) -> Result<()> {
        for hook in &self.hooks {
            hook.before_node_start(executor, node, cache).await?;
        }
        Ok(())
    }
}