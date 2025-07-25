# DAG Flow Engine - Updated Documentation

## Overview

The DAG Flow Engine has been significantly enhanced with parallel execution capabilities, improved resource management, and a simplified API. This document covers the latest features and changes.

## 🚀 Key Improvements

### Parallel Execution (Default Enabled)
- **Automatic parallelization** of independent nodes
- **Default configuration**: 3 concurrent nodes
- **~2x performance improvement** on parallel workflows
- Zero configuration needed - works out of the box

### Resource Management
- Built-in semaphore-based concurrency control
- Memory-efficient execution with configurable limits
- Incremental cache updates for long-running workflows

### Enhanced Logging
- Structured tracing with `tracing` crate
- Real-time visibility into parallel execution
- Performance metrics and timing information

## 📊 Quick Start

### Basic Usage

```rust
use dagger::{DagExecutor, DagConfig, Cache, register_action, insert_value};
use std::sync::{Arc, RwLock};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Create executor (parallel execution enabled by default)
    let registry = Arc::new(RwLock::new(HashMap::new()));
    let mut executor = DagExecutor::new(None, registry.clone(), "dag_db")?;
    
    // 2. Define and register actions
    async fn process_data(
        _executor: &mut DagExecutor,
        node: &Node,
        cache: &Cache
    ) -> Result<()> {
        let input: String = parse_input_from_name(cache, "data", &node.inputs)?;
        let result = input.to_uppercase();
        insert_value(cache, &node.id, "result", result)?;
        Ok(())
    }
    
    register_action!(executor, "process_data", process_data);
    
    // 3. Load workflow
    executor.load_yaml_file("workflow.yaml")?;
    
    // 4. Execute with inputs
    let cache = Cache::new();
    insert_value(&cache, "inputs", "data", "hello world")?;
    
    let (_, cancel_rx) = tokio::sync::oneshot::channel();
    let report = executor.execute_static_dag("my_workflow", &cache, cancel_rx).await?;
    
    println!("Success: {}", report.overall_success);
    Ok(())
}
```

### YAML Workflow Definition

```yaml
name: parallel_pipeline
description: Example pipeline with parallel branches
nodes:
  - id: start
    dependencies: []
    action: load_data
    
  # These two nodes run in parallel
  - id: branch_a
    dependencies: [start]
    action: process_type_a
    
  - id: branch_b
    dependencies: [start]
    action: process_type_b
    
  # Waits for both branches
  - id: merge
    dependencies: [branch_a, branch_b]
    action: merge_results
```

## ⚙️ Configuration

### Default Configuration (Optimized for Performance)

```rust
DagConfig {
    enable_parallel_execution: true,    // Parallel by default
    max_parallel_nodes: 3,              // Conservative limit
    max_attempts: Some(3),              // Retry failed nodes
    timeout_seconds: Some(3600),        // 1 hour timeout
    on_failure: OnFailure::Pause,       // Pause on failure
    enable_incremental_cache: false,    // Disabled by default
    cache_snapshot_interval: 300,       // 5 minutes
    // ... other fields
}
```

### Custom Configuration

```rust
let mut config = DagConfig::default();
config.max_parallel_nodes = 10;        // Increase parallelism
config.enable_incremental_cache = true; // Enable for long workflows
config.timeout_seconds = Some(7200);    // 2 hour timeout

let executor = DagExecutor::new(Some(config), registry, "dag_db")?;
```

## 🔄 API Changes

### Simplified Execution API

**Old API:**
```rust
executor.execute_static_dag(
    WorkflowSpec::Static { name: "workflow".to_string() },
    &cache,
    cancel_rx
).await?
```

**New API:**
```rust
executor.execute_static_dag("workflow", &cache, cancel_rx).await?
```

### Migration Guide

For existing code, simply replace:
- `WorkflowSpec::Static { name: workflow_name.to_string() }` → `&workflow_name`
- `WorkflowSpec::Agent { task: task_name.to_string() }` → Use `execute_agent_dag(&task_name, ...)`

## 📈 Performance

### Benchmark Results

With the new parallel execution engine:
- **Sequential Pipeline**: ~210ms
- **Parallel Pipeline**: ~107ms
- **Speedup**: 1.96x

### Optimization Tips

1. **Structure for Parallelism**: Minimize dependencies between nodes
2. **Resource Limits**: Adjust `max_parallel_nodes` based on your hardware
3. **Caching**: Enable incremental caching for workflows > 5 minutes
4. **Monitoring**: Use structured logging to identify bottlenecks

## 🎯 Common Patterns

### Fan-Out/Fan-In

```yaml
nodes:
  - id: splitter
    action: split_into_chunks
    
  # Fan-out: process chunks in parallel
  - id: process_1
    dependencies: [splitter]
    action: process_chunk
    
  - id: process_2
    dependencies: [splitter]
    action: process_chunk
    
  - id: process_3
    dependencies: [splitter]
    action: process_chunk
    
  # Fan-in: merge results
  - id: merger
    dependencies: [process_1, process_2, process_3]
    action: merge_chunks
```

### Error Recovery

```rust
async fn resilient_action(
    _: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    // Action automatically retried up to node.try_count times
    match external_api_call().await {
        Ok(data) => {
            insert_value(cache, &node.id, "data", data)?;
            Ok(())
        }
        Err(e) if e.is_transient() => {
            // Will be retried with exponential backoff
            Err(anyhow!("Transient error: {}", e))
        }
        Err(e) => {
            // Permanent failure, no retry
            Err(anyhow!("Permanent error: {}", e))
        }
    }
}
```

## 🔍 Debugging

### Enable Debug Logging

```rust
use tracing_subscriber::FmtSubscriber;

let subscriber = FmtSubscriber::builder()
    .with_max_level(tracing::Level::DEBUG)
    .finish();
tracing::subscriber::set_global_default(subscriber)?;
```

### Visualize Execution

```rust
// Generate DOT graph of execution
let dot = executor.serialize_tree_to_dot("workflow_name")?;
std::fs::write("execution.dot", dot)?;

// Convert to PNG: dot -Tpng execution.dot -o execution.png
```

### Inspect Cache State

```rust
let cache_json = serialize_cache_to_prettyjson(&cache)?;
println!("Cache state:\n{}", cache_json);
```

## 🚨 Important Notes

1. **Database Locks**: Each executor instance creates a sled database. Only one process can access it at a time.
2. **Async Context**: All actions must be async functions.
3. **Type Safety**: Cache values are type-checked at runtime. Ensure consistent types.
4. **Resource Cleanup**: Long-running workflows should implement periodic cleanup.

## 📚 Further Reading

- [Full Implementation Guide](./DAG_FLOW_IMPLEMENTATION.md)
- [Migration Guide](./MIGRATION_QUICKFIX.md)
- [Examples](../examples/dag_flow/)

## 🤝 Support

For issues or questions:
1. Check the [implementation guide](./DAG_FLOW_IMPLEMENTATION.md)
2. Review [examples](../examples/)
3. Open an issue with a minimal reproduction