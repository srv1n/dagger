use anyhow::{anyhow, Result};
use async_broadcast::{Receiver, Sender};
use async_trait::async_trait;
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{oneshot, RwLock};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::core::errors::{DaggerError, Result as DaggerResult};
use crate::dag_flow::Cache;

/// A message in the pub/sub system
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    pub timestamp: NaiveDateTime,
    pub source: String,          // Node ID of the publishing agent
    pub channel: Option<String>, // Set by publish
    pub task_id: Option<String>, // Links to a specific task
    pub payload: Value,          // Task-specific data
    pub message_id: String,      // Unique identifier for the message
}

impl Message {
    pub fn new(source: String, payload: Value) -> Self {
        Self {
            timestamp: chrono::Utc::now().naive_utc(),
            source,
            channel: None,
            task_id: None,
            message_id: Uuid::new_v4().to_string(),
            payload,
        }
    }

    pub fn with_task_id(source: String, task_id: String, payload: Value) -> Self {
        Self {
            timestamp: chrono::Utc::now().naive_utc(),
            source,
            channel: None,
            task_id: Some(task_id),
            message_id: Uuid::new_v4().to_string(),
            payload,
        }
    }
}

/// Trait defining the behavior of a Pub/Sub agent
#[async_trait]
pub trait PubSubAgent: Send + Sync + 'static {
    /// Returns the agent's name
    fn name(&self) -> String;

    /// Returns the agent's description
    fn description(&self) -> String;

    /// Returns the list of channels this agent subscribes to
    fn subscriptions(&self) -> Vec<String>;

    /// Returns the list of channels this agent publishes to
    fn publications(&self) -> Vec<String>;

    /// Returns the JSON schema for validating incoming messages
    fn input_schema(&self) -> Value;

    /// Returns the JSON schema for validating outgoing messages
    fn output_schema(&self) -> Value;

    /// Processes an incoming message from a subscribed channel
    async fn process_message(
        &self,
        node_id: &str,
        channel: &str,
        message: &Message,
        executor: &mut PubSubExecutor,
        cache: &Cache,
    ) -> Result<()>;

    /// Validates input against the input schema
    fn validate_input(&self, message: &Value) -> Result<()> {
        let schema = self.input_schema();
        let compiled_schema = jsonschema::validator_for(&schema)
            .map_err(|e| anyhow!("Failed to compile input schema: {}", e))?;
        if let Err(_errors) = compiled_schema.validate(message) {
            return Err(anyhow!("Invalid input message schema"));
        }
        Ok(())
    }

    /// Validates output against the output schema
    fn validate_output(&self, message: &Value) -> Result<()> {
        let schema = self.output_schema();
        let compiled_schema = jsonschema::validator_for(&schema)
            .map_err(|e| anyhow!("Failed to compile output schema: {}", e))?;
        if let Err(_errors) = compiled_schema.validate(message) {
            return Err(anyhow!("Invalid output message schema"));
        }
        Ok(())
    }
}

/// Agent metadata for registration
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentMetadata {
    pub name: String,
    pub description: String,
    pub subscriptions: Vec<String>,
    pub publications: Vec<String>,
    pub input_schema: Value,
    pub output_schema: Value,
    pub registered_at: String,
}

/// Internal agent entry
#[derive(Clone)]
struct AgentEntry {
    pub agent: Arc<dyn PubSubAgent>,
    pub metadata: AgentMetadata,
    pub node_id: String,
}

/// Channel information
#[derive(Debug, Clone)]
struct ChannelInfo {
    sender: Sender<Message>,
    _receiver: Receiver<Message>, // Keep receiver to prevent channel from closing
}

/// Error types for pub/sub operations
#[derive(Debug, thiserror::Error)]
pub enum PubSubError {
    #[error("Channel not found: {0}")]
    ChannelNotFound(String),
    #[error("Agent not found: {0}")]
    AgentNotFound(String),
    #[error("Message validation failed: {0}")]
    ValidationError(String),
    #[error("Execution error: {0}")]
    ExecutionError(String),
}

impl From<PubSubError> for DaggerError {
    fn from(err: PubSubError) -> Self {
        DaggerError::execution("pubsub", &err.to_string())
    }
}

/// Configuration for pub/sub system
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PubSubConfig {
    pub max_messages: Option<u64>,
    pub timeout_seconds: Option<u64>,
    pub max_attempts: u32,
}

