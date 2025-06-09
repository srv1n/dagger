use anyhow::{Error, Result};
use dagger::{
    insert_value, parse_input_from_name, register_action, serialize_cache_to_prettyjson, 
    DagExecutionReport, DagExecutor, Node, WorkflowSpec, NodeAction,
};
use dagger::dag_flow::Cache;
use std::collections::HashMap;
use tokio::sync::oneshot;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use std::sync::{Arc, RwLock};
use tokio::time::{sleep, Duration};

// Performs addition of two f64 numbers.
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
async fn square_number(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let input: f64 = parse_input_from_name(cache, "input".to_string(), &node.inputs)?;
    let squared = input * input;
    insert_value(cache, &node.id, &node.outputs[0].name, squared)?;
    insert_value(cache, &node.id, "input_terms", format!("{}", input))?;
    insert_value(cache, &node.id, "output_squared", squared)?;
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

    // Register the actions using register_action!
    register_action!(executor, "add_numbers", add_numbers);
    register_action!(executor, "square_number", square_number);
    register_action!(executor, "triple_number_and_add_string", triple_number_and_add_string);

    executor
        .load_yaml_file("pipeline.yaml")
        .map_err(|e| anyhow::anyhow!("Failed to load pipeline.yaml: {}", e))?;

    let dag_names = executor.list_dags()?;
    println!("Loaded DAGs: {:#?}", dag_names);

    let cache = Cache::new();
    insert_value(&cache, "inputs", "num1", 10.0)?;
    insert_value(&cache, "inputs", "num2", 20.0)?;

    let (cancel_tx, cancel_rx) = oneshot::channel();
    tokio::spawn(async move {
        sleep(Duration::from_secs(2)).await;
        let _ = cancel_tx.send(());
        println!("Cancellation signal sent");
    });

    let dag_report = run_dag(
        &mut executor,
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

async fn run_dag(
    executor: &mut DagExecutor,
    spec: WorkflowSpec,
    cache: &Cache,
    cancel_rx: oneshot::Receiver<()>,
) -> Result<DagExecutionReport, Error> {
    let report = executor.execute_dag(spec, cache, cancel_rx).await?;
    Ok(report)
}