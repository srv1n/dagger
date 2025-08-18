# DAG Flow Implementation Guide

A complete guide for implementing DAG Flow workflows using the new coordinator-based architecture.

## Table of Contents

1. [Quick Start](#quick-start)
2. [Architecture Overview](#architecture-overview)
3. [Setting Up the System](#setting-up-the-system)
4. [Creating Actions](#creating-actions)
5. [Implementing Hooks](#implementing-hooks)
6. [Working with Workflows](#working-with-workflows)
7. [Cache Operations](#cache-operations)
8. [Execution Control](#execution-control)
9. [Dynamic Graph Growth](#dynamic-graph-growth)
10. [Error Handling](#error-handling)
11. [Migration Guide](#migration-guide)
12. [Complete Examples](#complete-examples)

## Quick Start

```rust
use dagger::coord::{
    ActionRegistry, Coordinator, NodeAction, NodeCtx, NodeOutput,
    EventHook, HookContext, ExecutionEvent, ExecutorCommand
};
use dagger::dag_flow::{DagExecutor, DagConfig, Cache};
use std::sync::Arc;
use tokio::sync::oneshot;

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Create action registry and register actions
    let registry = ActionRegistry::new();
    registry.register(Arc::new(MyAction));
    
    // 2. Create hooks for dynamic behavior
    let hooks = vec![
        Arc::new(MyHook) as Arc<dyn EventHook>,
    ];
    
    // 3. Create coordinator
    let coordinator = Coordinator::new(hooks, 100, 100);
    
    // 4. Create executor and load workflow
    let mut executor = DagExecutor::new(
        None,
        Arc::new(Default::default()),  // Legacy registry (being phased out)
        "sqlite:workflows.db"
    ).await?;
    executor.load_yaml_file("workflow.yaml")?;
    
    // 5. Execute with coordinator
    let cache = Cache::new();
    let (_tx, rx) = oneshot::channel();
    coordinator.run_parallel(
        &mut executor,
        &cache,
        "my_workflow",
        "run_001",
        rx
    ).await?;
    
    Ok(())
}
```

## Architecture Overview

### What is DAG Flow?

DAG Flow is a coordinator-based workflow execution system that:
- Executes directed acyclic graphs (DAGs) with true parallelism
- Separates computation (NodeAction) from control (Coordinator) and policy (EventHook)
- Enables dynamic graph growth without borrow checker issues
- Uses message-passing for all communication
- Persists state to SQLite for recovery and resumption
- Provides thread-safe cache access via DashMap
- Supports event-driven workflow modification

### Key Components

1. **Coordinator**: Central orchestrator with exclusive `&mut DagExecutor` access
2. **NodeAction**: Pure computation trait (no state mutation)
3. **EventHook**: Policy layer that processes events into commands
4. **ActionRegistry**: Registry for NodeAction instances
5. **Cache**: Thread-safe dual-layer storage (DashMap + SQLite)
6. **Events/Commands**: Message protocol between components
7. **DagExecutor**: Core DAG state and execution logic

## Setting Up the System

### Basic Setup

```rust
use dagger::coord::{ActionRegistry, Coordinator, EventHook};
use dagger::dag_flow::{DagExecutor, DagConfig};
use std::sync::Arc;

// 1. Create action registry
let action_registry = ActionRegistry::new();

// 2. Register your actions
action_registry.register(Arc::new(ProcessAction));
action_registry.register(Arc::new(ValidateAction));
action_registry.register(Arc::new(TransformAction));

// 3. Create hooks for dynamic behavior
let hooks: Vec<Arc<dyn EventHook>> = vec![
    Arc::new(PlannerHook),    // Add nodes based on results
    Arc::new(MonitorHook),     // Track execution
    Arc::new(ErrorHook),       // Handle failures
];

// 4. Create coordinator with capacity for channels
let coordinator = Coordinator::new(
    hooks,
    100,  // Event channel capacity
    100   // Command channel capacity
);

// 5. Create executor
let mut executor = DagExecutor::new(
    None,
    Arc::new(Default::default()),  // Legacy registry
    "sqlite:workflows.db"
).await?;
```

### Custom Configuration

```rust
use dagger::{DagConfig, RetryStrategy, OnFailure};

let config = DagConfig {
    // Parallel execution settings
    enable_parallel_execution: true,
    max_parallel_nodes: 4,
    
    // Retry configuration
    max_attempts: Some(3),
    retry_strategy: RetryStrategy::Exponential {
        initial_delay_secs: 2,
        max_delay_secs: 60,
        multiplier: 2.0,
    },
    
    // Failure handling
    on_failure: OnFailure::Continue,  // Continue, Halt, or Pause
    
    // Timeouts
    timeout_seconds: Some(3600),  // Global timeout
    
    // Human intervention
    human_wait_minutes: Some(30),
    human_timeout_action: HumanTimeoutAction::Autopilot,
    
    // Resource limits
    max_tokens: Some(100_000),
    max_iterations: Some(10),
    
    // Cache settings
    enable_incremental_cache: true,
    cache_snapshot_interval: 60,  // seconds
    
    ..Default::default()
};

let mut executor = DagExecutor::new(Some(config), registry, "sqlite:workflows.db").await?;
```

## Creating Actions

### The New NodeAction Trait

```rust
use dagger::coord::{NodeAction, NodeCtx, NodeOutput};
use async_trait::async_trait;
use anyhow::Result;
use serde_json::{json, Value};

// Define an action (no &mut DagExecutor!)
struct ProcessAction;

#[async_trait]
impl NodeAction for ProcessAction {
    fn name(&self) -> &str {
        "process_data"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        // Pure computation - no executor access
        println!("Processing node: {}", ctx.node_id);
        
        // Access inputs from context
        let input_value = ctx.inputs.get("data")
            .ok_or_else(|| anyhow!("Missing input data"))?;
        
        // Perform computation
        let result = process_data(input_value)?;
        
        // Return output
        Ok(NodeOutput::success(json!({
            "processed": result,
            "timestamp": chrono::Utc::now()
        })))
    }
}

// Register with the action registry
action_registry.register(Arc::new(ProcessAction));
```

### Action with Cache Access

```rust
struct TransformAction {
    config: TransformConfig,
}

#[async_trait]
impl NodeAction for TransformAction {
    fn name(&self) -> &str {
        "transform_text"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        // Read from inputs
        let text = ctx.inputs.get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing text input"))?;
        
        // Access cache for additional data (read-only)
        if let Some(previous_result) = ctx.cache.get("previous_run", "result")? {
            println!("Found previous result: {:?}", previous_result);
        }
        
        // Transform the data
        let result = match self.config.mode {
            TransformMode::Upper => text.to_uppercase(),
            TransformMode::Lower => text.to_lowercase(),
            TransformMode::Reverse => text.chars().rev().collect(),
        };
        
        // Return multiple outputs
        Ok(NodeOutput::success(json!({
            "transformed": result,
            "original_length": text.len(),
            "processed_at": chrono::Utc::now(),
        })))
    }
}
```

### Action with Complex Types

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct ProcessingResult {
    status: String,
    data: Vec<String>,
    metadata: HashMap<String, Value>,
}

struct ComplexProcessor {
    processor_id: String,
}

#[async_trait]
impl NodeAction for ComplexProcessor {
    fn name(&self) -> &str {
        "complex_processor"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        // Parse complex input from context
        let config: HashMap<String, Value> = serde_json::from_value(
            ctx.inputs.get("config")
                .ok_or_else(|| anyhow!("Missing config"))?
                .clone()
        )?;
        
        // Process data
        let result = ProcessingResult {
            status: "completed".to_string(),
            data: vec!["item1".to_string(), "item2".to_string()],
            metadata: config,
        };
        
        // Return complex output
        Ok(NodeOutput::success(serde_json::to_value(result)?))
    }
}
```

## Implementing Hooks

### EventHook for Dynamic Behavior

```rust
use dagger::coord::{EventHook, HookContext, ExecutionEvent, ExecutorCommand, NodeSpec};
use async_trait::async_trait;

struct PlannerHook {
    max_depth: usize,
}

#[async_trait]
impl EventHook for PlannerHook {
    async fn handle(
        &self,
        ctx: &HookContext,
        event: &ExecutionEvent
    ) -> Vec<ExecutorCommand> {
        match event {
            ExecutionEvent::NodeCompleted { node, outcome } => {
                // Check if this was an analysis node
                if node.node_id.starts_with("analyze_") {
                    // Read the outcome to decide next steps
                    if let Some(analysis) = outcome.outputs.as_ref() {
                        if analysis["needs_validation"] == true {
                            // Add validation node
                            return vec![ExecutorCommand::AddNode {
                                dag_name: ctx.dag_name.clone(),
                                spec: NodeSpec::new("validate")
                                    .with_id(format!("validate_{}", node.node_id))
                                    .with_deps(vec![node.node_id.clone()])
                                    .with_inputs(json!({
                                        "data": analysis["data"]
                                    })),
                            }];
                        }
                    }
                }
            }
            ExecutionEvent::NodeFailed { node, error } => {
                // Add recovery node for failures
                return vec![ExecutorCommand::AddNode {
                    dag_name: ctx.dag_name.clone(),
                    spec: NodeSpec::new("recover")
                        .with_id(format!("recover_{}", node.node_id))
                        .with_deps(vec![node.node_id.clone()])
                        .with_inputs(json!({
                            "failed_node": node.node_id,
                            "error": error
                        })),
                }];
            }
            _ => {}
        }
        vec![]
    }
    
    async fn on_start(&self, ctx: &HookContext) -> Vec<ExecutorCommand> {
        println!("Starting workflow: {}", ctx.dag_name);
        vec![]
    }
    
    async fn on_complete(&self, ctx: &HookContext, success: bool) -> Vec<ExecutorCommand> {
        println!("Workflow {} completed: {}", ctx.dag_name, success);
        vec![]
    }
}
```

### Monitoring Hook

```rust
struct MonitorHook {
    metrics: Arc<Mutex<ExecutionMetrics>>,
}

#[async_trait]
impl EventHook for MonitorHook {
    async fn handle(
        &self,
        _ctx: &HookContext,
        event: &ExecutionEvent
    ) -> Vec<ExecutorCommand> {
        let mut metrics = self.metrics.lock().await;
        
        match event {
            ExecutionEvent::NodeStarted { node } => {
                metrics.nodes_started += 1;
                println!("⚡ Started: {}", node.node_id);
            }
            ExecutionEvent::NodeCompleted { node, .. } => {
                metrics.nodes_completed += 1;
                println!("✅ Completed: {}", node.node_id);
            }
            ExecutionEvent::NodeFailed { node, error } => {
                metrics.nodes_failed += 1;
                println!("❌ Failed: {} - {}", node.node_id, error);
            }
        }
        
        vec![]  // Monitor doesn't generate commands
    }
}
```

## Working with Workflows

### Loading Workflows

```rust
// Load from file
executor.load_yaml_file("workflows/pipeline.yaml")?;

// Load from string
let yaml_content = std::fs::read_to_string("pipeline.yaml")?;
executor.load_yaml_string(&yaml_content)?;

// Load multiple workflows
executor.load_yaml_file("workflow1.yaml")?;
executor.load_yaml_file("workflow2.yaml")?;
```

### YAML Workflow Structure

```yaml
name: data_processing_pipeline
description: Process customer data with validation
author: Engineering Team
version: 1.0.0
signature: unique_signature
tags:
  - production
  - customer_data
  - etl

nodes:
  - id: validate_input
    dependencies: []
    inputs:
      - name: raw_data
        description: Raw customer data
        reference: inputs.customer_data
    outputs:
      - name: valid_data
        description: Validated data
    action: validate_data
    failure: handle_validation_error  # Failure handler action
    onfailure: false  # Stop on failure
    timeout: 300  # 5 minutes
    try_count: 2  # Retry once

  - id: transform_data
    dependencies: [validate_input]
    inputs:
      - name: data
        reference: validate_input.valid_data
    outputs:
      - name: transformed
    action: transform_customer_data
    timeout: 600
    try_count: 3

  - id: parallel_analysis_a
    dependencies: [transform_data]
    inputs:
      - name: data
        reference: transform_data.transformed
    outputs:
      - name: analysis_a
    action: analyze_type_a
    timeout: 1800

  - id: parallel_analysis_b
    dependencies: [transform_data]
    inputs:
      - name: data
        reference: transform_data.transformed
    outputs:
      - name: analysis_b
    action: analyze_type_b
    timeout: 1800

  - id: combine_results
    dependencies: [parallel_analysis_a, parallel_analysis_b]
    inputs:
      - name: analysis_a
        reference: parallel_analysis_a.analysis_a
      - name: analysis_b
        reference: parallel_analysis_b.analysis_b
    outputs:
      - name: final_report
    action: generate_report
    timeout: 900
```

### Listing and Filtering Workflows

```rust
// List all loaded workflows
let all_dags = executor.list_dags()?;
println!("Available workflows: {:?}", all_dags);

// Filter workflows by tags
let production_dags = executor.list_dags_by_tags(&["production"])?;
println!("Production workflows: {:?}", production_dags);

// Get workflows with multiple tags (AND condition)
let customer_etl = executor.list_dags_by_tags(&["customer_data", "etl"])?;

// Check if a workflow exists
if executor.has_dag("data_processing_pipeline") {
    println!("Workflow exists!");
}

// Get workflow metadata
let metadata = executor.get_dag_metadata("data_processing_pipeline")?;
println!("Author: {}, Version: {}", metadata.author, metadata.version);
```

## Cache Operations

### Writing to Cache

```rust
use dagger::{Cache, insert_value};

// Create cache
let cache = Cache::new();

// Write simple values
insert_value(&cache, "inputs", "username", "john_doe")?;
insert_value(&cache, "inputs", "age", 30)?;
insert_value(&cache, "inputs", "active", true)?;

// Write complex structures
let config = json!({
    "timeout": 30,
    "retries": 3,
    "endpoints": ["api1.example.com", "api2.example.com"]
});
insert_value(&cache, "inputs", "config", config)?;

// Write to node-specific namespace
insert_value(&cache, "node1", "status", "completed")?;
insert_value(&cache, "node1", "result", vec![1, 2, 3, 4, 5])?;
```

### Reading from Cache

```rust
use dagger::{parse_input_from_name, get_value_from_cache};

// In an action function
async fn my_action(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    // Read input using node input references
    let username: String = parse_input_from_name(
        cache, "username", &node.inputs
    )?;
    
    // Read with type inference
    let age: i32 = parse_input_from_name(
        cache, "age", &node.inputs
    )?;
    
    // Read complex types
    let config: HashMap<String, Value> = parse_input_from_name(
        cache, "config", &node.inputs
    )?;
    
    // Direct cache access (outside of actions)
    let value = get_value_from_cache(&cache, "node1", "result")?;
    
    Ok(())
}
```

### Cache Persistence

The cache is automatically persisted to SQLite during execution. You can also manually manage persistence:

```rust
// Automatic persistence happens after each node execution
// The SQLite cache manages this internally

// Manual cache snapshot
executor.save_cache_snapshot("workflow_name", &cache).await?;

// Load cache from previous execution
let restored_cache = executor.load_cache_snapshot("workflow_name").await?;

// Get cache as JSON for debugging
let cache_json = serialize_cache_to_prettyjson(&cache)?;
println!("Cache contents:\n{}", cache_json);
```

### SQLite Storage Details

The DAG Flow system uses SQLite for comprehensive state management:

```rust
// Database initialization (happens automatically)
// When creating executor with "sqlite:workflows.db"
// The following tables are created:
// - artifacts: Cache data storage
// - execution_trees: Workflow execution history
// - snapshots: Point-in-time state captures
// - execution_state: Active/pending execution tracking

// Access the SQLite cache directly if needed
let sqlite_cache = &executor.sqlite_cache;

// Save incremental cache updates (delta)
let mut delta = HashMap::new();
let mut node_delta = HashMap::new();
node_delta.insert("output".to_string(), SerializableData::String("result".to_string()));
delta.insert("node1".to_string(), node_delta);
sqlite_cache.save_cache_delta("workflow_name", delta).await?;

// Load execution tree for visualization
if let Some(tree) = sqlite_cache.load_execution_tree("workflow_name").await? {
    println!("Execution tree has {} nodes", tree.nodes.len());
}

// Debug database contents
sqlite_cache.debug_print_db().await?;
```

### Recovery from Failures

SQLite persistence enables robust recovery:

```rust
// After a crash or failure, recover state
let mut executor = DagExecutor::new(
    Some(config),
    registry,
    "sqlite:workflows.db"  // Same database
).await?;

// Load the previous cache state
let cache = executor.load_cache_snapshot("workflow_name").await?;

// Check execution state
if let Some(state) = executor.sqlite_cache
    .load_execution_state("workflow_name", "active").await? {
    println!("Found active execution state");
}

// Resume execution from where it left off
let (_tx, rx) = oneshot::channel();
let report = executor.execute_static_dag(
    "workflow_name",
    &cache,  // Use recovered cache
    rx
).await?;
```

## Execution Control

### Coordinator-Based Execution

```rust
use dagger::coord::Coordinator;

// Execute with the coordinator
let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

// Run parallel execution via coordinator
coordinator.run_parallel(
    &mut executor,
    &cache,
    "workflow_name",
    "run_001",  // Unique run ID
    cancel_rx
).await?;

// The coordinator handles:
// - Spawning workers for ready nodes
// - Processing events through hooks
// - Applying commands to mutate DAG
// - Managing backpressure
// - Graceful shutdown on cancellation
```

### Cancellation

```rust
use tokio::time::{timeout, Duration};

let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

// Spawn execution in background
let executor_handle = tokio::spawn(async move {
    executor.execute_static_dag("workflow", &cache, cancel_rx).await
});

// Cancel after some condition
tokio::time::sleep(Duration::from_secs(10)).await;
cancel_tx.send(()).ok();

// Wait for graceful shutdown
let result = executor_handle.await?;
```

### Parallel Execution Control

```rust
// The coordinator always executes in parallel
// Control concurrency via semaphore in coordinator

let coordinator = Coordinator::new_with_concurrency(
    hooks,
    100,     // Event channel capacity
    100,     // Command channel capacity
    4        // Max parallel workers
);

// Or use DagConfig for executor-level control
let config = DagConfig {
    max_parallel_nodes: Some(4),  // Limit concurrent nodes
    enable_parallel_execution: true,  // Must be true for coordinator
    ..Default::default()
};
```

## Dynamic Graph Growth

### Adding Nodes During Execution

The coordinator architecture enables safe dynamic node addition:

```rust
struct DynamicBuilderHook;

#[async_trait]
impl EventHook for DynamicBuilderHook {
    async fn handle(
        &self,
        ctx: &HookContext,
        event: &ExecutionEvent
    ) -> Vec<ExecutorCommand> {
        match event {
            ExecutionEvent::NodeCompleted { node, outcome } => {
                if node.node_id == "analyze_task" {
                    // Build workflow based on analysis
                    let mut commands = vec![];
                    
                    if let Some(steps) = outcome.outputs["steps"].as_array() {
                        for (i, step) in steps.iter().enumerate() {
                            commands.push(ExecutorCommand::AddNode {
                                dag_name: ctx.dag_name.clone(),
                                spec: NodeSpec::new(step["action"].as_str().unwrap())
                                    .with_id(format!("step_{}", i))
                                    .with_deps(if i == 0 {
                                        vec![node.node_id.clone()]
                                    } else {
                                        vec![format!("step_{}", i - 1)]
                                    })
                                    .with_inputs(step["inputs"].clone()),
                            });
                        }
                    }
                    
                    return commands;
                }
            }
            _ => {}
        }
        vec![]
    }
}
```

### Batch Node Addition

```rust
// Add multiple nodes atomically
let commands = vec![ExecutorCommand::AddNodes {
    dag_name: "workflow".to_string(),
    specs: vec![
        NodeSpec::new("process_1")
            .with_deps(vec!["start"]),
        NodeSpec::new("process_2")
            .with_deps(vec!["start"]),
        NodeSpec::new("aggregate")
            .with_deps(vec!["process_1", "process_2"]),
    ],
}];
```

## Pause and Resume

### Implementing Pauseable Workflows with Hooks

```rust
struct HumanInterventionHook {
    intervention_required: Arc<Mutex<bool>>,
}

#[async_trait]
impl EventHook for HumanInterventionHook {
    async fn handle(
        &self,
        ctx: &HookContext,
        event: &ExecutionEvent
    ) -> Vec<ExecutorCommand> {
        match event {
            ExecutionEvent::NodeCompleted { node, outcome } => {
                // Check if human review is needed
                if outcome.outputs.get("needs_review") == Some(&json!(true)) {
                    *self.intervention_required.lock().await = true;
                    
                    // Pause the branch
                    return vec![ExecutorCommand::PauseBranch {
                        branch_id: node.node_id.clone(),
                        reason: Some("Human review required".to_string()),
                    }];
                }
            }
            _ => {}
        }
        vec![]
    }
}

// Resume after human input
struct ResumeAction;

#[async_trait]
impl NodeAction for ResumeAction {
    fn name(&self) -> &str {
        "resume_after_review"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        // Process human input
        let approval = ctx.inputs.get("approval")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        
        if approval {
            // Return command to resume
            Ok(NodeOutput::success(json!({
                "action": "resume",
                "branch": ctx.inputs["branch_id"]
            })))
        } else {
            Ok(NodeOutput::success(json!({
                "action": "cancel",
                "reason": "Not approved"
            })))
        }
    }
}
```

### Resume from Pause

```rust
// Check if workflow is paused
if executor.is_paused() {
    println!("Workflow is paused. Resuming...");
    
    // Optionally update cache with human input
    insert_value(&cache, "human_input", "approval", "approved")?;
    
    // Resume execution
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    executor.resume_from_pause(&cache, cancel_rx).await?;
}
```

### Checkpoint and Recovery

```rust
// Save execution state
executor.save_execution_snapshot("workflow_name").await?;

// Recover from checkpoint
let executor = DagExecutor::recover_from_snapshot(
    "workflow_name",
    registry,
    "sqlite:workflows.db"
).await?;

// Continue execution from last checkpoint
let report = executor.resume_execution(&cache, cancel_rx).await?;
```

## Error Handling

### Retry Strategies

```rust
use dagger::RetryStrategy;

// Exponential backoff
let retry_strategy = RetryStrategy::Exponential {
    initial_delay_secs: 2,
    max_delay_secs: 60,
    multiplier: 2.0,
};

// Linear backoff
let retry_strategy = RetryStrategy::Linear {
    delay_secs: 5,
};

// Immediate retry
let retry_strategy = RetryStrategy::Immediate;
```

### Failure Handlers

```rust
// Define a failure handler action
async fn handle_error(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    // Access error information
    let error_msg = get_value_from_cache(cache, &node.id, "error")?;
    
    // Log error
    eprintln!("Node {} failed: {}", node.id, error_msg);
    
    // Attempt recovery
    insert_value(cache, &node.id, "recovery_attempted", true)?;
    
    // Could send alerts, clean up resources, etc.
    
    Ok(())
}

register_action!(executor, "handle_error", handle_error);
```

### Error Recovery Patterns

```rust
use dagger::{DagError, OnFailure};

match executor.execute_static_dag("workflow", &cache, cancel_rx).await {
    Ok(report) => {
        println!("Success: {} nodes completed", report.completed_nodes);
    }
    Err(DagError::ExecutionTimeout(msg)) => {
        eprintln!("Timeout: {}", msg);
        // Could resume with extended timeout
    }
    Err(DagError::ValidationError(msg)) => {
        eprintln!("Invalid workflow: {}", msg);
        // Fix workflow definition
    }
    Err(DagError::NodeExecutionError { node_id, error }) => {
        eprintln!("Node {} failed: {}", node_id, error);
        // Could retry specific node
    }
    Err(e) => {
        eprintln!("Unexpected error: {}", e);
    }
}
```

## Migration Guide

### Migrating from Old NodeAction to New

#### Old Pattern (Direct Executor Access)
```rust
// OLD - Had access to &mut DagExecutor
async fn my_action(
    executor: &mut DagExecutor,  // Could mutate executor
    node: &Node,
    cache: &Cache
) -> Result<()> {
    // Could add nodes directly
    executor.add_node(...)?
    // Could mutate state
    executor.set_paused(true);
    Ok(())
}
```

#### New Pattern (Pure Computation)
```rust
// NEW - No executor access
struct MyAction;

#[async_trait]
impl NodeAction for MyAction {
    fn name(&self) -> &str { "my_action" }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        // Pure computation only
        // Return data, let hooks handle control flow
        Ok(NodeOutput::success(json!({
            "result": "computed_value",
            "needs_validation": true  // Hook can read this
        })))
    }
}
```

### Migrating Control Flow to Hooks

#### Old Pattern (In-Action Control)
```rust
// OLD - Control flow in action
async fn conditional_action(
    executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    let value = get_input(...);
    
    if value > threshold {
        executor.add_node("workflow", "extra_step", ...)?;
    }
    
    Ok(())
}
```

#### New Pattern (Hook-Based Control)
```rust
// NEW - Control flow in hook
struct ConditionalHook;

#[async_trait]
impl EventHook for ConditionalHook {
    async fn handle(
        &self,
        ctx: &HookContext,
        event: &ExecutionEvent
    ) -> Vec<ExecutorCommand> {
        if let ExecutionEvent::NodeCompleted { outcome, .. } = event {
            if let Some(value) = outcome.outputs["value"].as_f64() {
                if value > threshold {
                    return vec![ExecutorCommand::AddNode {
                        dag_name: ctx.dag_name.clone(),
                        spec: NodeSpec::new("extra_step"),
                    }];
                }
            }
        }
        vec![]
    }
}
```

### Complete Migration Example

```rust
```rust
// OLD SYSTEM
struct OldSystem;

impl OldSystem {
    async fn run_old() -> Result<()> {
        let registry = Arc::new(RwLock::new(HashMap::new()));
        let mut executor = DagExecutor::new(None, registry, "db").await?;
        
        // Register old-style actions
        register_action!(executor, "process", old_process_action);
        
        executor.load_yaml_file("workflow.yaml")?;
        let cache = Cache::new();
        executor.execute_static_dag("workflow", &cache, rx).await?
    }
}

async fn old_process_action(
    executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    // Direct executor access
    executor.add_node(...);
    Ok(())
}

// NEW SYSTEM
struct NewSystem;

impl NewSystem {
    async fn run_new() -> Result<()> {
        // 1. Create action registry
        let registry = ActionRegistry::new();
        registry.register(Arc::new(ProcessAction));
        
        // 2. Create hooks
        let hooks = vec![
            Arc::new(DynamicBuilderHook) as Arc<dyn EventHook>,
        ];
        
        // 3. Create coordinator
        let coordinator = Coordinator::new(hooks, 100, 100);
        
        // 4. Create executor
        let mut executor = DagExecutor::new(
            None,
            Arc::new(Default::default()),
            "db"
        ).await?;
        
        executor.load_yaml_file("workflow.yaml")?;
        
        // 5. Run with coordinator
        let cache = Cache::new();
        coordinator.run_parallel(
            &mut executor,
            &cache,
            "workflow",
            "run_001",
            rx
        ).await?
    }
}

struct ProcessAction;

#[async_trait]
impl NodeAction for ProcessAction {
    fn name(&self) -> &str { "process" }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        // Pure computation
        Ok(NodeOutput::success(json!({"processed": true})))
    }
}

struct DynamicBuilderHook;

#[async_trait]
impl EventHook for DynamicBuilderHook {
    async fn handle(
        &self,
        ctx: &HookContext,
        event: &ExecutionEvent
    ) -> Vec<ExecutorCommand> {
        // Dynamic node addition via commands
        vec![ExecutorCommand::AddNode { ... }]
    }
}
```

#### How Dynamic Nodes Affect Execution

**Execution Loop Behavior:**
- The executor continuously fetches the latest DAG state
- New nodes are discovered in topological order
- Already-executed nodes are tracked to prevent re-execution
- Execution continues until no new nodes are found

**Completion Detection:**
The DAG completes when:
1. No new nodes are discovered in an iteration
2. All reachable nodes have been executed
3. No supervisor or agent actions are adding nodes

**Example: Agent-Driven Dynamic DAG**

```rust
// Supervisor agent that builds workflow dynamically
async fn supervisor_step(
    executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    // Analyze the task
    let task: String = parse_input_from_name(cache, "task", &node.inputs)?;
    let analysis = analyze_task(&task)?;
    
    // Build workflow based on analysis
    let mut prev_node = node.id.clone();
    
    for (i, step) in analysis.steps.iter().enumerate() {
        let node_id = format!("step_{}", i);
        
        // Add node for this step
        executor.add_node(
            "agent_workflow",
            node_id.clone(),
            step.action.clone(),
            vec![prev_node]
        )?;
        
        prev_node = node_id;
    }
    
    // Add final validation node
    executor.add_node(
        "agent_workflow",
        "final_validation".to_string(),
        "validate_results".to_string(),
        vec![prev_node]
    )?;
    
    Ok(())
}
```

#### Best Practices for Dynamic Nodes

1. **Unique Node IDs**: Always ensure node IDs are unique
2. **Valid Dependencies**: Verify dependencies exist before adding
3. **Action Registration**: Ensure actions are registered before use
4. **State Management**: Store necessary data in cache before node execution
5. **Error Handling**: Handle validation errors when adding nodes

```rust
// Safe node addition with error handling
match executor.add_node(dag_name, node_id, action, deps) {
    Ok(_) => info!("Successfully added node: {}", node_id),
    Err(DagError::NodeAlreadyExists(id)) => {
        warn!("Node {} already exists, skipping", id);
    }
    Err(DagError::DependencyNotFound(dep)) => {
        error!("Dependency {} not found", dep);
        return Err(anyhow!("Invalid dependency"));
    }
    Err(e) => return Err(e.into()),
}
```

### Conditional Execution

```rust
async fn conditional_branch(
    executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    let condition: bool = parse_input_from_name(cache, "condition", &node.inputs)?;
    
    if condition {
        // Execute one branch
        insert_value(cache, &node.id, "branch", "true_branch")?;
        executor.enable_nodes(&["true_path_node1", "true_path_node2"])?;
        executor.disable_nodes(&["false_path_node1", "false_path_node2"])?;
    } else {
        // Execute other branch
        insert_value(cache, &node.id, "branch", "false_branch")?;
        executor.enable_nodes(&["false_path_node1", "false_path_node2"])?;
        executor.disable_nodes(&["true_path_node1", "true_path_node2"])?;
    }
    
    Ok(())
}
```

### Visualization and Debugging

```rust
// Generate DOT graph for visualization
let dot_graph = executor.serialize_tree_to_dot("workflow_name").await?;
std::fs::write("workflow.dot", dot_graph)?;
// Convert to PNG: dot -Tpng workflow.dot -o workflow.png

// Get execution metrics
let metrics = executor.get_execution_metrics("workflow_name")?;
println!("Average node execution time: {:?}", metrics.avg_node_time);
println!("Slowest node: {}", metrics.slowest_node);
println!("Cache hit rate: {:.2}%", metrics.cache_hit_rate * 100.0);

// Debug specific node
let node_state = executor.get_node_state("workflow_name", "node_id")?;
println!("Node status: {:?}", node_state.status);
println!("Attempts: {}", node_state.attempts);
println!("Last error: {:?}", node_state.last_error);
```

## Complete Examples

### Example 1: Data Processing Pipeline with Coordinator

```rust
use dagger::coord::*;
use dagger::dag_flow::*;
use async_trait::async_trait;
use anyhow::Result;
use serde_json::json;
use std::sync::Arc;

// Define Actions
struct FetchDataAction {
    source_adapter: Arc<SourceAdapter>,
}

#[async_trait]
impl NodeAction for FetchDataAction {
    fn name(&self) -> &str {
        "fetch_data"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        let source = ctx.inputs.get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("default");
        
        // Fetch data from source
        let data = self.source_adapter.fetch(source).await?;
        
        Ok(NodeOutput::success(json!({
            "raw_data": data,
            "fetch_time": chrono::Utc::now(),
            "record_count": data.len()
        })))
    }
}

struct ValidateDataAction;

#[async_trait]
impl NodeAction for ValidateDataAction {
    fn name(&self) -> &str {
        "validate_data"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        let data: Vec<String> = serde_json::from_value(
            ctx.inputs.get("data")
                .ok_or_else(|| anyhow!("Missing data"))?
                .clone()
        )?;
        
        // Validate each record
        let valid_data: Vec<String> = data
            .into_iter()
            .filter(|item| !item.is_empty())
            .collect();
        
        if valid_data.is_empty() {
            return Ok(NodeOutput::failure("No valid data found"));
        }
        
        Ok(NodeOutput::success(json!({
            "valid_data": valid_data,
            "validation_passed": true,
            "invalid_count": 0
        })))
    }
}

struct TransformDataAction {
    transform_config: TransformConfig,
}

#[async_trait]
impl NodeAction for TransformDataAction {
    fn name(&self) -> &str {
        "transform_data"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        let data: Vec<String> = serde_json::from_value(
            ctx.inputs.get("data")
                .ok_or_else(|| anyhow!("Missing data"))?
                .clone()
        )?;
        
        // Transform data
        let transformed: Vec<String> = data
            .iter()
            .map(|item| self.transform_config.apply(item))
            .collect();
        
        Ok(NodeOutput::success(json!({
            "transformed": transformed,
            "transform_type": self.transform_config.transform_type
        })))
    }
}

// Define Hooks
struct ValidationHook;

#[async_trait]
impl EventHook for ValidationHook {
    async fn handle(
        &self,
        ctx: &HookContext,
        event: &ExecutionEvent
    ) -> Vec<ExecutorCommand> {
        match event {
            ExecutionEvent::NodeCompleted { node, outcome } => {
                if node.node_id == "validate" {
                    if !outcome.success {
                        // Add error recovery node
                        return vec![ExecutorCommand::AddNode {
                            dag_name: ctx.dag_name.clone(),
                            spec: NodeSpec::new("handle_validation_error")
                                .with_deps(vec![node.node_id.clone()]),
                        }];
                    }
                }
            }
            _ => {}
        }
        vec![]
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Setup action registry
    let action_registry = ActionRegistry::new();
    action_registry.register(Arc::new(FetchDataAction {
        source_adapter: Arc::new(SourceAdapter::new()),
    }));
    action_registry.register(Arc::new(ValidateDataAction));
    action_registry.register(Arc::new(TransformDataAction {
        transform_config: TransformConfig::default(),
    }));
    
    // 2. Create hooks
    let hooks = vec![
        Arc::new(ValidationHook) as Arc<dyn EventHook>,
        Arc::new(MonitorHook::new()),
    ];
    
    // 3. Create coordinator
    let coordinator = Coordinator::new(hooks, 100, 100);
    
    // 4. Setup executor
    let config = DagConfig {
        enable_parallel_execution: true,
        max_parallel_nodes: Some(4),
        ..Default::default()
    };
    
    let mut executor = DagExecutor::new(
        Some(config),
        Arc::new(Default::default()),  // Legacy registry
        "sqlite:pipeline.db"
    ).await?;
    
    // 5. Load workflow
    executor.load_yaml_file("pipeline.yaml")?;
    
    // 6. Prepare cache with inputs
    let cache = Cache::new();
    cache.insert("inputs", "data_source", json!("database"))?;
    cache.insert("inputs", "output_destination", json!("warehouse"))?;
    
    // 7. Execute with coordinator
    let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    
    println!("Starting parallel pipeline execution...");
    
    // Run via coordinator for parallel execution
    coordinator.run_parallel(
        &mut executor,
        &cache,
        "data_pipeline",
        "run_001",
        cancel_rx
    ).await?;
    
    println!("\nPipeline completed successfully!");
    
    // Generate visualization
    let dot = executor.serialize_tree_to_dot("data_pipeline").await?;
    std::fs::write("pipeline.dot", dot)?;
    
    Ok(())
}
```

### Example 2: Dynamic Workflow with Agent-Based Planning

```rust
use dagger::coord::*;
use dagger::dag_flow::*;
use async_trait::async_trait;

// Agent that analyzes and plans workflow
struct PlannerAgent {
    llm_client: Arc<LlmClient>,
}

#[async_trait]
impl NodeAction for PlannerAgent {
    fn name(&self) -> &str {
        "planner_agent"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        let task = ctx.inputs.get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing task"))?;
        
        // Analyze task and generate plan
        let plan = self.llm_client.analyze_task(task).await?;
        
        Ok(NodeOutput::success(json!({
            "plan": plan,
            "steps": plan.steps,
            "requires_tools": plan.tool_calls
        })))
    }
}

// Hook that builds workflow from plan
struct WorkflowBuilderHook;

#[async_trait]
impl EventHook for WorkflowBuilderHook {
    async fn handle(
        &self,
        ctx: &HookContext,
        event: &ExecutionEvent
    ) -> Vec<ExecutorCommand> {
        match event {
            ExecutionEvent::NodeCompleted { node, outcome } => {
                if node.node_id == "planner" {
                    let mut commands = vec![];
                    
                    // Read the plan
                    if let Some(steps) = outcome.outputs["steps"].as_array() {
                        let mut prev_node = node.node_id.clone();
                        
                        for (i, step) in steps.iter().enumerate() {
                            let step_id = format!("step_{}", i);
                            
                            // Add tool invocation node if needed
                            if step["requires_tool"].as_bool().unwrap_or(false) {
                                let tool_id = format!("tool_{}", i);
                                commands.push(ExecutorCommand::AddNode {
                                    dag_name: ctx.dag_name.clone(),
                                    spec: NodeSpec::new("invoke_tool")
                                        .with_id(tool_id.clone())
                                        .with_deps(vec![prev_node.clone()])
                                        .with_inputs(step["tool_params"].clone()),
                                });
                                prev_node = tool_id;
                            }
                            
                            // Add execution step
                            commands.push(ExecutorCommand::AddNode {
                                dag_name: ctx.dag_name.clone(),
                                spec: NodeSpec::new("execute_step")
                                    .with_id(step_id.clone())
                                    .with_deps(vec![prev_node])
                                    .with_inputs(json!({
                                        "instruction": step["instruction"],
                                        "context": step["context"]
                                    })),
                            });
                            
                            prev_node = step_id;
                        }
                        
                        // Add final validation
                        commands.push(ExecutorCommand::AddNode {
                            dag_name: ctx.dag_name.clone(),
                            spec: NodeSpec::new("validate_result")
                                .with_deps(vec![prev_node]),
                        });
                    }
                    
                    return commands;
                }
            }
            _ => {}
        }
        vec![]
    }
}

// Main execution
#[tokio::main]
async fn main() -> Result<()> {
    // Register actions
    let registry = ActionRegistry::new();
    registry.register(Arc::new(PlannerAgent {
        llm_client: Arc::new(LlmClient::new()),
    }));
    registry.register(Arc::new(ToolInvokeAction));
    registry.register(Arc::new(ExecuteStepAction));
    registry.register(Arc::new(ValidateResultAction));
    
    // Create hooks
    let hooks = vec![
        Arc::new(WorkflowBuilderHook),
        Arc::new(ExecutionMonitor),
    ];
    
    // Create coordinator
    let coordinator = Coordinator::new(hooks, 200, 200);
    
    // Start with minimal DAG
    let initial_dag = r#"
    name: agent_workflow
    nodes:
      - id: planner
        action: planner_agent
        inputs:
          - name: task
            reference: inputs.task
    "#;
    
    let mut executor = DagExecutor::new(
        None,
        Arc::new(Default::default()),
        "sqlite:agent.db"
    ).await?;
    
    executor.load_yaml_string(initial_dag)?;
    
    // Set the task
    let cache = Cache::new();
    cache.insert("inputs", "task", json!("Analyze sales data and create report"))?;
    
    // Execute - workflow will be built dynamically
    let (_tx, rx) = oneshot::channel();
    coordinator.run_parallel(
        &mut executor,
        &cache,
        "agent_workflow",
        "run_001",
        rx
    ).await?;
    
    Ok(())
}
```

## Best Practices for Coordinator Architecture

1. **Pure NodeActions**: Keep actions computation-only, no side effects on executor
2. **Hook Composition**: Use multiple specialized hooks rather than one monolithic hook
3. **Command Batching**: Use `AddNodes` for atomic multi-node additions
4. **Error Recovery**: Implement error recovery in hooks, not actions
5. **Backpressure**: Use bounded channels to prevent memory issues
6. **Graceful Shutdown**: Always provide cancellation token
7. **State in Output**: Return all relevant state in NodeOutput for hooks to process
8. **Idempotent Commands**: Ensure commands can be safely retried
9. **Event Filtering**: Filter events early in hooks to reduce processing
10. **Testing**: Test actions and hooks independently

## Troubleshooting

### Common Issues

**Workflow doesn't execute**
- Check if workflow was loaded: `executor.has_dag("name")`
- Verify all actions are registered
- Check for cycles in dependencies

**Cache values not found**
- Verify the reference path is correct
- Check if previous node completed successfully
- Use `serialize_cache_to_prettyjson` to inspect cache

**Parallel execution not working**
- Verify `enable_parallel_execution = true`
- Check that nodes don't have unnecessary dependencies
- Monitor with execution reports

**Database locked errors**
- Ensure only one writer at a time
- Use WAL mode (automatically enabled)
- Check disk space

## API Reference Summary

### Coordinator System

```rust
// Action Registry
ActionRegistry::new() -> Self
registry.register(action: Arc<dyn NodeAction>)
registry.get(name: &str) -> Option<Arc<dyn NodeAction>>

// Coordinator
Coordinator::new(hooks, event_capacity, cmd_capacity) -> Self
coordinator.run_parallel(executor, cache, dag_name, run_id, cancel_rx) -> Result<()>

// NodeAction trait
trait NodeAction {
    fn name(&self) -> &str;
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput>;
}

// EventHook trait
trait EventHook {
    async fn handle(&self, ctx: &HookContext, event: &ExecutionEvent) -> Vec<ExecutorCommand>;
    async fn on_start(&self, ctx: &HookContext) -> Vec<ExecutorCommand>;
    async fn on_complete(&self, ctx: &HookContext, success: bool) -> Vec<ExecutorCommand>;
}

// Core types
NodeCtx { dag_name, node_id, inputs, cache, app_data }
NodeOutput { outputs, success, metadata }
ExecutionEvent { NodeStarted, NodeCompleted, NodeFailed }
ExecutorCommand { AddNode, AddNodes, SetNodeInputs, PauseBranch, ResumeBranch, CancelBranch }
```

## Integration Checklist

- [ ] Convert all old NodeActions to new trait
- [ ] Create EventHooks for control flow logic
- [ ] Set up ActionRegistry with all actions
- [ ] Initialize Coordinator with hooks
- [ ] Replace `execute_static_dag` with `coordinator.run_parallel`
- [ ] Update YAML workflows (structure remains same)
- [ ] Test parallel execution
- [ ] Verify dynamic node addition works
- [ ] Check error recovery via hooks
- [ ] Validate backpressure handling

## Next Steps

1. Review [coordinator_demo.rs](../examples/coordinator_demo.rs) for working example
2. Read [DAG_FLOW_ARCHITECTURE.md](DAG_FLOW_ARCHITECTURE.md) for architecture details
3. Check [COORDINATOR_IMPLEMENTATION_SUMMARY.md](COORDINATOR_IMPLEMENTATION_SUMMARY.md) for RZN-specific guidance
4. Migrate existing workflows incrementally
5. Test thoroughly with parallel execution enabled

The coordinator architecture provides clean separation of concerns, eliminates borrow checker issues, and enables true parallel execution with dynamic graph growth. All mutations go through the coordinator, ensuring thread safety and consistency.