impl Default for PubSubConfig {
    fn default() -> Self {
        Self {
            max_messages: Some(1000),
            timeout_seconds: Some(3600),
            max_attempts: 3,
        }
    }
}

/// Workflow specification for pub/sub execution
#[derive(Debug, Clone)]
pub enum PubSubWorkflowSpec {
    /// Start with an initial message to a channel
    StartWith { channel: String, message: Message },
    /// Wait for external messages
    Reactive,
}

/// Main pub/sub executor
pub struct PubSubExecutor {
    agents: Arc<RwLock<HashMap<String, AgentEntry>>>,
    channels: Arc<RwLock<HashMap<String, ChannelInfo>>>,
    config: PubSubConfig,
    stopped: Arc<RwLock<bool>>,
}

impl PubSubExecutor {
    /// Create a new pub/sub executor
    pub fn new(config: Option<PubSubConfig>) -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            channels: Arc::new(RwLock::new(HashMap::new())),
            config: config.unwrap_or_default(),
            stopped: Arc::new(RwLock::new(false)),
        }
    }

    /// Register an agent with the executor
    pub async fn register_agent(&mut self, agent: Arc<dyn PubSubAgent>) -> Result<()> {
        let name = agent.name();
        let node_id = format!("{}_{}", name, Uuid::new_v4().to_string()[..8].to_string());

        let metadata = AgentMetadata {
            name: name.clone(),
            description: agent.description(),
            subscriptions: agent.subscriptions(),
            publications: agent.publications(),
            input_schema: agent.input_schema(),
            output_schema: agent.output_schema(),
            registered_at: chrono::Utc::now().to_rfc3339(),
        };

        let entry = AgentEntry {
            agent,
            metadata,
            node_id: node_id.clone(),
        };

        // Create channels for agent subscriptions
        for channel in &entry.metadata.subscriptions {
            self.ensure_channel_exists(channel).await?;
        }

        // Create channels for agent publications
        for channel in &entry.metadata.publications {
            self.ensure_channel_exists(channel).await?;
        }

        let mut agents = self.agents.write().await;
        agents.insert(node_id, entry);

        info!("Registered agent: {}", name);
        Ok(())
    }

    /// Ensure a channel exists, create if it doesn't
    async fn ensure_channel_exists(&self, channel_name: &str) -> Result<()> {
        let mut channels = self.channels.write().await;
        if !channels.contains_key(channel_name) {
            let (sender, receiver) = async_broadcast::broadcast(1000);
            let channel_info = ChannelInfo {
                sender,
                _receiver: receiver,
            };
            channels.insert(channel_name.to_string(), channel_info);
            debug!("Created channel: {}", channel_name);
        }
        Ok(())
    }

    /// Publish a message to a channel
    pub async fn publish(
        &mut self,
        channel: &str,
        mut message: Message,
        _cache: &Cache,
        _task_id: Option<String>,
    ) -> Result<()> {
        message.channel = Some(channel.to_string());

        let channels = self.channels.read().await;
        let channel_info = channels
            .get(channel)
            .ok_or_else(|| anyhow!("Channel not found: {}", channel))?;

        if let Err(e) = channel_info.sender.broadcast(message.clone()).await {
            warn!("Failed to publish message to {}: {}", channel, e);
            return Err(anyhow!("Failed to publish message: {}", e));
        }

        debug!(
            "Published message {} to channel {}",
            message.message_id, channel
        );
        Ok(())
    }

    /// Execute the pub/sub workflow
    pub async fn execute(
        &mut self,
        workflow: PubSubWorkflowSpec,
        cache: &Cache,
        cancel_rx: oneshot::Receiver<()>,
    ) -> DaggerResult<()> {
        info!("Starting pub/sub execution");

        // Start agent listeners
        let mut tasks = Vec::new();

        let agents = self.agents.read().await;
        for (node_id, entry) in agents.iter() {
            for subscription in &entry.metadata.subscriptions {
                let task = self.start_agent_listener(
                    node_id.clone(),
                    subscription.clone(),
                    entry.agent.clone(),
                    cache.clone(),
                );
                tasks.push(task);
            }
        }
        drop(agents);

        // Handle workflow specification
        match workflow {
            PubSubWorkflowSpec::StartWith { channel, message } => {
                info!("Starting workflow with message to channel: {}", channel);
                self.publish(&channel, message, cache, None).await?;
            }
            PubSubWorkflowSpec::Reactive => {
                info!("Starting reactive workflow - waiting for external messages");
            }
        }

        // Wait for cancellation or completion
        tokio::select! {
            _ = cancel_rx => {
                info!("Received cancellation signal");
                let mut stopped = self.stopped.write().await;
                *stopped = true;
            }
            _ = async {
                for task in tasks {
                    let _ = task.await;
                }
            } => {
                info!("All agent tasks completed");
            }
        }

        Ok(())
    }

    /// Start a listener for an agent on a specific channel
    fn start_agent_listener(
        &self,
        node_id: String,
        channel: String,
        agent: Arc<dyn PubSubAgent>,
        cache: Cache,
    ) -> tokio::task::JoinHandle<()> {
        let channels = self.channels.clone();
        let stopped = self.stopped.clone();

        tokio::spawn(async move {
            let mut receiver = {
                let channels_read = channels.read().await;
                if let Some(channel_info) = channels_read.get(&channel) {
                    channel_info.sender.new_receiver()
                } else {
                    error!("Channel not found: {}", channel);
                    return;
                }
            };

            info!("Agent {} listening on channel {}", node_id, channel);

            loop {
                tokio::select! {
                    message_result = receiver.recv() => {
                        match message_result {
                            Ok(message) => {
                                // Don't process messages from ourselves
                                if message.source == node_id {
                                    continue;
                                }

                                debug!(
                                    "Agent {} received message {} on channel {}",
                                    node_id, message.message_id, channel
                                );

                                // Create a mutable executor for the agent to use
                                let config = PubSubConfig::default();
                                let mut executor = PubSubExecutor::new(Some(config));

                                // Copy channels and agents to the new executor
                                {
                                    let channels_read = channels.read().await;
                                    let mut executor_channels = executor.channels.write().await;
                                    for (name, info) in channels_read.iter() {
                                        executor_channels.insert(name.clone(), info.clone());
                                    }
                                }

                                // Process the message
                                if let Err(e) = agent
                                    .process_message(&node_id, &channel, &message, &mut executor, &cache)
                                    .await
                                {
                                    error!(
                                        "Agent {} failed to process message {}: {}",
                                        node_id, message.message_id, e
                                    );
                                }
                            }
                            Err(e) => {
                                error!("Error receiving message on channel {}: {}", channel, e);
                                break;
                            }
                        }
                    }
                    _ = async {
                        let stopped_read = stopped.read().await;
                        if *stopped_read {
                            return;
                        }
                        // Small delay to prevent busy waiting
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    } => {
                        break;
                    }
                }
            }

            info!("Agent {} stopped listening on channel {}", node_id, channel);
        })
    }

    /// Get list of registered agents
    pub async fn list_agents(&self) -> Vec<AgentMetadata> {
        let agents = self.agents.read().await;
        agents
            .values()
            .map(|entry| entry.metadata.clone())
            .collect()
    }

    /// Get list of available channels
    pub async fn list_channels(&self) -> Vec<String> {
        let channels = self.channels.read().await;
        channels.keys().cloned().collect()
    }

    /// Stop the executor
    pub async fn stop(&self) {
        let mut stopped = self.stopped.write().await;
        *stopped = true;
        info!("PubSub executor stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TestAgent {
        name: String,
    }

    #[async_trait]
    impl PubSubAgent for TestAgent {
        fn name(&self) -> String {
            self.name.clone()
        }

        fn description(&self) -> String {
            "Test agent".to_string()
        }

        fn subscriptions(&self) -> Vec<String> {
            vec!["test_input".to_string()]
        }

        fn publications(&self) -> Vec<String> {
            vec!["test_output".to_string()]
        }

        fn input_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "message": {"type": "string"}
                }
            })
        }

        fn output_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "response": {"type": "string"}
                }
            })
        }

        async fn process_message(
            &self,
            node_id: &str,
            _channel: &str,
            message: &Message,
            executor: &mut PubSubExecutor,
            cache: &Cache,
        ) -> Result<()> {
            let response_payload = json!({
                "response": format!("Processed: {}", message.payload["message"].as_str().unwrap_or(""))
            });
            let response = Message::new(node_id.to_string(), response_payload);
            executor
                .publish("test_output", response, cache, None)
                .await?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_basic_pubsub() {
        let mut executor = PubSubExecutor::new(None);

        let agent = Arc::new(TestAgent {
            name: "test_agent".to_string(),
        });

        executor.register_agent(agent).await.unwrap();

        let agents = executor.list_agents().await;
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "test_agent");
    }
}
