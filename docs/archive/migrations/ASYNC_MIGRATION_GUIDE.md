# Async Migration Guide - RwLock to tokio::sync::RwLock

## Overview

This document describes the migration from `std::sync::RwLock` to `tokio::sync::RwLock` throughout the Dagger codebase. This change was necessary to make DAG execution futures `Send`-compatible, which is required for integration with Tauri commands and other async contexts.

## Background

The fundamental issue was that `DagExecutor`'s methods were holding `std::sync::RwLock` guards across `.await` points, making the resulting futures `!Send`. This prevented the futures from being used in contexts that require `Send` bounds, such as:
- Tauri command handlers
- Tokio spawn operations
- Cross-thread async execution

## Key Changes

### 1. Core RwLock Migration

All `std::sync::RwLock` instances have been replaced with `tokio::sync::RwLock`:

```rust
// Before
use std::sync::{Arc, RwLock};
let registry = Arc::new(RwLock::new(HashMap::new()));

// After  
use std::sync::Arc;
use tokio::sync::RwLock;
let registry = Arc::new(RwLock::new(HashMap::new()));
```

### 2. Async Lock Operations

All RwLock read/write operations are now async and require `.await`:

```rust
// Before
let registry = self.function_registry.read()
    .map_err(|e| anyhow!("Failed to acquire lock: {}", e))?;

// After
let registry = self.function_registry.read().await;
```

Note: `tokio::sync::RwLock` operations are infallible (cannot panic), so `.map_err()` patterns have been removed.

### 3. Function Signature Updates

Many functions that use RwLock operations have become async:

```rust
// Before
pub fn new(config: Option<DagConfig>, registry: Arc<RwLock<...>>, db_path: &str) -> Result<Self>

// After  
pub async fn new(config: Option<DagConfig>, registry: Arc<RwLock<...>>, db_path: &str) -> Result<Self>
```

Key functions that are now async:
- `DagExecutor::new()`
- `DagExecutor::load_yaml_file()`
- `DagExecutor::list_dags()` and related listing functions
- `DagExecutor::build_dag_internal()`
- `validate_node_actions()`
- `Coordinator::is_complete()`
- `Coordinator::apply_command()`
- `HumanInterrupt::cancel()`

### 4. Macro Updates

The `register_action!` macro now returns a future that must be awaited:

```rust
// Before
register_action!(executor, "add_numbers", add_numbers);

// After
register_action!(executor, "add_numbers", add_numbers).await?;
```

### 5. Example Code Updates

All examples have been updated to use the new async APIs:

```rust
// Before
let mut executor = DagExecutor::new(None, registry.clone(), "dagger_db")?;
executor.load_yaml_file("pipeline.yaml")?;
let dag_names = executor.list_dags()?;

// After
let mut executor = DagExecutor::new(None, registry.clone(), "sqlite::memory:").await?;
executor.load_yaml_file("pipeline.yaml").await?;
let dag_names = executor.list_dags().await?;
```

## Migration Guide for Users

### Step 1: Update Imports

Replace all occurrences of:
```rust
use std::sync::{Arc, RwLock};
```

With:
```rust
use std::sync::Arc;
use tokio::sync::RwLock;
```

### Step 2: Update Function Signatures

Any function that calls RwLock operations must become async:

```rust
// Before
fn my_function(executor: &DagExecutor) -> Result<()> {
    let registry = executor.function_registry.read().unwrap();
    // ...
}

// After
async fn my_function(executor: &DagExecutor) -> Result<()> {
    let registry = executor.function_registry.read().await;
    // ...
}
```

### Step 3: Add .await to Async Calls

Update all calls to newly async functions:

```rust
// Before
let mut executor = DagExecutor::new(None, registry, "db_path")?;
register_action!(executor, "my_action", my_action_fn);

// After
let mut executor = DagExecutor::new(None, registry, "db_path").await?;
register_action!(executor, "my_action", my_action_fn).await?;
```

### Step 4: Remove Error Handling for Lock Operations

Since tokio RwLock operations are infallible, remove unnecessary error handling:

```rust
// Before
let guard = lock.read().map_err(|e| anyhow!("Lock poisoned: {}", e))?;

// After
let guard = lock.read().await;
```

## Benefits

1. **Send Futures**: All DAG execution futures are now `Send`, enabling use in more async contexts
2. **No Lock Poisoning**: tokio RwLocks cannot be poisoned, simplifying error handling
3. **Better Async Integration**: Natural fit with tokio-based async runtime
4. **Tauri Compatibility**: Can now be used directly in Tauri command handlers

## Performance Considerations

- tokio RwLocks are optimized for async contexts with minimal overhead
- Lock contention is handled more efficiently in async contexts
- No performance degradation observed in benchmarks

## SQLx Update

Additionally, SQLx has been updated from version 0.7 to 0.8.6 across all dependencies:

```toml
# Before
sqlx = { version = "0.7", features = [...] }

# After  
sqlx = { version = "0.8.6", features = [...] }
```

## Testing

All examples have been tested and confirmed working:
- `dag_flow` - YAML-based DAG execution with parallel support
- `simple_task` - Basic task execution demonstration
- `agent_simple` - Agent-based execution
- `agent_flow` - Complex agent workflows

## Backward Compatibility

This is a breaking change. Users will need to update their code to use the new async APIs. The migration is straightforward following the steps above.

## Common Issues and Solutions

### Issue: "() is not a future" error
**Solution**: You're trying to await a non-async function. Check if the function signature changed.

### Issue: "the trait Future is not implemented" 
**Solution**: You need to await an async function that you're not awaiting.

### Issue: Database file errors
**Solution**: Use `"sqlite::memory:"` for in-memory databases or ensure the database file path exists.

## Conclusion

The migration to `tokio::sync::RwLock` successfully resolves the `!Send` future issue, enabling Dagger to be used in a wider variety of async contexts including Tauri applications. The changes are comprehensive but follow a consistent pattern, making migration straightforward.