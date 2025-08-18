# Dagger Quick Reference

## Common Imports

```rust
use dagger::{
    DagExecutor, Cache, Node, DagConfig,
    register_action, insert_value, parse_input_from_name,
    get_global_input, NodeAction
};
use tokio::sync::RwLock;
use std::sync::Arc;
use anyhow::Result;
use serde_json::{json, Value};
```

## Initialization

```rust
// Create executor
let registry = Arc::new(RwLock::new(HashMap::new()));
let mut executor = DagExecutor::new(None, registry, "sqlite::memory:").await?;

// With configuration
let config = DagConfig {
    enable_parallel_execution: true,
    max_parallel_nodes: 4,
    timeout_seconds: Some(300),
    max_iterations: Some(100),
    ..Default::default()
};
let mut executor = DagExecutor::new(Some(config), registry, "sqlite::memory:").await?;
```

## Registering Actions

```rust
// Using macro (recommended)
register_action!(executor, "action_name", action_function).await?;

// Manual registration
struct MyAction;

#[async_trait]
impl NodeAction for MyAction {
    fn name(&self) -> String {
        "my_action".to_string()
    }
    
    async fn execute(&self, executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
        // Implementation
        Ok(())
    }
    
    fn schema(&self) -> Value {
        json!({
            "name": "my_action",
            "description": "Does something",
            "inputs": {},
            "outputs": {}
        })
    }
}

executor.register_action(Arc::new(MyAction)).await?;
```

## Loading Workflows

```rust
// Load single YAML file
executor.load_yaml_file("workflow.yaml").await?;

// Load all YAML files in directory
executor.load_yaml_dir("workflows/")?;

// List loaded workflows
let workflows = executor.list_dags().await?;
```

## Executing Workflows

```rust
// Create cache
let cache = Cache::new();

// Add inputs to cache
insert_value(&cache, "inputs", "param1", "value1")?;
insert_value(&cache, "inputs", "param2", 42)?;

// Create cancellation channel
let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

// Execute static DAG
let report = executor.execute_static_dag("workflow_name", &cache, cancel_rx).await?;

// Execute agent-driven flow
let report = executor.execute_agent_dag("task_description", &cache, cancel_rx).await?;
```

## Cache Operations

```rust
// Insert values
insert_value(&cache, "namespace", "key", value)?;
insert_value(&cache, &node.id, &output.name, result)?;

// Get values
let value: String = get_global_input(&cache, "namespace", "key")?;
let input: f64 = parse_input_from_name(&cache, "input_name", &node.inputs)?;

// Check existence
if cache.data.contains_key("namespace.key") {
    // Key exists
}

// Clear cache
cache.data.clear();
```

## Writing Actions

```rust
async fn process_data(
    executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    // Get inputs
    let input_data: Vec<f64> = parse_input_from_name(cache, "data", &node.inputs)?;
    
    // Process
    let result = input_data.iter().sum::<f64>() / input_data.len() as f64;
    
    // Store outputs
    insert_value(cache, &node.id, &node.outputs[0].name, result)?;
    
    // Store additional metadata
    insert_value(cache, &node.id, "processed_at", chrono::Utc::now())?;
    
    Ok(())
}
```

## YAML Workflow Format

```yaml
name: my_workflow
description: Example workflow
author: Your Name
version: 1.0.0

nodes:
  - id: node1
    action: fetch_data
    description: Fetches data from source
    timeout: 30
    try_count: 3
    outputs:
      - name: raw_data
        description: Raw fetched data
        
  - id: node2
    action: process_data  
    description: Processes the data
    dependencies: [node1]
    inputs:
      - name: data
        reference: node1.raw_data
    outputs:
      - name: processed_data
        
  - id: node3
    action: save_results
    dependencies: [node2]
    inputs:
      - name: results
        reference: node2.processed_data

config:
  enable_parallel_execution: true
  max_parallel_nodes: 4
  timeout_seconds: 300
```

## Error Handling

