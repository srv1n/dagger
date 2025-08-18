# Dagger Documentation

## Overview

Dagger is a production-ready Rust library for workflow orchestration, offering three distinct execution paradigms optimized for different use cases. Built with async Rust, SQLite persistence, and Send-compatible futures for seamless integration with modern Rust applications including Tauri.

## Quick Start Guide

- **[Getting Started](../README.md)** - Installation and basic usage
- **[Quick Reference](QUICK_REFERENCE.md)** - Common patterns and code snippets
- **[Examples](../examples/)** - Working examples for each paradigm
- **[API Reference](#api-reference)** - Complete API documentation

## Core Documentation

### System Architecture
- **[ARCHITECTURE.md](ARCHITECTURE.md)** - Complete system architecture
  - Design principles and patterns
  - Storage layer (SQLite with compression)
  - Execution paradigms overview
  - Performance considerations

### Execution Paradigms

#### 1. DAG Flow - Static Workflow Execution
- **[DAG_FLOW_IMPLEMENTATION_GUIDE.md](DAG_FLOW_IMPLEMENTATION_GUIDE.md)** - Complete implementation guide
  - YAML workflow definition
  - Parallel execution with Coordinator
  - NodeAction trait for pure computation
  - EventHook system for control flow
  - Cache operations and persistence

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

### Storage & Persistence
- **[SQLITE_DAG_STORAGE_GUIDE.md](SQLITE_DAG_STORAGE_GUIDE.md)** - SQLite storage implementation
  - Database schema and tables
  - ACID transactions
  - Compression (3-10x reduction)
  - Query patterns and optimization

## API Reference

### Core Types

```rust
// Main executor for DAG workflows
pub struct DagExecutor {
    pub config: DagConfig,
    pub function_registry: Arc<RwLock<HashMap<String, Arc<dyn NodeAction>>>>,
    // ... internal fields
}

// Configuration for DAG execution
pub struct DagConfig {
    pub enable_parallel_execution: bool,
    pub max_parallel_nodes: usize,
    pub timeout_seconds: Option<u64>,
    pub max_iterations: Option<u32>,
    pub enable_incremental_cache: bool,
    // ... other options
}

// Node in a DAG workflow
pub struct Node {
    pub id: String,
    pub action: String,
    pub dependencies: Vec<String>,
    pub inputs: Vec<IField>,
    pub outputs: Vec<OField>,
    pub timeout: u64,
    pub try_count: u32,
}

// Cache for sharing data between nodes
pub struct Cache {
    pub data: Arc<DashMap<String, Value>>,
}
```

### Key APIs

#### Creating an Executor

```rust
let registry = Arc::new(RwLock::new(HashMap::new()));
let config = DagConfig::default();
let executor = DagExecutor::new(Some(config), registry, "sqlite::memory:").await?;
```

#### Registering Actions

```rust
// Using the macro (recommended)
register_action!(executor, "action_name", action_function).await?;

// Manual registration
executor.register_action(Arc::new(MyAction)).await?;
```

#### Loading and Executing Workflows

```rust
// Load YAML workflow
executor.load_yaml_file("workflow.yaml").await?;

// Execute static DAG
let report = executor.execute_static_dag("workflow_name", &cache, cancel_rx).await?;

// Execute agent-driven flow
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

## Migration Guides

For historical context and migration information:
- [Archived Migration Docs](archive/migrations/) - Previous migration guides

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

- **Issues**: [GitHub Issues](https://github.com/yourusername/dagger/issues)
- **Examples**: [examples/](../examples/)
- **Tests**: [tests/](../tests/)

## License

MIT License - See [LICENSE](../LICENSE) for details.