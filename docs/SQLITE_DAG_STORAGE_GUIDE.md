# SQLite Storage for DAG Flow - Comprehensive Guide

## Overview

The DAG Flow engine uses SQLite for persistent storage of execution state, cache data, and workflow artifacts. This guide provides detailed documentation on initialization, reading, writing, and all interfaces for working with SQLite storage in the DAG flow system.

## Architecture

The SQLite storage system (`src/dag_flow/sqlite_cache.rs`) provides:
- Persistent cache storage for node execution results
- Execution tree serialization and recovery
- Snapshot capabilities for workflow state
- Active/pending execution state management
- Compressed storage using zstd compression

## Database Initialization

### Creating a SQLite Cache Instance

```rust
use dagger::dag_flow::SqliteCache;

// Initialize with file-based database
let cache = SqliteCache::new("sqlite:dag_cache.db").await?;

// Initialize with in-memory database (for testing)
let cache = SqliteCache::new("sqlite::memory:").await?;

// The database URL will be automatically prefixed with "sqlite:" if not present
let cache = SqliteCache::new("dag_cache.db").await?; // Also works
```

### Automatic Schema Creation

When `SqliteCache::new()` is called, it automatically creates the following tables:

#### 1. **artifacts** Table
Stores cache data from node executions.

```sql
CREATE TABLE IF NOT EXISTS artifacts (
    dag_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,  -- JSON-serialized data
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (dag_id, node_id, key)
)
```

#### 2. **execution_trees** Table
Stores serialized execution trees for workflow recovery.

```sql
CREATE TABLE IF NOT EXISTS execution_trees (
    dag_id TEXT PRIMARY KEY,
    tree_data BLOB NOT NULL,  -- Compressed binary data
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
)
```

#### 3. **snapshots** Table
Stores point-in-time snapshots of workflow state.

```sql
CREATE TABLE IF NOT EXISTS snapshots (
    dag_id TEXT NOT NULL,
    snapshot_data BLOB NOT NULL,  -- Compressed binary data
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (dag_id, created_at)
)
```

#### 4. **execution_state** Table
Stores active and pending execution states.

```sql
CREATE TABLE IF NOT EXISTS execution_state (
    dag_id TEXT NOT NULL,
    state_type TEXT NOT NULL,  -- 'active' or 'pending'
    state_data BLOB NOT NULL,  -- Compressed binary data
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (dag_id, state_type)
)
```

### Indexes

The following indexes are automatically created for performance:
- `idx_artifacts_dag_id` on `artifacts(dag_id)`
- `idx_snapshots_dag_id` on `snapshots(dag_id)`
- `idx_execution_state_dag_id` on `execution_state(dag_id)`

## Write Operations

### 1. Saving Cache Data

Save complete cache data for a DAG:

```rust
use dagger::dag_flow::{Cache, SqliteCache};

let cache = Cache::new();
// ... populate cache with data ...

// Save entire cache to SQLite
let sqlite_cache = SqliteCache::new("dag_cache.db").await?;
sqlite_cache.save_cache("my_dag_id", &cache).await?;
```

**Implementation Details:**
- Begins a transaction for atomic updates
- Deletes existing cache data for the DAG
- Inserts all cache entries as JSON-serialized values
- Commits transaction on success

### 2. Saving Cache Deltas (Incremental Updates)

For efficient incremental updates:

```rust
use std::collections::HashMap;
use dagger::dag_flow::SerializableData;

let mut delta = HashMap::new();
let mut node_delta = HashMap::new();
node_delta.insert("output".to_string(), SerializableData::String("result".to_string()));
delta.insert("node1".to_string(), node_delta);

// Apply incremental update
sqlite_cache.save_cache_delta("my_dag_id", delta).await?;
```

**Implementation Details:**
- Uses `INSERT OR REPLACE` for upsert behavior
- Only updates modified entries
- Updates the `updated_at` timestamp

### 3. Saving Execution Trees

Save the execution tree for workflow recovery:

```rust
use dagger::dag_flow::ExecutionTree;

let tree = ExecutionTree::new();
// ... populate tree ...

sqlite_cache.save_execution_tree("my_dag_id", &tree).await?;
```

