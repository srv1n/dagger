use std::error::Error;
use std::future::Future;
use std::pin::Pin;

use async_openai::types::{
    ChatCompletionMessageToolCall, ChatCompletionRequestAssistantMessageArgs,
    ChatCompletionRequestMessage, ChatCompletionRequestToolMessageArgs,
    ChatCompletionRequestUserMessageArgs, ChatCompletionToolArgs, ChatCompletionToolType,
    CreateChatCompletionRequestArgs, FunctionObjectArgs, ChatCompletionTool,
};
use serde_json::{json, Value};

use crate::utils::llm::client::CLIENT;
use crate::utils::llm::chat_stream::stream_chat_completion;

/// A type alias for a function that can be called by the LLM.
/// The function takes a JSON string of arguments and returns a JSON Value result.
pub type ToolFunction = Box<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<Value, Box<dyn Error>>> + Send>> + Send + Sync>;

/// A struct representing a tool that can be called by the LLM.
pub struct Tool {
    /// The name of the tool
    pub name: String,
    /// A description of what the tool does
    pub description: String,
    /// The JSON schema for the tool's parameters
    pub parameters: Value,
    /// The function to call when this tool is invoked
    pub function: ToolFunction,
}

/// Execute a chat completion with tool calling capabilities.
///
/// # Arguments
///
/// * `user_prompt` - The user's prompt text
/// * `tools` - A vector of Tool structs that can be called by the LLM
/// * `model` - The model to use (e.g., "gpt-4o", "gpt-3.5-turbo")
/// * `max_tokens` - Maximum number of tokens to generate
/// * `stream` - Whether to stream the final response
///
/// # Returns
///
/// * `Result<String, Box<dyn Error>>` - The response text or an error
///
/// # Example
///
/// ```
/// use serde_json::{json, Value};
/// use crate::utils::llm::tool_call::{Tool, execute_with_tools};
///
/// async fn example() -> Result<(), Box<dyn std::error::Error>> {
///     // Define a weather tool
///     let weather_tool = Tool {
///         name: "get_current_weather".to_string(),
///         description: "Get the current weather in a given location".to_string(),
///         parameters: json!({
///             "type": "object",
///             "properties": {
///                 "location": {
///                     "type": "string",
///                     "description": "The city and state, e.g. San Francisco, CA",
///                 },
///                 "unit": { "type": "string", "enum": ["celsius", "fahrenheit"] },
///             },
///             "required": ["location"],
///         }),
///         function: Box::new(|args| {
///             Box::pin(async move {
///                 let args_value: Value = serde_json::from_str(args)?;
///                 let location = args_value["location"].as_str().unwrap_or("unknown");
///                 let unit = args_value["unit"].as_str().unwrap_or("fahrenheit");
///                 
///                 // In a real app, you would call a weather API here
///                 let weather_info = json!({
///                     "location": location,
///                     "temperature": "72",
///                     "unit": unit,
///                     "forecast": "sunny"
///                 });
///                 
///                 Ok(weather_info)
///             })
///         }),
///     };
///     
///     let tools = vec![weather_tool];
///     let response = execute_with_tools(
///         "What's the weather like in San Francisco?",
///         tools,
///         "gpt-4o",
///         512,
///         false
///     ).await?;
///     
///     println!("{}", response);
///     Ok(())
/// }
/// ```
pub async fn execute_with_tools(
    user_prompt: &str,
    tools: Vec<Tool>,
    model: &str,
    max_tokens: u32,
    stream: bool,
) -> Result<String, Box<dyn Error>> {
    let client = &CLIENT;

    // Convert our Tool structs to ChatCompletionToolArgs
    let tool_args: Vec<ChatCompletionTool> = tools
        .iter()
        .map(|tool| {
            ChatCompletionToolArgs::default()
                .r#type(ChatCompletionToolType::Function)
                .function(
                    FunctionObjectArgs::default()
                        .name(&tool.name)
                        .description(&tool.description)
                        .parameters(tool.parameters.clone())
                        .build()
                        .unwrap(),
                )
                .build()
                .unwrap()
                .into()
        })
        .collect();

    // Create a map of tool names to their functions for easy lookup
    let tool_functions: std::collections::HashMap<String, &ToolFunction> = tools
        .iter()
        .map(|tool| (tool.name.clone(), &tool.function))
        .collect();

    // Initial request with user prompt
    let request = CreateChatCompletionRequestArgs::default()
        .max_tokens(max_tokens)
        .model(model)
        .messages([ChatCompletionRequestUserMessageArgs::default()
            .content(user_prompt)
            .build()?
            .into()])
        .tools(tool_args)
        .build()?;

    let response = client.chat().create(request).await?;
    let response_message = response.choices.first().unwrap().message.clone();

    // Check if the model wants to call tools
    if let Some(tool_calls) = response_message.tool_calls {
        println!("tool_calls: {:?}", tool_calls);
        // Process each tool call
        let mut function_responses = Vec::new();
        for tool_call in tool_calls {
            let name = tool_call.function.name.clone();
            let args = tool_call.function.arguments.clone();
            
            // Look up the function and call it
            if let Some(function) = tool_functions.get(&name) {
                let response_content = function(&args).await?;
                function_responses.push((tool_call, response_content));
            }
        }

        // Prepare messages for the follow-up request
        let mut messages: Vec<ChatCompletionRequestMessage> = vec![
            ChatCompletionRequestUserMessageArgs::default()
                .content(user_prompt)
                .build()?
                .into()
        ];

        // Add the assistant's tool calls
        let tool_calls: Vec<ChatCompletionMessageToolCall> = function_responses
            .iter()
            .map(|(tool_call, _)| tool_call.clone())
            .collect();

        let assistant_message = ChatCompletionRequestAssistantMessageArgs::default()
            .tool_calls(tool_calls)
            .build()?
            .into();

        messages.push(assistant_message);

        // Add the tool responses
        let tool_messages: Vec<ChatCompletionRequestMessage> = function_responses
            .iter()
            .map(|(tool_call, response_content)| {
                ChatCompletionRequestToolMessageArgs::default()
                    .content(response_content.to_string())
                    .tool_call_id(tool_call.id.clone())
                    .build()
                    .unwrap()
                    .into()
            })
            .collect();

        messages.extend(tool_messages);

        // Make the follow-up request
        if stream {
            // Stream the response
            stream_chat_completion(messages, model, max_tokens).await
        } else {
            // Get the response as a single message
            let subsequent_request = CreateChatCompletionRequestArgs::default()
                .max_tokens(max_tokens)
                .model(model)
                .messages(messages)
                .build()?;

            let response = client.chat().create(subsequent_request).await?;
            
            if let Some(choice) = response.choices.first() {
                if let Some(content) = &choice.message.content {
                    return Ok(content.clone());
                }
            }
            
            Err("No content in response".into())
        }
    } else {
        // If no tool calls were made, return the direct response
        if let Some(content) = &response_message.content {
            Ok(content.clone())
        } else {
            Err("No content in response".into())
        }
    }
}

// Example of creating a weather tool with proper lifetime handling
pub fn create_weather_tool() -> Tool {
    Tool {
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
    }
}

// Example of using the weather tool with execute_with_tools
pub async fn example_usage() -> Result<String, Box<dyn Error>> {
    let weather_tool = create_weather_tool();
    let tools = vec![weather_tool];
    
    let user_prompt = "What's the weather like in Seattle?";
    let model = "gpt-4-1106-preview";
    let max_tokens = 512u32;
    let stream = false;
    
    execute_with_tools(user_prompt, tools, model, max_tokens, stream).await
}
