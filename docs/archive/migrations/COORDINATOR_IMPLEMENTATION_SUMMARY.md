# Coordinator Implementation Summary for RZN Team

## Overview

We've implemented Sam's recommended coordinator-based architecture for parallel DAG execution. This completely solves the borrow checker issues while maintaining high parallelism and enabling dynamic graph growth.

## What Was Implemented (Dagger Team)

### 1. New Module Structure (`src/coord/`)

```
src/coord/
├── mod.rs          # Module exports
├── types.rs        # Core types (ExecutionEvent, ExecutorCommand, NodeSpec)
├── hooks.rs        # EventHook trait and HookContext
├── action.rs       # NodeAction trait (compute-only)
├── coordinator.rs  # Main Coordinator implementation
└── registry.rs     # ActionRegistry for NodeAction instances
```

### 2. Core Types (`coord/types.rs`)

```rust
// Node reference
pub struct NodeRef {
    pub dag_name: String,
    pub node_id: String,
}

// Events from workers
pub enum ExecutionEvent {
    NodeStarted { node: NodeRef },
    NodeCompleted { node: NodeRef, outcome: NodeOutcome },
    NodeFailed { node: NodeRef, error: String },
}

// Commands to mutate state
pub enum ExecutorCommand {
    AddNode { dag_name: String, spec: NodeSpec },
    AddNodes { dag_name: String, specs: Vec<NodeSpec> },
    SetNodeInputs { dag_name: String, node_id: String, inputs: Value },
    PauseBranch { branch_id: String, reason: Option<String> },
    ResumeBranch { branch_id: String },
    CancelBranch { branch_id: String },
    EmitEvent { event: RuntimeEventV2 },
}

// Node specification
pub struct NodeSpec {
    pub id: Option<String>,
    pub action: String,
    pub deps: Vec<String>,
    pub inputs: Value,
    pub timeout: Option<u64>,
    pub try_count: Option<u32>,
}
```

### 3. NodeAction Trait (`coord/action.rs`)

**This replaces the old NodeAction that required `&mut DagExecutor`**

```rust
// Context for node execution (immutable, clonable)
pub struct NodeCtx {
    pub dag_name: String,
    pub node_id: String,
    pub inputs: Value,
    pub cache: Cache,
    pub app_data: Option<Arc<dyn Any + Send + Sync>>, // For RZN-specific data
}

// Output from node execution
pub struct NodeOutput {
    pub outputs: Option<Value>,
    pub success: bool,
    pub metadata: Option<Value>,
}

// NEW compute-only trait
#[async_trait]
pub trait NodeAction: Send + Sync {
    fn name(&self) -> &str;
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput>;
}
```

### 4. EventHook Trait (`coord/hooks.rs`)

**Hooks process events and return commands, never mutating directly**

```rust
pub struct HookContext {
    pub run_id: String,
    pub dag_name: String,
    pub app_data: Option<Arc<dyn Any + Send + Sync>>, // For RZN-specific data
}

#[async_trait]
pub trait EventHook: Send + Sync {
    async fn handle(&self, ctx: &HookContext, event: &ExecutionEvent) -> Vec<ExecutorCommand>;
    async fn on_start(&self, ctx: &HookContext) -> Vec<ExecutorCommand> { vec![] }
    async fn on_complete(&self, ctx: &HookContext, success: bool) -> Vec<ExecutorCommand> { vec![] }
}
```

### 5. Coordinator (`coord/coordinator.rs`)

**The heart of the system - orchestrates everything**

```rust
pub struct Coordinator {
    evt_tx: mpsc::Sender<ExecutionEvent>,
    evt_rx: mpsc::Receiver<ExecutionEvent>,
    cmd_tx: mpsc::Sender<ExecutorCommand>,
    cmd_rx: mpsc::Receiver<ExecutorCommand>,
    hooks: Vec<Arc<dyn EventHook>>,
}

impl Coordinator {
    pub async fn run_parallel(
        mut self,
        exec: &mut DagExecutor,  // Only Coordinator has &mut
        cache: &Cache,
        dag_name: &str,
        run_id: &str,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<()>
}
```

Key features:
- **Backpressure**: Bounded channels prevent overwhelming the system
- **Graceful shutdown**: Cancel signal stops new work, waits for in-flight
- **Single mutation point**: Only `apply_command()` touches `&mut DagExecutor`
- **Parallel workers**: Spawn tasks with semaphore for concurrency control

### 6. ActionRegistry (`coord/registry.rs`)

```rust
pub struct ActionRegistry {
    actions: Arc<RwLock<HashMap<String, Arc<dyn NodeAction>>>>,
}

impl ActionRegistry {
    pub fn register(&self, action: Arc<dyn NodeAction>);
    pub fn get(&self, name: &str) -> Option<Arc<dyn NodeAction>>;
}
```

## What RZN Team Needs to Implement

### 1. Planner as EventHook

