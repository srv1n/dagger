# Task Core

This crate provides the core components for the task execution system in Dagger.

## Key Components

### Executor
The `Executor` struct manages a pool of workers that process tasks from a ready queue. It implements the persist-then-run pattern for reliable task execution.

Key features:
- Worker pool with configurable concurrency using semaphores
- Lock-free ready queue with backpressure support
- Persist-then-run pattern for crash recovery
- Automatic retry with configurable delays
- Task timeout support

### Storage
The `Storage` trait defines the interface for task persistence, with `SqliteStorage` providing an embedded database implementation.

Features:
- Task and job persistence
- Efficient indexing by job, agent, status, and parent
- Atomic updates using compare-and-swap
- Support for querying tasks by various criteria

### Ready Queue
A lock-free queue implementation using crossbeam's `SegQueue` with capacity control for backpressure.

### Agent System
Two trait hierarchies for implementing task agents:

1. **Agent trait**: Core interface for task execution
   - `name()`: Unique agent identifier
   - `description()`: Human-readable description
   - `execute()`: Async task execution

2. **JsonAgent trait**: Extended interface with JSON schema validation
   - `input_schema()`: JSON schema for input validation
   - `output_schema()`: JSON schema for output validation
   - Automatic validation in the blanket Agent implementation

### Task Context
Provides all necessary information and utilities for task execution:
- Task metadata (ID, job ID, parent, dependencies)
- Cache access for storing intermediate results
- Storage access for querying other tasks
- TaskHandle for creating child tasks dynamically
- Shared state for inter-task communication

### Agent Registry
Manages available agents with optional metadata:
- Register agents with name, description, version, author, and tags
- Look up agents by name
- List all registered agents

## Usage Example

```rust
use task_core::{Executor, ExecutorConfig, AgentRegistry, SqliteStorage};
use dagger::taskagent::{Task, TaskStatus, Cache};

// Create storage
let storage = Arc::new(SqliteStorage::open("./task_db/tasks.db").await?);

// Create agent registry and register agents
let registry = Arc::new(AgentRegistry::new());
registry.register(Arc::new(MyAgent::new()));

// Configure executor
let config = ExecutorConfig {
    max_workers: 10,
    queue_capacity: 1000,
    task_timeout: Some(Duration::from_secs(300)),
    retry_delay: Duration::from_secs(1),
};

// Create and run executor
let executor = Executor::new(storage, registry, cache, config);
let (shutdown_tx, shutdown_rx) = oneshot::channel();
executor.run(shutdown_rx).await?;
```

## Integration with Scheduler

The executor is designed to work with the Scheduler component, which:
1. Monitors task dependencies
2. Enqueues ready tasks to the executor
3. Handles job-level orchestration

The separation of concerns allows the executor to focus on reliable task execution while the scheduler handles workflow orchestration.