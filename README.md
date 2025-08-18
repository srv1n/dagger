# Dagger

Multi-paradigm workflow orchestration library for Rust. Execute YAML-defined DAGs, manage dynamic task graphs, or build event-driven systems with SQLite persistence and parallel execution.

## Installation

```toml
[dependencies]
dagger = { path = "path/to/dagger" }
tokio = { version = "1.36", features = ["full"] }
serde_json = "1.0"
anyhow = "1.0"
sqlx = { version = "0.8.6", features = ["runtime-tokio-rustls", "sqlite"] }
```

## Quick Start

```rust
use dagger::{DagExecutor, Cache, register_action, Node};
use tokio::sync::RwLock;
use std::sync::Arc;
use std::collections::HashMap;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize
    let registry = Arc::new(RwLock::new(HashMap::new()));
    let mut executor = DagExecutor::new(None, registry, "sqlite::memory:").await?;
    
    // Register action
    register_action!(executor, "process", process_data).await?;
    
    // Load workflow
    executor.load_yaml_file("workflow.yaml").await?;
    
    // Execute
    let cache = Cache::new();
    let (_tx, rx) = tokio::sync::oneshot::channel();
    let report = executor.execute_static_dag("pipeline", &cache, rx).await?;
    
    println!("Completed: {}", report.success);
    Ok(())
}

async fn process_data(_: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    // Implementation
    Ok(())
}
```

## Three Execution Paradigms

### 1. DAG Flow
Static workflows defined in YAML with automatic dependency resolution and parallel execution.

```yaml
name: pipeline
nodes:
  - id: fetch
    action: fetch_data
  - id: process
    action: process_data
    dependencies: [fetch]
```

### 2. Task Agent
Dynamic task graphs with runtime dependency creation and agent-based execution.

```rust
#[task_agent]
async fn analyze(task: Task) -> Result<Value> {
    // Create subtasks dynamically based on input
    Ok(json!({ "status": "processing" }))
}
```

### 3. Pub/Sub
Event-driven communication between decoupled agents.

```rust
#[pubsub_agent(subscribe = "events", publish = "results")]
async fn handler(msg: Message) -> Result<()> {
    // Process and publish
    Ok(())
}
```

## Key Features

- **Parallel Execution**: Automatic parallelization of independent nodes
- **SQLite Persistence**: ACID-compliant storage with compression
- **Send-Compatible**: Works with Tauri and cross-thread async contexts
- **Retry Logic**: Configurable retry strategies with exponential backoff
- **Visual Debugging**: Export execution graphs as DOT files

## Architecture

The library uses a Coordinator pattern for parallel execution without borrow checker issues:

- **Workers**: Execute nodes in parallel without mutable access to executor
- **Coordinator**: Single point for state mutations
- **Message Passing**: Event-driven communication via channels

Storage uses SQLite with zstd compression for 3-10x size reduction.

## Examples

Working examples in `examples/`:
- `dag_flow/` - YAML workflow execution
- `simple_task/` - Basic task demonstration
- `agent_simple/` - Agent-based execution
- `taskmanager_toy/` - Task manager patterns

Run with:
```bash
cargo run --bin simple_task
```

## Documentation

- [Quick Reference](docs/QUICK_REFERENCE.md) - Common patterns and snippets
- [Architecture](docs/ARCHITECTURE.md) - System design and components
- [DAG Flow Guide](docs/DAG_FLOW_IMPLEMENTATION_GUIDE.md) - YAML workflows
- [Task Agent Guide](docs/TASK_AGENT_ARCHITECTURE.md) - Dynamic tasks
- [Storage Guide](docs/SQLITE_DAG_STORAGE_GUIDE.md) - Persistence layer

## Recent Changes

The codebase recently migrated from `std::sync::RwLock` to `tokio::sync::RwLock` for Send-compatible futures. Many APIs are now async. See migration guide in docs/archive/ if upgrading.

## License

MIT