```rust
pub struct PlannerHook {
    state_db: Arc<StateDb>,  // Your state.db handle
}

#[async_trait]
impl EventHook for PlannerHook {
    async fn handle(&self, ctx: &HookContext, event: &ExecutionEvent) -> Vec<ExecutorCommand> {
        match event {
            ExecutionEvent::NodeCompleted { node, .. } => {
                // Check if this was an LLM node
                if node.node_id.starts_with("llm_") {
                    // Read LlmTurnOutput from state.db
                    let output = self.state_db.get_node_state(&node.node_id).await;
                    
                    // Parse for tool calls
                    if let Some(tool_calls) = parse_tool_calls(&output) {
                        let mut commands = vec![];
                        
                        // Add tool nodes
                        for tool in tool_calls {
                            commands.push(ExecutorCommand::AddNode {
                                dag_name: ctx.dag_name.clone(),
                                spec: NodeSpec::new("tool_invoke")
                                    .with_deps(vec![node.node_id.clone()])
                                    .with_inputs(json!({
                                        "tool_name": tool.name,
                                        "args": tool.args,
                                    })),
                            });
                        }
                        
                        // Add continuation node
                        // Add next LLM node
                        
                        return commands;
                    }
                }
            }
            _ => {}
        }
        vec![]
    }
}
```

### 2. Migrate Your NodeActions

Convert your existing actions to the new trait:

```rust
// OLD
async fn provider_llm_node(
    executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache,
) -> Result<()> {
    // Had access to executor
}

// NEW
pub struct ProviderLlmAction {
    provider_adapter: Arc<ProviderAdapter>,
}

#[async_trait]
impl NodeAction for ProviderLlmAction {
    fn name(&self) -> &str {
        "provider_llm"
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
        // No executor access, pure computation
        let deployment_id = ctx.get_input::<String>("deployment_id")?;
        
        // Do LLM call
        let response = self.provider_adapter.complete(...).await?;
        
        // Return output
        Ok(NodeOutput::success(json!({
            "response": response,
        })))
    }
}
```

### 3. Register Actions and Hooks

```rust
// In your app initialization
let action_registry = ActionRegistry::new();
action_registry.register(Arc::new(ProviderLlmAction { ... }));
action_registry.register(Arc::new(ToolInvokeAction { ... }));
action_registry.register(Arc::new(ContinuationAction { ... }));

// Create hooks
let hooks = vec![
    Arc::new(PlannerHook { state_db }),
    Arc::new(HitlHook { ... }),
    Arc::new(BudgetHook { ... }),
];

// Create coordinator
let coordinator = Coordinator::new(hooks, 100, 100);

// Run
coordinator.run_parallel(&mut executor, &cache, dag_name, run_id, cancel_rx).await?;
```

### 4. App Data Passing

Use `app_data` field for RZN-specific data:

```rust
// In your hook
impl EventHook for YourHook {
    async fn handle(&self, ctx: &HookContext, event: &ExecutionEvent) -> Vec<ExecutorCommand> {
        // Downcast app_data to your type
        if let Some(app_data) = &ctx.app_data {
            if let Some(rzn_data) = app_data.downcast_ref::<RznAppData>() {
                // Use your app-specific data
                let state_db = &rzn_data.state_db;
                let user_db = &rzn_data.user_db;
            }
        }
        vec![]
    }
}
```

## Key Architecture Benefits

1. **No Borrow Checker Issues**: Workers and hooks never see `&mut DagExecutor`
2. **True Parallelism**: Multiple nodes execute concurrently
3. **Dynamic Growth**: Hooks can add nodes via commands
4. **Clean Separation**: 
   - Workers: Pure computation
   - Hooks: Policy and planning
   - Coordinator: State mutation
5. **Testable**: Each component can be tested in isolation

## Migration Checklist

- [ ] Convert all NodeActions to new trait (no `&mut DagExecutor`)
- [ ] Implement PlannerHook as EventHook
- [ ] Implement HITL hook for pause/resume
- [ ] Implement budget/goal monitoring hooks
- [ ] Update action registry with all actions
- [ ] Replace `execute_dag_parallel` calls with `Coordinator::run_parallel`
- [ ] Test parallel execution with dynamic growth

## Example Usage

```rust
// Complete example in examples/coordinator_demo.rs
cargo run --example coordinator_demo
```

## API Quick Reference

### Creating a Coordinator
```rust
let coordinator = Coordinator::new(hooks, event_capacity, command_capacity);
```

### Running Execution
```rust
coordinator.run_parallel(executor, cache, dag_name, run_id, cancel_rx).await
```

### Implementing a Hook
```rust
#[async_trait]
impl EventHook for MyHook {
    async fn handle(&self, ctx: &HookContext, event: &ExecutionEvent) -> Vec<ExecutorCommand>
}
```

### Implementing an Action
```rust
#[async_trait]
impl NodeAction for MyAction {
    fn name(&self) -> &str;
    async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput>
}
```

## Support

The coordinator architecture is fully implemented and ready for integration. The example in `examples/coordinator_demo.rs` demonstrates a working system with parallel execution and dynamic node addition via hooks.

---

*Implementation complete as per Sam's specifications*  
*No backward compatibility maintained as requested*  
*RZN team can start integration immediately*