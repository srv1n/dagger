use anyhow::Result;
use dagger::{
    insert_value, parse_input_from_name, register_action, serialize_cache_to_prettyjson,
    DagExecutor, Node,
};
use dagger::dag_flow::{Cache, ExecutionContext};
use dagger::coord::ActionRegistry;
use serde_json::json;
use std::env;
use std::sync::Arc;
use tokio::sync::{oneshot, RwLock, Semaphore};
use tokio::time::{sleep, Duration, Instant};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

// ========== Action Functions ==========

/// Adds two numbers together
async fn add_numbers(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let num1: f64 = parse_input_from_name(cache, "num1".to_string(), &node.inputs)?;
    let num2: f64 = parse_input_from_name(cache, "num2".to_string(), &node.inputs)?;
    let sum = num1 + num2;
    
    // Store outputs
    insert_value(cache, &node.id, &node.outputs[0].name, sum)?;
    insert_value(cache, &node.id, "input_terms", format!("{} + {}", num1, num2))?;
    insert_value(cache, &node.id, "output_sum", sum)?;
    
    println!("  [{}] Computing: {} + {} = {}", node.id, num1, num2, sum);
    Ok(())
}

/// Squares a number
async fn square_number(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    // Simulate some processing time
    sleep(Duration::from_millis(100)).await;
    
    let input: f64 = parse_input_from_name(cache, "input".to_string(), &node.inputs)?;
    let squared = input * input;
    
    // Store outputs
    insert_value(cache, &node.id, &node.outputs[0].name, squared)?;
    insert_value(cache, &node.id, "input_terms", format!("{}", input))?;
    insert_value(cache, &node.id, "output_squared", squared)?;
    
    println!("  [{}] Computing: {}² = {}", node.id, input, squared);
    Ok(())
}

/// Triples a number and adds a string output
async fn triple_number_and_add_string(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache,
) -> Result<()> {
    let input: f64 = parse_input_from_name(cache, "input".to_string(), &node.inputs)?;
    let tripled = input * 3.0;
    
    // Store outputs
    insert_value(cache, &node.id, &node.outputs[0].name, tripled)?;
    insert_value(cache, &node.id, &node.outputs[1].name, "example_string".to_string())?;
    insert_value(cache, &node.id, "input_terms", format!("{}", input))?;
    insert_value(cache, &node.id, "output_tripled", tripled)?;
    
    println!("  [{}] Computing: {} × 3 = {}", node.id, input, tripled);
    Ok(())
}

/// Prints a message (for demonstration)
async fn print_message(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let message = if node.inputs.is_empty() {
        "No input provided".to_string()
    } else {
        parse_input_from_name(cache, "message".to_string(), &node.inputs)
            .unwrap_or_else(|_| "Default message".to_string())
    };
    
    insert_value(cache, &node.id, "output", message.clone())?;
    println!("  [{}] Message: {}", node.id, message);
    Ok(())
}

// ========== Helper Functions ==========

fn print_help() {
    println!("\nUsage: cargo run -- [OPTIONS]\n");
    println!("Options:");
    println!("  --help                Show this help message");
    println!("  --list                List all available DAGs");
    println!("  --dag <name>          Execute a specific DAG (default: all DAGs)");
    println!("  --parallel            Use parallel execution (default)");
    println!("  --sequential          Use sequential execution");
    println!("  --input <key=value>   Set input values (can be used multiple times)");
    println!("  --verbose             Enable verbose logging");
    println!("\nExamples:");
    println!("  cargo run -- --list");
    println!("  cargo run -- --dag parallel_demo --input num1=10 --input num2=20");
    println!("  cargo run -- --dag infolder --sequential");
}

fn parse_args() -> (Option<String>, bool, Vec<(String, f64)>, bool) {
    let args: Vec<String> = env::args().collect();
    let mut dag_filter = None;
    let mut use_parallel = true;
    let mut inputs = Vec::new();
    let mut verbose = false;
    
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--list" | "-l" => {
                return (Some("__LIST__".to_string()), use_parallel, inputs, verbose);
            }
            "--dag" | "-d" => {
                if i + 1 < args.len() {
                    dag_filter = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--parallel" | "-p" => {
                use_parallel = true;
            }
            "--sequential" | "-s" => {
                use_parallel = false;
            }
            "--input" | "-i" => {
                if i + 1 < args.len() {
                    if let Some((key, val)) = args[i + 1].split_once('=') {
                        if let Ok(num) = val.parse::<f64>() {
                            inputs.push((key.to_string(), num));
                        }
                    }
                    i += 1;
                }
            }
            "--verbose" | "-v" => {
                verbose = true;
            }
            _ => {}
        }
        i += 1;
    }
    
    (dag_filter, use_parallel, inputs, verbose)
}

