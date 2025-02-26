use anyhow::{anyhow, Result};
use dagger::{Cache, Message, PubSubConfig, PubSubError, PubSubExecutor, PubSubWorkflowSpec};
use dagger_macros::pubsub_agent;
use serde_json::{json, Value};
use std::{collections::HashMap, time::Duration};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};


#[pubsub_agent(
    name = "SupervisorAgent",
    description = "Orchestrates the entire process",
    subscribe = "start",
    publish = "plan_requests",
    input_schema = r#"{"type": "object", "properties": {"query": {"type": "string"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"plan_request": {"type": "string"}, "iteration": {"type": "integer"}}}"#
)]
async fn supervisor_agent(
    node_id: &str,
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    let query = message.payload["query"].as_str().ok_or(anyhow!("Missing query"))?;
    info!("Supervisor received query: {}", query);
    for i in 0..3 {
        tokio::time::sleep(Duration::from_secs(0)).await;
        let payload = json!({"plan_request": query, "iteration": i});
        let msg = Message::new(node_id.to_string(), payload);
        info!("Supervisor publishing plan request iteration {}", i);
        executor.publish("plan_requests", msg, cache).await?;
    }
    Ok(())
}

#[pubsub_agent(
    name = "PlanningAgent",
    description = "Breaks down the query into search questions",
    subscribe = "plan_requests",
    publish = "search_requests",
    input_schema = r#"{"type": "object", "properties": {"plan_request": {"type": "string"}, "iteration": {"type": "integer"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"search_question": {"type": "string"}, "iteration": {"type": "integer"}}}"#
)]
async fn planning_agent(
    node_id: &str, // Now receives node_id
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
  let plan_request = message.payload["plan_request"].as_str().ok_or(anyhow!("Missing plan_request"))?;
        let iteration = message.payload["iteration"].as_u64().ok_or(anyhow!("Missing iteration"))?;
        info!("Planning agent received plan request: {} (iteration {})", plan_request, iteration);
        tokio::time::sleep(Duration::from_secs(0)).await;
        let search_questions = vec!["school ratings", "school locations", "school programs"];
        // let search_questions = vec!["school ratings"];
        for (idx, question) in search_questions.iter().enumerate() {
            let payload = json!({"search_question": format!("{} - part {}", question, idx + 1), "iteration": iteration});
            let msg = Message::new(node_id.to_string(), payload);
            info!("Planning agent publishing search question: {} (iteration {})", question, iteration);
            executor.publish("search_requests", msg, &cache).await?;
        }
        Ok(())
}
#[pubsub_agent(
    name = "RetrievalAgent",
    description = "Performs searches based on questions",
    subscribe = "search_requests",
    publish = "search_results",
    input_schema = r#"{"type": "object", "properties": {"search_question": {"type": "string"}, "iteration": {"type": "integer"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"search_question": {"type": "string"}, "results": {"type": "array", "items": {"type": "string"}}, "iteration": {"type": "integer"}}}"#
)]
async fn retrieval_agent(
    node_id: &str, // Now receives node_id
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    let search_question = message.payload["search_question"].as_str().ok_or(anyhow!("Missing search_question"))?;
        let iteration = message.payload["iteration"].as_u64().ok_or(anyhow!("Missing iteration"))?;
        info!("Retrieval agent processing: {} (iteration {})", search_question, iteration);
        tokio::time::sleep(Duration::from_secs(0)).await;
        let results = vec![
            format!("Result 1 for {} (iter {})", search_question, iteration),
            format!("Result 2 for {} (iter {})", search_question, iteration),
        ];
        let payload = json!({"search_question": search_question, "results": results, "iteration": iteration});
        let msg = Message::new(node_id.to_string(), payload);
        executor.publish("search_results", msg, &cache).await?;
        Ok(())
}

