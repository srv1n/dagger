# Dagger: A Rust Library for Executing Directed Acyclic Graphs (DAGs)

Dagger is a lightweight, flexible Rust library designed for executing directed acyclic graphs (DAGs) with custom actions. It supports both predefined workflows loaded from YAML files and dynamic, agent-driven flows orchestrated at runtime. Whether you're automating data pipelines, orchestrating complex computations, or building adaptive workflows with human-in-the-loop capabilities, Dagger provides a robust foundation for task execution with minimal overhead.

## Table of Contents
- [What is Dagger?](#what-is-dagger)
- [Features](#features)
- [Installation](#installation)
- [Usage](#usage)
  - [Option 1: YAML-Based DAG Execution](#option-1-yaml-based-dag-execution)
  - [Option 2: Agent-Driven Flow Execution](#option-2-agent-driven-flow-execution)
  - [Option 3: Task Agent System](#option-3-task-agent-system)
- [Dagger Macros (Optional)](#dagger-macros-optional)
- [Key Concepts](#key-concepts)
- [Examples](#examples)
- [Contributing](#contributing)
- [License](#license)

## What is Dagger?
Dagger is a DAG execution engine built in Rust, emphasizing simplicity, extensibility, and performance. It solves the problem of orchestrating dependent tasks—whether static, predefined workflows or dynamic, adaptive processes—by providing a unified interface for defining, registering, and executing actions within a graph structure. It's particularly suited for:
- Data Pipelines: Execute sequential or parallel computations with clear dependencies
- Automation: Define reusable workflows in YAML for consistent execution
- Agent Systems: Build dynamic flows where a supervisor or agent decides the next steps at runtime
- Human-in-the-Loop: Integrate human intervention points within automated processes

Dagger abstracts the complexity of task scheduling, dependency management, and error handling, allowing developers to focus on the logic of their actions while leveraging Rust's safety and concurrency features.

## Features
- Static DAG Execution: Load and execute predefined workflows from YAML files with explicit dependencies and inputs/outputs
- Dynamic Agent Flows: Define actions executed on-the-fly by a supervisor or agent, adapting to runtime conditions
- Action Registration: Register custom Rust functions as actions with minimal boilerplate, using either macros or manual implementations
- Concurrency: Leverage Tokio for asynchronous execution, ensuring efficient handling of I/O-bound or CPU-bound tasks
- Error Handling: Built-in retry strategies, timeouts, and failure policies configurable per node or globally
- Caching: Persistent cache for inputs and outputs, supporting intermediate results and debugging
- Visualization: Generate DOT graphs of execution trees for analysis and debugging
- Extensibility: Optional dagger-macros crate for enhanced action definition with metadata (e.g., JSON schemas)

## Installation
Add dagger to your Cargo.toml:

```toml
[dependencies]
dagger = { path = "path/to/dagger" } # Replace with version or path
anyhow = "1.0"
serde_json = "1.0"
tokio = { version = "1.0", features = ["full"] }
tracing = "0.1"
tracing-subscriber = "0.3"
```

Optionally, include dagger-macros for macro-based action definitions:

```toml
[dependencies]
dagger-macros = { path = "path/to/dagger-macros" } # Replace with version or path
```

Ensure your Rust version is 1.70 or higher for compatibility with the latest features used.

## Usage
Dagger supports multiple execution modes: YAML-based static DAGs, agent-driven dynamic flows, and a task agent system for more complex orchestration. All methods rely on registering actions with a DagExecutor, but they differ in how the workflow is defined and executed.

### Option 1: YAML-Based DAG Execution
Overview: Define a DAG in a YAML file with nodes, dependencies, inputs, and outputs, then execute it using WorkflowSpec::Static. This approach is ideal for predefined, repeatable workflows.

#### Steps
1. Define Actions: Write async Rust functions to perform the tasks
2. Register Actions: Use register_action! (or optionally #[action] with dagger-macros) to register them with the executor
3. Load YAML: Use DagExecutor::load_yaml_file to load the DAG definition
4. Execute: Run the DAG with execute_dag using WorkflowSpec::Static

#### Without dagger-macros
```rust
use anyhow::{Error, Result};
use dagger::{
    insert_value, parse_input_from_name, register_action, serialize_cache_to_prettyjson, Cache,
    DagExecutionReport, DagExecutor, Node, WorkflowSpec,
};
use std::collections::HashMap;
use tokio::sync::oneshot;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use std::sync::{Arc, RwLock};
use tokio::time::{sleep, Duration};

// Action: Adds two numbers
async fn add_numbers(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let num1: f64 = parse_input_from_name(cache, "num1".to_string(), &node.inputs)?;
    let num2: f64 = parse_input_from_name(cache, "num2".to_string(), &node.inputs)?;
    let sum = num1 + num2;
    insert_value(cache, &node.id, &node.outputs[0].name, sum)?;
    insert_value(cache, &node.id, "input_terms", format!("{} + {}", num1, num2))?;
    insert_value(cache, &node.id, "output_sum", sum)?;
    Ok(())
}

// Action: Squares a number
async fn square_number(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let input: f64 = parse_input_from_name(cache, "input".to_string(), &node.inputs)?;
    let squared = input * input;
    insert_value(cache, &node.id, &node.outputs[0].name, squared)?;
    insert_value(cache, &node.id, "input_terms", format!("{}", input))?;
    insert_value(cache, &node.id, "output_squared", squared)?;
    Ok(())
}

// Action: Triples a number and adds a string
async fn triple_number_and_add_string(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache,
) -> Result<()> {
    let input: f64 = parse_input_from_name(cache, "input".to_string(), &node.inputs)?;
    let tripled = input * 3.0;
    insert_value(cache, &node.id, &node.outputs[0].name, tripled)?;
    insert_value(cache, &node.id, &node.outputs[1].name, "example_string".to_string())?;
    insert_value(cache, &node.id, "input_terms", format!("{}", input))?;
    insert_value(cache, &node.id, "output_tripled", tripled)?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let registry = Arc::new(RwLock::new(HashMap::new()));
    let mut executor = DagExecutor::new(None, registry.clone(), "dagger_db")?;

    // Register actions
    register_action!(executor, "add_numbers", add_numbers);
    register_action!(executor, "square_number", square_number);
    register_action!(executor, "triple_number_and_add_string", triple_number_and_add_string);

    executor
        .load_yaml_file("pipeline.yaml")
        .map_err(|e| anyhow::anyhow!("Failed to load pipeline.yaml: {}", e))?;

    let dag_names = executor.list_dags()?;
    println!("Loaded DAGs: {:#?}", dag_names);

    let cache = Cache::new(HashMap::new());
    insert_value(&cache, "inputs", "num1", 10.0)?;
    insert_value(&cache, "inputs", "num2", 20.0)?;

    let (cancel_tx, cancel_rx) = oneshot::channel();
    tokio::spawn(async move {
        sleep(Duration::from_secs(2)).await;
        let _ = cancel_tx.send(());
        println!("Cancellation signal sent");
    });

    let dag_report = executor
        .execute_dag(
            WorkflowSpec::Static {
                name: "infolder".to_string(),
            },
            &cache,
            cancel_rx,
        )
        .await?;

    let json_output = serialize_cache_to_prettyjson(&cache)?;
    println!("Cache as JSON:\n{}", json_output);
    println!("DAG Execution Report: {:#?}", dag_report);

    let dot_output = executor.serialize_tree_to_dot("infolder")?;
    println!("Execution Tree (DOT):\n{}", dot_output);

    Ok(())
}

#### YAML File (pipeline.yaml)
```yaml
name: infolder
description: A simple pipeline for mathematical operations
author: Rzn,Inc
version: 0.0.1
signature: example
tags:
  - infolder
  - physics
nodes:
  - id: node1
    dependencies: []
    inputs:
      - name: num1
        description: First number to add
        reference: inputs.num1
      - name: num2
        description: Second number to add
        reference: inputs.num2
    outputs:
      - name: sum
        description: Sum of num1 and num2
    action: add_numbers
    failure: failure_function1
    onfailure: true
    description: Adds two numbers
    timeout: 3600
    try_count: 3
  - id: node2
    dependencies:
      - node1
    inputs:
      - name: input
        description: Result from node1 to square
        reference: node1.sum
    outputs:
      - name: squared_result
        description: Squared result
    action: square_number
    failure: failure_function2
    onfailure: false
    description: Squares the sum from node1
    timeout: 1800
    try_count: 2
  - id: node3
    dependencies:
      - node2
    inputs:
      - name: input
        description: Result from node2 to triple
        reference: node2.squared_result
    outputs:
      - name: tripled_result
        description: Tripled result
      - name: test_string
        description: Example string output
    action: triple_number_and_add_string
    failure: failure_function3
    onfailure: true
    description: Triples the squared result and adds a string
    timeout: 3600
    try_count: 3
```

#### With dagger-macros
If you prefer the macro-based approach for YAML execution:
Add dagger-macros to your dependencies.
Use the #[action] macro on your functions and register with the generated uppercase static names.
```rust
use anyhow::{Error, Result};
use dagger::{
    insert_value, parse_input_from_name, serialize_cache_to_prettyjson, Cache,
    DagExecutionReport, DagExecutor, Node, WorkflowSpec,
};
use dagger_macros::action;
use std::collections::HashMap;
use tokio::sync::oneshot;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use std::sync::{Arc, RwLock};
use tokio::time::{sleep, Duration};

// Performs addition of two f64 numbers.
#[action(description = "Adds two numbers")]
async fn add_numbers(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let num1: f64 = parse_input_from_name(cache, "num1".to_string(), &node.inputs)?;
    let num2: f64 = parse_input_from_name(cache, "num2".to_string(), &node.inputs)?;
    let sum = num1 + num2;
    insert_value(cache, &node.id, &node.outputs[0].name, sum)?;
    insert_value(cache, &node.id, "input_terms", format!("{} + {}", num1, num2))?;
    insert_value(cache, &node.id, "output_sum", sum)?;
    Ok(())
}

// Squares a number.
#[action(description = "Squares a number")]
async fn square_number(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let input: f64 = parse_input_from_name(cache, "input".to_string(), &node.inputs)?;
    let squared = input * input;
    insert_value(cache, &node.id, &node.outputs[0].name, squared)?;
    insert_value(cache, &node.id, "input_terms", format!("{}", input))?;
    insert_value(cache, &node.id, "output_squared", squared)?;
    Ok(())
}

// Triples a number and adds a string output.
#[action(description = "Triples a number and adds a string")]
async fn triple_number_and_add_string(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache,
) -> Result<()> {
    let input: f64 = parse_input_from_name(cache, "input".to_string(), &node.inputs)?;
    let tripled = input * 3.0;
    insert_value(cache, &node.id, &node.outputs[0].name, tripled)?;
    insert_value(cache, &node.id, &node.outputs[1].name, "example_string".to_string())?;
    insert_value(cache, &node.id, "input_terms", format!("{}", input))?;
    insert_value(cache, &node.id, "output_tripled", tripled)?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let registry = Arc::new(RwLock::new(HashMap::new()));
    let mut executor = DagExecutor::new(None, registry.clone(), "dagger_db")?;

    // Register actions using uppercase static names from the macro
    executor.register_action(Arc::new(ADD_NUMBERS.clone()));
    executor.register_action(Arc::new(SQUARE_NUMBER.clone()));
    executor.register_action(Arc::new(TRIPLE_NUMBER_AND_ADD_STRING.clone()));

    executor
        .load_yaml_file("pipeline.yaml")
        .map_err(|e| anyhow::anyhow!("Failed to load pipeline.yaml: {}", e))?;

    let dag_names = executor.list_dags()?;
    println!("Loaded DAGs: {:#?}", dag_names);

    let cache = Cache::new(HashMap::new());
    insert_value(&cache, "inputs", "num1", 10.0)?;
    insert_value(&cache, "inputs", "num2", 20.0)?;

    let (cancel_tx, cancel_rx) = oneshot::channel();
    tokio::spawn(async move {
        sleep(Duration::from_secs(2)).await;
        let _ = cancel_tx.send(());
        println!("Cancellation signal sent");
    });

    let dag_report = executor
        .execute_dag(
            WorkflowSpec::Static {
                name: "infolder".to_string(),
            },
            &cache,
            cancel_rx,
        )
        .await?;

    let json_output = serialize_cache_to_prettyjson(&cache)?;
    println!("Cache as JSON:\n{}", json_output);
    println!("DAG Execution Report: {:#?}", dag_report);

    let dot_output = executor.serialize_tree_to_dot("infolder")?;
    println!("Execution Tree (DOT):\n{}", dot_output);

    Ok(())
}

### Key Differences:
The macro adds a JSON schema to each action, which is useful for introspection or LLM integration but not required for YAML execution.
Registration uses uppercase static names (e.g., ADD_NUMBERS) instead of register_action!.

### Option 2: Agent-Driven Flow Execution
Overview: Define a supervisor action that dynamically adds nodes to the DAG at runtime, executed using WorkflowSpec::Agent. This is ideal for adaptive workflows where the flow isn't fully predefined.

#### Steps
1. Define Actions: Write async functions, including a supervisor that orchestrates the flow
2. Register Actions: Use register_action! or #[action] to register them
3. Execute: Run with WorkflowSpec::Agent, letting the supervisor build the DAG

#### Without dagger-macros
```rust
use anyhow::Result;
use dagger::{
    append_global_value, generate_node_id, get_global_input, insert_global_value, insert_value,
    register_action, serialize_cache_to_prettyjson, Cache, DagExecutor, Node, NodeAction,
    WorkflowSpec,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::oneshot;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

async fn google_search(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    println!("Running Google Search Node: {}", node.id);
    let terms: Vec<String> = get_global_input(cache, "analyze", "search_terms").unwrap_or(vec![]);
    insert_value(cache, &node.id, "input_terms", &terms)?;
    let query = terms.get(0).cloned().unwrap_or("AI trends".to_string());
    let results = vec![format!("Google result for '{}'", query)];
    insert_value(cache, &node.id, "output_results", &results)?;
    Ok(())
}

async fn twitter_search(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    println!("Running Twitter Search Node: {}", node.id);
    let terms: Vec<String> = get_global_input(cache, "analyze", "search_terms").unwrap_or(vec![]);
    insert_value(cache, &node.id, "input_terms", &terms)?;
    let query = terms.get(0).cloned().unwrap_or("AI trends".to_string());
    let results = vec![format!("Tweet about '{}'", query)];
    insert_value(cache, &node.id, "output_results", &results)?;
    Ok(())
}

async fn review(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    println!("Running Review Node: {}", node.id);
    let google_results: Vec<String> = get_global_input(cache, "analyze", "google_results").unwrap_or(vec![]);
    let twitter_results: Vec<String> = get_global_input(cache, "analyze", "twitter_results").unwrap_or(vec![]);
    insert_value(cache, &node.id, "input_google_results", &google_results)?;
    insert_value(cache, &node.id, "input_twitter_results", &twitter_results)?;
    let all_results = [google_results, twitter_results].concat();
    let summary = format!("Summary of {} items: {}", all_results.len(), all_results.join("; "));
    insert_value(cache, &node.id, "output_summary", summary)?;
    Ok(())
}

async fn supervisor_step(executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let dag_name = "analyze";
    println!("Supervisor Node: {}, DAG: {}", node.id, dag_name);
    let iteration: usize = get_global_input(cache, dag_name, "supervisor_iteration").unwrap_or(0);
    let next_iteration = iteration + 1;

    match iteration {
        0 => {
            let google_id = generate_node_id("google_search");
            executor.add_node(dag_name, google_id.clone(), "google_search".to_string(), vec![node.id.clone()])?;
            let twitter_id = generate_node_id("twitter_search");
            executor.add_node(dag_name, twitter_id.clone(), "twitter_search".to_string(), vec![node.id.clone()])?;
            insert_global_value(cache, dag_name, "search_terms", vec!["AI trends"])?;
            let next_supervisor = generate_node_id("supervisor_step");
            executor.add_node(dag_name, next_supervisor, "supervisor_step".to_string(), vec![google_id, twitter_id])?;
        }
        1 => {
            let review_id = generate_node_id("review");
            executor.add_node(dag_name, review_id.clone(), "review".to_string(), vec![node.id.clone()])?;
            let next_supervisor = generate_node_id("supervisor_step");
            executor.add_node(dag_name, next_supervisor, "supervisor_step".to_string(), vec![review_id])?;
        }
        2 => {
            println!("Supervisor finished after collecting and reviewing data");
            *executor.stopped.write().unwrap() = true;
        }
        _ => unreachable!("Unexpected iteration"),
    }

    insert_global_value(cache, dag_name, "supervisor_iteration", next_iteration)?;
    insert_global_value(cache, dag_name, &format!("output_next_iteration_{}", node.id), next_iteration)?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let subscriber = FmtSubscriber::builder().with_max_level(Level::INFO).finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
    let registry = Arc::new(RwLock::new(HashMap::new()));
    let mut executor = DagExecutor::new(None, registry.clone(), "dagger_db")?;

    register_action!(executor, "google_search", google_search);
    register_action!(executor, "twitter_search", twitter_search);
    register_action!(executor, "review", review);
    register_action!(executor, "supervisor_step", supervisor_step);

    let cache = Cache::new(HashMap::new());
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    let report = executor
        .execute_dag(WorkflowSpec::Agent { task: "analyze".to_string() }, &cache, cancel_rx)
        .await?;

    let json_output = serialize_cache_to_prettyjson(&cache)?;
    println!("Final Cache:\n{}", json_output);
    println!("Execution Report: {:#?}", report);
    let dot_output = executor.serialize_tree_to_dot("analyze")?;
    println!("Execution Tree (DOT):\n{}", dot_output);

    Ok(())
}

### Key Differences:
The supervisor dynamically adds nodes using add_node, building the DAG at runtime.
WorkflowSpec::Agent starts with an initial task ("analyze"), and the supervisor dictates the flow.

### Option 3: Task Agent System
Overview: The Task Agent System provides a more sophisticated approach to task orchestration with features like job persistence, task dependencies, and agent-specific task execution. This system is ideal for complex workflows where tasks need to be claimed, executed, and tracked by specific agents.

#### Steps
1. Define Task Agents: Create structs that implement the required methods for task execution
2. Use the `task_agent` macro: Apply the macro to your implementation to generate the TaskAgent trait
3. Create Tasks: Use the `task_builder` macro to generate builder patterns for task creation
4. Execute: Run tasks through the TaskManager, which handles dependencies and state management

#### Task Agent Implementation
```rust
use dagger::taskagent::{TaskManager, Task};
use dagger_macros::task_agent;
use serde_json::Value;

struct MyTaskAgent;

impl MyTaskAgent {
    // Required methods that will be checked by the macro
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "data": {"type": "string"}
            }
        })
    }
    
    fn output_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "result": {"type": "string"}
            }
        })
    }
    
    async fn execute(&self, input: serde_json::Value, task_id: &str, job_id: &str) -> Result<serde_json::Value, String> {
        // Process the task
        let data = input["data"].as_str().unwrap_or("default");
        let result = format!("Processed: {}", data);
        
        Ok(serde_json::json!({
            "result": result
        }))
    }
}

// Apply the task_agent macro to generate the TaskAgent trait implementation
#[task_agent(name = "my_task_agent", description = "A simple task agent")]
impl MyTaskAgent {
    // The implementation block where the required methods are defined
}
```

#### Task Builder Implementation
```rust
use dagger::taskagent::TaskManager;
use dagger_macros::task_builder;

struct MyTaskBuilder;

// Apply the task_builder macro to generate the TaskBuilder struct and methods
#[task_builder(agent = "my_task_agent")]
impl MyTaskBuilder {
    // The implementation can be empty as the macro generates all needed methods
}

// Usage example
fn create_task(task_manager: &TaskManager, job_id: &str) -> String {
    let builder = MyTaskBuilder{};
    
    // Create and configure a task
    builder.create_task(job_id)
        .with_description("Process some data")
        .with_input(serde_json::json!({"data": "sample input"}))
        .with_timeout(60) // 60 seconds timeout
        .with_max_retries(3)
        .build(task_manager)
}
```

#### TaskManager Usage
```rust
use dagger::taskagent::{TaskManager, TaskAgentRegistry};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a registry and register your agents
    let registry = TaskAgentRegistry::new();
    registry.register(Arc::new(MyTaskAgent{}));
    
    // Create a task manager with the registry
    let task_manager = TaskManager::new(registry, "task_db")?;
    
    // Create a new job
    let job_id = task_manager.create_job()?;
    
    // Create tasks for the job
    let task_id = create_task(&task_manager, &job_id);
    
    // Start the job
    task_manager.start_job(&job_id)?;
    
    // Process tasks (in a real application, this would be done by agent workers)
    loop {
        // Get tasks that are ready for the agent
        let agent = MyTaskAgent{};
        let ready_tasks = agent.get_ready_tasks(&task_manager);
        
        for task in ready_tasks {
            // Claim the task
            if agent.claim_task(&task_manager, &task.id) {
                // Execute the task
                let result = agent.execute(task.input.clone(), &task.id, &task.job_id).await;
                
                // Complete the task
                agent.complete_task(&task_manager, &task.id, result);
            }
        }
        
        // Check if job is complete
        if task_manager.is_job_complete(&job_id)? {
            break;
        }
        
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    
    // Get the job results
    let results = task_manager.get_job_results(&job_id)?;
    println!("Job results: {:?}", results);
    
    // Generate a DOT graph of the task execution
    let dot_graph = task_manager.get_dot_graph(Some(&job_id))?;
    println!("Task execution graph:\n{}", dot_graph);
    
    Ok(())
}
```

#### Key Features of the Task Agent System
- **Persistence**: Job and task states are persisted using sled database
- **Task Dependencies**: Tasks can depend on other tasks, creating a DAG
- **Agent-Specific Tasks**: Tasks are assigned to specific agent types
- **Task Claiming**: Agents claim tasks to prevent duplicate execution
- **Retry Mechanism**: Failed tasks can be retried automatically
- **Timeout Handling**: Tasks can have timeouts to prevent hanging
- **Visualization**: Generate DOT graphs of task execution for analysis
- **Caching**: Store intermediate results in a thread-safe cache

#### Advanced Usage: Job Resumption
The Task Agent System supports resuming jobs that were interrupted:

```rust
// Resume a previously started job
let job_id = "previous_job_id";
task_manager.resume_job(job_id)?;

// Continue processing tasks as before
```

This is particularly useful for long-running workflows that need to survive process restarts or for implementing checkpointing in distributed systems.

## Dagger Macros (Optional)
The dagger-macros crate provides several macros to simplify working with Dagger:

### Available Macros

#### 1. `#[action]` - Define DAG Actions
The `#[action]` macro simplifies the creation of actions for YAML-based and agent-driven flows.

```rust
use dagger_macros::action;

#[action(description = "Adds two numbers", retry_count = 3, timeout = 60)]
async fn add_numbers(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    // Implementation
    Ok(())
}

// Registration
executor.register_action(Arc::new(ADD_NUMBERS.clone()));
```

**Parameters:**
- `description`: String describing the action (required)
- `retry_count`: Number of retry attempts (optional)
- `timeout`: Timeout in seconds (optional)

**Generated:**
- A static `ADD_NUMBERS` constant implementing `NodeAction`
- JSON schema generation based on function signature
- Metadata for introspection

#### 2. `#[task_agent]` - Define Task Agents
The `#[task_agent]` macro generates a `TaskAgent` trait implementation for a struct.

```rust
use dagger_macros::task_agent;

struct MyAgent;

impl MyAgent {
    fn input_schema(&self) -> serde_json::Value {
        // Schema definition
        serde_json::json!({})
    }
    
    fn output_schema(&self) -> serde_json::Value {
        // Schema definition
        serde_json::json!({})
    }
    
    async fn execute(&self, input: serde_json::Value, task_id: &str, job_id: &str) 
        -> Result<serde_json::Value, String> {
        // Implementation
        Ok(serde_json::json!({}))
    }
}

#[task_agent(name = "my_agent", description = "Performs a specific task")]
impl MyAgent {
    // Implementation block with required methods
}
```

**Parameters:**
- `name`: String identifier for the agent (required)
- `description`: String describing the agent (optional)

**Requirements:**
The struct must implement:
- `input_schema()` - Returns JSON schema for inputs
- `output_schema()` - Returns JSON schema for outputs
- `execute()` - Async function that performs the task

**Generated:**
- `TaskAgent` trait implementation with helper methods
- Integration with TaskManager for task claiming and completion

#### 3. `#[task_builder]` - Create Task Builder
The `#[task_builder]` macro generates a builder pattern for creating tasks for a specific agent.

```rust
use dagger_macros::task_builder;

struct MyTaskBuilder;

#[task_builder(agent = "my_agent")]
impl MyTaskBuilder {
    // Empty implementation block
}

// Usage
let builder = MyTaskBuilder{};
let task_id = builder.create_task("job_123")
    .with_description("Process data")
    .with_input(serde_json::json!({"key": "value"}))
    .with_timeout(60)
    .build(&task_manager);
```

**Parameters:**
- `agent`: String name of the agent that will execute tasks (required)

**Generated:**
- `TaskBuilder` struct with fluent builder methods
- `create_task()` method on the original struct
- Methods for configuring task properties

#### 4. `#[task_workflow]` - Define Task Workflows
The `#[task_workflow]` macro helps define reusable task workflow templates.

```rust
use dagger_macros::task_workflow;

#[task_workflow(name = "data_processing", description = "Process and analyze data")]
struct DataProcessingWorkflow {
    input_path: String,
    output_path: String,
}

impl DataProcessingWorkflow {
    // Methods to create and configure the workflow
    pub fn create_tasks(&self, task_manager: &TaskManager, job_id: &str) -> Result<(), String> {
        // Create and connect tasks
        // Return the entry point task ID
        Ok(())
    }
}
```

**Parameters:**
- `name`: String identifier for the workflow (required)
- `description`: String describing the workflow (optional)

**Generated:**
- Metadata for the workflow
- Helper methods for workflow creation and management

### Helper Functions

#### Cache Manipulation

```rust
// Insert a value into the cache
insert_value(cache, &node.id, "output_key", value)?;

// Parse input from a named reference
let input: f64 = parse_input_from_name(cache, "input_name", &node.inputs)?;

// Get a global input value
let terms: Vec<String> = get_global_input(cache, "dag_name", "input_key").unwrap_or_default();

// Insert a global value
insert_global_value(cache, "dag_name", "output_key", value)?;

// Append to a global value (for arrays)
append_global_value(cache, "dag_name", "array_key", new_item)?;

// Serialize cache to JSON
let json = serialize_cache_to_prettyjson(&cache)?;
```

#### Node Management

```rust
// Generate a unique node ID with a prefix
let node_id = generate_node_id("prefix");

// Add a node to a DAG dynamically
executor.add_node(
    "dag_name",
    node_id.clone(),
    "action_name",
    vec!["dependency1", "dependency2"]
)?;
```

#### Visualization

```rust
// Generate DOT graph for a DAG
let dot = executor.serialize_tree_to_dot("dag_name")?;

// Generate detailed DOT graph with task execution data
let detailed_dot = task_manager.get_dot_graph(Some("job_id"))?;
```

### When to Use Each Macro

- **#[action]**: Use for defining actions in YAML-based or agent-driven flows when you want schema generation and metadata.
- **register_action!**: Use for simpler action registration without schema generation.
- **#[task_agent]**: Use when implementing agents for the Task Agent System.
- **#[task_builder]**: Use to create builder patterns for task creation.
- **#[task_workflow]**: Use to define reusable workflow templates.

### Benefits of Using Macros

1. **Reduced Boilerplate**: Eliminates repetitive code for trait implementations
2. **Schema Generation**: Automatic JSON schema creation for inputs and outputs
3. **Type Safety**: Compile-time checks for required methods and parameters
4. **Consistency**: Enforces consistent patterns across your codebase
5. **Introspection**: Adds metadata for debugging and visualization

### Trade-offs

1. **Complexity**: Macros can make code harder to understand for newcomers
2. **Debugging**: Macro-generated code can be harder to debug
3. **Flexibility**: Less control over implementation details

Choose the approach that best fits your project's needs and your team's familiarity with Rust macros.

## Key Concepts
- DagExecutor: The core engine managing action registration, DAG loading, and execution
- NodeAction: Trait for actions (`name()`, `execute()`, `schema()`)
- Cache: A shared RwLock<HashMap> storing inputs and outputs
- WorkflowSpec: Enum specifying execution mode (`Static` for YAML, `Agent` for dynamic flows)
- Cancellation: Supports cancellation via a oneshot::Receiver
- Error Handling: Configurable retries, timeouts, and failure behaviors

## Examples
See the [Usage](#usage) section above for complete code snippets.

## Contributing
Contributions are welcome! Please:
1. Fork the repository
2. Create a feature branch
3. Submit a pull request with clear descriptions and tests

Issues can be reported on the GitHub issue tracker.

## License
Dagger is licensed under the MIT License. See LICENSE for details.

## Simplified Usage

Dagger now provides a simplified API through `TaskSystemBuilder` that makes it much easier to get started:

```rust
use dagger::taskagent::TaskSystemBuilder;
use serde_json::json;

// Create a task system with one line
let mut task_system = TaskSystemBuilder::new()
    .register_agent("my_agent")?
    .build()?;

// Run a simple objective
let result = task_system.run_objective(
    "My task description",
    "my_agent",
    json!({"key": "value"}),
).await?;

println!("Task completed with result: {}", result);
```

### Defining Agents

Agents are defined using the `task_agent` macro:

```rust
use dagger::task_agent;
use serde_json::{json, Value};

#[task_agent(
    name = "my_agent", 
    description = "Performs a specific task",
    input_schema = r#"{"type": "object", "properties": {"key": {"type": "string"}}, "required": ["key"]}"#,
    output_schema = r#"{"type": "object", "properties": {"result": {"type": "string"}}, "required": ["result"]}"#
)]
async fn my_agent(input: Value, task_id: &str, job_id: &str) -> Result<Value, String> {
    // Extract input
    let value = input["key"].as_str().unwrap_or("default");
    
    // Process and return
    Ok(json!({"result": format!("Processed: {}", value)}))
}
```

### Multi-Agent Workflows

You can also create multi-agent workflows with dependencies:

```rust
// Create system with multiple agents
let mut task_system = TaskSystemBuilder::new()
    .register_agents(&["agent1", "agent2", "agent3"])?
    .build()?;

// Create tasks with dependencies
let task1_id = task_system.add_task(
    "First task".to_string(),
    "agent1".to_string(),
    vec![], // No dependencies
    json!({"input": "value"}),
)?;

let task2_id = task_system.add_task(
    "Second task".to_string(),
    "agent2".to_string(),
    vec![task1_id.clone()], // Depends on task1
    json!({"from_task1": "Will be populated automatically"}),
)?;
```

See the examples directory for complete working examples.