// ========== Main Function ==========

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse command-line arguments
    let (dag_filter, use_parallel, custom_inputs, verbose) = parse_args();
    
    // Set up logging
    let log_level = if verbose { Level::DEBUG } else { Level::INFO };
    let subscriber = FmtSubscriber::builder()
        .with_max_level(log_level)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    
    println!("\n=== DAG Flow Execution Demo ===\n");
    
    // Create configuration
    let config = dagger::DagConfig {
        enable_parallel_execution: use_parallel,
        max_parallel_nodes: 4,
        enable_incremental_cache: true,
        cache_snapshot_interval: 30,
        ..Default::default()
    };
    
    println!("Configuration:");
    println!("  Execution mode: {}", if use_parallel { "Parallel" } else { "Sequential" });
    println!("  Max parallel nodes: {}", config.max_parallel_nodes);
    println!();
    
    // Create action registry
    let registry = ActionRegistry::new();
    
    // Initialize executor
    let mut executor = DagExecutor::new(
        Some(config.clone()),
        registry.clone(),
        "sqlite::memory:"
    ).await?;
    
    // Register actions
    println!("Registering actions...");
    register_action!(executor, "add_numbers", add_numbers).await?;
    register_action!(executor, "square_number", square_number).await?;
    register_action!(executor, "triple_number_and_add_string", triple_number_and_add_string).await?;
    register_action!(executor, "print_message", print_message).await?;
    println!("  ✓ Registered 4 actions\n");
    
    // Load YAML files
    println!("Loading DAG definitions...");
    let yaml_files = vec!["pipeline.yaml", "pipeline_parallel.yaml"];
    for file in &yaml_files {
        match executor.load_yaml_file(file).await {
            Ok(_) => println!("  ✓ Loaded: {}", file),
            Err(e) => println!("  ✗ Failed to load {}: {}", file, e),
        }
    }
    println!();
    
    // List available DAGs
    let dag_names = executor.list_dags().await?;
    
    // Handle --list option
    if dag_filter.as_deref() == Some("__LIST__") {
        println!("Available DAGs:");
        for (name, description) in &dag_names {
            println!("  • {} - {}", name, description);
        }
        return Ok(());
    }
    
    // Filter DAGs based on command-line argument
    let dags_to_run: Vec<_> = if let Some(filter) = &dag_filter {
        dag_names.iter()
            .filter(|(name, _)| name == filter)
            .collect()
    } else {
        dag_names.iter().collect()
    };
    
    if dags_to_run.is_empty() {
        if let Some(filter) = dag_filter {
            println!("Error: DAG '{}' not found", filter);
            println!("\nAvailable DAGs:");
            for (name, _) in &dag_names {
                println!("  • {}", name);
            }
        } else {
            println!("No DAGs found to execute");
        }
        return Ok(());
    }
    
    // Execute selected DAGs
    for (dag_name, description) in dags_to_run {
        println!("========================================");
        println!("Executing DAG: {}", dag_name);
        println!("Description: {}", description);
        println!("========================================\n");
        
        // Create cache and set inputs
        let cache = Cache::new();
        
        // Set default inputs
        insert_value(&cache, "inputs", "num1", 10.0)?;
        insert_value(&cache, "inputs", "num2", 20.0)?;
        
        // Override with custom inputs
        for (key, value) in &custom_inputs {
            insert_value(&cache, "inputs", key, *value)?;
            println!("  Set input: {} = {}", key, value);
        }
        
        // Set up execution context for parallel execution
        if use_parallel {
            executor.execution_context = Some(ExecutionContext {
                max_parallel_nodes: config.max_parallel_nodes,
                semaphore: Arc::new(Semaphore::new(config.max_parallel_nodes)),
                cache_last_snapshot: Arc::new(RwLock::new(Instant::now())),
                cache_delta_size: Arc::new(RwLock::new(0)),
            });
        }
        
        // Execute the DAG
        println!("\nExecuting nodes:");
        let start_time = std::time::Instant::now();
        let (_cancel_tx, cancel_rx) = oneshot::channel();
        
        let report = executor.execute_static_dag(dag_name, &cache, cancel_rx).await?;
        
        let elapsed = start_time.elapsed();
        
        // Print execution summary
        println!("\n--- Execution Summary ---");
        println!("  Status: {}", if report.overall_success { "✓ Success" } else { "✗ Failed" });
        println!("  Nodes executed: {}", report.node_outcomes.len());
        println!("  Time taken: {:?}", elapsed);
        
        if let Some(error) = &report.error {
            println!("  Error: {}", error);
        }
        
        // Print failed nodes if any
        let failed_nodes: Vec<_> = report.node_outcomes.iter()
            .filter(|o| !o.success)
            .collect();
        
        if !failed_nodes.is_empty() {
            println!("\n  Failed nodes:");
            for outcome in failed_nodes {
                println!("    ✗ {} - {}", outcome.node_id, 
                    outcome.final_error.as_ref().unwrap_or(&"Unknown error".to_string()));
            }
        }
        
        // Print cache contents if verbose
        if verbose {
            println!("\n--- Cache Contents ---");
            let json_output = serialize_cache_to_prettyjson(&cache)?;
            println!("{}", json_output);
        }
        
        // Generate and print DOT graph if verbose
        if verbose {
            if let Ok(dot_output) = executor.serialize_tree_to_dot(dag_name).await {
                println!("\n--- Execution Tree (DOT) ---");
                println!("{}", dot_output);
            }
        }
        
        println!();
    }
    
    println!("=== All DAG executions complete ===\n");
    
    Ok(())
}