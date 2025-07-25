# Quick Migration Fix for DAG Flow API Changes

## The Problem

The `execute_static_dag` method signature changed from:
```rust
pub async fn execute_static_dag(
    &mut self,
    spec: WorkflowSpec,
    cache: &Cache,
    cancel_rx: oneshot::Receiver<()>,
) -> Result<DagExecutionReport, DagError>
```

To:
```rust
pub async fn execute_static_dag(
    &mut self,
    name: &str,
    cache: &Cache,
    cancel_rx: oneshot::Receiver<()>,
) -> Result<DagExecutionReport, DagError>
```

## Quick Fix

### For Static Workflows

Replace:
```rust
executor.execute_static_dag(
    WorkflowSpec::Static { name: workflow_name.to_string() },
    &cache,
    cancel_rx
)
```

With:
```rust
executor.execute_static_dag(&workflow_name, &cache, cancel_rx)
```

### For Agent Workflows

Replace:
```rust
executor.execute_dag(
    WorkflowSpec::Agent { task: task_name.to_string() },
    &cache,
    cancel_rx
)
```

With:
```rust
executor.execute_agent_dag(&task_name, &cache, cancel_rx)
```

## Automated Fix

You can use this sed command to fix most cases:

```bash
# For static workflows
find . -name "*.rs" -type f -exec sed -i '' \
  's/execute_static_dag(\s*WorkflowSpec::Static\s*{\s*name:\s*\([^}]*\)\s*},/execute_static_dag(\1,/g' {} +

# Simplify .to_string() calls
find . -name "*.rs" -type f -exec sed -i '' \
  's/execute_static_dag(\s*\([^.]*\)\.to_string(),/execute_static_dag(\&\1,/g' {} +
```

## Manual Examples

### Example 1: add_files.rs
```rust
// OLD
.execute_static_dag(
    WorkflowSpec::Static {
        name: workflow_name.to_string(),
    },
    &cache,
    cancel_rx
)

// NEW
.execute_static_dag(&workflow_name, &cache, cancel_rx)
```

### Example 2: chat_fanout.rs
```rust
// OLD
.execute_static_dag(workflow_spec, &cache, cancel_rx)

// NEW (if workflow_spec was WorkflowSpec::Static { name })
.execute_static_dag(&name, &cache, cancel_rx)
```

### Example 3: Direct string literals
```rust
// OLD
.execute_static_dag(
    WorkflowSpec::Static {
        name: "folder_walker_modern".to_string(),
    },
    &cache,
    cancel_rx
)

// NEW
.execute_static_dag("folder_walker_modern", &cache, cancel_rx)
```

## If You Need the Old Behavior

If you need to support both static and agent workflows dynamically, use the generic method:

```rust
// For dynamic dispatch between static and agent workflows
let report = match workflow_type {
    WorkflowType::Static(name) => {
        executor.execute_static_dag(&name, &cache, cancel_rx).await?
    }
    WorkflowType::Agent(task) => {
        executor.execute_agent_dag(&task, &cache, cancel_rx).await?
    }
};
```

## Why This Change?

1. **Simpler API**: Most users only need to pass a workflow name
2. **Better Performance**: Avoids unnecessary string allocations
3. **Clearer Intent**: Separate methods for static vs agent workflows
4. **Type Safety**: Compile-time guarantee of workflow type