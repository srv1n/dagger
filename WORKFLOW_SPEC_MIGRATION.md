# WorkflowSpec Simplification Migration Guide

## Overview

The `WorkflowSpec` enum has been simplified to reduce verbosity while maintaining backward compatibility. Both variants (`Static` and `Agent`) essentially contained just a string, so we've introduced simpler alternatives.

## What Changed

### Before (Verbose)
```rust
// Static DAG execution
executor.execute_dag(
    WorkflowSpec::Static { 
        name: "my_workflow".to_string() 
    }, 
    &cache, 
    cancel_rx
).await?;

// Agent DAG execution  
executor.execute_dag(
    WorkflowSpec::Agent { 
        task: "analyze_task".to_string() 
    }, 
    &cache, 
    cancel_rx
).await?;
```

### After (Simplified)
```rust
// Static DAG execution
executor.execute_static_dag("my_workflow", &cache, cancel_rx).await?;

// Agent DAG execution
executor.execute_agent_dag("analyze_task", &cache, cancel_rx).await?;
```

## Migration Options

### Option 1: Use New Simplified Methods (RECOMMENDED)
```rust
// Replace this:
executor.execute_dag(WorkflowSpec::Static { name: "workflow".to_string() }, &cache, rx).await?;
// With this:
executor.execute_static_dag("workflow", &cache, rx).await?;

// Replace this:
executor.execute_dag(WorkflowSpec::Agent { task: "task".to_string() }, &cache, rx).await?;
// With this:
executor.execute_agent_dag("task", &cache, rx).await?;
```

### Option 2: Use ExecutionMode Enum
```rust
executor.execute_dag_with_mode(ExecutionMode::Static, "workflow", &cache, rx).await?;
executor.execute_dag_with_mode(ExecutionMode::Agent, "task", &cache, rx).await?;
```

### Option 3: No Changes Required (Backward Compatible)
The old `WorkflowSpec` enum still works - no immediate migration required.

## Benefits of the Simplification

✅ **Less Verbose**: Just pass strings instead of enum variants  
✅ **Cleaner API**: Purpose-specific methods  
✅ **Backward Compatible**: Old `WorkflowSpec` still works  
✅ **Type Safety**: `ExecutionMode` prevents confusion  
✅ **Better Developer Experience**: More intuitive method names  

## Key Behavioral Differences Preserved

The simplification preserves the important behavioral differences:

### Static Mode
- Executes pre-loaded DAGs from YAML files
- Simple execution path
- No dynamic node creation

### Agent Mode  
- Dynamic DAG creation with supervisor nodes
- Runtime DAG modifications
- State persistence in Sled database
- Iteration tracking and bootstrap logic

## Migration Timeline

- **Immediate**: New simplified methods available
- **Deprecated**: `WorkflowSpec` marked as deprecated
- **Future**: `WorkflowSpec` may be removed in a future major version

## Examples

See updated examples in:
- `examples/dag_flow/src/main.rs` - Shows simplified static execution
- `examples/agent_flow/src/main.rs` - Shows agent execution  
- `examples/simple_task/src/main.rs` - Demonstrates all approaches 