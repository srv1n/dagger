use anyhow::{Error, Result};
use dagger::{
    insert_value, parse_input_from_name, register_action, serialize_cache_to_prettyjson, Cache,
    DagExecutionReport, DagExecutor, Node, WorkflowSpec,
};
use std::collections::HashMap;
use tokio::sync::oneshot;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use dagger::NodeAction;
use std::sync::Arc;
use tokio::time::{sleep, Duration};

// Performs addition of two f64 numbers.
async fn add_numbers(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    println!("node: {:#?}", node);
    println!("cache: {:#?}", cache);
    let num1: f64 = parse_input_from_name(cache, "num1".to_string(), &node.inputs)?;
    let num2: f64 = parse_input_from_name(cache, "num2".to_string(), &node.inputs)?;

    let sum = num1 + num2;

    insert_value(cache, &node.id, &node.outputs[0].name, sum)?;
    Ok(())
}

// Squares a number.
async fn square_number(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let input: f64 = parse_input_from_name(cache, "input".to_string(), &node.inputs)?;

    let squared = input * input;

    insert_value(cache, &node.id, &node.outputs[0].name, squared)?;
    Ok(())
}

// Triples a number and adds a string output.
async fn triple_number_and_add_string(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache,
) -> Result<()> {
    let input: f64 = parse_input_from_name(cache, "input".to_string(), &node.inputs)?;

    let tripled = input * 3.0;

    insert_value(cache, &node.id, &node.outputs[0].name, tripled)?;
    insert_value(cache, &node.id, &node.outputs[1].name, "example_string".to_string())?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup tracing subscriber for logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO) // Adjusted to INFO for cleaner output
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    // Initialize the DAG executor with default config
    let mut executor = DagExecutor::new(None)?;

    // Register the actions with the executor
    register_action!(executor, "add_numbers", add_numbers);
    register_action!(executor, "square_number", square_number);
    register_action!(
        executor,
        "triple_number_and_add_string",
        triple_number_and_add_string
    );

    // Load DAG definition from YAML file with error handling
    executor
        .load_yaml_file("pipeline.yaml")
        .map_err(|e| anyhow::anyhow!("Failed to load pipeline.yaml: {}", e))?;

    // List loaded DAGs
    let dag_names = executor.list_dags()?;
    println!("Loaded DAGs: {:#?}", dag_names);

    // Initialize the cache with initial input values
    let cache = Cache::new(HashMap::new());
    insert_value(&cache, "inputs", "num1", 10.0)?;
    insert_value(&cache, "inputs", "num2", 20.0)?;

    // Create a oneshot channel for cancellation and demonstrate cancellation
    let (cancel_tx, cancel_rx) = oneshot::channel();

    // Spawn a task to cancel after 2 seconds (for demonstration)
    tokio::spawn(async move {
        sleep(Duration::from_secs(2)).await;
        let _ = cancel_tx.send(());
        println!("Cancellation signal sent");
    });

    // Execute the DAG
    let dag_report = run_dag(
        &mut executor,
        WorkflowSpec::Static {
            name: "infolder".to_string(),
        },
        &cache,
        cancel_rx,
    )
    .await?;

    // Serialize and print the cache
    let json_output = serialize_cache_to_prettyjson(&cache)?;
    println!("Cache as JSON:\n{}", json_output);
    println!("DAG Execution Report: {:#?}", dag_report);

    Ok(())
}

// Executes a specified DAG and returns the execution report.
async fn run_dag(
    executor: &mut DagExecutor,
    spec: WorkflowSpec,
    cache: &Cache,
    cancel_rx: oneshot::Receiver<()>,
) -> Result<DagExecutionReport, Error> {
    let report = executor.execute_dag(spec, cache, cancel_rx).await?;
    Ok(report)
}