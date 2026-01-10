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
use dagger::{action, coord::ActionRegistry, Cache, DagExecutor};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize (macro-registered actions are loaded here)
    let registry = ActionRegistry::new();
    let mut executor = DagExecutor::new(None, registry, "sqlite::memory:").await?;
    
    // Load workflow
    executor.load_yaml_file("workflow.yaml").await?;
    
    // Execute
    let cache = Cache::new();
    let (_tx, rx) = tokio::sync::oneshot::channel();
    let report = executor.execute_static_dag("pipeline", &cache, rx).await?;
    
    println!("Completed: {}", report.success);
    Ok(())
}

#[action(name = "process")]
async fn process_data(input: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    Ok(json!({ "status": "ok", "input": input }))
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
#[dagger::task_agent]
async fn analyze(task: Task) -> anyhow::Result<serde_json::Value> {
    // Create subtasks dynamically based on input
    Ok(json!({ "status": "processing" }))
}
```

### 3. Pub/Sub
Event-driven communication between decoupled agents.

```rust
#[dagger::pubsub_agent(subscribe = "events", publish = "results")]
async fn handler(msg: Message) -> anyhow::Result<()> {
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
- `dag_flow_basic.rs` - YAML workflow execution with `#[action]` (`examples/fixtures/basic_pipeline.yaml`)
- `dag_flow_pipeline.rs` - End-to-end order processing pipeline (`examples/fixtures/order_pipeline.yaml`)
- `dag_flow_cli.rs` - CLI runner for multiple YAML DAGs (`examples/fixtures/pipeline*.yaml`)
- `dag_flow_dot.rs` - Generate DOT graph for visualization (Graphviz)
- `task_agent_basic.rs` - Task Agent mode with `#[task_agent]`
- `pubsub_basic.rs` - Pub/Sub mode with `#[pubsub_agent]`
- `dynamic_nodes_demo.rs` - Dynamic DAG growth
- `coordinator_demo.rs` - Coordinator-based execution

Run with:
```bash
cargo run --example dag_flow_basic
```

Sample outputs and notes on nondeterminism: `examples/README.md`.

## Documentation

- [Architecture](docs/ARCHITECTURE.md) - System design and components
- [DAG Flow Guide](docs/DAG_FLOW_COMPLETE_GUIDE.md) - YAML workflows
- [Task Agent Guide](docs/TASK_AGENT_ARCHITECTURE.md) - Dynamic tasks
- [Pub/Sub Guide](docs/PUBSUB_ARCHITECTURE.md) - Event-driven agents
- [Upgrading / Adoption Notes](docs/UPGRADING.md) - Downstream migration checklist

## Recent Changes

This snapshot standardizes on macro-based registration for all three modes. If you are upgrading from earlier versions, read the adoption notes in `docs/UPGRADING.md`.

## License

MIT