```rust
// In actions
async fn safe_action(executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let input = parse_input_from_name(cache, "input", &node.inputs)
        .map_err(|e| anyhow!("Failed to parse input: {}", e))?;
    
    match process(input) {
        Ok(result) => {
            insert_value(cache, &node.id, "output", result)?;
            Ok(())
        }
        Err(e) => {
            // Log error
            tracing::error!("Processing failed: {}", e);
            // Store error state
            insert_value(cache, &node.id, "error", e.to_string())?;
            Err(e.into())
        }
    }
}

// Retry configuration in YAML
nodes:
  - id: resilient_node
    action: unreliable_action
    try_count: 3
    retry_strategy:
      type: exponential_backoff
      base_ms: 100
      max_ms: 5000
```

## Parallel Execution

```rust
// Enable in config
let config = DagConfig {
    enable_parallel_execution: true,
    max_parallel_nodes: 8,
    ..Default::default()
};

// Nodes with no shared dependencies execute in parallel automatically
```

## Visualization

```rust
// Generate DOT graph
let dot = executor.serialize_tree_to_dot("workflow_name").await?;
std::fs::write("workflow.dot", dot)?;

// Convert to image (requires graphviz)
std::process::Command::new("dot")
    .args(&["-Tpng", "workflow.dot", "-o", "workflow.png"])
    .status()?;
```

## Task Agent Macros

```rust
use dagger_macros::task_agent;

#[task_agent]
async fn my_agent(task: Task) -> Result<Value> {
    // Access task data
    let input = &task.input;
    let task_id = &task.id;
    
    // Create subtasks
    if need_subtask(input) {
        create_subtask("subtask_type", json!({ "data": "value" }))?;
    }
    
    // Return result
    Ok(json!({
        "status": "completed",
        "result": process(input)?
    }))
}
```

## Pub/Sub Agents

```rust
use dagger_macros::pubsub_agent;

#[pubsub_agent(
    subscribe = "input_channel",
    publish = "output_channel"
)]
async fn process_messages(msg: Message) -> Result<()> {
    let data = msg.payload;
    let processed = transform(data)?;
    
    publish("output_channel", processed).await?;
    Ok(())
}
```

## Common Patterns

### Sequential Pipeline
```yaml
nodes:
  - id: step1
    action: action1
  - id: step2
    action: action2
    dependencies: [step1]
  - id: step3
    action: action3
    dependencies: [step2]
```

### Parallel Branches
```yaml
nodes:
  - id: start
    action: prepare
  - id: branch1
    action: process_a
    dependencies: [start]
  - id: branch2
    action: process_b
    dependencies: [start]
  - id: merge
    action: combine
    dependencies: [branch1, branch2]
```

### Conditional Execution
```rust
async fn conditional_action(executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let condition = parse_input_from_name(cache, "condition", &node.inputs)?;
    
    if condition {
        // Execute path A
        executor.add_node("workflow", "dynamic_node", "action_a", vec![]).await?;
    } else {
        // Execute path B
        executor.add_node("workflow", "dynamic_node", "action_b", vec![]).await?;
    }
    
    Ok(())
}
```

## Debugging Tips

1. **Enable logging**:
```rust
use tracing_subscriber::FmtSubscriber;

let subscriber = FmtSubscriber::builder()
    .with_max_level(tracing::Level::DEBUG)
    .finish();
tracing::subscriber::set_global_default(subscriber)?;
```

2. **Inspect cache**:
```rust
let json = serialize_cache_to_prettyjson(&cache)?;
println!("Cache state: {}", json);
```

3. **Check execution report**:
```rust
let report = executor.execute_static_dag("workflow", &cache, rx).await?;
println!("Success: {}", report.success);
println!("Nodes executed: {}", report.nodes_executed);
for outcome in &report.node_outcomes {
    println!("  {}: {}", outcome.node_id, outcome.status);
}
```

## Performance Tips

1. Use `sqlite::memory:` for testing
2. Enable parallel execution for independent nodes
3. Use Arc for shared immutable data
4. Batch database operations when possible
5. Set appropriate timeouts to prevent hanging
6. Use compression for large data in cache

## Migration from 0.x to 1.x

Key changes:
- All RwLock operations are now async (add `.await`)
- `DagExecutor::new()` is async
- `register_action!` returns a future (add `.await?`)
- `load_yaml_file()` is async
- SQLx updated to 0.8.6

See [Migration Guide](archive/migrations/ASYNC_MIGRATION_GUIDE.md) for details.