**Implementation Details:**
- Converts `ExecutionTree` to `SerializableExecutionTree`
- Serializes to JSON
- Compresses with zstd (compression level 3)
- Stores as BLOB in database

### 4. Saving Snapshots

Create point-in-time snapshots:

```rust
let snapshot_data = b"serialized workflow state";
sqlite_cache.save_snapshot("my_dag_id", snapshot_data).await?;
```

**Implementation Details:**
- Compresses data with zstd
- Stores with timestamp for versioning
- Multiple snapshots per DAG are supported

### 5. Saving Execution State

Save active or pending execution states:

```rust
let state_data = b"serialized state";
sqlite_cache.save_execution_state("my_dag_id", "active", state_data).await?;
sqlite_cache.save_execution_state("my_dag_id", "pending", state_data).await?;
```

## Read Operations

### 1. Loading Cache Data

Load complete cache for a DAG:

```rust
let cache = sqlite_cache.load_cache("my_dag_id").await?;
```

**Return Value:**
- Returns `Cache` object with all stored data
- Data is organized by node_id and key
- Values are deserialized from JSON

### 2. Loading Execution Trees

Recover execution tree:

```rust
let tree_option = sqlite_cache.load_execution_tree("my_dag_id").await?;
if let Some(tree) = tree_option {
    // Use recovered tree
}
```

**Return Value:**
- Returns `Option<ExecutionTree>`
- `None` if no tree exists for the DAG
- Automatically decompresses and deserializes

### 3. Loading Latest Snapshot

Get the most recent snapshot:

```rust
let snapshot_option = sqlite_cache.load_latest_snapshot("my_dag_id").await?;
if let Some(snapshot_data) = snapshot_option {
    // Process snapshot data
}
```

**Return Value:**
- Returns `Option<Vec<u8>>`
- Retrieves most recent snapshot by timestamp
- Automatically decompresses data

### 4. Loading Execution State

Load active or pending states:

```rust
let active_state = sqlite_cache.load_execution_state("my_dag_id", "active").await?;
let pending_state = sqlite_cache.load_execution_state("my_dag_id", "pending").await?;
```

## Complete API Reference

### SqliteCache Methods

| Method | Parameters | Returns | Description |
|--------|------------|---------|-------------|
| `new` | `database_url: &str` | `Result<Self>` | Create new SQLite cache instance |
| `save_cache` | `dag_id: &str, cache: &Cache` | `Result<()>` | Save complete cache data |
| `save_cache_delta` | `dag_id: &str, delta: HashMap<String, HashMap<String, SerializableData>>` | `Result<()>` | Save incremental cache updates |
| `load_cache` | `dag_id: &str` | `Result<Cache>` | Load complete cache data |
| `save_execution_tree` | `dag_id: &str, tree: &ExecutionTree` | `Result<()>` | Save execution tree |
| `load_execution_tree` | `dag_id: &str` | `Result<Option<ExecutionTree>>` | Load execution tree |
| `save_snapshot` | `dag_id: &str, snapshot_data: &[u8]` | `Result<()>` | Save workflow snapshot |
| `load_latest_snapshot` | `dag_id: &str` | `Result<Option<Vec<u8>>>` | Load most recent snapshot |
| `save_execution_state` | `dag_id: &str, state_type: &str, state_data: &[u8]` | `Result<()>` | Save execution state |
| `load_execution_state` | `dag_id: &str, state_type: &str` | `Result<Option<Vec<u8>>>` | Load execution state |
| `debug_print_db` | | `Result<()>` | Print database contents for debugging |

## Integration with DAG Executor

### Creating DAG Executor with SQLite

```rust
use dagger::{DagExecutor, DagConfig};
use std::sync::{Arc, RwLock};
use std::collections::HashMap;

let config = DagConfig::default();
let registry = Arc::new(RwLock::new(HashMap::new()));

// Create executor with SQLite storage
let mut executor = DagExecutor::new(
    Some(config),
    registry,
    "sqlite:dag_cache.db"  // SQLite database path
).await?;

// Or use in-memory database for testing
let mut executor = DagExecutor::new(
    Some(config),
    registry,
    "sqlite::memory:"
).await?;
```

### Automatic Persistence

The DAG executor automatically:
1. Saves cache data after each node execution
2. Updates execution trees during workflow progress
3. Creates snapshots at configurable intervals
4. Manages active/pending states for recovery

