# Task-Core Demo Examples

## Overview

The new task-core system provides a high-performance, lock-free task execution framework. Here are clean examples showing how to use it.

## 1. Simple Hello World Example

```rust
use task_core::{
    TaskSystem, TaskSystemBuilder, TaskConfig,
    model::{NewTaskSpec, TaskId, AgentId, Durability, TaskType, AgentError},
    executor::{Agent, TaskContext, AgentRegistry},
    AGENTS,
};
use dagger_macros::task_agent;
use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;
use smallvec::smallvec;

/// Simple greeting agent
#[task_agent(
    name = "greeter",
    description = "Says hello"
)]
async fn greeter(input: Bytes, _ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    let name = String::from_utf8(input.to_vec())
        .map_err(|e| AgentError::User(format!("Invalid UTF-8: {}", e)))?;
    
    let greeting = format!("Hello, {}!", name);
    Ok(Bytes::from(greeting))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup
    let config = TaskConfig::default();
    let mut registry = AgentRegistry::new();
    
    // Register agents from linkme
    for register_fn in AGENTS {
        register_fn(&mut registry);
    }
    
    // Start system
    let system = TaskSystem::start("tasks.db", config, registry).await?;
    
    // Submit task
    let task_id = system.submit_task(NewTaskSpec {
        agent: greeterAgent::AGENT_ID,
        input: Bytes::from("World"),
        dependencies: smallvec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Greet the world"),
        timeout: Some(Duration::from_secs(5)),
        max_retries: Some(3),
        parent: None,
    }).await?;
    
    println!("Submitted task: {}", task_id);
    
    // Wait and shutdown
    tokio::time::sleep(Duration::from_secs(2)).await;
    system.shutdown().await?;
    
    Ok(())
}
```

## 2. Multi-Agent Workflow Example

```rust
use task_core::{
    model::{NewTaskSpec, TaskId, Durability, TaskType, AgentError},
    executor::{Agent, TaskContext},
};
use dagger_macros::task_agent;
use bytes::Bytes;
use std::sync::Arc;
use smallvec::smallvec;

/// Planner agent that creates subtasks
#[task_agent(
    name = "planner",
    description = "Plans workflow"
)]
async fn planner(input: Bytes, ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    let topic = String::from_utf8(input.to_vec())?;
    
    // Create research task
    let research_id = ctx.handle.spawn_task(NewTaskSpec {
        agent: researcherAgent::AGENT_ID,
        input: input.clone(),
        dependencies: smallvec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Research topic"),
        timeout: None,
        max_retries: Some(3),
        parent: ctx.parent,
    }).await?;
    
    // Create analysis task depending on research
    let analysis_id = ctx.handle.spawn_task(NewTaskSpec {
        agent: analyzerAgent::AGENT_ID,
        input: Bytes::from(research_id.to_string()),
        dependencies: smallvec![research_id],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Analyze results"),
        timeout: None,
        max_retries: Some(2),
        parent: ctx.parent,
    }).await?;
    
    // Store workflow info in shared state
    ctx.shared.put("workflow", &topic, 
        format!("{},{}", research_id, analysis_id).as_bytes())?;
    
    Ok(Bytes::from("Workflow planned"))
}

/// Researcher agent
#[task_agent(
    name = "researcher",
    description = "Performs research"
)]
async fn researcher(input: Bytes, ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    let topic = String::from_utf8(input.to_vec())?;
    
    // Simulate research
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    let results = format!("Research results for: {}", topic);
    
    // Store in shared state
    ctx.shared.put("research", &topic, results.as_bytes())?;
    
    Ok(Bytes::from(results))
}

/// Analyzer agent
#[task_agent(
    name = "analyzer",
    description = "Analyzes research"
)]
async fn analyzer(input: Bytes, ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    let research_id = String::from_utf8(input.to_vec())?
        .parse::<TaskId>()?;
    
    // Get research output
    let research_output = ctx.dependency_output(research_id).await?
        .ok_or_else(|| AgentError::User("Research not found".into()))?;
    
    let research = String::from_utf8(research_output.to_vec())?;
    let analysis = format!("Analysis of: {}", research);
    
    Ok(Bytes::from(analysis))
}
```

## Key Features Demonstrated

1. **Simple Agent Definition**: Use `#[task_agent]` macro
2. **Dynamic Task Creation**: Agents can spawn subtasks
3. **Dependencies**: Tasks wait for dependencies automatically
4. **Shared State**: Inter-task communication via shared state
5. **Error Handling**: Proper error propagation with `AgentError`
6. **Durability**: Choose between `BestEffort` and `AtMostOnce`

## Performance Benefits

- **Lock-free queues**: No mutex contention
- **Zero-copy where possible**: Using `Bytes` and `Arc<str>`
- **Atomic operations**: For status updates
- **Efficient storage**: Binary serialization with bincode
- **Parallel execution**: Configurable worker pool

## Running the Examples

1. Add dependencies to your `Cargo.toml`:
```toml
[dependencies]
task-core = { path = "path/to/task-core" }
dagger-macros = { path = "path/to/dagger-macros" }
tokio = { version = "1", features = ["full"] }
bytes = "1"
smallvec = "1"
```

2. Run with:
```bash
cargo run --release
```

The system will:
- Create a persistent task database
- Execute tasks with automatic retry
- Handle crashes with recovery
- Provide high throughput (25K+ tasks/second)