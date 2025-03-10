use std::{env, error::Error};

use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
    ChatCompletionRequestUserMessage, CreateChatCompletionRequestArgs, ResponseFormat,
    ResponseFormatJsonSchema,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use schemars::{schema_for, JsonSchema};
use crate::utils::llm::client::CLIENT;
use crate::utils::prompts;

/// Request a structured output from OpenAI using a JSON schema.
///
/// # Arguments
///
/// * `system_prompt` - The system prompt to guide the model
/// * `user_prompt` - The user's prompt text
/// * `schema` - The JSON schema that defines the structure of the response
/// * `model` - The model to use (e.g., "gpt-4o", "gpt-3.5-turbo")
/// * `max_tokens` - Maximum number of tokens to generate
///
/// # Returns
///
/// * `Result<Value, Box<dyn Error>>` - The structured JSON response or an error
///
/// # Example
///
/// ```
/// use serde_json::json;
/// use crate::utils::llm::structured_outputs::get_structured_output;
///
/// async fn example() -> Result<(), Box<dyn std::error::Error>> {
///     let schema = json!({
///         "type": "object",
///         "properties": {
///             "name": { "type": "string" },
///             "age": { "type": "integer" },
///             "interests": { 
///                 "type": "array",
///                 "items": { "type": "string" }
///             }
///         },
///         "required": ["name", "age", "interests"]
///     });
///     
///     let result = get_structured_output(
///         "You are a helpful assistant that extracts user information.",
///         "My name is Alice, I'm 30 years old and I like hiking, reading, and coding.",
///         schema,
///         "gpt-4o",
///         512
///     ).await?;
///     
///     println!("{}", result);
///     Ok(())
/// }
/// ```
pub async fn get_structured_output(
    system_prompt: &str,
    user_prompt: &str,
    schema: Value,
    model: &str,
    max_tokens: u32,
) -> Result<Value, Box<dyn Error>> {
    let client = &CLIENT;

    // let response_format = ResponseFormat::JsonSchema {
    //     json_schema: ResponseFormatJsonSchema {
    //         description: None,
    //         name: "json_object".into(),
    //         schema: Some(schema),
    //         strict: Some(true),
    //     },
    // };

    let response_format = ResponseFormat::JsonObject;

    let request = CreateChatCompletionRequestArgs::default()
        .max_tokens(max_tokens)
        .model(model)
        .messages([
            ChatCompletionRequestSystemMessage::from(system_prompt).into(),
            ChatCompletionRequestUserMessage::from(user_prompt).into(),
        ])
        .response_format(response_format)
        .build()?;

    let response = client.chat().create(request).await?;
    
    if let Some(choice) = response.choices.first() {
        if let Some(content) = &choice.message.content {
            let json_value: Value = serde_json::from_str(content)?;
            return Ok(json_value);
        }
    }
    
    Err("No content in response".into())
}


pub async fn structured_output<T: serde::Serialize + DeserializeOwned + JsonSchema>(
    messages: Vec<ChatCompletionRequestMessage>,
) -> Result<Option<T>, Box<dyn Error>> {
    println!("structured_output");
    let schema = schema_for!(T);
    println!("schema: {:?}", schema);
    let schema_value = serde_json::to_value(&schema)?;
    let response_format = ResponseFormat::JsonObject;

    println!("response_format: {:#?}", response_format);
    let request = CreateChatCompletionRequestArgs::default()
        .max_tokens(512u32)
        .model(&env::var("OPEN_AI_MODEL").unwrap_or_else(|_| "qwen-qwq-32b".to_string()))
        .messages(messages)
        .response_format(response_format)
        .build()?;

    let client = &CLIENT;
    let response = client.chat().create(request).await?;

    for choice in response.choices {
        if let Some(content) = choice.message.content {
            return Ok(Some(serde_json::from_str::<T>(&content)?));
        }
    }

    Ok(None)
}


