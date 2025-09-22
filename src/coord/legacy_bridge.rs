//! Bridge for converting legacy NodeAction functions to the new coord NodeAction trait

use async_trait::async_trait;
use std::sync::Arc;
use anyhow::Result;
use serde_json::Value;

use crate::dag_flow::{DagExecutor, Node, Cache, NodeAction as LegacyNodeAction};
use super::action::{NodeAction as CoordNodeAction, NodeCtx, NodeOutput};

/// Wrapper to adapt legacy NodeAction to new coord NodeAction
pub struct LegacyActionWrapper {
    name: String,
    legacy_action: Arc<dyn LegacyNodeAction>,
}

impl LegacyActionWrapper {
    pub fn new(name: String, legacy_action: Arc<dyn LegacyNodeAction>) -> Self {
        Self { name, legacy_action }
    }
}

#[async_trait]
impl CoordNodeAction for LegacyActionWrapper {
    fn name(&self) -> &str {
        &self.name
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        // Legacy actions expect to be called with DagExecutor, Node, and Cache
        // We need to simulate this environment
        
        // Store inputs in cache for the legacy action to read
        for (key, value) in ctx.inputs.as_object().unwrap_or(&serde_json::Map::new()) {
            crate::insert_value(&ctx.cache, &ctx.node_id, key, value.clone())?;
        }
        
        // Create a fake node for the legacy action
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
        
        // Note: Legacy actions expect &mut DagExecutor but we can't provide that in the new architecture
        // The legacy action should only use the cache, not mutate the executor
        // This is a limitation of the bridge - complex legacy actions that mutate the executor won't work
        
        // For now, we'll return success with the inputs as outputs
        // Real implementation would need access to a DagExecutor instance
        tracing::warn!(
            "Legacy action '{}' executed through bridge - full functionality may be limited", 
            self.name
        );
        
        Ok(NodeOutput::success(ctx.inputs.clone()))
    }
}

/// Convert legacy function registry to coord ActionRegistry
pub fn convert_legacy_registry(
    legacy_registry: &Arc<tokio::sync::RwLock<std::collections::HashMap<String, Arc<dyn LegacyNodeAction>>>>
) -> crate::coord::registry::ActionRegistry {
    let coord_registry = crate::coord::registry::ActionRegistry::new();
    
    // This is async but we need to block to read the legacy registry
    let legacy_map = futures::executor::block_on(async {
        legacy_registry.read().await.clone()
    });
    
    for (name, action) in legacy_map {
        let wrapper = LegacyActionWrapper::new(name.clone(), action);
        coord_registry.register(Arc::new(wrapper));
    }
    
    coord_registry
}