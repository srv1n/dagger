# Dagger: A Rust Library for Executing Directed Acyclic Graphs (DAGs)

Dagger is a lightweight, flexible Rust library designed for executing directed acyclic graphs (DAGs) with custom actions. It supports both predefined workflows loaded from YAML files and dynamic, agent-driven flows orchestrated at runtime. Whether you're automating data pipelines, orchestrating complex computations, or building adaptive workflows with human-in-the-loop capabilities, Dagger provides a robust foundation for task execution with minimal overhead.

## Table of Contents
- [What is Dagger?](#what-is-dagger)
- [Features](#features)
- [Installation](#installation)
- [Usage](#usage)
  - [Option 1: YAML-Based DAG Execution](#option-1-yaml-based-dag-execution)
  - [Option 2: Agent-Driven Flow Execution](#option-2-agent-driven-flow-execution)
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
Dagger supports two primary execution modes: YAML-based static DAGs and agent-driven dynamic flows. Both methods rely on registering actions with a DagExecutor, but they differ in how the DAG is defined and executed.

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

## Dagger Macros (Optional)
The dagger-macros crate provides an optional #[action] macro to simplify action definition:

### Purpose
Automatically generates NodeAction implementations with JSON schemas.

### Usage
Annotate async functions with #[action(description = "...")], register with Arc::new(UPPERCASE_NAME.clone()).

### Benefits
- Adds metadata (e.g., schema) for introspection or LLM integration
- Reduces boilerplate

### Trade-off
Slightly more complex registration syntax (UPPERCASE_NAME.clone() vs. register_action!)

### When to Use
- Use #[action] for agent-driven flows where schema metadata is valuable
- Skip it for YAML flows if schema isn't needed

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