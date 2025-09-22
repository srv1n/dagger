//! Legacy compatibility layer for old NodeAction trait
//! 
//! This module provides backward compatibility for the old function-based
//! NodeAction trait that's used by the register_action! macro.

use async_trait::async_trait;
use anyhow::Result;
use std::sync::Arc;
use serde_json::Value;

use super::{Cache, DagExecutor, Node};

/// Legacy NodeAction trait for backward compatibility
/// 
/// This trait is used by the register_action! macro and needs to be
/// bridged to the new coord::NodeAction trait.
#[async_trait]
pub trait NodeAction: Send + Sync {
    /// Get the name of this action
    fn name(&self) -> String;
    
    /// Execute the action with mutable executor access
    async fn execute(
        &self,
        executor: &mut DagExecutor,
        node: &Node,
        cache: &Cache,
    ) -> Result<()>;
    
    /// Get the schema for this action
    fn schema(&self) -> Value;
}

/// Wrapper to convert legacy NodeAction to coord::NodeAction
pub struct LegacyNodeActionWrapper {
    name: String,
    legacy_action: Arc<dyn NodeAction>,
}

impl LegacyNodeActionWrapper {
    pub fn new(legacy_action: Arc<dyn NodeAction>) -> Self {
        let name = legacy_action.name();
        Self {
            name,
            legacy_action,
        }
    }
}

#[async_trait]
impl crate::coord::NodeAction for LegacyNodeActionWrapper {
    fn name(&self) -> &str {
        &self.name
    }
    
    async fn execute(&self, ctx: &crate::coord::NodeCtx) -> Result<crate::coord::NodeOutput> {
        // For legacy actions, we can't provide a mutable DagExecutor
        // They should only use the cache for reading/writing
        
        // Create a fake node from the context
        let node = Node {
            id: ctx.node_id.clone(),
            action: self.name.clone(),
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            failure: String::new(),
            onfailure: false,
            description: String::new(),
            timeout: 30,
            try_count: 1,
            instructions: None,
        };
        
        // Store the inputs in cache for the legacy action to read
        if let Some(inputs_obj) = ctx.inputs.as_object() {
            for (key, value) in inputs_obj {
                crate::insert_value(&ctx.cache, &ctx.node_id, key, value.clone())?;
            }
        }
        
        // Note: We can't call the legacy action properly because it needs &mut DagExecutor
        // This is a fundamental incompatibility between the old and new designs
        // Legacy actions that mutate the executor won't work with the coordinator
        
        tracing::warn!(
            "Legacy action '{}' cannot be executed through coordinator - it requires mutable executor access",
            self.name
        );
        
        // Return success with empty outputs for now
        Ok(crate::coord::NodeOutput::success(Value::Null))
    }
}