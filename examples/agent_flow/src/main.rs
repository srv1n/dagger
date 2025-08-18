use anyhow::{Result, anyhow};
use dagger::{
    Cache, DagExecutor, Node, NodeAction, WorkflowSpec, 
    register_action, insert_value, get_input, generate_node_id, 
    serialize_cache_to_prettyjson
};
use async_openai::{Client, ChatCompletionBuilder}; // Hypothetical async OpenAI client
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::oneshot;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> Result<()> {
    // Setup tracing
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    // Initialize Dagger executor
    let registry = Arc::new(RwLock::new(std::collections::HashMap::new()));
    let mut executor = DagExecutor::new(None, registry.clone(), "sqlite::memory:").await?;

    // Register actions
    register_action!(executor, "supervisor_step", supervisor_step);
    register_action!(executor, "web_search", web_search);
    register_action!(executor, "final_answer", final_answer);

    // Initialize cache
    let cache = Cache::new(std::collections::HashMap::new());

    // Set initial task
    let task = "How many seconds would it take for a leopard at full speed to run through Pont des Arts?";
    insert_value(&cache, "analyze", "task", task)?;

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

    let dot_output = executor.serialize_tree_to_dot("analyze")?;
    println!("Execution Tree (DOT):\n{}", dot_output);

    Ok(())
}

// Web Search Action (Simplified)
async fn web_search(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let query: String = get_input(cache, &node.id, "query")?;
    info!("Performing web search for: {}", query);
    
    // Simulate search (replace with actual DuckDuckGo API call if available)
    let results = format!("Search results for '{}': Leopard speed ~50mph, Pont des Arts length ~155m", query);
    insert_value(cache, &node.id, "output_results", results)?;
    Ok(())
}

// Final Answer Action
async fn final_answer(_executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let answer: String = get_input(cache, &node.id, "answer")?;
    info!("Final answer: {}", answer);
    insert_value(cache, "analyze", "final_result", answer)?;
    Ok(())
}

// Supervisor Step with OpenAI LLM
async fn supervisor_step(executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    let dag_name = "analyze";
    let task: String = get_input(cache, dag_name, "task")?;
    let iteration: usize = get_input(cache, &node.id, "iteration").unwrap_or(0);
    let next_iteration = iteration + 1;

    // Initialize OpenAI client (replace with your API key)
    let client = Client::new("your_openai_api_key");

    // Define tools for function calling
    let tools = vec![
        json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Performs a web search for the given query",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "The search query"}
                    },
                    "required": ["query"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "final_answer",
                "description": "Provides the final answer to the task",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "answer": {"type": "string", "description": "The final answer"}
                    },
                    "required": ["answer"]
                }
            }
        }),
    ];

    // Prepare messages
    let mut messages = vec![
        json!({"role": "system", "content": "You are an expert assistant solving tasks using tools."}),
        json!({"role": "user", "content": task}),
    ];

    // Add previous observations if any
    if iteration > 0 {
        let previous_results: String = get_input(cache, &node.id, "previous_results").unwrap_or("".to_string());
        messages.push(json!({"role": "assistant", "content": previous_results}));
    }

    // Make LLM call with function calling
    let response = ChatCompletionBuilder::new(&client)
        .messages(messages)
        .tools(tools)
        .tool_choice("auto")
        .build()?
        .execute()
        .await?;

    let message = response.choices[0].message.clone();
    if let Some(tool_calls) = message.tool_calls {
        for tool_call in tool_calls {
            let function = tool_call.function;
            let args: Value = serde_json::from_str(&function.arguments)?;

            match function.name.as_str() {
                "web_search" => {
                    let query = args["query"].as_str().ok_or(anyhow!("Missing query"))?.to_string();
                    let search_id = generate_node_id("web_search");
                    executor.add_node(
                        dag_name,
                        search_id.clone(),
                        "web_search".to_string(),
                        vec![node.id.clone()],
                    )?;
                    insert_value(cache, &search_id, "query", query)?;

                    // Queue next supervisor step
                    let next_supervisor = generate_node_id("supervisor_step");
                    executor.add_node(
                        dag_name,
                        next_supervisor,
                        "supervisor_step".to_string(),
                        vec![search_id],
                    )?;
                }
                "final_answer" => {
                    let answer = args["answer"].as_str().ok_or(anyhow!("Missing answer"))?.to_string();
                    let final_id = generate_node_id("final_answer");
                    executor.add_node(
                        dag_name,
                        final_id.clone(),
                        "final_answer".to_string(),
                        vec![node.id.clone()],
                    )?;
                    insert_value(cache, &final_id, "answer", answer)?;
                    executor.stopped = Arc::new(RwLock::new(true)); // Signal completion
                }
                _ => return Err(anyhow!("Unknown tool: {}", function.name)),
            }
        }
    } else {
        // Handle text response (e.g., intermediate reasoning)
        let content = message.content.unwrap_or_default();
        insert_value(cache, &node.id, "previous_results", content.clone())?;

        // Queue next supervisor step if not finished
        if iteration < 5 { // Arbitrary limit to prevent infinite loop
            let next_supervisor = generate_node_id("supervisor_step");
            executor.add_node(
                dag_name,
                next_supervisor,
                "supervisor_step".to_string(),
                vec![node.id.clone()],
            )?;
        }
    }

    // Update iteration
    insert_value(cache, &node.id, "iteration", next_iteration)?;
    Ok(())
}