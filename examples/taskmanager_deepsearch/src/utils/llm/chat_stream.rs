use std::error::Error;
use std::io::{stdout, Write};

use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestUserMessageArgs,
    CreateChatCompletionRequestArgs,
};
use futures::StreamExt;
use crate::utils::llm::client::CLIENT;



/// Stream a chat completion from OpenAI and process the response chunks.
///
/// # Arguments
///
/// * `messages` - A vector of chat messages to send to the API
/// * `model` - The model to use (e.g., "gpt-4o", "gpt-3.5-turbo")
/// * `max_tokens` - Maximum number of tokens to generate
///
/// # Returns
///
/// * `Result<String, Box<dyn Error>>` - The complete response text or an error
///
/// # Example
///
/// ```
/// use async_openai::types::{ChatCompletionRequestUserMessageArgs, ChatCompletionRequestMessage};
/// use crate::utils::llm::chat_stream::stream_chat_completion;
///
/// async fn example() -> Result<(), Box<dyn std::error::Error>> {
///     let messages = vec![
///         ChatCompletionRequestUserMessageArgs::default()
///             .content("Hello, how are you?")
///             .build()?
///             .into()
///     ];
///     
///     let response = stream_chat_completion(messages, "gpt-3.5-turbo", 512).await?;
///     println!("{}", response);
///     Ok(())
/// }
/// ```
pub async fn stream_chat_completion(
    messages: Vec<ChatCompletionRequestMessage>,
    model: &str,
    max_tokens: u32,
) -> Result<String, Box<dyn Error>> {
    let client = &CLIENT;

    let request = CreateChatCompletionRequestArgs::default()
        .model(model)
        .max_tokens(max_tokens)
        .messages(messages)
        .build()?;

    let mut stream = client.chat().create_stream(request).await?;

    let mut response_content = String::new();
    let mut lock = stdout().lock();
    
    while let Some(result) = stream.next().await {
        match result {
            Ok(response) => {
                for chat_choice in response.choices.iter() {
                    if let Some(content) = &chat_choice.delta.content {
                        write!(lock, "{}", content).unwrap();
                        response_content.push_str(content);
                    }
                }
            }
            Err(err) => {
                return Err(Box::new(err));
            }
        }
        stdout().flush()?;
    }

    Ok(response_content)
}

/// A simpler function to stream a chat completion with just a user prompt.
///
/// # Arguments
///
/// * `prompt` - The user's prompt text
/// * `model` - The model to use (e.g., "gpt-4o", "gpt-3.5-turbo")
/// * `max_tokens` - Maximum number of tokens to generate
///
/// # Returns
///
/// * `Result<String, Box<dyn Error>>` - The complete response text or an error
pub async fn stream_chat_prompt(
    prompt: &str,
    model: &str,
    max_tokens: u32,
) -> Result<String, Box<dyn Error>> {
    let messages = vec![
        ChatCompletionRequestUserMessageArgs::default()
            .content(prompt)
            .build()?
            .into(),
    ];

    stream_chat_completion(messages, model, max_tokens).await
}
