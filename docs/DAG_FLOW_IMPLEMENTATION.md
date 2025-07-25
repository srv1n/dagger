# DAG Flow Engine Implementation Guide

## Overview

The DAG Flow Engine is a high-performance, async execution engine for directed acyclic graphs (DAGs) in Rust. It supports both static YAML-defined workflows and dynamic agent-driven execution with built-in parallelism, retry mechanisms, and comprehensive error handling.

## Key Features

- **Parallel Execution**: Automatically executes independent nodes in parallel (enabled by default)
- **Resource Management**: Configurable limits on concurrent execution
- **Retry Mechanisms**: Built-in retry with exponential backoff
- **Persistent State**: Execution state saved to embedded database (sled)
- **Incremental Caching**: Optional incremental cache updates during execution
- **Structured Logging**: Comprehensive tracing with the `tracing` crate
- **YAML Workflow Definition**: Define complex workflows in YAML
- **Dynamic Execution**: Support for agent-driven dynamic workflows

## Architecture

### Core Components

```rust
// Main executor
pub struct DagExecutor {
    pub config: DagConfig,
    pub function_registry: ActionRegistry,
    pub sled_db: sled::Db,
    pub tree: Arc<RwLock<HashMap<String, ExecutionTree>>>,
    pub graphs: Arc<RwLock<HashMap<String, Graph>>>,
    pub execution_context: Option<ExecutionContext>,
    // ... other fields
}

// Node definition
pub struct Node {
    pub id: String,
    pub dependencies: Vec<String>,
    pub inputs: Vec<IField>,
    pub outputs: Vec<OField>,
    pub action: String,
    pub timeout: u64,
    pub try_count: u32,
    // ... other fields
}

// Execution cache
pub type Cache = Arc<DashMap<String, HashMap<String, DynAny>>>;
```

### Configuration

```rust
pub struct DagConfig {
    pub max_attempts: Option<u32>,              // Default: 3
    pub on_failure: OnFailure,                  // Default: Pause
    pub timeout_seconds: Option<u64>,           // Default: 3600
    pub max_iterations: Option<u32>,            // Default: 50
    pub enable_parallel_execution: bool,        // Default: true
    pub max_parallel_nodes: usize,              // Default: 3
    pub enable_incremental_cache: bool,         // Default: false
    pub cache_snapshot_interval: u64,           // Default: 300
    // ... other fields
}
```

## API Reference

### Creating an Executor

```rust
use dagger::{DagExecutor, DagConfig, ActionRegistry};
use std::sync::{Arc, RwLock};
use std::collections::HashMap;

// Create with default config (parallel execution enabled)
let registry = Arc::new(RwLock::new(HashMap::new()));
let mut executor = DagExecutor::new(None, registry.clone(), "dag_db")?;

// Create with custom config
let mut config = DagConfig::default();
config.max_parallel_nodes = 5;
config.enable_incremental_cache = true;
let mut executor = DagExecutor::new(Some(config), registry.clone(), "dag_db")?;
```

### Registering Actions

Actions are async functions that process nodes. Use the `register_action!` macro:

```rust
use dagger::{register_action, Node, Cache, DagExecutor};
use anyhow::Result;

// Define an action
async fn process_data(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    // Read inputs
    let input_data: String = parse_input_from_name(cache, "data", &node.inputs)?;
    
    // Process
    let result = input_data.to_uppercase();
    
    // Write outputs
    insert_value(cache, &node.id, "result", result)?;
    Ok(())
}

// Register the action
register_action!(executor, "process_data", process_data);
```

### Loading YAML Workflows

```yaml
# workflow.yaml
name: data_pipeline
description: Example data processing pipeline
author: Your Name
version: 1.0.0
nodes:
  - id: load_data
    dependencies: []
    inputs:
      - name: file_path
        reference: inputs.file_path
    outputs:
      - name: data
    action: load_file
    timeout: 300
    try_count: 3
    
  - id: process_data
    dependencies: [load_data]
    inputs:
      - name: data
        reference: load_data.data
    outputs:
      - name: result
    action: process_data
    timeout: 600
    try_count: 2
```

```rust
// Load the workflow
executor.load_yaml_file("workflow.yaml")?;

// List available workflows
let workflows = executor.list_dags()?;
```

### Executing Workflows

