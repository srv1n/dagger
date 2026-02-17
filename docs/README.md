# Dagger Documentation

## Overview

Dagger is a production-ready Rust library for workflow orchestration, offering three distinct execution paradigms optimized for different use cases. Built with async Rust, SQLite persistence, and Send-compatible futures for seamless integration with modern Rust applications including Tauri.

## Quick Start Guide

- **[Getting Started](../README.md)** - Installation and basic usage
- **[Examples](../examples/)** - Working examples for each paradigm
- **[Example Outputs](../examples/README.md)** - Sample outputs and expected markers

## Core Documentation

### System Architecture
- **[ARCHITECTURE.md](ARCHITECTURE.md)** - Complete system architecture
  - Design principles and patterns
  - Storage layer (SQLite with compression)
  - Execution paradigms overview
  - Performance considerations
- **[WORK_QUEUE_CONTRACT.md](WORK_QUEUE_CONTRACT.md)** - Dagger ↔ host integration contract for Work Queue execution
  - Batch fanout + deterministic IDs
  - ExecSpec/ExecResult + host policy hooks
  - Runtime events semantics + domain events
  - HITL checkpoints + resume tokens (contract)
- **[WORK_QUEUE_BATCH_SEND_RETRY.md](WORK_QUEUE_BATCH_SEND_RETRY.md)** - Worked example: batch send, partial failure, retry-only-failed

### Execution Paradigms

#### 1. DAG Flow - Static and Dynamic Workflow Execution
- **[DAG_FLOW_COMPLETE_GUIDE.md](DAG_FLOW_COMPLETE_GUIDE.md)** - Comprehensive DAG Flow documentation
  - Architecture and execution flow
  - YAML workflow definition
  - Parallel execution with Coordinator
  - Dynamic node addition with hooks
  - NodeAction trait and action system
  - Cache operations and persistence
  - Performance optimization
  - Complete API reference
  - Troubleshooting guide

#### 2. Task Agent - Dynamic Task Orchestration  
- **[TASK_AGENT_ARCHITECTURE.md](TASK_AGENT_ARCHITECTURE.md)** - Task system architecture
  - Agent-based task execution
  - Dynamic dependency creation
  - Persistence and recovery
  - Task scheduling and retry logic

#### 3. Pub/Sub - Event-Driven Communication
- **[PUBSUB_ARCHITECTURE.md](PUBSUB_ARCHITECTURE.md)** - Event system architecture
  - Multi-agent communication
  - Dynamic channel creation
  - Message routing and validation
  - Schema enforcement

## API Reference

### Core Types (High-level)

```rust
// Main executor for DAG workflows
pub struct DagExecutor { /* internal fields */ }

// Configuration for DAG execution
pub struct DagConfig { /* parallelism, retries, timeouts, cache */ }

// Node execution context (immutable)
pub struct NodeCtx { /* inputs, cache, node_id, dag_name, app_data */ }

// Cache for sharing data between nodes
pub struct Cache { /* DashMap-backed */ }
```

### Key APIs

#### Creating an Executor

```rust
use dagger::coord::ActionRegistry;

let registry = ActionRegistry::new();
let config = DagConfig::default();
let executor = DagExecutor::new(Some(config), registry, "sqlite::memory:").await?;
```

#### Registering Actions

`DagExecutor::new(...)` automatically registers all `#[dagger::action]` functions compiled into the binary (via `linkme`). Use manual registration only when you need to control the registry contents.

```rust
// Using the macro (recommended) - auto-registered on executor creation
#[dagger::action(name = "action_name")]
async fn action_function(input: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    Ok(input)
}

// Manual registration (explicit control)
let registry = ActionRegistry::new();
registry.register(Arc::new(MyAction));
```

#### Loading and Executing Workflows

```rust
// Load YAML workflow
executor.load_yaml_file("workflow.yaml").await?;

// Execute static DAG
let report = executor.execute_static_dag("workflow_name", &cache, cancel_rx).await?;

// Execute agent-driven flow (dynamic DAG)
let report = executor.execute_agent_dag("task_description", &cache, cancel_rx).await?;
```

#### Cache Operations

```rust
// Insert values
insert_value(&cache, "namespace", "key", value)?;

// Parse inputs
let value: T = parse_input_from_name(&cache, "key", &node.inputs)?;

// Get global values
let value: T = get_global_input(&cache, "namespace", "key")?;
```

## Migration Notes

- [Upgrading / Adoption Notes](UPGRADING.md) - Downstream migration checklist

## Examples

### DAG Flow Example
```rust
async fn process_data(executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let input = parse_input_from_name(cache, "data", &node.inputs)?;
    let processed = transform(input);
    insert_value(cache, &node.id, "output", processed)?;
    Ok(())
}
```

### Task Agent Example
```rust
#[task_agent]
async fn analyze_task(task: Task) -> Result<Value> {
    let result = perform_analysis(&task.input)?;
    Ok(json!({ "analysis": result }))
}
```

### Pub/Sub Example
```rust
#[pubsub_agent(
    subscribe = "input_events",
    publish = "output_events"
)]
async fn process_events(message: Message) -> Result<()> {
    let processed = handle_event(message.payload)?;
    publish("output_events", processed).await?;
    Ok(())
}
```

## Best Practices

1. **Use SQLite in-memory for testing**: `"sqlite::memory:"`
2. **Enable parallel execution** for better performance
3. **Implement proper error handling** in actions
4. **Use caching strategically** to share data between nodes
5. **Set appropriate timeouts** for long-running operations
6. **Use the Coordinator pattern** for complex control flow

## Support

- **Examples**: [examples/](../examples/)
- **Tests**: [tests/](../tests/)

## License

MIT License - See [LICENSE](../LICENSE) for details.
