use anyhow::{Error, Result, anyhow};
use async_trait::async_trait;
use dagger::{
    insert_global_value, append_global_value, get_global_input, register_action, serialize_cache_to_prettyjson, 
    Cache, DagExecutor, NodeAction, WorkflowSpec, Node, generate_node_id
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::oneshot;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

// Simulated Google Search node
async fn google_search(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    println!("Running Google Search Node: {}", node.id);
    let dag_name = "analyze"; // Hardcoded for simplicity; could derive from executor
    let terms: Vec<String> = get_global_input(cache, dag_name, "search_terms").unwrap_or(vec![]);
    // Simulate searching for the first term
    let query = terms.get(0).cloned().unwrap_or("AI trends".to_string());
    let results = vec![format!("Google result for '{}'", query)]; // Simulated output
    append_global_value(cache, dag_name, "scraped_text", results.clone())?;
    println!("Google scraped: {:?}", results);
    Ok(())
}

// Simulated Twitter Search node
async fn twitter_search(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    println!("Running Twitter Search Node: {}", node.id);
    let dag_name = "analyze";
    let terms: Vec<String> = get_global_input(cache, dag_name, "search_terms").unwrap_or(vec![]);
    let query = terms.get(0).cloned().unwrap_or("AI trends".to_string());
    let results = vec![format!("Tweet about '{}'", query)]; // Simulated output
    append_global_value(cache, dag_name, "scraped_text", results.clone())?;
    println!("Twitter scraped: {:?}", results);
    Ok(())
}

// Review node to process scraped text
async fn review(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    println!("Running Review Node: {}", node.id);
    let dag_name = "analyze";
    let scraped_text: Vec<String> = get_global_input(cache, dag_name, "scraped_text").unwrap_or(vec![]);
    let summary = format!("Summary of {} items: {}", scraped_text.len(), scraped_text.join("; "));
    insert_global_value(cache, dag_name, "review_output", summary.clone())?;
    println!("Review output: {}", summary);
    Ok(())
}

// Supervisor node to orchestrate the flow
async fn supervisor_step(executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let dag_name = "analyze"; // Hardcoded for simplicity
    println!("Supervisor Node: {}, DAG: {}", node.id, dag_name);

    // Get current iteration
    let iteration: usize = get_global_input(cache, dag_name, "supervisor_iteration").unwrap_or(0);
    let next_iteration = iteration + 1;

    match iteration {
        0 => {
            // Step 1: Add scraping nodes
            let google_id = generate_node_id("google_search");
            executor.add_node(dag_name, google_id.clone(), "google_search".to_string(), vec![node.id.clone()])?;
            let twitter_id = generate_node_id("twitter_search");
            executor.add_node(dag_name, twitter_id.clone(), "twitter_search".to_string(), vec![node.id.clone()])?;
            // Initialize search terms (could come from user or LLM)
            insert_global_value(cache, dag_name, "search_terms", vec!["AI trends"])?;
            // Queue next supervisor step
            let next_supervisor = generate_node_id("supervisor_step");
            executor.add_node(dag_name, next_supervisor, "supervisor_step".to_string(), vec![google_id, twitter_id])?;
        }
        1 => {
            // Step 2: Add review node after scraping
            let review_id = generate_node_id("review");
            executor.add_node(dag_name, review_id.clone(), "review".to_string(), vec![node.id.clone()])?;
            // Queue final supervisor step
            let next_supervisor = generate_node_id("supervisor_step");
            executor.add_node(dag_name, next_supervisor, "supervisor_step".to_string(), vec![review_id])?;
        }
        2 => {
            // Step 3: Finish
            println!("Supervisor finished after collecting and reviewing data");
            // executor.stopped = true; // Signal completion
        }
        _ => unreachable!("Unexpected iteration"),
    }

    // Update iteration
    insert_global_value(cache, dag_name, "supervisor_iteration", next_iteration)?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup tracing subscriber
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    // Initialize the DAG executor with default config
    let mut executor = DagExecutor::new(None)?;

    // Register actions
    register_action!(executor, "supervisor_step", supervisor_step);
    register_action!(executor, "google_search", google_search);
    register_action!(executor, "twitter_search", twitter_search);
    register_action!(executor, "review", review);

    // Initialize cache
    let cache = Cache::new(HashMap::new());

    // Cancellation channel
    let (_cancel_tx, cancel_rx) = oneshot::channel();

    // Execute agentic flow
    let report = executor
        .execute_dag(
            WorkflowSpec::Agent {
                task: "analyze".to_string(),
            },
            &cache,
            cancel_rx,
        )
        .await?;

    // Print results
    let json_output = serialize_cache_to_prettyjson(&cache)?;
    println!("Final Cache:\n{}", json_output);
    println!("Execution Report: {:#?}", report);

    Ok(())
}