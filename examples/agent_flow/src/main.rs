use anyhow::{Error, Result};
use async_openai::{
    types::{
        ChatCompletionFunctionsArgs, ChatCompletionRequestFunctionMessageArgs,
        ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequestArgs,
    },
    Client,
};
use dagger::{
    append_global_value, generate_node_id, get_global_input, insert_global_value, insert_value,
    parse_input_from_name, register_action, serialize_cache_to_prettyjson, Cache, DagConfig, DagExecutionReport,
    DagExecutor, HumanInterrupt, InfoRetrievalAgent, Node, NodeAction, WorkflowSpec,
};
use reqwest;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc, sync::RwLock};
use tokio::sync::oneshot;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use tokio::time::{sleep, Duration};


// Actual Google search implementation
async fn google_search(executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    info!("Running Google Search Node: {}", node.id);

    // Get search query from cache
    let query: String = get_global_input(cache, "research", "search_query")
        .unwrap_or(  "AI trends".to_string())   ;
    
    // Store input for tracking
    insert_value(cache, &node.id, "input_query", &query)?;

    // Perform actual search using custom search API
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let cx = std::env::var("GOOGLE_SEARCH_CX")?;
    let client = reqwest::Client::new();
    
    let response = client
        .get("https://www.googleapis.com/customsearch/v1")
        .query(&[
            ("key", api_key),
            ("cx", cx),
            ("q", query.clone()),
        ])
        .send()
        .await?
        .json::<Value>()
        .await?;

    // Extract and store results
    let results = response["items"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .take(5)
        .map(|item| {
            format!(
                "Title: {}\nSnippet: {}\nLink: {}", 
                item["title"].as_str().unwrap_or(""),
                item["snippet"].as_str().unwrap_or(""),
                item["link"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<String>>();

    insert_value(cache, &node.id, "search_results", &results)?;
    
    Ok(())
}

// Intelligent supervisor using OpenAI with function calling
async fn supervisor_step(executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    info!("Running Supervisor Node: {}", node.id);

    let client = Client::new();
    let iteration: usize = get_global_input(cache, "research", "iteration").unwrap_or(0);
    
    // Get task and any previous results
    let task: String = get_global_input(cache, "research", "task")
        .unwrap_or("Research AI trends".to_string());
    
    let previous_results: Vec<String> = get_global_input(cache, "research", "all_results")
        .unwrap_or_default();
     

    let model = "gpt-4o-mini";

    let request = CreateChatCompletionRequestArgs::default()
        .max_tokens(512u32)
        .model(model)
        .messages([ChatCompletionRequestUserMessageArgs::default()
            .content("What's the weather like in Boston?")
            .build()?
            .into()])
        .functions([ChatCompletionFunctionsArgs::default()
            .name("get_current_weather")
            .description("Get the current weather in a given location")
            .parameters(json!({
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "The city and state, e.g. San Francisco, CA",
                    },
                    "unit": { "type": "string", "enum": ["celsius", "fahrenheit"] },
                },
                "required": ["location"],
            }))
            .build()?])
        .function_call("auto")
        .build()?;

    let response_message = client
        .chat()
        .create(request)
        .await?
        .choices
        .first()
        .unwrap()
        .message
        .clone();

    if let Some(function_call) = response_message.function_call {
        let mut available_functions: HashMap<&str, fn(&str, &str) -> serde_json::Value> =
            HashMap::new();
        available_functions.insert("get_current_weather", get_current_weather);
        let function_name = function_call.name;
        let function_args: serde_json::Value = function_call.arguments.parse().unwrap();

        let location = function_args["location"].as_str().unwrap();
        let unit = "fahrenheit";
        let function = available_functions.get(function_name.as_str()).unwrap();
        let function_response = function(location, unit);

        let message = vec![
            ChatCompletionRequestUserMessageArgs::default()
                .content("What's the weather like in Boston?")
                .build()?
                .into(),
            ChatCompletionRequestFunctionMessageArgs::default()
                .content(function_response.to_string())
                .name(function_name)
                .build()?
                .into(),
        ];

        println!("{}", serde_json::to_string(&message).unwrap());

        let request = CreateChatCompletionRequestArgs::default()
            .max_tokens(512u32)
            .model(model)
            .messages(message)
            .build()?;

        let response = client.chat().create(request).await?;

        println!("\nResponse:\n");
        for choice in response.choices {
            println!(
                "{}: Role: {}  Content: {:?}",
                choice.index, choice.message.role, choice.message.content
            );
        }
    }

    
    // Define available functions
    let functions = vec![
        ChatCompletionFunctionsArgs::default()
            .name("perform_search")
            .description("Perform a Google search with the specified query")
            .parameters(json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query to execute",
                    },
                    "reason": {
                        "type": "string",
                        "description": "Explanation of why this search is needed",
                    }
                },
                "required": ["query", "reason"]
            }))
            .build()?,
        ChatCompletionFunctionsArgs::default()
            .name("finish_research")
            .description("Complete the research task and summarize findings")
            .parameters(json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "Final summary of the research findings",
                    }
                },
                "required": ["summary"]
            }))
            .build()?
    ];

    // Prepare context for OpenAI
    let messages = vec![
        ChatCompletionRequestMessage {
            role: Role::System,
            content: "You are a research supervisor agent. Plan and coordinate research steps using the available functions. Make decisions based on the task and previous results.".into(),
            name: None,
            function_call: None,
        },
        ChatCompletionRequestMessage {
            role: Role::User,
            content: format!(
                "Task: {}\nIteration: {}\nPrevious results: {:?}\n\nWhat should be the next step?",
                task, iteration, previous_results
            ),
            name: None,
            function_call: None,
        },
    ];

    // Get OpenAI's decision with function calling
    let response = openai
        .chat()
        .create(CreateChatCompletionRequest {
            model: "gpt-4".into(),
            messages: messages.clone(),
            functions: Some(functions),
            function_call: Some(serde_json::json!("auto")),
            temperature: Some(0.7),
            max_tokens: Some(500),
            ..Default::default()
        })
        .await?;

    let message = &response.choices[0].message;
    
    // Handle function calls
    if let Some(function_call) = &message.function_call {
        let function_args: Value = serde_json::from_str(&function_call.arguments)?;
        
        match function_call.name.as_str() {
            "perform_search" => {
                let query = function_args["query"].as_str().unwrap();
                let reason = function_args["reason"].as_str().unwrap();
                
                info!("Initiating search: {} (Reason: {})", query, reason);
                
                // Add search node
                let search_id = generate_node_id("google_search");
                insert_global_value(cache, "research", "search_query", query.to_string())?;
                executor.add_node(
                    "research",
                    search_id.clone(),
                    "google_search".to_string(),
                    vec![node.id.clone()],
                )?;

                // Queue next supervisor step
                let next_supervisor = generate_node_id("supervisor_step");
                executor.add_node(
                    "research",
                    next_supervisor,
                    "supervisor_step".to_string(),
                    vec![search_id],
                )?;
            },
            "finish_research" => {
                let summary = function_args["summary"].as_str().unwrap();
                info!("Research complete: {}", summary);
                insert_value(cache, &node.id, "final_summary", summary)?;
                executor.stopped = Arc::new(RwLock::new(true));
            },
            _ => {
                info!("Unknown function call: {}", function_call.name);
            }
        }
    }

    // Update iteration count
    insert_global_value(cache, "research", "iteration", iteration + 1)?;
    
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup tracing
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    // Initialize executor with configuration
    let config = DagConfig {
        max_iterations: Some(10),
        timeout_seconds: Some(300),
        human_wait_minutes: Some(1),
        human_timeout_action: dagger::HumanTimeoutAction::Autopilot,
        review_frequency: Some(2),
        ..Default::default()
    };

    let registry = Arc::new(RwLock::new(HashMap::new()));
    let mut executor = DagExecutor::new(Some(config), registry.clone(), "dagger_db")?;

    // Register actions
    register_action!(executor, "supervisor_step", supervisor_step);
    register_action!(executor, "google_search", google_search);

    // Initialize cache and set initial task
    let cache = Cache::new(HashMap::new());
    insert_global_value(
        &cache,
        "research",
        "task",
        "Research the latest developments in AI safety".to_string(),
    )?;

    // Execute agent flow
    let (_cancel_tx, cancel_rx) = oneshot::channel();
    let report = executor
        .execute_dag(
            WorkflowSpec::Agent {
                task: "research".to_string(),
            },
            &cache,
            cancel_rx,
        )
        .await?;

    // Output results
    println!("Final Cache:\n{}", serialize_cache_to_prettyjson(&cache)?);
    println!("Execution Report: {:#?}", report);
    println!(
        "Execution Tree (DOT):\n{}",
        executor.serialize_tree_to_dot("research")?
    );

    Ok(())
}