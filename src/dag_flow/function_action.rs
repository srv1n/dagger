//! Function-based action wrapper for legacy compatibility
//! 
//! This allows using simple async functions as actions without needing
//! the full DagExecutor access.

use async_trait::async_trait;
use anyhow::Result;
use std::sync::Arc;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

use super::{Cache, DagExecutor, Node};

/// Type alias for the legacy action function signature
pub type ActionFunction = for<'a> fn(&'a mut DagExecutor, &'a Node, &'a Cache) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// Wrapper for function-based actions
pub struct FunctionAction {
    name: String,
    func: ActionFunction,
}

impl FunctionAction {
    pub fn new(name: String, func: ActionFunction) -> Self {
        Self { name, func }
    }
}

#[async_trait]
impl crate::coord::NodeAction for FunctionAction {
    fn name(&self) -> &str {
        &self.name
    }
    
    async fn execute(&self, ctx: &crate::coord::NodeCtx) -> Result<crate::coord::NodeOutput> {
        // Try to get the real node from app_data if available
        let node = if let Some(app_data) = &ctx.app_data {
            if let Some(node) = app_data.downcast_ref::<Node>() {
                node.clone()
            } else {
                // Fallback: create a fake node with resolved inputs
                let mut inputs = Vec::new();
                if let Some(inputs_obj) = ctx.inputs.as_object() {
                    for (key, _value) in inputs_obj {
                        inputs.push(crate::dag_flow::IField {
                            name: key.clone(),
                            description: None,
                            reference: format!("{}.{}", ctx.node_id, key),
                        });
                    }
                }
                
                // Create dummy outputs - many legacy actions expect at least one output
                let outputs = vec![crate::dag_flow::OField {
                    name: "output".to_string(),
                    description: None,
                }];
                
                Node {
                    id: ctx.node_id.clone(),
                    action: self.name.clone(),
                    dependencies: vec![],
                    inputs,
                    outputs,
                    failure: String::new(),
                    onfailure: false,
                    description: String::new(),
                    timeout: 30,
                    try_count: 1,
                    instructions: None,
                }
            }
        } else {
            // Fallback: create a fake node
            let mut inputs = Vec::new();
            if let Some(inputs_obj) = ctx.inputs.as_object() {
                for (key, _value) in inputs_obj {
                    inputs.push(crate::dag_flow::IField {
                        name: key.clone(),
                        description: None,
                        reference: format!("{}.{}", ctx.node_id, key),
                    });
                }
            }
            
            let outputs = vec![crate::dag_flow::OField {
                name: "output".to_string(),
                description: None,
            }];
            
            Node {
                id: ctx.node_id.clone(),
                action: self.name.clone(),
                dependencies: vec![],
                inputs,
                outputs,
                failure: String::new(),
                onfailure: false,
                description: String::new(),
                timeout: 30,
                try_count: 1,
                instructions: None,
            }
        };
        
        // Store the inputs in cache for the function to read
        if let Some(inputs_obj) = ctx.inputs.as_object() {
            for (key, value) in inputs_obj {
                crate::insert_value(&ctx.cache, &ctx.node_id, key, value.clone())?;
            }
        }
        
        // Create a minimal executor - this is a hack but necessary for compatibility
        // The function shouldn't actually use the executor for mutations
        let registry = crate::coord::ActionRegistry::new();
        let config = crate::DagConfig::default();
        let mut temp_executor = DagExecutor::new(
            Some(config),
            registry,
            "sqlite::memory:"
        ).await?;
        
        // Call the function
        (self.func)(&mut temp_executor, &node, &ctx.cache).await?;
        
        // Return success
        Ok(crate::coord::NodeOutput::success(Value::Null))
    }
}