```rust
use dagger::{Cache, insert_value};
use tokio::sync::oneshot;

// Create cache and set inputs
let cache = Cache::new();
insert_value(&cache, "inputs", "file_path", "data.txt")?;

// Create cancellation channel
let (cancel_tx, cancel_rx) = oneshot::channel();

// Execute static workflow (NEW API)
let report = executor.execute_static_dag("data_pipeline", &cache, cancel_rx).await?;

// Check results
if report.overall_success {
    println!("Workflow completed successfully");
} else {
    println!("Workflow failed: {:?}", report.error);
}
```

## Migration Guide

### API Changes

The `execute_static_dag` method now takes a simple string name instead of `WorkflowSpec`:

```rust
// OLD API
let report = executor.execute_static_dag(
    WorkflowSpec::Static { name: "workflow".to_string() },
    &cache,
    cancel_rx
).await?;

// NEW API
let report = executor.execute_static_dag("workflow", &cache, cancel_rx).await?;

// For dynamic workflows, use execute_agent_dag
let report = executor.execute_agent_dag("task_name", &cache, cancel_rx).await?;
```

### Quick Fix for Your Errors

Replace all occurrences of:
```rust
.execute_static_dag(
    WorkflowSpec::Static { name: workflow_name.to_string() },
    &cache,
    cancel_rx
)
```

With:
```rust
.execute_static_dag(&workflow_name, &cache, cancel_rx)
```

## Common Use Cases

### 1. Simple Sequential Pipeline

```rust
// Define actions
async fn fetch_data(_: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let url: String = parse_input_from_name(cache, "url", &node.inputs)?;
    let data = reqwest::get(&url).await?.text().await?;
    insert_value(cache, &node.id, "data", data)?;
    Ok(())
}

async fn parse_json(_: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let data: String = parse_input_from_name(cache, "data", &node.inputs)?;
    let parsed: serde_json::Value = serde_json::from_str(&data)?;
    insert_value(cache, &node.id, "json", parsed)?;
    Ok(())
}

// Register actions
register_action!(executor, "fetch_data", fetch_data);
register_action!(executor, "parse_json", parse_json);

// Execute
let cache = Cache::new();
insert_value(&cache, "inputs", "url", "https://api.example.com/data")?;
let report = executor.execute_static_dag("fetch_and_parse", &cache, cancel_rx).await?;
```

### 2. Parallel Processing

```yaml
# parallel_pipeline.yaml
name: parallel_processor
nodes:
  - id: splitter
    dependencies: []
    action: split_data
    
  - id: process_chunk_1
    dependencies: [splitter]
    action: process_chunk
    
  - id: process_chunk_2
    dependencies: [splitter]
    action: process_chunk
    
  - id: process_chunk_3
    dependencies: [splitter]
    action: process_chunk
    
  - id: merger
    dependencies: [process_chunk_1, process_chunk_2, process_chunk_3]
    action: merge_results
```

The engine automatically executes `process_chunk_1`, `process_chunk_2`, and `process_chunk_3` in parallel.

### 3. Error Handling with Retries

```rust
async fn unreliable_api_call(
    _: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    let attempt = node.try_count; // Current attempt number
    
    // Simulate intermittent failures
    if attempt < 2 && rand::random::<f32>() > 0.5 {
        return Err(anyhow!("API temporarily unavailable"));
    }
    
    // Success case
    insert_value(cache, &node.id, "result", "success")?;
    Ok(())
}
```

### 4. Accessing Execution Context

```rust
async fn resource_aware_action(
    executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    // Check if we're in parallel mode
    if executor.config.enable_parallel_execution {
        println!("Running with max {} parallel nodes", 
                 executor.config.max_parallel_nodes);
    }
    
    // Access execution state
    if let Some(context) = &executor.execution_context {
        // Use semaphore for additional resource control
        let _permit = context.semaphore.acquire().await?;
        // Do resource-intensive work
    }
    
    Ok(())
}
```

## Performance Optimization

### 1. Parallel Execution

- Enabled by default with 3 concurrent nodes
- Adjust `max_parallel_nodes` based on your workload
- Monitor with structured logging

### 2. Caching Strategy

```rust
// Enable incremental caching for long-running workflows
config.enable_incremental_cache = true;
config.cache_snapshot_interval = 60; // Save every minute
```

### 3. Memory Management

- Use `DashMap` for concurrent cache access
- Implement cleanup in long-running workflows:

