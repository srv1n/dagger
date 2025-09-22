# DAG Flow Complete Guide

## Table of Contents
1. [Executive Summary](#executive-summary)
2. [Architecture Overview](#architecture-overview)
3. [Core Concepts](#core-concepts)
4. [Execution Flow](#execution-flow)
5. [Getting Started](#getting-started)
6. [Action System](#action-system)
7. [Execution Modes](#execution-modes)
8. [Dynamic Node Addition](#dynamic-node-addition)
9. [Cache System](#cache-system)
10. [Configuration](#configuration)
11. [Performance Optimization](#performance-optimization)
12. [API Reference](#api-reference)
13. [Troubleshooting](#troubleshooting)

## Executive Summary

DAG Flow is a high-performance, parallel execution engine for Directed Acyclic Graphs (DAGs) in Rust. It provides:

- **Parallel Execution**: Execute independent nodes concurrently with configurable parallelism
- **Dynamic Node Addition**: Add nodes at runtime based on execution results
- **Robust Error Handling**: Configurable retry strategies with exponential backoff
- **Persistent Caching**: SQLite-backed cache for execution state and results
- **Hook System**: Event-driven hooks for monitoring and dynamic workflow modification
- **Type-Safe Actions**: Strongly-typed node actions with JSON schema validation

### When to Use DAG Flow

DAG Flow is ideal for:
- Complex multi-step data processing pipelines
- Workflow orchestration with dependencies
- Machine learning pipelines
- ETL processes
- Any scenario requiring controlled parallel execution with dependency management

## Architecture Overview

### System Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                         User Code                           │
├─────────────────────────────────────────────────────────────┤
│                      DagExecutor                            │
│  ┌──────────────────────────────────────────────────────┐  │
│  │               Coordinator (Parallel)                  │  │
│  │  ┌──────────────────────────────────────────────┐    │  │
│  │  │   Event Loop                                 │    │  │
│  │  │   ┌───────────┐  ┌───────────┐             │    │  │
│  │  │   │  Commands │  │  Events   │             │    │  │
│  │  │   └───────────┘  └───────────┘             │    │  │
│  │  └──────────────────────────────────────────────┘    │  │
│  │                                                       │  │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐         │  │
│  │  │ Worker 1 │  │ Worker 2 │  │ Worker N │         │  │
│  │  └──────────┘  └──────────┘  └──────────┘         │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                             │
│  ┌──────────────────────────────────────────────────────┐  │
│  │            Action Registry                           │  │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐         │  │
│  │  │ Action A │  │ Action B │  │ Action C │  ...    │  │
│  │  └──────────┘  └──────────┘  └──────────┘         │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                             │
│  ┌──────────────────────────────────────────────────────┐  │
│  │         Cache Layer (In-Memory + SQLite)             │  │
│  └──────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

### Component Responsibilities

1. **DagExecutor**: Main entry point, manages DAG definitions and execution
2. **Coordinator**: Orchestrates parallel execution using channels and events
3. **Workers**: Execute individual nodes asynchronously
4. **Action Registry**: Stores and manages node action implementations
5. **Cache Layer**: Provides persistent storage for inputs, outputs, and state
6. **Hook System**: Processes events and generates commands for dynamic behavior

## Core Concepts

### Node
A node represents a single unit of computation in the DAG:

```rust
pub struct Node {
    pub id: String,                    // Unique identifier
    pub action: String,                 // Action name from registry
    pub inputs: Vec<IField>,           // Input specifications
    pub outputs: Vec<OField>,          // Output specifications
    pub dependencies: Vec<String>,     // Node IDs this depends on
    pub timeout: u64,                  // Execution timeout in seconds
    pub try_count: u8,                 // Retry attempts
    pub on_failure: OnFailure,         // Failure behavior
    pub cache_strategy: CacheStrategy, // Caching behavior
}
```

### Action
Actions define the computation logic for nodes:

```rust
#[async_trait]
pub trait NodeAction: Send + Sync {
    fn name(&self) -> &str;
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput>;
}
```

### Graph
A complete DAG definition:

```rust
pub struct Graph {
    pub name: String,
    pub description: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<(String, String)>,  // (from_id, to_id)
    pub config: Option<DagConfig>,
    pub tags: Vec<String>,
    pub author: String,
    pub version: String,
}
```

## Execution Flow

### Sequential Execution Flow

```
Start → Load DAG → Topological Sort → Execute Node 1 → Execute Node 2 → ... → Complete
```

### Parallel Execution Flow (Coordinator-based)

```
Start
  ↓
Initialize Coordinator
  ↓
Schedule Ready Nodes ←─────────┐
  ↓                           │
Spawn Workers                 │
  ↓                           │
Execute Nodes (Parallel)      │
  ↓                           │
Send Events                   │
  ↓                           │
Process Events                │
  ↓                           │
Apply Commands                │
  ↓                           │
Check Dependencies ───────────┘
  ↓
All Complete?
  ↓
End
```

### Dynamic Node Addition Flow

```
Node Completes → Hook Processes Event → Generate AddNode Command → 
Apply Command → Update DAG → Schedule New Node → Execute
```

## Getting Started

### Basic Setup

```rust
use dagger::{
    DagExecutor, DagConfig, Cache, 
    coord::{Coordinator, ActionRegistry, NodeAction},
    dag_flow::{OnFailure, RetryStrategy},
};

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Create action registry
    let mut registry = ActionRegistry::new();
    registry.register(Arc::new(MyAction));
    
    // 2. Configure executor
    let config = DagConfig {
        max_parallel_nodes: 4,
        retry_strategy: RetryStrategy::Immediate,
        on_failure: OnFailure::Stop,
        timeout_seconds: Some(300),
        ..Default::default()
    };
    
    // 3. Create executor
    let mut executor = DagExecutor::new(
        Some(config),
        registry,
        "sqlite::memory:",  // Or file path
    ).await?;
    
    // 4. Load DAG from YAML
    executor.load_yaml_file("workflow.yaml").await?;
    
    // 5. Execute with Coordinator
    let cache = Cache::new();
    let coordinator = Coordinator::new(vec![], 100, 100);
    let (_, cancel_rx) = oneshot::channel();
    
    coordinator.run_parallel(
        &mut executor,
        &cache,
        "workflow_name",
        "run_001",
        cancel_rx,
    ).await?;
    
    Ok(())
}
```

### YAML DAG Definition

```yaml
name: data_pipeline
description: "Example data processing pipeline"
tags: ["production", "data"]
author: "team"
version: "1.0.0"
signature: ""

nodes:
  - id: fetch_data
    action: http_fetch
    inputs:
      - name: url
        reference: inputs.data_url
    outputs:
      - name: raw_data
    timeout: 30
    try_count: 3

  - id: process_data
    action: transform
    inputs:
      - name: data
        reference: fetch_data.raw_data
    outputs:
      - name: processed
    dependencies: [fetch_data]
    
  - id: save_results
    action: database_save
    inputs:
      - name: data
        reference: process_data.processed
    dependencies: [process_data]
```

## Action System

### Creating Custom Actions

Actions are the building blocks of DAG nodes. There are two ways to create them:

#### Method 1: Implement NodeAction Trait

```rust
use dagger::coord::{NodeAction, NodeCtx, NodeOutput};
use async_trait::async_trait;

struct MyCustomAction;

#[async_trait]
impl NodeAction for MyCustomAction {
    fn name(&self) -> &str {
        "my_custom_action"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> anyhow::Result<NodeOutput> {
        // Access inputs
        let input_value = ctx.inputs.get("input_param")
            .and_then(|v| v.as_str())
            .unwrap_or("default");
        
        // Perform computation
        let result = process_data(input_value).await?;
        
        // Store in cache for dependent nodes
        dagger::insert_value(&ctx.cache, &ctx.node_id, "output", result)?;
        
        // Return output
        Ok(NodeOutput::success(json!({
            "result": result,
            "status": "completed"
        })))
    }
}
```

#### Method 2: Function-based Actions (Legacy)

```rust
async fn simple_action(
    executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache,
) -> Result<()> {
    // Get inputs from cache
    let input = get_input::<String>(cache, &node.id, "input")?;
    
    // Process
    let output = input.to_uppercase();
    
    // Store outputs
    insert_value(cache, &node.id, "output", output)?;
    
    Ok(())
}
```

### Registering Actions

```rust
// For trait-based actions
registry.register(Arc::new(MyCustomAction));

// For function-based actions (legacy)
registry.register_fn("simple_action", simple_action);
```

### Input/Output Resolution

Inputs are resolved using references in the IField structure:

- `inputs.param_name` - References DAG-level inputs
- `node_id.output_name` - References another node's output
- Direct cache access - Using `get_input` and `insert_value`

## Execution Modes

### Sequential Execution

Execute nodes one at a time in topological order:

```rust
executor.execute_dag_sequential(
    "dag_name",
    &cache,
    cancel_rx
).await?;
```

**Use when:**
- Order matters strictly
- Resources are limited
- Debugging execution flow

### Parallel Execution (Coordinator)

Execute independent nodes concurrently:

```rust
let coordinator = Coordinator::new(hooks, 100, 100);
coordinator.run_parallel(
    &mut executor,
    &cache,
    "dag_name",
    "run_id",
    cancel_rx
).await?;
```

**Use when:**
- Maximum performance needed
- Nodes are CPU/IO intensive
- Have independent branches

### Configuration

```rust
let config = DagConfig {
    // Parallelism
    max_parallel_nodes: 8,          // Max concurrent nodes
    enable_parallel_execution: true, // Enable parallel mode
    
    // Error handling
    on_failure: OnFailure::Stop,    // Stop, Pause, or Continue
    max_attempts: Some(3),          // Retry attempts per node
    
    // Timing
    timeout_seconds: Some(3600),    // Overall DAG timeout
    
    // Retry strategy
    retry_strategy: RetryStrategy::Exponential {
        initial_delay_secs: 1,
        max_delay_secs: 30,
        multiplier: 2.0,
    },
};
```

## Dynamic Node Addition

### Overview

Dynamic node addition allows DAGs to grow during execution based on runtime conditions. This is achieved through the Hook system and ExecutorCommands.

### Hook System

Hooks process execution events and return commands:

```rust
#[async_trait]
pub trait EventHook: Send + Sync {
    async fn handle(&self, ctx: &HookContext, event: &ExecutionEvent) 
        -> Vec<ExecutorCommand>;
    
    async fn on_start(&self, ctx: &HookContext) -> Vec<ExecutorCommand> {
        Vec::new()
    }
    
    async fn on_complete(&self, ctx: &HookContext, success: bool) 
        -> Vec<ExecutorCommand> {
        Vec::new()
    }
}
```

### Implementation Example

```rust
struct DynamicExpansionHook {
    threshold: f64,
}

#[async_trait]
impl EventHook for DynamicExpansionHook {
    async fn handle(&self, ctx: &HookContext, event: &ExecutionEvent) 
        -> Vec<ExecutorCommand> {
        let mut commands = Vec::new();
        
        if let ExecutionEvent::NodeCompleted { node, outcome } = event {
            if let NodeOutcome::Success { outputs: Some(out) } = outcome {
                if let Some(value) = out.get("score").and_then(|v| v.as_f64()) {
                    if value > self.threshold {
                        // Add a new processing node
                        commands.push(ExecutorCommand::AddNode {
                            dag_name: ctx.dag_name.clone(),
                            spec: NodeSpec {
                                id: Some(format!("dynamic_{}", node.node_id)),
                                action: "deep_analysis".to_string(),
                                deps: vec![node.node_id.clone()],
                                inputs: json!({ "score": value }),
                                timeout: Some(60),
                                try_count: Some(2),
                            },
                        });
                    }
                }
            }
        }
        
        commands
    }
}
```

### Using Dynamic Hooks

```rust
let hook = Arc::new(DynamicExpansionHook { threshold: 0.8 });
let coordinator = Coordinator::new(vec![hook], 100, 100);

// Hooks are automatically called during execution
coordinator.run_parallel(&mut executor, &cache, dag_name, run_id, cancel_rx).await?;
```

### Available Commands

```rust
pub enum ExecutorCommand {
    // Add single node
    AddNode { dag_name: String, spec: NodeSpec },
    
    // Add multiple nodes atomically
    AddNodes { dag_name: String, specs: Vec<NodeSpec> },
    
    // Update node inputs
    SetNodeInputs { dag_name: String, node_id: String, inputs: Value },
    
    // Control flow
    PauseBranch { branch_id: String, reason: Option<String> },
    ResumeBranch { branch_id: String },
    CancelBranch { branch_id: String },
    
    // Events
    EmitEvent { event: Value },
}
```

## Cache System

### Overview

The cache system provides persistent storage for node inputs/outputs and execution state.

### Cache Architecture

```
┌─────────────────────────────┐
│      In-Memory Cache        │
│   (DashMap for speed)       │
└──────────┬──────────────────┘
           │
           │ Periodic snapshots
           ↓
┌─────────────────────────────┐
│      SQLite Backend         │
│  (Persistent storage)       │
└─────────────────────────────┘
```

### Cache Operations

#### Writing to Cache

```rust
use dagger::{insert_value, Cache};

// Store simple values
insert_value(&cache, "node_id", "key", "value")?;

// Store JSON values
insert_value(&cache, "node_id", "result", json!({
    "status": "success",
    "data": [1, 2, 3]
}))?;

// Store complex types (must implement Serialize)
#[derive(Serialize)]
struct MyData {
    id: String,
    values: Vec<f64>,
}
insert_value(&cache, "node_id", "data", MyData { ... })?;
```

#### Reading from Cache

```rust
use dagger::get_input;

// Read with type inference
let value: String = get_input(&cache, "node_id", "key")?;

// Read JSON values
let data: serde_json::Value = get_input(&cache, "node_id", "result")?;

// Read custom types (must implement Deserialize)
let my_data: MyData = get_input(&cache, "node_id", "data")?;
```

### Cache Strategies

Nodes can specify caching behavior:

```rust
pub enum CacheStrategy {
    /// Always use cache if available
    Always,
    
    /// Never use cache, always recompute
    Never,
    
    /// Use cache if less than N seconds old
    Ttl(u64),
    
    /// Custom cache key generation
    Custom(String),
}
```

### SQLite Configuration

The SQLite backend can be configured:

```rust
// In-memory (fast, non-persistent)
let executor = DagExecutor::new(config, registry, "sqlite::memory:").await?;

// File-based (persistent)
let executor = DagExecutor::new(config, registry, "sqlite:dag_state.db").await?;

// With WAL mode for better concurrency
let executor = DagExecutor::new(
    config, 
    registry, 
    "sqlite:dag_state.db?mode=wal"
).await?;
```

## Configuration

### DagConfig Structure

```rust
pub struct DagConfig {
    // Execution control
    pub max_attempts: Option<u8>,              // Max retry attempts per node
    pub on_failure: OnFailure,                 // Behavior on node failure
    pub timeout_seconds: Option<u64>,          // Overall DAG timeout
    
    // Parallelism
    pub max_parallel_nodes: usize,             // Max concurrent nodes
    pub enable_parallel_execution: bool,       // Enable parallel mode
    
    // Retry strategy
    pub retry_strategy: RetryStrategy,         // How to handle retries
    
    // Human interaction
    pub human_wait_minutes: Option<u32>,       // Wait for human input
    pub human_timeout_action: HumanTimeoutAction, // Action on timeout
    
    // Resource limits
    pub max_tokens: Option<u64>,               // Token limit for LLM nodes
    pub max_iterations: Option<u32>,           // Max loop iterations
    
    // Monitoring
    pub review_frequency: Option<u32>,         // Human review frequency
    
    // Cache
    pub enable_incremental_cache: bool,        // Incremental cache updates
    pub cache_snapshot_interval: u64,          // Snapshot frequency (seconds)
}
```

### Retry Strategies

```rust
pub enum RetryStrategy {
    /// No delay between retries
    Immediate,
    
    /// Fixed delay between retries
    Linear { delay_secs: u64 },
    
    /// Exponential backoff (default)
    Exponential {
        initial_delay_secs: u64,  // Starting delay
        max_delay_secs: u64,      // Maximum delay
        multiplier: f64,          // Backoff multiplier
    },
}

// Default: Exponential with 2s initial, 60s max, 2x multiplier
```

### Failure Behaviors

```rust
pub enum OnFailure {
    /// Continue executing other nodes
    Continue,
    
    /// Pause execution (can be resumed)
    Pause,
    
    /// Stop execution immediately
    Stop,
}
```

## Performance Optimization

### 1. Minimize Retry Delays

For time-sensitive operations, use immediate retry:

```rust
let config = DagConfig {
    retry_strategy: RetryStrategy::Immediate,
    ..Default::default()
};
```

### 2. Optimize Parallelism

Set `max_parallel_nodes` based on your system:

```rust
let config = DagConfig {
    max_parallel_nodes: num_cpus::get() * 2,  // CPU-bound tasks
    // OR
    max_parallel_nodes: 50,  // IO-bound tasks
    ..Default::default()
};
```

### 3. Use WAL Mode for SQLite

For better concurrency with SQLite:

```rust
let executor = DagExecutor::new(
    config,
    registry,
    "sqlite:state.db?mode=wal&busy_timeout=5000"
).await?;
```

### 4. Cache Strategically

Use appropriate cache strategies:

```rust
// For expensive computations
node.cache_strategy = CacheStrategy::Always;

// For real-time data
node.cache_strategy = CacheStrategy::Never;

// For time-sensitive data
node.cache_strategy = CacheStrategy::Ttl(300); // 5 minutes
```

### 5. Avoid Blocking Operations

In actions, use async operations:

```rust
// Good
let data = reqwest::get(url).await?;

// Bad - blocks the executor
let data = std::thread::sleep(Duration::from_secs(5));
```

## API Reference

### DagExecutor Methods

```rust
impl DagExecutor {
    /// Create new executor
    pub async fn new(
        config: Option<DagConfig>,
        registry: ActionRegistry,
        database_url: &str,
    ) -> Result<Self>;
    
    /// Load DAG from YAML file
    pub async fn load_yaml_file(&mut self, file_path: &str) -> Result<()>;
    
    /// Load all YAML files from directory
    pub fn load_yaml_dir(&mut self, dir_path: &str);
    
    /// Add node to DAG
    pub async fn add_node(
        &mut self,
        dag_name: &str,
        node_id: String,
        action_name: String,
        dependencies: Vec<String>,
    ) -> Result<()>;
    
    /// Add multiple nodes atomically
    pub async fn add_nodes_atomic(
        &mut self,
        dag_name: &str,
        specs: Vec<NodeSpec>,
        cache: &Cache,
    ) -> Result<Vec<String>>;
    
    /// Set node inputs from JSON
    pub async fn set_node_inputs_json(
        &mut self,
        dag_name: &str,
        node_id: &str,
        inputs: Value,
        cache: &Cache,
    ) -> Result<()>;
    
    /// Execute DAG sequentially
    pub async fn execute_dag_sequential(
        &mut self,
        dag_name: &str,
        cache: &Cache,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<DagExecutionReport>;
    
    /// Pause execution branch
    pub fn pause_branch(&mut self, branch_id: &str, reason: Option<&str>);
    
    /// Resume paused branch
    pub fn resume_branch(&mut self, branch_id: &str);
    
    /// Cancel branch
    pub fn cancel_branch(&mut self, branch_id: &str, reason: Option<&str>);
    
    /// Generate unique node ID
    pub fn gen_node_id(&self, prefix: &str) -> String;
    
    /// Get DOT graph representation
    pub fn serialize_tree_to_dot(&self, dag_name: &str) -> Result<String>;
}
```

### Coordinator Methods

```rust
impl Coordinator {
    /// Create new coordinator
    pub fn new(
        hooks: Vec<Arc<dyn EventHook>>,
        cap_events: usize,
        cap_cmds: usize,
    ) -> Self;
    
    /// Run parallel execution
    pub async fn run_parallel(
        self,
        exec: &mut DagExecutor,
        cache: &Cache,
        dag_name: &str,
        run_id: &str,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<()>;
}
```

### Cache Functions

```rust
/// Insert value into cache
pub fn insert_value<T: Serialize>(
    cache: &Cache,
    node_id: &str,
    key: &str,
    value: T,
) -> Result<()>;

/// Get value from cache
pub fn get_input<T: DeserializeOwned>(
    cache: &Cache,
    node_id: &str,
    key: &str,
) -> Result<T>;

/// Check if value exists
pub fn has_value(
    cache: &Cache,
    node_id: &str,
    key: &str,
) -> bool;

/// Clear node cache
pub fn clear_node_cache(
    cache: &Cache,
    node_id: &str,
) -> Result<()>;
```

### ActionRegistry Methods

```rust
impl ActionRegistry {
    /// Create new registry
    pub fn new() -> Self;
    
    /// Register action
    pub fn register(&mut self, action: Arc<dyn NodeAction>);
    
    /// Get action by name
    pub fn get(&self, name: &str) -> Option<Arc<dyn NodeAction>>;
    
    /// Check if action exists
    pub fn contains(&self, name: &str) -> bool;
    
    /// List all registered actions
    pub fn list_actions(&self) -> Vec<String>;
}
```

## Troubleshooting

### Common Issues and Solutions

#### 1. 10-15 Second Delays in Execution

**Cause**: Default retry strategy uses exponential backoff with 2-second initial delay.

**Solution**: Use immediate retry strategy:
```rust
config.retry_strategy = RetryStrategy::Immediate;
```

#### 2. "Node not found: DAG not found" Error

**Cause**: DAG not loaded or initialized.

**Solution**: Ensure DAG is loaded before execution:
```rust
executor.load_yaml_file("dag.yaml").await?;
// OR create minimal DAG
executor.add_node(dag_name, "node1", "action", vec![]).await?;
```

#### 3. Nodes Receiving Empty/Null Inputs

**Cause**: Inputs not properly set or cache references incorrect.

**Solution**: Use `set_node_inputs_json` to properly configure inputs:
```rust
executor.set_node_inputs_json(
    dag_name,
    node_id,
    json!({ "param": "value" }),
    &cache
).await?;
```

#### 4. SQLite Database Locked Errors

**Cause**: Multiple connections trying to write simultaneously.

**Solution**: Use WAL mode:
```rust
"sqlite:file.db?mode=wal&busy_timeout=5000"
```

#### 5. Coordinator Hanging After Completion

**Cause**: Event loop not detecting completion (fixed in latest version).

**Solution**: Ensure using latest version with completion checks.

#### 6. Dynamic Nodes Not Executing

**Cause**: Dependencies not satisfied or inputs not available.

**Solution**: Ensure parent nodes are in deps and inputs are stored:
```rust
NodeSpec {
    deps: vec![parent_node_id],  // Must include dependencies
    inputs: json!({ ... }),       // Must provide required inputs
    ..
}
```

### Debugging Tips

#### Enable Detailed Logging

```rust
use tracing_subscriber;

tracing_subscriber::fmt()
    .with_max_level(tracing::Level::DEBUG)
    .init();
```

#### Inspect Cache Contents

```rust
use dagger::serialize_cache_to_prettyjson;

let json = serialize_cache_to_prettyjson(&cache)?;
println!("Cache state: {}", json);
```

#### Generate Execution Graph

```rust
let dot = executor.serialize_tree_to_dot(dag_name)?;
// Save to file and visualize with Graphviz
std::fs::write("dag.dot", dot)?;
```

#### Monitor Execution Events

```rust
struct LoggingHook;

#[async_trait]
impl EventHook for LoggingHook {
    async fn handle(&self, ctx: &HookContext, event: &ExecutionEvent) 
        -> Vec<ExecutorCommand> {
        println!("Event: {:?}", event);
        Vec::new()
    }
}
```

### Performance Profiling

#### Measure Node Execution Time

```rust
struct TimingHook {
    start_times: Arc<DashMap<String, Instant>>,
}

#[async_trait]
impl EventHook for TimingHook {
    async fn handle(&self, ctx: &HookContext, event: &ExecutionEvent) 
        -> Vec<ExecutorCommand> {
        match event {
            ExecutionEvent::NodeStarted { node } => {
                self.start_times.insert(node.node_id.clone(), Instant::now());
            }
            ExecutionEvent::NodeCompleted { node, .. } => {
                if let Some((_, start)) = self.start_times.remove(&node.node_id) {
                    println!("Node {} took {:?}", node.node_id, start.elapsed());
                }
            }
            _ => {}
        }
        Vec::new()
    }
}
```

## Migration Guide

### From Sequential to Parallel Execution

Replace:
```rust
executor.execute_dag_sequential(dag_name, &cache, cancel_rx).await?;
```

With:
```rust
let coordinator = Coordinator::new(vec![], 100, 100);
coordinator.run_parallel(&mut executor, &cache, dag_name, run_id, cancel_rx).await?;
```

### From Function Actions to Trait Actions

Replace:
```rust
async fn my_action(exec: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    // ...
}
registry.register_fn("my_action", my_action);
```

With:
```rust
struct MyAction;

#[async_trait]
impl NodeAction for MyAction {
    fn name(&self) -> &str { "my_action" }
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        // ...
    }
}
registry.register(Arc::new(MyAction));
```

### API Breaking Changes (Latest)

The following functions now require a `cache` parameter:
- `set_node_inputs_json(..., cache: &Cache)`
- `add_nodes_atomic(..., cache: &Cache)`

Update your code:
```rust
// Old
executor.set_node_inputs_json(dag_name, node_id, inputs).await?;

// New
executor.set_node_inputs_json(dag_name, node_id, inputs, &cache).await?;
```

## Best Practices

1. **Always Use Async Operations**: Avoid blocking the executor thread
2. **Configure Appropriate Timeouts**: Set node-level timeouts based on expected execution time
3. **Use Immediate Retry for Fast Failures**: Avoid exponential backoff for quick retry scenarios
4. **Cache Expensive Computations**: Use `CacheStrategy::Always` for deterministic, expensive operations
5. **Monitor with Hooks**: Implement EventHooks for logging, metrics, and debugging
6. **Use WAL Mode for SQLite**: Better concurrency for multi-node execution
7. **Set Reasonable Parallelism**: Based on CPU cores for compute-bound, higher for I/O-bound
8. **Handle Errors Gracefully**: Use `OnFailure::Continue` for non-critical nodes
9. **Version Your DAGs**: Use the version field in Graph for tracking changes
10. **Test with Small DAGs First**: Validate logic before scaling up

## Conclusion

DAG Flow provides a robust, high-performance system for executing complex workflows with parallel execution, dynamic node addition, and comprehensive error handling. By following this guide and best practices, you can build scalable, maintainable data pipelines and workflow orchestration systems.

For additional support, consult the example implementations in the `examples/` directory or refer to the inline documentation in the source code.