//! Planning utilities for DAG Flow
//! 
//! Provides declarative planning helpers for building DAG nodes

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Specification for a node to be added to a DAG
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSpec {
    /// Optional ID for the node (will be generated if None)
    pub id: Option<String>,
    /// The action to execute
    pub action: String,
    /// Dependencies (node IDs that must complete first)
    pub deps: Vec<String>,
    /// Input data for the node
    pub inputs: Value,
    /// Optional timeout in seconds
    pub timeout: Option<u64>,
    /// Optional retry count
    pub try_count: Option<u32>,
}

impl NodeSpec {
    /// Create a new NodeSpec with minimal fields
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            id: None,
            action: action.into(),
            deps: Vec::new(),
            inputs: json!({}),
            timeout: None,
            try_count: None,
        }
    }
    
    /// Set the node ID
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }
    
    /// Add dependencies
    pub fn with_deps(mut self, deps: Vec<String>) -> Self {
        self.deps = deps;
        self
    }
    
    /// Add a single dependency
    pub fn with_dep(mut self, dep: impl Into<String>) -> Self {
        self.deps.push(dep.into());
        self
    }
    
    /// Set inputs
    pub fn with_inputs(mut self, inputs: Value) -> Self {
        self.inputs = inputs;
        self
    }
    
    /// Set timeout
    pub fn with_timeout(mut self, timeout: u64) -> Self {
        self.timeout = Some(timeout);
        self
    }
    
    /// Set retry count
    pub fn with_retries(mut self, count: u32) -> Self {
        self.try_count = Some(count);
        self
    }
}

/// A declarative plan builder for constructing DAG nodes
#[derive(Debug, Clone)]
pub struct Plan {
    /// The nodes to be added
    pub nodes: Vec<NodeSpec>,
    /// Track generated IDs for reference
    id_map: HashMap<String, String>,
    /// Counter for generating unique IDs
    id_counter: usize,
}

impl Plan {
    /// Create a new empty plan
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            id_map: HashMap::new(),
            id_counter: 0,
        }
    }
    
    /// Generate a unique node ID with a prefix
    fn gen_id(&mut self, prefix: &str) -> String {
        self.id_counter += 1;
        format!("{}_{}", prefix, self.id_counter)
    }
    
    /// Add a generic node to the plan
    pub fn add_node(&mut self, spec: NodeSpec) -> String {
        let node_id = spec.id.clone().unwrap_or_else(|| self.gen_id(&spec.action));
        let mut spec = spec;
        spec.id = Some(node_id.clone());
        self.nodes.push(spec);
        node_id
    }
    
    /// Add a tool invocation node
    pub fn add_tool(
        &mut self,
        dep: &str,
        name: &str,
        args: Value,
        run_id: &str,
    ) -> String {
        let node_id = self.gen_id(&format!("tool_{}", name));
        let inputs = json!({
            "run_id": run_id,
            "tool_name": name,
            "args": args,
        });
        
        self.nodes.push(NodeSpec {
            id: Some(node_id.clone()),
            action: "tool_invoke_node".to_string(),
            deps: vec![dep.to_string()],
            inputs,
            timeout: Some(30),
            try_count: Some(2),
        });
        
        self.id_map.insert(format!("tool_{}", name), node_id.clone());
        node_id
    }
    
    /// Add a continuation node (for processing tool results)
    pub fn add_continuation(
        &mut self,
        dep_tool_ids: &[String],
        inputs: Value,
    ) -> String {
        let node_id = self.gen_id("continuation");
        
        self.nodes.push(NodeSpec {
            id: Some(node_id.clone()),
            action: "continuation_node".to_string(),
            deps: dep_tool_ids.to_vec(),
            inputs,
            timeout: None,
            try_count: None,
        });
        
        self.id_map.insert("continuation".to_string(), node_id.clone());
        node_id
    }
    
    /// Add a next LLM node
    pub fn add_next_llm(
        &mut self,
        dep: &str,
        inputs: Value,
    ) -> String {
        let node_id = self.gen_id("llm");
        
        self.nodes.push(NodeSpec {
            id: Some(node_id.clone()),
            action: "provider_llm_node".to_string(),
            deps: vec![dep.to_string()],
            inputs,
            timeout: Some(120),
            try_count: Some(1),
        });
        
        self.id_map.insert("llm".to_string(), node_id.clone());
        node_id
    }
    
    /// Get a previously added node ID by its logical name
    pub fn get_node_id(&self, logical_name: &str) -> Option<&String> {
        self.id_map.get(logical_name)
    }
    
    /// Clear the plan
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.id_map.clear();
        self.id_counter = 0;
    }
    
    /// Get the number of nodes in the plan
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    
    /// Check if the plan is empty
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

impl Default for Plan {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper to create a plan from LLM output
pub fn plan_from_llm_output(
    run_id: &str,
    llm_node_id: &str,
    output: &Value,
) -> Option<Plan> {
    let mut plan = Plan::new();
    
    // Parse tool calls from LLM output
    if let Some(tool_calls) = output.get("tool_calls").and_then(|v| v.as_array()) {
        let mut tool_ids = Vec::new();
        
        for tool_call in tool_calls {
            if let (Some(name), Some(args)) = (
                tool_call.get("name").and_then(|v| v.as_str()),
                tool_call.get("arguments"),
            ) {
                let tool_id = plan.add_tool(llm_node_id, name, args.clone(), run_id);
                tool_ids.push(tool_id);
            }
        }
        
        if !tool_ids.is_empty() {
            // Add continuation node to process tool results
            let cont_id = plan.add_continuation(&tool_ids, json!({
                "run_id": run_id,
                "tool_count": tool_ids.len(),
            }));
            
            // Add next LLM node
            plan.add_next_llm(&cont_id, json!({
                "run_id": run_id,
                "continuation": true,
            }));
            
            return Some(plan);
        }
    }
    
    None
}