## Usage Examples

### Example 1: Complete Workflow with Persistence

```rust
use dagger::{DagExecutor, DagConfig, Cache, insert_value};
use std::sync::{Arc, RwLock};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize executor with SQLite
    let config = DagConfig::default();
    let registry = Arc::new(RwLock::new(HashMap::new()));
    let mut executor = DagExecutor::new(
        Some(config),
        registry,
        "sqlite:workflow.db"
    ).await?;
    
    // Load workflow
    executor.load_yaml_file("workflow.yaml")?;
    
    // Create cache with initial data
    let cache = Cache::new();
    insert_value(&cache, "inputs", "param1", 42)?;
    
    // Execute DAG (automatically persists to SQLite)
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let report = executor.execute_static_dag(
        "my_workflow",
        &cache,
        cancel_rx
    ).await?;
    
    // Cache is automatically saved to SQLite
    Ok(())
}
```

### Example 2: Recovery from Previous Execution

```rust
use dagger::dag_flow::{SqliteCache, Cache};

#[tokio::main]
async fn main() -> Result<()> {
    // Connect to existing database
    let sqlite_cache = SqliteCache::new("workflow.db").await?;
    
    // Load previous execution state
    let cache = sqlite_cache.load_cache("my_workflow").await?;
    
    // Load execution tree if exists
    if let Some(tree) = sqlite_cache.load_execution_tree("my_workflow").await? {
        println!("Recovered execution tree with {} nodes", tree.nodes.len());
    }
    
    // Continue execution from saved state...
    Ok(())
}
```

### Example 3: Debugging Database Contents

```rust
let sqlite_cache = SqliteCache::new("workflow.db").await?;
sqlite_cache.debug_print_db().await?;
```

Output:
```
=== SQLite DB Contents ===
Artifacts by DAG:
  DAG 'workflow1': 25 entries
  DAG 'workflow2': 18 entries
Execution trees:
  DAG 'workflow1': 2048 bytes
  DAG 'workflow2': 1536 bytes
Snapshots by DAG:
  DAG 'workflow1': 3 snapshots
  DAG 'workflow2': 2 snapshots
Execution states:
  DAG 'workflow1' active: 512 bytes
  DAG 'workflow1' pending: 256 bytes
```

## Performance Considerations

### Compression
- All BLOB data is compressed with zstd level 3
- Provides good balance between compression ratio and speed
- Typical compression ratios: 3-10x for JSON data

### Transaction Management
- Bulk operations use transactions for atomicity
- Cache saves are atomic (all-or-nothing)
- Delta updates minimize database writes

### Connection Pooling
- Uses SQLx connection pool for concurrent access
- Default pool configuration handles multiple readers
- Single writer ensures consistency

## Error Handling

All methods return `Result<T>` with detailed error messages:

```rust
match sqlite_cache.save_cache("dag_id", &cache).await {
    Ok(_) => println!("Cache saved successfully"),
    Err(e) => eprintln!("Failed to save cache: {}", e),
}
```

Common error scenarios:
- Database connection failures
- Serialization/deserialization errors
- Compression/decompression failures
- Schema migration issues

## Migration from Sled

If migrating from Sled to SQLite:

1. The API remains largely the same
2. Replace Sled database paths with SQLite URLs
3. Data is automatically migrated on first use
4. Performance characteristics differ (SQLite better for concurrent reads)

## Best Practices

1. **Use transactions for bulk operations**
   - Group related writes in transactions
   - Use delta updates for incremental changes

2. **Regular snapshots**
   - Create snapshots at workflow milestones
   - Use for checkpointing long-running workflows

3. **Database maintenance**
   - Periodically vacuum database files
   - Monitor database size growth
   - Archive old snapshots if needed

4. **Testing**
   - Use in-memory databases for unit tests
   - Test recovery scenarios
   - Verify data integrity after crashes

## Troubleshooting

### Database Locked Errors
- Ensure only one process accesses the database
- Check for uncommitted transactions
- Use appropriate timeout settings

### Performance Issues
- Create additional indexes if needed
- Use delta updates instead of full saves
- Consider partitioning large workflows

### Data Recovery
- Use snapshots for point-in-time recovery
- Execution trees preserve workflow structure
- Cache data can be partially recovered