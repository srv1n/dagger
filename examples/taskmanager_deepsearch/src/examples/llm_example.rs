use std::{env, error::Error};
use serde::{Deserialize, Serialize};
use serde_json::json;
use dotenv::dotenv;
use schemars::{schema_for, JsonSchema};
use async_openai::types::{ChatCompletionRequestSystemMessage, ChatCompletionRequestUserMessage};
use crate::utils::llm::{
    execute_with_tools, get_structured_output_typed, stream_chat_prompt, Tool,
};

// Example struct for structured output
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Person {
    name: String,
    age: u32,
    interests: Vec<String>,
    address: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Load environment variables from .env file
    dotenv().ok();
    
    println!("=== LLM Utilities Example ===\n");

    // Example 1: Simple chat completion with streaming
    println!("Example 1: Simple Chat Completion");
    println!("-------------------------------");
    let response = stream_chat_prompt(
        "Write a short haiku about programming in Rust.",
        &env::var("OPEN_AI_MODEL").unwrap_or_else(|_| "qwen-qwq-32b".to_string()),
        100,
    )
    .await?;
    println!("\nFull response: {}\n", response);

    // Example 2: Structured output
    println!("Example 2: Structured Output");
    println!("---------------------------");
    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "integer" },
            "interests": { 
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["name", "age", "interests"]
    });

    let person_schema = schema_for!(Person);
    println!("person_schema: {:#?}", serde_json::to_string(&person_schema).unwrap());

    let person: Person = get_structured_output_typed(
        &format!("You are a helpful assistant that extracts user information. Please return the response in JSON format. {}", serde_json::to_string(&person_schema).unwrap()),
        "My name is Alice, I'm 30 years old and I like hiking, reading, and coding.",
        schema,
        &env::var("OPEN_AI_MODEL").unwrap_or_else(|_| "qwen-qwq-32b".to_string()),
        1512,
    )
    .await?;

    println!("Extracted person: {:?}\n", person);

    // Example 3: Tool calling
    println!("Example 3: Tool Calling");
    println!("----------------------");
    
    // Define a weather tool
    let weather_tool = Tool {
        name: "get_current_weather".to_string(),
        description: "Get the current weather in a given location".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "The city and state, e.g. San Francisco, CA",
                },
                "unit": { "type": "string", "enum": ["celsius", "fahrenheit"] },
            },
            "required": ["location"],
        }),
        function: Box::new(|args| {
            // Clone the args string to own the data
            let args_owned = args.to_string();
            Box::pin(async move {
                let args_value: serde_json::Value = serde_json::from_str(&args_owned)?;
                let location = args_value["location"].as_str().unwrap_or("unknown");
                let unit = args_value["unit"].as_str().unwrap_or("fahrenheit");
                
                // In a real app, you would call a weather API here
                // This is just a mock response
                let weather_info = json!({
                    "location": location,
                    "temperature": "72",
                    "unit": unit,
                    "forecast": "sunny"
                });
                
                Ok(weather_info)
            })
        }),
    };
    
    let tools = vec![weather_tool];
    let response = execute_with_tools(
        "What's the weather like in San Francisco?",
        tools,
      &env::var("OPEN_AI_MODEL").unwrap_or_else(|_| "qwen-qwq-32b".to_string()),
        3512,
        false
    ).await?;
    
    println!("Response: {}", response);

    Ok(())
} 