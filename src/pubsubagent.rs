use crate::{async_trait, Cache, ExecutionTree, HumanTimeoutAction, NodeSnapshot, RetryStrategy};
use crate::{NodeExecutionOutcome, SerializableData};
use anyhow::anyhow;
use anyhow::Result;
use async_broadcast::{Receiver, Sender};
use chrono::NaiveDateTime;
use petgraph::graph::DiGraph;
use petgraph::visit::EdgeRef;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::Value;
use sled::Db;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, error, info, warn};
/// Trait defining the behavior of a Pub/Sub agent in the dagger library.
///
/// Agents implementing this trait can subscribe to channels, publish to channels,
/// and process incoming messages asynchronously. The trait ensures thread safety
/// and provides schema validation capabilities.
///
/// # Requirements
/// - Must be `Send + Sync + 'static` for safe concurrent execution across threads.
/// - Methods should minimize cloning and avoid unnecessary allocations for performance.
#[async_trait]
pub trait PubSubAgent: Send + Sync + 'static {
    ///name
    fn name(&self) -> String;
    /// # Returns
    /// A `Vec<String>` containing channel names (e.g., `"new_tasks"`).
    fn subscriptions(&self) -> Vec<String>;

    fn description(&self) -> String;

    /// Returns the list of channels this agent publishes to.
    ///
    /// # Returns
    /// A `Vec<String>` containing channel names (e.g., `"results"`).
    fn publications(&self) -> Vec<String>;

    /// Returns the JSON schema for validating incoming messages.
    ///
    /// # Returns
    /// A `serde_json::Value` representing the schema (e.g., `{"type": "object", "properties": {...}}`).
    fn input_schema(&self) -> Value;

    /// Returns the JSON schema for validating outgoing messages.
    ///
    /// # Returns
    /// A `serde_json::Value` representing the schema.
    fn output_schema(&self) -> Value;

    /// Processes an incoming message from a subscribed channel.
    ///
    /// # Arguments
    /// - `channel`: The name of the channel the message was received from.
    /// - `message`: The incoming message as a `serde_json::Value`.
    /// - `executor`: A mutable reference to the `PubSubExecutor` for publishing or state management.
    ///
    /// # Returns
    /// A `Result<()>` indicating success or failure of message processing.
    ///
    /// # Tracing
    /// Logs entry and exit points with message details for traceability.
    async fn process_message(
        &self,
        node_id: &str, // New parameter: the agent's assigned node_id
        channel: &str,
        message: Message,
        executor: &mut PubSubExecutor,
        cache: Arc<Cache>,
    ) -> Result<()>;

    fn validate_input(&self, message: &Value) -> Result<()> {
        let schema = self.input_schema();
        let compiled_schema = jsonschema::validator_for(&schema)
            .map_err(|e| anyhow::anyhow!("Failed to compile input schema: {}", e))?;
        if let Err(errors) = compiled_schema.validate(message) {
            warn!("Input validation failed for agent {}: {}", self.name(), errors);
            return Err(anyhow::anyhow!("Invalid input: {}", errors));
        }
        Ok(())
    }

    fn validate_output(&self, message: &Value) -> Result<()> {
        let schema = self.output_schema();
        let compiled_schema = jsonschema::validator_for(&schema)
            .map_err(|e| anyhow::anyhow!("Failed to compile output schema: {}", e))?;
        if let Err(errors) = compiled_schema.validate(message) {
            warn!("Output validation failed for agent {}: {}", self.name(), errors);
            return Err(anyhow::anyhow!("Invalid output: {}", errors));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    pub timestamp: NaiveDateTime,
    pub source: String, // The node_id of the publishing agent or initial publish
    pub channel: Option<String>, // Optional, set by publish
    pub payload: Value, // The actual message content
}

impl Message {
    pub fn new(source: String, payload: Value) -> Self {
        Self {
            timestamp: chrono::Local::now().naive_local(),
            source,
            channel: None,
            payload,
        }
    }
}

#[derive(Clone)]
struct AgentEntry {
    pub agent: Arc<dyn PubSubAgent>,
    pub metadata: AgentMetadata,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentMetadata {
    pub name: String,
    pub subscriptions: Vec<String>,
    pub publications: Vec<String>,
    pub input_schema: Value,
    pub output_schema: Value,
    pub registered_at: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PubSubConfig {
    pub max_messages: Option<u64>,
    pub timeout_seconds: Option<u64>,
    pub human_wait_minutes: Option<u32>,
    pub human_timeout_action: HumanTimeoutAction,
    pub max_tokens: Option<u64>,
    pub retry_strategy: RetryStrategy,
    pub max_attempts: u32,
}

impl Default for PubSubConfig {
    fn default() -> Self {
        Self {
            max_messages: Some(1000),
            timeout_seconds: Some(3600),
            human_wait_minutes: None,
            human_timeout_action: HumanTimeoutAction::Pause,
            max_tokens: None,
            retry_strategy: RetryStrategy::default(),
            max_attempts: 3,
        }
    }
}

impl PubSubConfig {
    pub fn validate(&self) -> Result<()> {
        if let Some(timeout) = self.timeout_seconds {
            if timeout == 0 {
                return Err(anyhow!("timeout_seconds must be greater than 0"));
            }
            if timeout > 86400 {
                return Err(anyhow!("timeout_seconds cannot exceed 24 hours"));
            }
        }
        if let Some(max_msgs) = self.max_messages {
            if max_msgs == 0 {
                return Err(anyhow!("max_messages must be greater than 0"));
            }
        }
        Ok(())
    }
}

// Standardized Error System for PubSubExecutor
#[derive(Debug, thiserror::Error)]
pub enum PubSubError {
    #[error("Lock error: {0}")]
    LockError(String),

    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    #[error("Channel not found: {0}")]
    ChannelNotFound(String),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Database error: {0}")]
    DatabaseError(#[from] sled::Error),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Execution error: {0}")]
    ExecutionError(String),

    #[error("Cancelled")]
    Cancelled,

    #[error("Timeout exceeded")]
    TimeoutExceeded,

    #[error("No agents registered")]
    NoAgentsRegistered,
}

// Execution Report for PubSubExecutor
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PubSubExecutionReport {
    pub agent_outcomes: Vec<NodeExecutionOutcome>, // Outcomes per agent processing attempt
    pub overall_success: bool,                     // True if no critical failures
    pub error: Option<String>,                     // Consolidated error message if failed
}

impl PubSubExecutionReport {
    fn new(outcomes: Vec<NodeExecutionOutcome>, success: bool, error: Option<String>) -> Self {
        Self {
            agent_outcomes: outcomes,
            overall_success: success,
            error,
        }
    }
}

// Workflow specification for PubSubExecutor (similar to DagExecutor's WorkflowSpec)
#[derive(Debug, Clone)]
pub enum PubSubWorkflowSpec {
    /// Start a Pub/Sub workflow with an initial message
    EventDriven {
        channel: String,
        initial_message: Message,
    },
}

// Add agent factory support
pub type AgentFactory = Arc<dyn Fn() -> Arc<dyn PubSubAgent> + Send + Sync>;

/// Executor for managing Pub/Sub workflows in the dagger library.
///
/// This struct orchestrates the execution of `PubSubAgent`s, routing messages between
/// subscribed and published channels, and maintains an execution tree for traceability.
///
/// # Fields
/// - `agents`: Thread-safe registry of registered agents.
/// - `channels`: Thread-safe map of channel names to MPMC sender/receiver pairs.
/// - `config`: Configuration for execution limits and behavior.
/// - `sled_db`: Persistent storage for execution trees and logs.
/// - `execution_tree`: Graph tracking message flows for DOT visualization.
/// - `stopped`: Flag to halt execution.
/// - `start_time`: Timestamp of executor initialization.

// PubSubExecutor struct (updated with execute method)
pub struct PubSubExecutor {
    pub agent_registry: Arc<RwLock<HashMap<String, AgentEntry>>>,
    pub agent_factories: Arc<RwLock<HashMap<String, AgentFactory>>>,
    pub channels: Arc<RwLock<HashMap<String, (Sender<Message>, Receiver<Message>)>>>,
    pub config: PubSubConfig,
    pub sled_db: Db,
    pub execution_tree: Arc<RwLock<ExecutionTree>>,
    pub cache: Arc<Cache>,
    pub stopped: Arc<RwLock<bool>>,
    pub start_time: NaiveDateTime,
    pub current_agent_id: Option<String>,
}

impl PubSubExecutor {
    /// Creates a new PubSubExecutor instance asynchronously.
    ///
    /// # Arguments
    /// * `config` - Optional configuration for the executor
    /// * `sled_path` - Path to the sled database
    /// * `cache` - Shared cache instance
    ///
    /// # Returns
    /// A Result containing the new PubSubExecutor instance
    pub async fn new(
        config: Option<PubSubConfig>,
        sled_path: &str,
        cache: Arc<Cache>,
    ) -> Result<Self> {
        let sled_db = sled::open(sled_path)?;
        let config = config.unwrap_or_default();
        config
            .validate()
            .map_err(|e| PubSubError::ValidationError(e.to_string()))?;

        let executor = Self {
            agent_registry: Arc::new(RwLock::new(HashMap::new())),
            agent_factories: Arc::new(RwLock::new(HashMap::new())),
            channels: Arc::new(RwLock::new(HashMap::new())),
            config: config.clone(),
            sled_db,
            execution_tree: Arc::new(RwLock::new(DiGraph::new())),
            cache: cache.clone(),
            stopped: Arc::new(RwLock::new(false)),
            start_time: chrono::Local::now().naive_local(),
            current_agent_id: None,
        };

        // Attempt to load persisted state
        if let Err(e) = executor.load_state(&cache).await {
            warn!("Failed to load persisted state: {}. Starting fresh.", e);
        }

        info!(
            sled_path = sled_path,
            max_messages = ?config.max_messages,
            "Initialized PubSubExecutor"
        );

        Ok(executor)
    }

    /// Ensures a channel exists, creating it if necessary.
    async fn ensure_channel(&self, channel: &str) -> Result<()> {
        let mut channels = self.channels.write().await;
        if !channels.contains_key(channel) {
            let capacity = self.config.max_messages.unwrap_or(1000) as usize;
            let (mut tx, rx) = async_broadcast::broadcast(capacity);
            tx.set_overflow(true); // Drop oldest messages when full
            channels.insert(channel.to_string(), (tx, rx));
            info!(channel = %channel, capacity = capacity, "Created new channel dynamically");

            let metadata = json!({
                "channel": channel,
                "capacity": capacity,
                "created_at": chrono::Local::now().to_rfc3339(),
                "active_subscribers": 0
            });
            let channels_tree = self.sled_db.open_tree("pubsub_channels")?;
            channels_tree.insert(channel.as_bytes(), serde_json::to_vec(&metadata)?)?;
        } else {
            let channels_tree = self.sled_db.open_tree("pubsub_channels")?;
            if let Some(existing) = channels_tree.get(channel.as_bytes())? {
                let mut metadata: Value = serde_json::from_slice(&existing)?;
                let active_subscribers = channels.get(channel).unwrap().0.receiver_count() as u64;
                metadata["active_subscribers"] = json!(active_subscribers);
                info!(channel = %channel, active_subscribers = active_subscribers, "Channel metadata updated");
                channels_tree.insert(channel.as_bytes(), serde_json::to_vec(&metadata)?)?;
            }
        }
        Ok(())
    }

    pub async fn subscribe_agent(&self, agent_name: &str, channel: &str) -> Result<()> {
        self.ensure_channel(channel).await?;
        let mut registry = self.agent_registry.write().await;
        let entry = registry
            .get_mut(agent_name)
            .ok_or(PubSubError::AgentNotFound(agent_name.to_string()))?;
        entry.metadata.subscriptions.push(channel.to_string());
        info!(agent = %agent_name, channel = %channel, "Agent subscribed to channel");
        Ok(())
    }

    /// Cleans up channels with no active subscribers.
    async fn cleanup_channels(&self) -> Result<()> {
        let agents = self.agent_registry.read().await;
        let mut channels = self.channels.write().await;
        let channels_tree = self.sled_db.open_tree("pubsub_channels")?;

        let mut active_channels = HashSet::new();
        for (_, entry) in agents.iter() {
            for sub in entry.metadata.subscriptions.iter() {
                active_channels.insert(sub.clone());
            }
            for publ in entry.metadata.publications.iter() {
                active_channels.insert(publ.clone());
            }
        }

        let mut removed = Vec::new();
        channels.retain(|channel, (tx, _)| {
            let is_active = active_channels.contains(channel);
            if !is_active && tx.receiver_count() == 0 {
                removed.push(channel.clone());
                false // Remove the channel
            } else {
                true // Keep the channel
            }
        });

        for channel in removed {
            channels_tree.remove(channel.as_bytes())?;
            info!(channel = %channel, "Cleaned up unused channel");
        }

        Ok(())
    }

    //  pub fn new_with_registry(config: Option<PubSubConfig>, sled_path: &str, cache: Cache, registry: AgentRegistry) -> Result<Self> {
    //     let mut executor = Self::new(config, sled_path, cache)?;
    //     executor.agent_registry = registry.agents;
    //     Ok(executor)
    // }

    /// Registers a PubSub agent with validation and proper error handling
    pub async fn register_agent(&mut self, agent: Arc<dyn PubSubAgent>) -> Result<(), PubSubError> {
        let subscriptions = agent.subscriptions();
        if subscriptions.is_empty() {
            return Err(PubSubError::ValidationError(
                "Agent must have at least one subscription".to_string(),
            ));
        }

        let name = subscriptions[0].clone();
        let publications = agent.publications();

        // Ensure channels exist
        for channel in subscriptions.iter().chain(publications.iter()) {
            self.ensure_channel(channel)
                .await
                .map_err(|e| PubSubError::ExecutionError(e.to_string()))?;
        }

        let metadata = AgentMetadata {
            name: name.clone(),
            subscriptions: subscriptions.clone(),
            publications: publications.clone(),
            input_schema: agent.input_schema(),
            output_schema: agent.output_schema(),
            registered_at: chrono::Local::now().to_rfc3339(),
        };

        // Register in memory
        {
            let mut registry = self.agent_registry.write().await;
            registry.insert(
                name.clone(),
                AgentEntry {
                    agent: agent.clone(),
                    metadata: metadata.clone(),
                },
            );
        }

        // Persist to storage
        let agents_tree = self.sled_db.open_tree("pubsub_agents")?;
        agents_tree.insert(name.as_bytes(), serde_json::to_vec(&metadata)?)?;

        info!(agent_name = %name, "Registered Pub/Sub agent");
        Ok(())
    }
    pub async fn publish(
        &self,
        channel: &str,
        mut message: Message,
        cache: &Cache,
    ) -> Result<String, PubSubError> {
        if *self.stopped.read().await {
            return Err(PubSubError::ExecutionError("Executor is stopped".to_string()));
        }

        let elapsed = chrono::Local::now().naive_local() - self.start_time;
        if let Some(timeout) = self.config.timeout_seconds {
            if elapsed.num_seconds() > timeout as i64 {
                return Err(PubSubError::TimeoutExceeded);
            }
        }

        // Validate payload against subscribing agents' schemas
        let registry = self.agent_registry.read().await;
        for (_, entry) in registry.iter() {
            if entry.metadata.subscriptions.contains(&channel.to_string()) {
                let schema = entry.metadata.input_schema.clone();
                let compiled_schema = jsonschema::validator_for(&schema)
                    .map_err(|e| PubSubError::ValidationError(e.to_string()))?;
                if let Err(errors) = compiled_schema.validate(&message.payload) {
                    return Err(PubSubError::ValidationError(format!(
                        "Message payload validation failed: {}",
                        errors
                    )));
                }
            }
        }

        message.channel = Some(channel.to_string());
        self.ensure_channel(channel)
            .await
            .map_err(|e| PubSubError::ExecutionError(e.to_string()))?;
        let channels = self.channels.read().await;
        let (sender, _) = channels
            .get(channel)
            .ok_or(PubSubError::ChannelNotFound(channel.to_string()))?;

        let node_id = self.get_current_agent_id().unwrap_or("default_agent".to_string());
      
        let messages_tree = self.sled_db.open_tree("pubsub_messages")?;
        message.source = node_id.clone();
        messages_tree.insert(node_id.as_bytes(), serde_json::to_vec(&message)?)?;

        if let Some(max_msgs) = self.config.max_messages {
            if sender.len() >= max_msgs as usize {
                warn!(
                    channel = channel,
                    capacity = max_msgs,
                    current_len = sender.len(),
                    "Channel at capacity, dropping oldest message"
                );
                let mut rx = sender.new_receiver();
                if let Err(_) = rx.try_recv() {}
            }
        }

         let max_attempts = self.config.max_attempts;
        let mut attempts = max_attempts;

        while attempts > 0 {
            match sender.broadcast(message.clone()).await {
                Ok(_) => {
                    // crate::insert_value(&cache, channel, &msg_id, &message.payload)
                    //     .map_err(|e| PubSubError::ExecutionError(e.to_string()))?;

                    info!(
                        channel = channel, 
                        node_id = %node_id, 
                        "Published message successfully"
                    );
                    return Ok(node_id);
                }
                Err(e) => {
                    attempts -= 1;
                    
                    if attempts > 0 {
                        let delay = match self.config.retry_strategy {
                            RetryStrategy::Exponential {
                                initial_delay_secs,
                                max_delay_secs,
                                multiplier,
                            } => {
                                let attempt = max_attempts - attempts;
                                (initial_delay_secs as f64 * multiplier.powi(attempt as i32))
                                    .min(max_delay_secs as f64)
                                    as u64
                            }
                            RetryStrategy::Linear { delay_secs } => delay_secs,
                            RetryStrategy::Immediate => 0,
                        };
                        
                        warn!(
                            channel = channel,
                            attempt = max_attempts - attempts + 1,
                            max_attempts = max_attempts,
                            delay_secs = delay,
                            error = %e,
                            "Publish attempt failed, retrying"
                        );
                        
                        if delay > 0 {
                            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                        }
                    } else {
                        error!(
                            channel = channel,
                            max_attempts = max_attempts,
                            error = %e,
                            "Failed to publish message after all retry attempts"
                        );
                        
                        return Err(PubSubError::ExecutionError(format!(
                            "Failed to broadcast after {} attempts: {}",
                            max_attempts, e
                        )));
                    }
                }
            }
        }
        unreachable!()
    }
    pub async fn get_agent_metadata(&self, name: &str) -> Option<AgentMetadata> {
        let registry = self.agent_registry.read().await;
        registry.get(name).map(|entry| entry.metadata.clone())
    }

    /// Executes a Pub/Sub workflow based on a specification.
    ///
    /// # Arguments
    /// - `spec`: The workflow specification (e.g., initial message to kickstart execution).
    /// - `cache`: Shared cache for state persistence.
    /// - `cancel_rx`: Receiver for cancellation signal.
    ///
    /// # Returns
    /// A `Result` containing a `PubSubExecutionReport` or a `PubSubError`.

    pub async fn execute(
        &mut self,
        spec: PubSubWorkflowSpec,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<(PubSubExecutionReport, mpsc::Receiver<NodeExecutionOutcome>), PubSubError> {
        match spec {
            PubSubWorkflowSpec::EventDriven {
                channel,
                initial_message,
            } => {
                let agents = self.agent_registry.read().await;
                if agents.is_empty() {
                    return Err(PubSubError::NoAgentsRegistered);
                }

                let initial_node_id = format!("initial_{}", chrono::Utc::now().timestamp_millis());
                let initial_msg = Message {
                    timestamp: chrono::Local::now().naive_local(),
                    source: initial_node_id.clone(),
                    channel: Some(channel.clone()),
                    payload: initial_message.payload,
                };
                self.publish(&channel, initial_msg, &self.cache).await?;
                let (outcome_tx, mut outcome_rx) = mpsc::channel::<NodeExecutionOutcome>(100);
                let processed_messages = Arc::new(RwLock::new(HashSet::new()));
                let agents_clone = Arc::clone(&self.agent_registry);
                let channels_clone = Arc::clone(&self.channels);
                let stopped_clone = Arc::clone(&self.stopped);
                let cache = self.cache.clone();
                let agent_configs: Vec<(String, Arc<dyn PubSubAgent>, Vec<String>)> = {
                    agents
                        .iter()
                        .map(|(name, entry)| {
                            (
                                name.clone(),
                                entry.agent.clone(),
                                entry.metadata.subscriptions.clone(),
                            )
                        })
                        .collect()
                };

                let result = tokio::select! {
                    result = async {
                        let mut handles = Vec::new();
                        for (name, agent, subscriptions) in agent_configs {
                            let agent_node_id = format!("{}_instance", name); // Stable ID per agent
                            for sub in subscriptions {
                                let rx = {
                                    let channels_guard = channels_clone.read().await;
                                    channels_guard.get(&sub).map(|(_, rx)| rx.clone())
                                };
                                if let Some(mut rx_clone) = rx {
                                    let agent_clone = agent.clone();
                                    let executor_clone = self.clone();
                                    let channel_name = sub.clone();
                                    let name_clone = name.clone();
                                    let processed_messages_clone = Arc::clone(&processed_messages);
                                    let outcome_tx_clone = outcome_tx.clone();
                                    let cache = cache.clone();
                                    let agent_node_id = agent_node_id.clone();
                                    let handle = tokio::spawn(async move {
                                        while !*executor_clone.stopped.read().await {
                                            match rx_clone.recv().await {
                                                Ok(message) => {
                                                    let msg_id = format!("msg_{}_{}", channel_name, chrono::Utc::now().timestamp_millis());
                                                    {
                                                        let mut processed = processed_messages_clone.write().await;
                                                        if processed.contains(&msg_id) { continue; }
                                                        processed.insert(msg_id.clone());
                                                    }

                                                    let mut executor = executor_clone.clone();
                                                    let result = agent_clone
                                                        .process_message(
                                                            &agent_node_id,
                                                            &channel_name,
                                                            message.clone(),
                                                            &mut executor,
                                                            cache.clone(),
                                                        )
                                                        .await;
                                                    let outcome = NodeExecutionOutcome {
                                                        node_id: name_clone.clone(),
                                                        success: result.is_ok(),
                                                        retry_messages: match &result {
                                                            Err(e) => vec![e.to_string()],
                                                            Ok(_) => Vec::new(),
                                                        },
                                                        final_error: match &result {
                                                            Err(e) => Some(e.to_string()),
                                                            Ok(_) => None,
                                                        },
                                                    };

                                                    if outcome_tx_clone.send(outcome).await.is_err() {
                                                        error!(agent = %name_clone, "Failed to send outcome");
                                                        break;
                                                    }
                                                    if let Err(e) = result {
                                                        error!(agent = %name_clone, channel = %channel_name, "Processing failed: {}", e);
                                                    }
                                                }
                                                Err(e) => {
                                                    error!(agent = %name_clone, channel = %channel_name, "Receive failed: {}", e);
                                                    break;
                                                }
                                            }
                                        }
                                    });
                                    handles.push(handle);
                                }
                            }
                        }

                        let mut final_outcomes = Vec::new();
                        while let Some(outcome) = outcome_rx.recv().await {
                            final_outcomes.push(outcome);
                        }
                        for handle in handles {
                            if let Err(e) = handle.await {
                                error!("Task failed: {}", e);
                            }
                        }
                        // self.save_state().await.map_err(|e| PubSubError::ExecutionError(e.to_string()))?;
                        Result::<_, PubSubError>::Ok(PubSubExecutionReport::new(
                            final_outcomes.clone(),
                            final_outcomes.iter().all(|o| o.success),
                            None,
                        ))
                    } => match result {
                        Ok(report) => Ok((report, outcome_rx)),
                        Err(e) => {
                            self.save_state().await.map_err(|e| PubSubError::ExecutionError(e.to_string()))?;
                            Err(PubSubError::ExecutionError(e.to_string()))
                        }
                    },
                    _ = cancel_rx => {
                        self.stop().await?;
                        let mut final_outcomes = Vec::new();
                        while let Ok(outcome) = outcome_rx.try_recv() {
                            final_outcomes.push(outcome);
                        }
                        Ok((PubSubExecutionReport::new(final_outcomes, false, Some("Execution cancelled".to_string())), outcome_rx))
                    },
                };
                result
            }
        }
    }

    /// Creates a lightweight clone of the executor, reusing thread-safe components.
    fn clone(&self) -> Self {
        Self {
            agent_registry: Arc::clone(&self.agent_registry),
            agent_factories: Arc::clone(&self.agent_factories),
            channels: Arc::clone(&self.channels),
            config: self.config.clone(),
            sled_db: self.sled_db.clone(), // sled::Db implements Clone efficiently
            execution_tree: Arc::clone(&self.execution_tree),
            cache: self.cache.clone(),
            stopped: Arc::clone(&self.stopped),
            start_time: self.start_time,
            current_agent_id: self.current_agent_id.clone(),
        }
    }

    async fn save_state(&self) -> Result<()> {
        let tree = self.execution_tree.read().await;
        let serializable_tree = crate::SerializableExecutionTree::from_execution_tree(&tree);
        let serialized = serde_json::to_vec(&serializable_tree)?;
        let compressed = zstd::encode_all(&*serialized, 3)?;
        let tree_store = self.sled_db.open_tree("pubsub_execution_trees")?;
        tree_store.insert(b"latest", compressed)?;
        info!("Saved PubSubExecutor state");
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), PubSubError> {
        let mut stopped = self.stopped.write().await;
        *stopped = true;
        self.cleanup_channels()
            .await
            .map_err(|e| PubSubError::ExecutionError(e.to_string()))?;
        self.save_state()
            .await
            .map_err(|e| PubSubError::ExecutionError(e.to_string()))?;
        info!("PubSubExecutor stopped");
        Ok(())
    }

    pub async fn serialize_tree_to_dot(
        &self,
        workflow_id: &str,
        cache: &Cache,
    ) -> Result<String, PubSubError> {
        let tree = self.execution_tree.read().await;
        let mut dot = String::from("digraph PubSubFlow {\n");
        dot.push_str("  graph [rankdir=LR, nodesep=0.5, ranksep=1.0];\n");
        dot.push_str("  node [shape=box, style=rounded, fontname=\"Helvetica\", width=1.5];\n");
        dot.push_str("  edge [fontsize=10, color=gray];\n\n");

        // Create subgraphs for different node types
        dot.push_str("  // Agent nodes\n");
        dot.push_str("  subgraph cluster_agents {\n");
        dot.push_str("    label=\"Agents\";\n");
        dot.push_str("    style=dashed;\n");
        dot.push_str("    color=blue;\n");
        dot.push_str("    node [style=filled, fillcolor=lightblue];\n\n");
        
        // First pass: identify node types and create appropriate subgraphs
        let mut processed_nodes = HashSet::new();
        let mut agent_nodes = Vec::new();
        let mut publish_nodes = Vec::new();
        
        for node_idx in tree.node_indices() {
            let node = &tree[node_idx];
            if processed_nodes.contains(&node.node_id) { continue; }
            processed_nodes.insert(node.node_id.clone());
            
            // Determine node type based on ID pattern
            if node.node_id.starts_with("publish_") {
                publish_nodes.push(node_idx);
            } else {
                agent_nodes.push(node_idx);
            }
        }
        
        // Add agent nodes to their subgraph
        for node_idx in &agent_nodes {
            let node = &tree[*node_idx];
            let agent_name = node.node_id.split('_').next().unwrap_or("unknown");
            
            let label = format!(
                "<<TABLE BORDER=\"0\" CELLBORDER=\"1\" CELLSPACING=\"0\">\n\
                 <TR><TD BGCOLOR=\"#E8E8E8\"><B>Agent: {}</B></TD></TR>\n\
                 <TR><TD>Channel: {}</TD></TR>\n\
                 <TR><TD BGCOLOR=\"{}\">{}</TD></TR>\n\
                 <TR><TD>Time: {}</TD></TR>\n\
                 </TABLE>>",
                agent_name,
                node.channel.as_deref().unwrap_or("unknown"),
                if node.outcome.success { "#E6FFE6" } else { "#FFE6E6" },
                if node.outcome.success { "✓ Success" } else { "✗ Failed" },
                node.timestamp.format("%H:%M:%S")
            );

            let color = if node.outcome.success { "green" } else { "red" };
            dot.push_str(&format!(
                "    \"{}\" [label={}, color={}, fontcolor=black];\n",
                node.node_id, label, color
            ));
        }
        dot.push_str("  }\n\n");
        
        // Add publish nodes to their own subgraph
        dot.push_str("  // Publish nodes\n");
        dot.push_str("  subgraph cluster_publish {\n");
        dot.push_str("    label=\"Published Messages\";\n");
        dot.push_str("    style=dashed;\n");
        dot.push_str("    color=orange;\n");
        dot.push_str("    node [style=filled, fillcolor=lightyellow, shape=ellipse];\n\n");
        
        for node_idx in &publish_nodes {
            let node = &tree[*node_idx];
            let channel = node.channel.as_deref().unwrap_or("unknown");
            
            let label = format!(
                "<<TABLE BORDER=\"0\" CELLBORDER=\"1\" CELLSPACING=\"0\">\n\
                 <TR><TD BGCOLOR=\"#FFF8E8\"><B>Publish: {}</B></TD></TR>\n\
                 <TR><TD>Message ID: {}</TD></TR>\n\
                 <TR><TD BGCOLOR=\"{}\">{}</TD></TR>\n\
                 <TR><TD>Time: {}</TD></TR>\n\
                 </TABLE>>",
                channel,
                node.message_id.as_deref().unwrap_or("unknown"),
                if node.outcome.success { "#E6FFE6" } else { "#FFE6E6" },
                if node.outcome.success { "✓ Sent" } else { "✗ Failed" },
                node.timestamp.format("%H:%M:%S")
            );

            let color = if node.outcome.success { "green" } else { "red" };
            dot.push_str(&format!(
                "    \"{}\" [label={}, color={}, fontcolor=black];\n",
                node.node_id, label, color
            ));
        }
        dot.push_str("  }\n\n");

        dot.push_str("\n  // Edges\n");
        for edge in tree.edge_references() {
            let source = &tree[edge.source()].node_id;
            let target = &tree[edge.target()].node_id;
            let label = &edge.weight().label;
            
            // Format edge labels to be more descriptive
            let formatted_label = if label.starts_with("via_") {
                let channel = label.strip_prefix("via_").unwrap_or(label);
                format!("received from {}", channel)
            } else if label.starts_with("published_to_") {
                let channel = label.strip_prefix("published_to_").unwrap_or(label);
                format!("published to {}", channel)
            } else {
                label.clone()
            };
            
            dot.push_str(&format!(
                "  \"{}\" -> \"{}\" [label=\"{}\"];\n",
                source, target, formatted_label
            ));
        }

        dot.push_str("}\n");
        info!("Generated DOT graph for workflow '{}'", workflow_id);
        Ok(dot)
    }

    pub async fn load_state(&self, cache: &Cache) -> Result<()> {
        // Load execution tree from compressed storage
        let tree_store = self.sled_db.open_tree("pubsub_execution_trees")?;
        if let Some(compressed) = tree_store.get(b"latest")? {
            let bytes = zstd::decode_all(&compressed[..])?;
            let serializable_tree: crate::SerializableExecutionTree =
                serde_json::from_slice(&bytes)?;
            let mut tree = self.execution_tree.write().await;
            *tree = serializable_tree.to_execution_tree();
            info!("Loaded execution tree with {} nodes", tree.node_count());
        }

        // Load cache state
        let cache_tree = self.sled_db.open_tree("pubsub_cache")?;
        if let Some(cache_bytes) = cache_tree.get(b"latest")? {
            let cache_str = String::from_utf8(cache_bytes.to_vec())?;
            let loaded_cache = crate::load_cache_from_json(&cache_str)?;
            let mut current_cache = cache
                .write()
                .map_err(|e| anyhow!("Cache lock error: {}", e))?;
            *current_cache = loaded_cache
                .read()
                .map_err(|e| anyhow!("Cache read error: {}", e))?
                .clone();
            info!("Loaded cache with {} entries", current_cache.len());
        }

        // Replay unprocessed messages
        let messages_tree = self.sled_db.open_tree("pubsub_messages")?;
        let mut replayed_count = 0;
        for result in messages_tree.iter() {
            let (key, value) = result?;
            let msg_id = String::from_utf8(key.to_vec())?;
            let message: Message = serde_json::from_slice(&value)?;
            let channel = msg_id.split('_').nth(1).unwrap_or("unknown");

            // Check if this message has already been processed
            let is_processed = {
                let cache_read = cache
                    .read()
                    .map_err(|e| anyhow!("Cache lock error: {}", e))?;
                cache_read
                    .get(channel)
                    .and_then(|m| m.get(&msg_id))
                    .is_some()
            };

            if !is_processed {
                debug!(msg_id = %msg_id, channel = %channel, "Replaying unprocessed message");
                self.publish(channel, message, cache).await?;
                replayed_count += 1;
            }
        }

        if replayed_count > 0 {
            info!(count = replayed_count, "Replayed unprocessed messages");
        }

        // Load agent metadata (note: actual agent implementations must be re-registered)
        let agents_tree = self.sled_db.open_tree("pubsub_agents")?;
        let mut loaded_metadata = Vec::new();
        for result in agents_tree.iter() {
            let (key, value) = result?;
            let name = String::from_utf8(key.to_vec())?;
            let metadata: AgentMetadata = serde_json::from_slice(&value)?;
            loaded_metadata.push((name.clone(), metadata));
            debug!(agent_name = %name, "Found persisted agent metadata");
        }

        // Recreate agents using factories
        let factories = self.agent_factories.read().await;
        for (name, factory) in factories.iter() {
            let agent = factory();
            let mut registry = self.agent_registry.write().await;

            if let Some(metadata) = loaded_metadata.iter().find(|(n, _)| n == name) {
                registry.insert(
                    name.clone(),
                    AgentEntry {
                        agent: agent.clone(),
                        metadata: metadata.1.clone(),
                    },
                );
                debug!(agent_name = %name, "Recreated agent from factory");
            }
        }

        // Update channels based on loaded metadata
        for (name, metadata) in loaded_metadata {
            // Ensure channels exist for all subscriptions and publications
            for channel in metadata
                .subscriptions
                .iter()
                .chain(metadata.publications.iter())
            {
                self.ensure_channel(channel).await?;
            }
            debug!(agent_name = %name, "Restored channels for agent");
        }

        info!("Successfully loaded PubSubExecutor state from persistence");
        Ok(())
    }

    /// Lists all active channels with their current status.
    ///
    /// # Returns
    /// A vector of tuples containing:
    /// - Channel name
    /// - Number of active subscribers
    /// - Current message count in channel
    /// - Last activity timestamp
    pub async fn list_channels(&self) -> Result<Vec<ChannelStatus>, PubSubError> {
        let channels = self.channels.read().await;
        let channels_tree = self.sled_db.open_tree("pubsub_channels")?;

        let mut channel_status = Vec::new();
        for (name, (sender, _)) in channels.iter() {
            let subscriber_count = sender.receiver_count();
            let message_count = sender.len();

            // Get last activity from metadata
            let metadata: Value = if let Some(data) = channels_tree.get(name.as_bytes())? {
                serde_json::from_slice(&data)?
            } else {
                continue; // Skip if no metadata (shouldn't happen)
            };

            channel_status.push(ChannelStatus {
                name: name.clone(),
                subscriber_count,
                message_count,
                created_at: metadata["created_at"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                is_active: subscriber_count > 0,
            });

            debug!(
                channel = %name,
                subscribers = subscriber_count,
                messages = message_count,
                "Channel status retrieved"
            );
        }

        Ok(channel_status)
    }

    /// Explicitly shuts down a channel, removing it from the executor.
    ///
    /// # Arguments
    /// * `channel` - Name of the channel to shut down
    /// * `force` - If true, closes channel even with active subscribers
    ///
    /// # Returns
    /// Result indicating success or failure
    pub async fn shutdown_channel(&self, channel: &str, force: bool) -> Result<(), PubSubError> {
        let mut channels = self.channels.write().await;

        if let Some((sender, _)) = channels.get(channel) {
            let subscriber_count = sender.receiver_count();

            if subscriber_count > 0 && !force {
                return Err(PubSubError::ExecutionError(format!(
                    "Channel '{}' has {} active subscribers. Use force=true to shutdown anyway.",
                    channel, subscriber_count
                )));
            }

            // Remove from memory
            channels.remove(channel);

            // Remove from persistent storage
            let channels_tree = self.sled_db.open_tree("pubsub_channels")?;
            channels_tree.remove(channel.as_bytes())?;

            info!(
                channel = %channel,
                force = force,
                "Channel shutdown complete"
            );
            Ok(())
        } else {
            Err(PubSubError::ChannelNotFound(channel.to_string()))
        }
    }

    // Add a method to list registered agents
    pub async fn list_agents(&self) -> Vec<AgentMetadata> {
        let registry = self.agent_registry.read().await;
        registry
            .values()
            .map(|entry| entry.metadata.clone())
            .collect()
    }

    /// Registers multiple agents at once
    pub async fn register_agents(
        &mut self,
        agents: Vec<Arc<dyn PubSubAgent>>,
    ) -> Result<(), PubSubError> {
        for agent in agents {
            self.register_agent(agent).await?;
        }
        Ok(())
    }

    // Update register_agent to support factories
    pub async fn register_agent_with_factory(
        &mut self,
        factory: AgentFactory,
    ) -> Result<(), PubSubError> {
        let agent = factory();
        let name = agent.name();

        {
            let mut factories = self.agent_factories.write().await;
            factories.insert(name.clone(), factory);
        }

        self.register_agent(agent).await
    }
    pub async fn start(&mut self) -> Result<mpsc::Receiver<NodeExecutionOutcome>, PubSubError> {
        let agents = self.agent_registry.read().await.clone();
        let channels = Arc::clone(&self.channels);
        let stopped = Arc::clone(&self.stopped);
        let cache = self.cache.clone();
        let (outcome_tx, outcome_rx) = mpsc::channel::<NodeExecutionOutcome>(100);

        for (name, entry) in agents.iter() {
            let agent = entry.agent.clone();
            for sub in entry.metadata.subscriptions.iter() {
                let mut rx = {
                    let channels = channels.read().await;
                    channels.get(sub).unwrap().1.clone()
                };
                let channel_name = sub.clone();
                let name_clone = name.clone();
                let executor_clone = self.clone();
                let stopped_clone = Arc::clone(&stopped);
                let agent_clone = agent.clone();
                let cache_clone = cache.clone();
                let outcome_tx_clone = outcome_tx.clone();
                tokio::spawn(async move {
                    while !*stopped_clone.read().await {
                        if let Ok(message) = rx.recv().await {
                            let mut executor = executor_clone.clone();
                            let result = agent_clone
                                .process_message(
                                    &name_clone,
                                    &channel_name,
                                    message.clone(),
                                    &mut executor,
                                    cache_clone.clone(),
                                )
                                .await;
                            let outcome = NodeExecutionOutcome {
                                node_id: name_clone.clone(),
                                success: result.is_ok(),
                                retry_messages: result
                                    .as_ref()
                                    .err()
                                    .map(|e| vec![e.to_string()])
                                    .unwrap_or_default(),
                                final_error: result.as_ref().err().map(|e| e.to_string()),
                            };
                            if outcome_tx_clone.send(outcome).await.is_err() {
                                error!("Failed to send outcome for agent {}", name_clone);
                                break;
                            }
                            if let Err(e) = result {
                                error!(
                                    "Agent {} failed on channel {}: {}",
                                    name_clone, channel_name, e
                                );
                            }
                        }
                    }
                });
            }
        }
        info!("PubSubExecutor started all agent listeners");
        Ok(outcome_rx)
    }

    pub async fn register_supervisor_agent(
        &mut self,
        agent: Arc<dyn PubSubAgent>,
    ) -> Result<(), PubSubError> {
        // Check if registry is empty without holding the lock across the register_agent call
        let is_empty = {
            let registry = self.agent_registry.read().await;
            registry.is_empty()
        };

        if !is_empty {
            return Err(PubSubError::ExecutionError(
                "Supervisor agent must be registered first".to_string(),
            ));
        }

        self.register_agent(agent).await?;
        info!("Supervisor agent registered");
        Ok(())
    }

    pub async fn register_from_config(&mut self, config_json: &str) -> Result<(), PubSubError> {
        let config: Value =
            serde_json::from_str(config_json).map_err(|e| PubSubError::SerializationError(e))?;
        if let Some(agents) = config.get("agents").and_then(|v| v.as_array()) {
            for agent_config in agents {
                let name = agent_config.get("name").and_then(|n| n.as_str()).ok_or(
                    PubSubError::ValidationError("Missing agent name".to_string()),
                )?;
                self.register_from_global(name).await?;
            }
        }
        Ok(())
    }

    pub async fn find_agents_for_input(&self, input: &Value) -> Vec<AgentMetadata> {
        let registry = self.agent_registry.read().await;
        registry
            .values()
            .filter(|entry| {
                let schema = entry.metadata.input_schema.clone();
                jsonschema::validator_for(&schema)
                    .map(|v| v.validate(input).is_ok())
                    .unwrap_or(false)
            })
            .map(|entry| entry.metadata.clone())
            .collect()
    }
}

/// Represents the current status of a channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelStatus {
    pub name: String,
    pub subscriber_count: usize,
    pub message_count: usize,
    pub created_at: String,
    pub is_active: bool,
}

/// HumanInterruptAgent pauses execution for human input
pub struct HumanInterruptAgent {
    input_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
    cancel_tx: Arc<RwLock<Option<mpsc::Sender<()>>>>,
}

impl HumanInterruptAgent {
    pub fn new() -> Self {
        Self {
            input_tx: Arc::new(RwLock::new(None)),
            cancel_tx: Arc::new(RwLock::new(None)),
        }
    }

    /// Provides input to continue execution when paused for human review
    ///
    /// # Returns
    /// - `Ok(())` if input was successfully provided
    /// - `Err` if there's no active human review session or sending failed
    pub async fn provide_input(&self) -> Result<()> {
        let tx = self.input_tx.read().await;
        if let Some(tx) = tx.as_ref() {
            tx.send(())
                .await
                .map_err(|e| anyhow!("Failed to send input: {}", e))?;
            info!("Human input provided successfully");
            Ok(())
        } else {
            Err(anyhow!("No active human review session"))
        }
    }

    pub async fn cancel(&self) {
        // Get read lock on cancel_tx
        let cancel_guard = self.cancel_tx.read().await;
        // If there's a sender in the Option, try to send cancellation
        if let Some(tx) = &*cancel_guard {
            let _ = tx.try_send(());
        }
    }
}

#[async_trait]
impl PubSubAgent for HumanInterruptAgent {
    fn name(&self) -> String {
        "HumanInterruptAgent".to_string()
    }

    fn description(&self) -> String {
        "HumanInterruptAgent pauses execution for human input".to_string()
    }

    fn subscriptions(&self) -> Vec<String> {
        vec!["human_review".to_string()]
    }

    fn publications(&self) -> Vec<String> {
        vec![]
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {"review": {"type": "string"}}})
    }

    fn output_schema(&self) -> Value {
        json!({"type": "null"})
    }

    async fn process_message(
        &self,
        node_id: &str,
        channel: &str,
        message: Message,
        executor: &mut PubSubExecutor,
        cache: Arc<Cache>,
    ) -> Result<()> {
        let wait_minutes = executor.config.human_wait_minutes.unwrap_or(5);
        info!(
            "HumanInterruptAgent pausing on channel {} for {} minutes",
            channel, wait_minutes
        );

        let wait_duration = tokio::time::Duration::from_secs(wait_minutes as u64 * 60);
        let (input_tx, mut input_rx) = mpsc::channel(1);
        let (cancel_tx, mut cancel_rx) = mpsc::channel(1);

        {
            let mut tx_write = self.input_tx.write().await;
            let mut cancel_write = self.cancel_tx.write().await;
            *tx_write = Some(input_tx);
            *cancel_write = Some(cancel_tx);
        }

        // Remove the ChannelCleanup struct and handle cleanup after select
        let result = tokio::select! {
            _ = tokio::time::sleep(wait_duration) => {
                match executor.config.human_timeout_action {
                    HumanTimeoutAction::Autopilot => {
                        info!("No human input received, proceeding in autopilot");
                        Ok(())
                    }
                    HumanTimeoutAction::Pause => {
                        info!("No human input received, pausing execution");
                        let mut stopped = executor.stopped.write().await;
                        *stopped = true;
                        executor.save_state().await?;
                        Err(anyhow!("Paused for human input"))
                    }
                }
            }
            _ = input_rx.recv() => {
                info!("Received human input, continuing execution");
                Ok(())
            }
            _ = cancel_rx.recv() => {
                info!("Human interrupt cancelled");
                Err(anyhow!("Human interrupt cancelled"))
            }
        };

        // Explicit cleanup after select completes
        {
            let mut tx_write = self.input_tx.write().await;
            let mut cancel_write = self.cancel_tx.write().await;
            *tx_write = None;
            *cancel_write = None;
        }

        result
    }
}

/// Global registry for managing PubSub agents across the system
pub struct GlobalAgentRegistry {
    agents: Arc<RwLock<HashMap<String, AgentFactory>>>,
}

impl GlobalAgentRegistry {
    /// Creates a new instance of the global agent registry
    pub fn new() -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Registers a new agent factory with the global registry
    pub async fn register(&self, name: String, factory: AgentFactory) -> Result<(), PubSubError> {
        let mut agents = self.agents.write().await;
        if agents.contains_key(&name) {
            return Err(PubSubError::ExecutionError(format!(
                "Agent '{}' already registered",
                name
            )));
        }
        agents.insert(name.clone(), factory);
        info!(agent_name = %name, "Registered agent in global registry");
        Ok(())
    }

    /// Retrieves an agent factory by name
    pub async fn get_factory(&self, name: &str) -> Option<AgentFactory> {
        let agents = self.agents.read().await;
        agents.get(name).cloned()
    }

    /// Lists all registered agent names
    pub async fn list_agents(&self) -> Vec<String> {
        let agents = self.agents.read().await;
        agents.keys().cloned().collect()
    }

    /// Creates a new instance of an agent using its factory
    pub async fn create_agent(&self, name: &str) -> Result<Arc<dyn PubSubAgent>, PubSubError> {
        let factory = self.get_factory(name).await.ok_or_else(|| {
            PubSubError::AgentNotFound(format!("Agent factory '{}' not found", name))
        })?;
        Ok(factory())
    }
}

// Create a lazy static instance of the global registry
lazy_static::lazy_static! {
    pub static ref GLOBAL_REGISTRY: GlobalAgentRegistry = GlobalAgentRegistry::new();
}

// Update PubSubExecutor to work with the global registry
impl PubSubExecutor {
    /// Registers an agent from the global registry
    pub async fn register_from_global(&mut self, agent_name: &str) -> Result<(), PubSubError> {
        let agent = GLOBAL_REGISTRY.create_agent(agent_name).await?;
        self.register_agent(agent).await
    }

    /// Registers multiple agents from the global registry
    pub async fn register_from_global_many(
        &mut self,
        agent_names: &[&str],
    ) -> Result<(), PubSubError> {
        for name in agent_names {
            self.register_from_global(name).await?;
        }
        Ok(())
    }

    pub fn set_current_agent_id(&mut self, id: String) {
        // Store the current agent ID in thread-local storage or as a field
        self.current_agent_id = Some(id);
    }
    
    /// Gets the current agent ID if available
    pub fn get_current_agent_id(&self) -> Option<String> {
        self.current_agent_id.clone()
    }

     /// Clears the current agent ID
    pub fn clear_current_agent_id(&mut self) {
        self.current_agent_id = None;
    }
}