/// Request a structured output from OpenAI and deserialize it into a specific type.
///
/// # Type Parameters
///
/// * `T` - The type to deserialize the response into. Must implement DeserializeOwned.
///
/// # Arguments
///
/// * `system_prompt` - The system prompt to guide the model
/// * `user_prompt` - The user's prompt text
/// * `schema` - The JSON schema that defines the structure of the response
/// * `model` - The model to use (e.g., "gpt-4o", "gpt-3.5-turbo")
/// * `max_tokens` - Maximum number of tokens to generate
///
/// # Returns
///
/// * `Result<T, Box<dyn Error>>` - The deserialized structured response or an error
pub async fn get_structured_output_typed<T: DeserializeOwned>(
    system_prompt: &str,
    user_prompt: &str,
    schema: Value,
    model: &str,
    max_tokens: u32,
) -> Result<T, Box<dyn Error>> {
    let json_value = get_structured_output(system_prompt, user_prompt, schema, model, max_tokens).await?;
    let typed_result = serde_json::from_value(json_value)?;
    Ok(typed_result)
}

/// Request a structured output from OpenAI using a template from the templates.yaml file.
///
/// # Arguments
///
/// * `template_name` - The name of the template in the templates.yaml file
/// * `context` - The context to render the template with
/// * `schema` - The JSON schema that defines the structure of the response
/// * `model` - The model to use (e.g., "gpt-4o", "gpt-3.5-turbo")
/// * `max_tokens` - Maximum number of tokens to generate
///
/// # Returns
///
/// * `Result<Value, Box<dyn Error>>` - The structured JSON response or an error
pub async fn get_structured_output_with_template(
    template_name: &str,
    context: &Value,
    schema: Value,
    model: &str,
    max_tokens: u32,
) -> Result<Value, Box<dyn Error>> {
    // Get the template and render it with the context
    let prompt = prompts::render_template(template_name, context)?;
    // println!("Prompt: {}", prompt);
    
    // Use the rendered template as the system prompt
    get_structured_output(&prompt, "", schema, model, max_tokens).await
}

/// Request a structured output from OpenAI using a template and deserialize it into a specific type.
///
/// # Type Parameters
///
/// * `T` - The type to deserialize the response into. Must implement DeserializeOwned.
///
/// # Arguments
///
/// * `template_name` - The name of the template in the templates.yaml file
/// * `context` - The context to render the template with
/// * `schema` - The JSON schema that defines the structure of the response
/// * `model` - The model to use (e.g., "gpt-4o", "gpt-3.5-turbo")
/// * `max_tokens` - Maximum number of tokens to generate
///
/// # Returns
///
/// * `Result<T, Box<dyn Error>>` - The deserialized structured response or an error
pub async fn get_structured_output_typed_with_template<T: DeserializeOwned>(
    template_name: &str,
    context: &Value,
    schema: Value,
    model: &str,
    max_tokens: u32,
) -> Result<T, Box<dyn Error>> {
    let json_value = get_structured_output_with_template(template_name, context, schema, model, max_tokens).await?;
    let typed_result = serde_json::from_value(json_value)?;
    Ok(typed_result)
}

/// Request a structured output from OpenAI using a template and a type's JSON schema.
///
/// # Type Parameters
///
/// * `T` - The type to deserialize the response into. Must implement DeserializeOwned and JsonSchema.
///
/// # Arguments
///
/// * `template_name` - The name of the template in the templates.yaml file
/// * `context` - The context to render the template with
/// * `model` - The model to use (e.g., "gpt-4o", "gpt-3.5-turbo")
/// * `max_tokens` - Maximum number of tokens to generate
///
/// # Returns
///
/// * `Result<T, Box<dyn Error>>` - The deserialized structured response or an error
pub async fn get_structured_output_from_template<T: DeserializeOwned + JsonSchema>(
    template_name: &str,
    context: &Value,
    model: &str,
    max_tokens: u32,
) -> Result<T, Box<dyn Error>> {
    let schema = schema_for!(T);
    let schema_value = serde_json::to_value(&schema)?;
    
    get_structured_output_typed_with_template::<T>(template_name, context, schema_value, model, max_tokens).await
}
