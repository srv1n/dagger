//! NodeActionV2 - compute-only node actions
//! 
//! This replaces the old NodeAction trait that required &mut DagExecutor.
//! Actions are now pure computation with no direct state mutation.

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use std::any::Any;

/// Context for node execution (immutable, clonable)
#[derive(Clone)]
pub struct NodeCtx {
    /// DAG name this node belongs to
    pub dag_name: String,
    /// Node ID
    pub node_id: String,
    /// Node inputs (from workflow or dynamically set)
    pub inputs: Value,
    /// Dagger cache for reading/writing intermediate values
    pub cache: crate::Cache,
    /// Optional application data (for RZN team)
    pub app_data: Option<Arc<dyn Any + Send + Sync>>,
}

impl NodeCtx {
    /// Create a new node context
    pub fn new(
        dag_name: impl Into<String>,
        node_id: impl Into<String>,
        inputs: Value,
        cache: crate::Cache,
    ) -> Self {
        Self {
            dag_name: dag_name.into(),
            node_id: node_id.into(),
            inputs,
            cache,
            app_data: None,
        }
    }
    
    /// Set application data
    pub fn with_app_data(mut self, data: Arc<dyn Any + Send + Sync>) -> Self {
        self.app_data = Some(data);
        self
    }
    
    /// Get input value by key
    pub fn get_input<T: serde::de::DeserializeOwned>(&self, key: &str) -> anyhow::Result<T> {
        let value = self.inputs.get(key)
            .ok_or_else(|| anyhow::anyhow!("Input '{}' not found", key))?;
        serde_json::from_value(value.clone())
            .map_err(|e| anyhow::anyhow!("Failed to deserialize input '{}': {}", key, e))
    }
    
    /// Get optional input value
    pub fn get_input_opt<T: serde::de::DeserializeOwned>(&self, key: &str) -> anyhow::Result<Option<T>> {
        match self.inputs.get(key) {
            Some(value) if !value.is_null() => {
                let parsed = serde_json::from_value(value.clone())
                    .map_err(|e| anyhow::anyhow!("Failed to deserialize input '{}': {}", key, e))?;
                Ok(Some(parsed))
            }
            _ => Ok(None),
        }
    }
}

/// Output from node execution
#[derive(Debug, Clone)]
pub struct NodeOutput {
    /// Outputs to store in cache (optional)
    pub outputs: Option<Value>,
    /// Whether the node succeeded
    pub success: bool,
    /// Optional metadata about the execution
    pub metadata: Option<Value>,
}

impl NodeOutput {
    /// Create a successful output
    pub fn success(outputs: Value) -> Self {
        Self {
            outputs: Some(outputs),
            success: true,
            metadata: None,
        }
    }
    
    /// Create a successful output with no data
    pub fn success_empty() -> Self {
        Self {
            outputs: None,
            success: true,
            metadata: None,
        }
    }
    
    /// Create a failed output
    pub fn failure() -> Self {
        Self {
            outputs: None,
            success: false,
            metadata: None,
        }
    }
    
    /// Add metadata
    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// NodeAction - compute-only node action
/// 
/// Actions are pure computation - they read inputs, perform work,
/// and return outputs. No direct state mutation allowed.
#[async_trait]
pub trait NodeAction: Send + Sync {
    /// Get the name of this action
    fn name(&self) -> &str;
    
    /// Execute the node action
    /// 
    /// This should be pure computation - read inputs, do work, return outputs.
    /// State mutations happen via commands returned by hooks.
    async fn execute(&self, ctx: &NodeCtx) -> anyhow::Result<NodeOutput>;
    
    /// Optional: Validate inputs before execution
    fn validate_inputs(&self, _inputs: &Value) -> anyhow::Result<()> {
        Ok(())
    }
    
    /// Optional: Get input schema for validation
    fn input_schema(&self) -> Option<&str> {
        None
    }
    
    /// Optional: Get output schema for validation
    fn output_schema(&self) -> Option<&str> {
        None
    }
}


/// Example: Echo action that just returns its inputs
pub struct EchoAction;

#[async_trait]
impl NodeAction for EchoAction {
    fn name(&self) -> &str {
        "echo"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> anyhow::Result<NodeOutput> {
        tracing::info!("Echo action: {} with inputs: {}", ctx.node_id, ctx.inputs);
        Ok(NodeOutput::success(ctx.inputs.clone()))
    }
}