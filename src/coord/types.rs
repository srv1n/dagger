//! Core types for coordinator-based execution
//! 
//! These types enable safe parallel execution without borrow checker conflicts.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Reference to a node in a DAG
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeRef {
    pub dag_name: String,
    pub node_id: String,
}

/// Events that occur during node execution
#[derive(Clone, Debug)]
pub enum ExecutionEvent {
    NodeStarted { 
        node: NodeRef 
    },
    NodeCompleted { 
        node: NodeRef, 
        outcome: NodeOutcome 
    },
    NodeFailed { 
        node: NodeRef, 
        error: String 
    },
}

/// Outcome of a node execution
#[derive(Clone, Debug)]
pub enum NodeOutcome {
    Success { 
        outputs: Option<Value> 
    },
    Skipped,
}

/// Specification for a node to be added to the DAG
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeSpec {
    /// Optional ID (will be generated if None)
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
            inputs: Value::Object(serde_json::Map::new()),
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

/// Commands that can mutate the executor state
#[derive(Clone, Debug)]
pub enum ExecutorCommand {
    /// Add a single node
    AddNode { 
        dag_name: String, 
        spec: NodeSpec 
    },
    /// Add multiple nodes atomically
    AddNodes { 
        dag_name: String, 
        specs: Vec<NodeSpec> 
    },
    /// Set inputs for a node
    SetNodeInputs { 
        dag_name: String, 
        node_id: String, 
        inputs: Value 
    },
    /// Pause a branch
    PauseBranch { 
        branch_id: String, 
        reason: Option<String> 
    },
    /// Resume a branch
    ResumeBranch { 
        branch_id: String 
    },
    /// Cancel a branch
    CancelBranch { 
        branch_id: String 
    },
    /// Emit a runtime event (for RZN integration)
    EmitEvent { 
        event: RuntimeEventV2 
    },
}

/// Runtime event for RZN integration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeEventV2 {
    pub version: u32,
    pub sequence: u64,
    pub run_id: String,
    pub timestamp: u64,
    pub event_type: String,
    pub data: Value,
}