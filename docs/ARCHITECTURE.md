# Dagger Architecture

## System Overview

Dagger is a multi-paradigm workflow orchestration library built in Rust, designed for production systems requiring sophisticated workflow management with persistence, retry logic, and visual debugging capabilities.

## Table of Contents

1. [Core Architecture](#core-architecture)
2. [Storage Layer](#storage-layer)
3. [Execution Paradigms](#execution-paradigms)
4. [Parallel Execution](#parallel-execution)
5. [Implementation Guide](#implementation-guide)
6. [Performance Considerations](#performance-considerations)
7. [Best Practices](#best-practices)

## Core Architecture

### System Overview

```
┌──────────────────────────────────────────────────────────────┐
│                     Application Layer                         │
│  (Your code using Dagger's APIs and macros)                 │
└──────────────────────────────────────────────────────────────┘
                              │
┌──────────────────────────────────────────────────────────────┐
│                    Orchestration Layer                        │
├────────────────┬─────────────────┬───────────────────────────┤
│   DAG Flow     │   Task-Core     │      Pub/Sub              │
│   Executor     │    System       │     Executor              │
├────────────────┴─────────────────┴───────────────────────────┤
│                     Common Services                           │
│  • Cache (DashMap)  • Registry  • Error Handling             │
└──────────────────────────────────────────────────────────────┘
                              │
┌──────────────────────────────────────────────────────────────┐
│                    Storage Layer (SQLite)                     │
│  • Persistence  • Transactions  • Queries  • Artifacts       │
└──────────────────────────────────────────────────────────────┘
                              │
┌──────────────────────────────────────────────────────────────┐
│                      Runtime Layer                            │
│         Tokio Async Runtime + Thread Pool                     │
└──────────────────────────────────────────────────────────────┘
```

### Key Design Principles

1. **Async-First**: All I/O operations are async using Tokio
2. **Zero-Copy**: Extensive use of Arc and reference counting
3. **Type Safety**: Leverage Rust's type system for correctness
4. **Modular**: Three independent paradigms that can be used separately
5. **Persistent**: SQLite provides ACID-compliant storage

## Storage Layer

### SQLite Schema Architecture

The storage layer uses SQLite as a unified persistence backend, replacing the previous Sled KV store. This provides:

- **ACID Transactions**: Atomic operations across multiple tables
- **Queryability**: SQL queries for analytics and debugging
- **Portability**: Single file database, easy backup/restore
- **Performance**: WAL mode for concurrent reads

### Core Tables

#### Flow Execution Tables

```sql
-- Workflow execution tracking
CREATE TABLE flow_runs (
    run_id TEXT PRIMARY KEY,
    flow_name TEXT NOT NULL,
    status TEXT NOT NULL,
    config JSON,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

-- Node execution within flows
CREATE TABLE node_runs (
    run_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    invocation_key TEXT NOT NULL DEFAULT 'default',  -- For fan-out
    attempt INTEGER NOT NULL DEFAULT 1,
    state TEXT NOT NULL,
    input_hash TEXT NOT NULL,  -- For cache invalidation
    output_artifact_id TEXT,
    PRIMARY KEY (run_id, node_id, invocation_key, attempt)
);
```

#### Artifact Storage Strategy

```sql
CREATE TABLE artifacts (
    artifact_id TEXT PRIMARY KEY,  -- SHA256 of content
    kind TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    storage_location TEXT CHECK(storage_location IN ('inline','file')),
    inline_data BLOB,  -- For small artifacts < 32KB
    file_path TEXT,    -- For larger artifacts
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);
```

**Storage Tiers**:
- **Inline**: Artifacts < 32KB stored directly in SQLite
- **File**: Larger artifacts stored on filesystem, path in DB

#### Task Management Tables

```sql
CREATE TABLE tasks (
    task_id INTEGER PRIMARY KEY,
    status TEXT NOT NULL,
    agent_type TEXT NOT NULL,
    priority INTEGER DEFAULT 0,
    payload BLOB,
    lease_expires_at DATETIME,  -- For worker assignment
    version INTEGER DEFAULT 0    -- For optimistic concurrency
);
```

### Connection Management

```rust
pub struct SqliteStorage {
    pool: SqlitePool,  // Connection pool
    artifact_threshold: usize,  // 32KB default
}

// Configuration
let pool = SqlitePoolOptions::new()
    .max_connections(5)
    .connect("sqlite:workflow.db").await?;

// Enable WAL mode for better concurrency
sqlx::query("PRAGMA journal_mode = WAL").execute(&pool).await?;
```

## Execution Paradigms

### 1. DAG Flow: YAML-Based Workflows

The DAG Flow system executes workflows defined in YAML files with automatic dependency resolution.

#### Architecture

```rust
pub struct DagExecutor {
    pub registry: Arc<RwLock<HashMap<String, Arc<dyn NodeAction>>>>,
    pub dags: HashMap<String, GraphDefinition>,
    pub sqlite_cache: Arc<SqliteCache>,
    pub config: DagConfig,
    pub execution_context: Option<ExecutionContext>,
}
```

#### Workflow Loading

```yaml
name: data_pipeline
nodes:
  - id: fetch
    dependencies: []
    action: fetch_data
    
  - id: process_a
    dependencies: [fetch]
    action: process_type_a
    
  - id: process_b
    dependencies: [fetch]
    action: process_type_b
    
  - id: combine
    dependencies: [process_a, process_b]
    action: combine_results
```

The system:
1. Parses YAML into `GraphDefinition`
2. Validates for cycles using petgraph
3. Builds execution graph
4. Identifies parallelization opportunities

### 2. Task-Core: Dynamic Task System

A sophisticated task execution system with dynamic dependency creation.

#### Key Features

```rust
pub trait Storage: Send + Sync {
    async fn put(&self, task: &Task) -> Result<()>;
    async fn get(&self, id: TaskId) -> Result<Option<Task>>;
    async fn update_status(&self, id: TaskId, old: TaskStatus, new: TaskStatus) -> Result<()>;
    // Compare-and-swap for lock-free updates
}
```

#### Dynamic Dependencies

Tasks can create new dependencies at runtime:

```rust
// Task discovers it needs more data
let new_task_id = system.create_task(/* ... */)?;
system.add_dependency(current_task_id, new_task_id)?;
system.update_status(current_task_id, TaskStatus::Blocked)?;
```

### 3. Pub/Sub: Event-Driven Communication

Enables loosely-coupled agent communication through channels.

#### Architecture

```rust
pub struct PubSubExecutor {
    agents: Vec<Box<dyn PubSubAgent>>,
    channels: Arc<RwLock<HashMap<String, Sender<Message>>>>,
}

#[async_trait]
pub trait PubSubAgent {
    fn subscriptions(&self) -> Vec<String>;
    fn publications(&self) -> Vec<String>;
    async fn handle_message(&mut self, message: Message) -> Result<()>;
}
```

## Parallel Execution

### How It Works

The DAG Flow system supports two execution modes:

#### Sequential Execution
```rust
// All nodes execute one at a time
let config = DagConfig {
    enable_parallel_execution: false,
    ..Default::default()
};
```

Execution order: A → B → C → D (even if B and C have no dependencies)

#### Parallel Execution (Default)
```rust
let config = DagConfig {
    enable_parallel_execution: true,
    max_parallel_nodes: 4,  // Limit concurrency
    ..Default::default()
};
```

Execution: A → [B, C in parallel] → D

### Implementation Details

```rust
// Parallel execution using FuturesUnordered
async fn execute_parallel(&mut self) -> Result<()> {
    let semaphore = Arc::new(Semaphore::new(self.config.max_parallel_nodes));
    let mut futures = FuturesUnordered::new();
    
    while let Some(ready_nodes) = self.get_ready_nodes() {
        for node in ready_nodes {
            let permit = semaphore.clone().acquire_owned().await?;
            futures.push(async move {
                let result = self.execute_node(node).await;
                drop(permit);  // Release semaphore
                result
            });
        }
        
        // Wait for any to complete
        if let Some(result) = futures.next().await {
            self.handle_completion(result)?;
        }
    }
}
```

### Performance Comparison

From the dag_flow example:
- **Sequential**: ~300ms for 3-node workflow
- **Parallel**: ~100ms (66% reduction)
- **Speedup**: Proportional to parallelizable nodes

## Implementation Guide

### Creating a Custom Action

```rust
use dagger::{DagExecutor, Node, Cache, register_action};

async fn process_data(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    // Get inputs
    let input: String = parse_input_from_name(cache, "data", &node.inputs)?;
    
    // Process
    let result = input.to_uppercase();
    
    // Store output
    insert_value(cache, &node.id, "result", result)?;
    
    Ok(())
}

// Register with executor
register_action!(executor, "process_data", process_data);
```

### Using the Macro System

```rust
use dagger_macros::action;

#[action(
    input_schema = r#"{"type": "object", "properties": {"text": {"type": "string"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"result": {"type": "string"}}}"#
)]
async fn transform_text(input: Value) -> Result<Value> {
    let text = input["text"].as_str().unwrap();
    Ok(json!({"result": text.to_uppercase()}))
}
```

### Working with Storage

```rust
// Initialize storage
let storage = SqliteStorage::new("sqlite:workflows.db").await?;
storage.init_database().await?;

// Store artifact
let artifact_id = storage.store_artifact(
    data.as_bytes(),
    "json",
    "run_123",
    "node_456"
).await?;

// Retrieve artifact
let data = storage.get_artifact(&artifact_id).await?;
```

### Error Handling

```rust
use dagger::DagError;

match executor.execute_static_dag("workflow", &cache, cancel_rx).await {
    Ok(report) => println!("Success: {:?}", report),
    Err(DagError::ExecutionTimeout(msg)) => eprintln!("Timeout: {}", msg),
    Err(DagError::ValidationError(msg)) => eprintln!("Invalid: {}", msg),
    Err(e) => eprintln!("Error: {}", e),
}
```

## Performance Considerations

### 1. Cache Strategy

- **In-Memory Cache**: DashMap for hot data
- **SQLite Cache**: Persistent storage for checkpoints
- **Artifact Storage**: Two-tier (inline/file) based on size

### 2. Concurrency

- **Connection Pooling**: 5 connections default
- **WAL Mode**: Enable concurrent reads
- **Semaphore Limiting**: Control parallel node execution

### 3. Optimization Tips

```rust
// Batch database operations
let mut tx = pool.begin().await?;
for task in tasks {
    sqlx::query!("INSERT INTO tasks ...").execute(&mut tx).await?;
}
tx.commit().await?;

// Use prepared statements
let stmt = sqlx::query!("SELECT * FROM tasks WHERE status = ?");
let tasks: Vec<Task> = stmt.bind("ready").fetch_all(&pool).await?;
```

## Best Practices

### 1. Workflow Design

- **Keep nodes focused**: Single responsibility per node
- **Minimize dependencies**: Only declare necessary dependencies
- **Use caching**: Leverage input_hash for cache reuse
- **Handle failures**: Implement proper retry logic

### 2. Storage Management

- **Regular cleanup**: Delete old artifacts periodically
- **Monitor size**: Track database and artifact directory size
- **Backup strategy**: Regular SQLite backups
- **Vacuum periodically**: `VACUUM` command for space reclamation

### 3. Production Deployment

```rust
// Production configuration
let config = DagConfig {
    enable_parallel_execution: true,
    max_parallel_nodes: num_cpus::get(),
    max_attempts: Some(3),
    timeout_seconds: Some(3600),
    retry_strategy: RetryStrategy::Exponential {
        initial_delay_secs: 5,
        max_delay_secs: 300,
        multiplier: 2.0,
    },
    on_failure: OnFailure::Continue,  // Don't halt entire workflow
    ..Default::default()
};
```

### 4. Monitoring

```rust
// Add instrumentation
use tracing::{info, instrument};

#[instrument(skip(cache))]
async fn execute_node(node: &Node, cache: &Cache) -> Result<()> {
    info!("Executing node: {}", node.id);
    let start = Instant::now();
    
    // Execute...
    
    info!("Node {} completed in {:?}", node.id, start.elapsed());
    Ok(())
}
```

## Migration from Sled

The library recently migrated from Sled to SQLite for better queryability and ACID compliance. Key changes:

1. **Storage Interface**: Same trait-based API, different backend
2. **Async Operations**: Storage operations now async
3. **Better Queries**: SQL instead of key-value scans
4. **Transactions**: Multi-table atomic operations

## Troubleshooting

### Common Issues

1. **Database Locked**: Ensure only one writer, use WAL mode
2. **Memory Growth**: Check artifact cleanup, cache expiration
3. **Slow Queries**: Add indexes, use EXPLAIN QUERY PLAN
4. **Parallel Deadlock**: Check dependency cycles

### Debug Tools

```rust
// Generate execution graph
let dot = executor.serialize_tree_to_dot("workflow").await?;
std::fs::write("debug.dot", dot)?;
// View with: dot -Tpng debug.dot -o debug.png

// Query execution state
let status = sqlx::query!("SELECT * FROM node_runs WHERE run_id = ?")
    .bind(run_id)
    .fetch_all(&pool).await?;
```

## Contributing

See [CONTRIBUTING.md](../CONTRIBUTING.md) for guidelines on:
- Code style
- Testing requirements
- Pull request process
- Issue reporting

## License

MIT - See [LICENSE](../LICENSE) for details.