```rust
async fn cleanup_action(
    _: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    // Remove large intermediate data
    cache.remove("large_intermediate_data");
    Ok(())
}
```

## Debugging and Monitoring

### 1. Enable Detailed Logging

```rust
use tracing_subscriber::FmtSubscriber;

let subscriber = FmtSubscriber::builder()
    .with_max_level(tracing::Level::DEBUG)
    .finish();
tracing::subscriber::set_global_default(subscriber)?;
```

### 2. Visualize Execution

```rust
// Generate DOT graph
let dot = executor.serialize_tree_to_dot("workflow_name")?;
std::fs::write("execution.dot", dot)?;

// Convert to image: dot -Tpng execution.dot -o execution.png
```

### 3. Inspect Cache State

```rust
let cache_json = serialize_cache_to_prettyjson(&cache)?;
println!("Cache state: {}", cache_json);
```

## Advanced Features

### 1. Dynamic Workflows

```rust
// Execute agent-driven workflows
let report = executor.execute_agent_dag("analyze_data", &cache, cancel_rx).await?;
```

### 2. Custom Retry Strategies

```rust
config.retry_strategy = RetryStrategy {
    initial_delay_ms: 100,
    max_delay_ms: 30000,
    multiplier: 2.0,
    randomization_factor: 0.1,
};
```

### 3. Human-in-the-Loop

```rust
config.human_wait_minutes = Some(5);
config.human_timeout_action = HumanTimeoutAction::Autopilot;
```

## Best Practices

1. **Action Design**
   - Keep actions focused and single-purpose
   - Use descriptive names for inputs/outputs
   - Handle errors gracefully with proper error messages

2. **Workflow Structure**
   - Minimize dependencies for better parallelism
   - Use meaningful node IDs
   - Document complex workflows

3. **Resource Management**
   - Set appropriate timeouts
   - Configure retry counts based on action reliability
   - Monitor memory usage in long-running workflows

4. **Testing**
   - Test actions independently
   - Use small test workflows before scaling up
   - Validate YAML schemas before deployment

## Common Pitfalls

1. **Circular Dependencies**: The engine validates DAGs but ensure no cycles
2. **Cache Key Conflicts**: Use unique keys for different data types
3. **Resource Exhaustion**: Monitor parallel execution in resource-constrained environments
4. **Type Mismatches**: Ensure input/output types match between connected nodes

## Example: Complete E2E Flow

```rust
use dagger::*;
use anyhow::Result;
use std::sync::{Arc, RwLock};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Setup
    let registry = Arc::new(RwLock::new(HashMap::new()));
    let mut executor = DagExecutor::new(None, registry.clone(), "my_dag_db")?;
    
    // 2. Define actions
    async fn validate_input(
        _: &mut DagExecutor,
        node: &Node,
        cache: &Cache
    ) -> Result<()> {
        let data: String = parse_input_from_name(cache, "raw_data", &node.inputs)?;
        if data.is_empty() {
            return Err(anyhow!("Input data cannot be empty"));
        }
        insert_value(cache, &node.id, "validated_data", data)?;
        Ok(())
    }
    
    async fn transform_data(
        _: &mut DagExecutor,
        node: &Node,
        cache: &Cache
    ) -> Result<()> {
        let data: String = parse_input_from_name(cache, "data", &node.inputs)?;
        let transformed = data.to_uppercase();
        insert_value(cache, &node.id, "transformed", transformed)?;
        Ok(())
    }
    
    // 3. Register actions
    register_action!(executor, "validate", validate_input);
    register_action!(executor, "transform", transform_data);
    
    // 4. Load workflow
    executor.load_yaml_file("pipeline.yaml")?;
    
    // 5. Prepare inputs
    let cache = Cache::new();
    insert_value(&cache, "inputs", "raw_data", "hello world")?;
    
    // 6. Execute
    let (_, cancel_rx) = tokio::sync::oneshot::channel();
    let report = executor.execute_static_dag("my_pipeline", &cache, cancel_rx).await?;
    
    // 7. Check results
    if report.overall_success {
        let result: String = cache
            .get("transform_node")
            .and_then(|m| m.get("transformed"))
            .and_then(|v| DynAny::from_value(v))
            .ok_or_else(|| anyhow!("Result not found"))?;
        println!("Result: {}", result);
    }
    
    Ok(())
}
```

This implementation guide provides a comprehensive overview of the DAG Flow Engine, covering everything from basic usage to advanced features and optimization strategies.