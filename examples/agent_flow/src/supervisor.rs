use async_openai::{
    types::{
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
        ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequestArgs,
        FunctionCall as OpenAIFunctionCall, AssistantMessageContent,
    },
    Client,
};
use anyhow::{Error, Result};
use dagger::{
    insert_value, parse_input_from_name, Cache, DagExecutor, Node, NodeAction, SerializableData,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{info, warn};

/// Supervisor action that manages dynamic DAG updates with LLM integration
#[derive(Default)]
pub struct SupervisorStep {
    /// OpenAI client for LLM calls
    client: Client,
}

impl SupervisorStep {
    pub fn new() -> Self {
        Self {
            client: Client::new(), // Uses OPENAI_API_KEY from env by default
        }
    }

    /// Validates and parses instruction JSON
    fn validate_instruction(instruction_str: &str) -> Result<SupervisorInstruction> {
        let instruction_value: Value = serde_json::from_str(instruction_str)
            .map_err(|e| anyhow!("Invalid instruction format: {}", e))?;

        let action = instruction_value
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing or invalid 'action' field"))?
            .to_string();

        if !["retrieve_info", "human_review"].contains(&action.as_str()) {
            return Err(anyhow!("Unsupported action: {}", action));
        }

        Ok(SupervisorInstruction {
            action,
            params: instruction_value.get("params").cloned(),
            priority: instruction_value
                .get("priority")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            timestamp: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// Calls the LLM to generate travel itinerary suggestions
    async fn call_llm(
        &self,
        task: &str,
        instructions: Vec<String>,
        iteration: usize,
    ) -> Result<Vec<SupervisorInstruction>> {
        let system_prompt = "You are a travel planning assistant. Your job is to suggest activities for a trip based on the user's task and any provided instructions. Respond with function calls to 'add_activity' to suggest activities, or 'request_human_review' to ask for user input.";
        
        let user_prompt = format!(
            "Task: {}\nCurrent iteration: {}\nPrevious instructions: {:?}\nSuggest the next steps for the itinerary.",
            task, iteration, instructions
        );

        // Define OpenAI function spec
        let functions = vec![
            async_openai::types::FunctionObject {
                name: "add_activity".to_string(),
                description: Some("Add a travel activity to the itinerary".to_string()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The activity to add (e.g., 'Visit Eiffel Tower')"
                        }
                    },
                    "required": ["query"]
                })),
            },
            async_openai::types::FunctionObject {
                name: "request_human_review".to_string(),
                description: Some("Request human review of the current itinerary".to_string()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {}
                })),
            },
        ];

        // Build the chat completion request
        let request = CreateChatCompletionRequestArgs::default()
            .model("gpt-4o") // Use a suitable model
            .messages(vec![
                ChatCompletionRequestSystemMessageArgs::default()
                    .content(system_prompt)
                    .build()?
                    .into(),
                ChatCompletionRequestUserMessageArgs::default()
                    .content(user_prompt)
                    .build()?
                    .into(),
            ])
            .functions(functions.clone())
            .function_call(Some("auto".into())) // Auto-call functions
            .max_tokens(500u16)
            .build()?;

        // Call the LLM
        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|e| anyhow!("LLM call failed: {}", e))?;

        // Process the response
        let mut instructions = Vec::new();
        if let Some(choice) = response.choices.get(0) {
            if let Some(function_call) = choice.message.function_call.clone() {
                instructions.push(self.process_function_call(&function_call)?);
            }
            // Handle multiple function calls if the model supports it in the future
        }

        Ok(instructions)
    }

    /// Processes an OpenAI function call into a SupervisorInstruction
    fn process_function_call(&self, call: &OpenAIFunctionCall) -> Result<SupervisorInstruction> {
        let action = match call.name.as_str() {
            "add_activity" => "retrieve_info",
            "request_human_review" => "human_review",
            _ => return Err(anyhow!("Unknown function: {}", call.name)),
        };

        let params: Option<Value> = call.arguments.clone().map(|args| {
            serde_json::from_str(&args).unwrap_or_else(|e| {
                warn!("Failed to parse function args: {}", e);
                Value::Null
            })
        });

        Ok(SupervisorInstruction {
            action: action.to_string(),
            params,
            priority: Some(1), // Default priority
            timestamp: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// Processes the instruction queue and LLM suggestions
    async fn process_instructions(
        &self,
        executor: &mut DagExecutor,
        node: &Node,
        cache: &Cache,
        dag_name: &str,
        task: &str,
    ) -> Result<()> {
        // Get current iteration
        let iteration: usize =
            parse_input_from_name(cache, "iteration".to_string(), &node.inputs).unwrap_or(0);

        // Fetch pending instructions from cache
        let mut instructions = {
            let cache_read = cache.read().unwrap();
            if let Some(global) = cache_read.get("global") {
                global
                    .iter()
                    .filter(|(k, _)| k.starts_with("instruction_"))
                    .map(|(_, v)| v.value.clone())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        };

        // Call LLM for suggestions if no instructions or on first iteration
        if instructions.is_empty() || iteration == 0 {
            let llm_instructions = self.call_llm(task, instructions.clone(), iteration).await?;
            instructions.extend(llm_instructions.into_iter().map(|i| serde_json::to_string(&i).unwrap()));
        }

        // Validate and process instructions
        let mut valid_instructions = Vec::new();
        for instruction_str in &instructions {
            match Self::validate_instruction(instruction_str) {
                Ok(instruction) => valid_instructions.push(instruction),
                Err(e) => {
                    warn!("Skipping invalid instruction: {}", e);
                    insert_value(
                        cache,
                        "global",
                        &format!("invalid_instruction_{}", chrono::Utc::now().timestamp()),
                        serde_json::json!({
                            "instruction": instruction_str,
                            "error": e.to_string(),
                            "timestamp": chrono::Utc::now().to_rfc3339(),
                        }),
                    )?;
                }
            }
        }

        // Sort by priority and timestamp
        valid_instructions.sort_by(|a, b| {
            b.priority.unwrap_or(0).cmp(&a.priority.unwrap_or(0))
                .then_with(|| a.timestamp.cmp(&b.timestamp))
        });

        // Process instructions
        for instruction in valid_instructions {
            match instruction.action.as_str() {
                "retrieve_info" => {
                    let node_id = format!("retrieve_activities_{}", cuid2::create_id());
                    executor.add_node(
                        dag_name,
                        node_id.clone(),
                        Arc::new(dagger::InfoRetrievalAgent::new()),
                        vec![node.id.clone()],
                    )?;
                    if let Some(params) = instruction.params {
                        insert_value(cache, &node_id, "action_params", params)?;
                    }
                }
                "human_review" => {
                    let node_id = format!("human_check_{}", cuid2::create_id());
                    executor.add_node(
                        dag_name,
                        node_id.clone(),
                        Arc::new(dagger::HumanInterrupt::new()),
                        vec![node.id.clone()],
                    )?;
                }
                _ => warn!("Skipping unknown action: {}", instruction.action),
            }
        }

        // Clear processed instructions
        {
            let mut cache_write = cache.write().unwrap();
            if let Some(global) = cache_write.get_mut("global") {
                global.retain(|k, _| !k.starts_with("instruction_"));
            }
        }

        // Periodic human review based on config
        let review_frequency = executor
            .graphs
            .read()
            .map_err(|e| anyhow!("Failed to acquire graphs read lock: {}", e))?
            .get(dag_name)
            .and_then(|g| g.config.as_ref())
            .and_then(|c| c.review_frequency)
            .unwrap_or(5);

        if review_frequency > 0 && iteration > 0 && iteration % review_frequency as usize == 0 {
            let node_id = format!("human_review_{}", cuid2::create_id());
            executor.add_node(
                dag_name,
                node_id.clone(),
                Arc::new(dagger::HumanInterrupt::new()),
                vec![node.id.clone()],
            )?;
        }

        Ok(())
    }
}

#[async_trait]
impl NodeAction for SupervisorStep {
    fn name(&self) -> String {
        "supervisor_step".to_string()
    }

    async fn execute(&self, executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
        let dag_name = executor
            .graphs
            .read()
            .unwrap()
            .iter()
            .find(|(_, g)| g.nodes.iter().any(|n| n.id == node.id))
            .map(|(n, _)| n.clone())
            .ok_or_else(|| anyhow!("DAG not found for node {}", node.id))?;

        let task = executor
            .prebuilt_dags
            .read()
            .unwrap()
            .get(&dag_name)
            .map(|_| dag_name.clone())
            .ok_or_else(|| anyhow!("Task not found for DAG {}", dag_name))?;

        // Process instructions and LLM suggestions
        self.process_instructions(executor, node, cache, &dag_name, &task).await?;

        // Update state
        let iteration: usize =
            parse_input_from_name(cache, "iteration".to_string(), &node.inputs).unwrap_or(0);
        let next_iteration = iteration + 1;
        insert_value(cache, &node.id, "iteration", next_iteration)?;
        insert_value(
            cache,
            &node.id,
            "timestamp",
            chrono::Utc::now().to_rfc3339(),
        )?;

        info!("Supervisor step completed for iteration {}", next_iteration);
        Ok(())
    }
}

/// Represents a validated instruction for the supervisor
#[derive(Debug, Serialize, Deserialize)]
pub struct SupervisorInstruction {
    pub action: String,
    pub params: Option<Value>,
    pub priority: Option<u32>,
    pub timestamp: String,
}