use anyhow::Result;
use dagger::{
    append_global_value, generate_node_id, get_global_input, insert_global_value, insert_value,
    register_action, serialize_cache_to_prettyjson, Cache, DagExecutor, Node, NodeAction,
    WorkflowSpec,
};
use dagger_macros::action;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::oneshot;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use async_trait::async_trait;

// Simulated Google Search node
#[action(description = "Performs a Google search and returns results")]
async fn google_search(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    println!("Running Google Search Node: {}", node.id);
    let terms: Vec<String> = get_global_input(cache, "analyze", "search_terms").unwrap_or(vec![]);
    insert_value(cache, &node.id, "input_terms", &terms)?;
    let query = terms.get(0).cloned().unwrap_or("AI trends".to_string());
    let results = vec![format!("Google result for '{}'", query)];
    insert_value(cache, &node.id, "output_results", &results)?;
    Ok(())
}

// Simulated Twitter Search node
#[action(description = "Performs a Twitter search and returns results")]
async fn twitter_search(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    println!("Running Twitter Search Node: {}", node.id);
    let terms: Vec<String> = get_global_input(cache, "analyze", "search_terms").unwrap_or(vec![]);
    insert_value(cache, &node.id, "input_terms", &terms)?;
    let query = terms.get(0).cloned().unwrap_or("AI trends".to_string());
    let results = vec![format!("Tweet about '{}'", query)];
    insert_value(cache, &node.id, "output_results", &results)?;
    Ok(())
}

// Review node
#[action(description = "Reviews the scraped text and returns a summary")]
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

// Supervisor node (Manual implementation)
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

    // Register actions using UPPERCASE static names with clone
    let _ = register_action!(executor, "supervisor_step", supervisor_step);
    let _ = executor.register_action(Arc::new(GOOGLE_SEARCH.clone()));
    let _ = executor.register_action(Arc::new(TWITTER_SEARCH.clone()));
    let _ = executor.register_action(Arc::new(REVIEW.clone()));

    let cache = Cache::new();
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    let report = executor
        .execute_dag(WorkflowSpec::Agent { task: "analyze".to_string() }, &cache, cancel_rx)
        .await?;

    let json_output = serialize_cache_to_prettyjson(&cache)?;
    println!("Final Cache:\n{}", json_output);
    println!("Execution Report: {:#?}", report);
    let dot_output = executor.serialize_tree_to_dot("analyze")?;
    println!("Execution Tree (DOT):\n{}", dot_output);

    let tools = executor.get_tool_schemas();
    println!("Tools: {:#?}", tools);

    Ok(())
}