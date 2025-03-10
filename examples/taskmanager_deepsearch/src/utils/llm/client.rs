use async_openai::{config::OpenAIConfig, Client};
use once_cell::sync::Lazy;
use std::env;
use std::sync::Arc;
use std::collections::HashMap;
use std::sync::RwLock;
/// A singleton instance of the OpenAI client.
/// This allows us to create the client once and reuse it throughout the application.
lazy_static::lazy_static! {    
      
    pub static ref CLIENT:Client<OpenAIConfig> = {
      
        let openai_config = OpenAIConfig::default().with_api_key(env::var("OPEN_AI_API_KEY").expect("$OPEN AI API KEY is not set")).with_api_base(env::var("OPEN_AI_URL").expect("$OPEN AI URL is not set"));
       Client::with_config(openai_config)
    };
   
}


