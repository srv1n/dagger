# LLM Utilities

This module provides utilities for interacting with OpenAI's large language models using the `async-openai` crate.

## Features

- **Singleton Client**: A shared OpenAI client instance for efficient API usage
- **Chat Streaming**: Stream chat completions for real-time responses
- **Structured Outputs**: Get structured JSON responses using JSON schema
- **Tool Calling**: Define and use tools that the LLM can call

## Setup

Make sure to set your OpenAI API key as an environment variable:

```bash
export OPENAI_API_KEY='sk-...'
```

## Usage Examples

### Basic Chat Completion

```rust
use crate::utils::llm::stream_chat_prompt;

async fn example() -> Result<(), Box<dyn std::error::Error>> {
    let response = stream_chat_prompt(
        "Write a short haiku about programming in Rust.",
        "gpt-3.5-turbo",
        100,
    ).await?;
    
    println!("{}", response);
    Ok(())
}
```

### Structured Output

```rust
use serde::{Deserialize, Serialize};
use serde_json::json;
use crate::utils::llm::get_structured_output_typed;

#[derive(Debug, Serialize, Deserialize)]
struct Person {
    name: String,
    age: u32,
    interests: Vec<String>,
}

async fn example() -> Result<(), Box<dyn std::error::Error>> {
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

    let person: Person = get_structured_output_typed(
        "You are a helpful assistant that extracts user information.",
        "My name is Alice, I'm 30 years old and I like hiking, reading, and coding.",
        schema,
        "gpt-4o",
        512,
    ).await?;

    println!("Extracted person: {:?}", person);
    Ok(())
}
```

### Tool Calling

```rust
use serde_json::json;
use crate::utils::llm::{Tool, execute_with_tools};

async fn example() -> Result<(), Box<dyn std::error::Error>> {
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
            Box::pin(async move {
                let args_value: serde_json::Value = serde_json::from_str(args)?;
                let location = args_value["location"].as_str().unwrap_or("unknown");
                let unit = args_value["unit"].as_str().unwrap_or("fahrenheit");
                
                // In a real app, you would call a weather API here
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
        "gpt-4o",
        512,
        false
    ).await?;
    
    println!("Response: {}", response);
    Ok(())
}
```

## Full Example

See `src/examples/llm_example.rs` for a complete example of using all these features. 