#[pubsub_agent(
    name = "ReviewAgent",
    description = "Reviews the search results",
    subscribe = "search_results",
    publish = "reviewed_results",
    input_schema = r#"{"type": "object", "properties": {"search_question": {"type": "string"}, "results": {"type": "array", "items": {"type": "string"}}, "iteration": {"type": "integer"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"search_question": {"type": "string"}, "reviewed_results": {"type": "array", "items": {"type": "string"}}, "iteration": {"type": "integer"}}}"#
)]
async fn review_agent(
    node_id: &str, // Now receives node_id
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
   let search_question = message.payload["search_question"].as_str().ok_or(anyhow!("Missing search_question"))?;
        let results = message.payload["results"].as_array().ok_or(anyhow!("Missing results"))?;
        let iteration = message.payload["iteration"].as_u64().ok_or(anyhow!("Missing iteration"))?;
        info!("Review agent reviewing: {} (iteration {})", search_question, iteration);
        tokio::time::sleep(Duration::from_secs(0)).await;
        let reviewed_results = results.clone();
        let payload = json!({"search_question": search_question, "reviewed_results": reviewed_results, "iteration": iteration});
        let msg = Message::new(node_id.to_string(), payload);
        executor.publish("reviewed_results", msg, &cache).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let refined_results = reviewed_results.iter().map(|r| json!(format!("Refined: {}", r.as_str().unwrap()))).collect::<Vec<_>>();
        let refined_payload = json!({"search_question": search_question, "reviewed_results": refined_results, "iteration": iteration});
        let msg = Message::new(node_id.to_string(), refined_payload);
        executor.publish("reviewed_results", msg, &cache).await?;
        Ok(())
}

#[pubsub_agent(
    name = "DraftingAgent",
    description = "Drafts the final copy based on reviewed results",
    subscribe = "reviewed_results",
    publish = "final_draft",
    input_schema = r#"{"type": "object", "properties": {"search_question": {"type": "string"}, "reviewed_results": {"type": "array", "items": {"type": "string"}}, "iteration": {"type": "integer"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"draft": {"type": "string"}, "iteration": {"type": "integer"}}}"#
)]
async fn drafting_agent(
    node_id: &str, // Now receives node_id
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    let search_question = message.payload["search_question"].as_str().ok_or(anyhow!("Missing search_question"))?;
        let reviewed_results = message.payload["reviewed_results"].as_array().ok_or(anyhow!("Missing reviewed_results"))?;
        let iteration = message.payload["iteration"].as_u64().ok_or(anyhow!("Missing iteration"))?;
        info!("Drafting agent processing: {} (iteration {})", search_question, iteration);
        tokio::time::sleep(Duration::from_secs(0)).await;
        let draft = format!(
            "Draft for {} (iter {}): {}",
            search_question,
            iteration,
            reviewed_results.iter().map(|v| v.as_str().unwrap_or("")).collect::<Vec<&str>>().join(", ")
        );
        let payload = json!({"draft": draft, "iteration": iteration});
        let msg = Message::new(node_id.to_string(), payload);
        executor.publish("final_draft", msg, &cache).await?;
        Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cache = Arc::new(Cache::new(HashMap::new()));
    let mut executor = PubSubExecutor::new(None, "pubsub_db", cache.clone()).await?;

    // Register supervisor first
    executor.register_supervisor_agent(Arc::new(__supervisor_agentAgent::new())).await?;
    executor.register_agents(vec![
        Arc::new(__planning_agentAgent::new()),
        Arc::new(__retrieval_agentAgent::new()),
        Arc::new(__review_agentAgent::new()),
        Arc::new(__drafting_agentAgent::new()),
    ]).await?;

    // Start continuous execution and collect outcomes
    let mut outcome_rx = executor.start().await?;
    let mut outcomes = Vec::new();

    // Publish initial task
    let initial_message = json!({"query": "Research schools for my kids"});
    let node_id = "SupervisorAgent".to_string();
    let msg = Message::new(node_id, initial_message);
    println!("Publishing initial message: {:#?}", msg);
    executor.publish("start", msg, &cache).await?;

    // Monitor outcomes for 30 seconds to capture multiple iterations
    tokio::time::sleep(Duration::from_secs(30)).await;

    while let Ok(outcome) = outcome_rx.try_recv() {
        outcomes.push(outcome);
    }

    executor.stop().await?;

    // Print outcomes
    println!("Execution Outcomes:");
    for outcome in &outcomes {
        println!("- Agent: {}, Success: {}, Error: {:?}", outcome.node_id, outcome.success, outcome.final_error);
    }

    // Print final cache
    println!("Final Cache: {:#?}", cache.read().unwrap());

    // Print DOT graph
    let dot = executor.serialize_tree_to_dot("workflow1", &cache).await?;
    println!("DOT Graph:\n{}", dot);

    Ok(())
}