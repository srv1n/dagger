use anyhow::{Error, Result};
use dagger::{
    insert_value, parse_input_from_name, register_action, serialize_cache_to_prettyjson, 
    DagExecutionReport, DagExecutor, Node, NodeAction, ExecutionContext,
};
use dagger::dag_flow::Cache;
use std::collections::HashMap;
use tokio::sync::{oneshot, Semaphore};
use tokio::time::{sleep, Duration, Instant};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use std::sync::Arc;
use tokio::sync::RwLock;

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
    // Add a small delay to simulate work
    sleep(Duration::from_millis(100)).await;
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

    // Create config with parallel execution disabled for sequential comparison
    let mut config_sequential = dagger::DagConfig::default();
    config_sequential.enable_parallel_execution = false;
    
    // Create config with parallel execution explicitly enabled (though it's now the default)
    let mut config_parallel = dagger::DagConfig::default();
    config_parallel.enable_parallel_execution = true;
    config_parallel.max_parallel_nodes = 4; // Override default of 3 for this test
    
    let registry = Arc::new(RwLock::new(HashMap::new()));
    let mut executor = DagExecutor::new(Some(config_sequential.clone()), registry.clone(), "sqlite::memory:").await?;

    // Register the actions using register_action!
    register_action!(executor, "add_numbers", add_numbers).await?;
    register_action!(executor, "square_number", square_number).await?;
    register_action!(executor, "triple_number_and_add_string", triple_number_and_add_string).await?;

    // Load both pipelines
    executor
        .load_yaml_file("pipeline.yaml")
        .await
        .map_err(|e| anyhow::anyhow!("Failed to load pipeline.yaml: {}", e))?;
    executor
        .load_yaml_file("pipeline_parallel.yaml")
        .await
        .map_err(|e| anyhow::anyhow!("Failed to load pipeline_parallel.yaml: {}", e))?;

    let dag_names = executor.list_dags().await?;
    println!("Loaded DAGs: {:#?}", dag_names);

    let cache = Cache::new();
    insert_value(&cache, "inputs", "num1", 10.0)?;
    insert_value(&cache, "inputs", "num2", 20.0)?;

    // First run with sequential execution for comparison
    println!("\n=== Executing pipeline with SEQUENTIAL execution ===");
    executor.config = config_sequential;
    executor.execution_context = None; // Reset context
    let (_cancel_tx1, cancel_rx1) = oneshot::channel();
    let start_time_seq = std::time::Instant::now();
    let _dag_report_seq = executor.execute_static_dag("parallel_demo", &cache, cancel_rx1).await?;
    let elapsed_seq = start_time_seq.elapsed();
    println!("Sequential execution completed in: {:?}", elapsed_seq);
    
    // Clear cache for fair comparison
    cache.data.clear();
    insert_value(&cache, "inputs", "num1", 10.0)?;
    insert_value(&cache, "inputs", "num2", 20.0)?;
    
    // Now run with parallel execution
    println!("\n=== Executing pipeline with PARALLEL execution ===");
    executor.config = config_parallel.clone();
    executor.execution_context = Some(ExecutionContext {
        max_parallel_nodes: config_parallel.max_parallel_nodes,
        semaphore: Arc::new(Semaphore::new(config_parallel.max_parallel_nodes)),
        cache_last_snapshot: Arc::new(RwLock::new(Instant::now())),
        cache_delta_size: Arc::new(RwLock::new(0)),
    });
    let (_cancel_tx2, cancel_rx2) = oneshot::channel();
    let start_time_par = std::time::Instant::now();
    let dag_report = executor.execute_static_dag("parallel_demo", &cache, cancel_rx2).await?;
    let elapsed_par = start_time_par.elapsed();
    println!("Parallel execution completed in: {:?}", elapsed_par);
    
    println!("\n=== Performance Comparison ===");
    println!("Sequential: {:?}", elapsed_seq);
    println!("Parallel:   {:?}", elapsed_par);
    if elapsed_seq > elapsed_par {
        let speedup = elapsed_seq.as_secs_f64() / elapsed_par.as_secs_f64();
        println!("Speedup:    {:.2}x faster with parallel execution", speedup);
    }

    let json_output = serialize_cache_to_prettyjson(&cache)?;
    println!("Cache as JSON:\n{}", json_output);
    println!("DAG Execution Report: {:#?}", dag_report);

    let dot_output = executor.serialize_tree_to_dot("parallel_demo").await?;
    println!("Execution Tree (DOT):\n{}", dot_output);

    Ok(())
}

// This helper function is no longer needed with the simplified API
// async fn run_dag(
//     executor: &mut DagExecutor,
//     spec: WorkflowSpec,
//     cache: &Cache,
//     cancel_rx: oneshot::Receiver<()>,
// ) -> Result<DagExecutionReport, Error> {
//     let report = executor.execute_dag(spec, cache, cancel_rx).await?;
//     Ok(report)
// }