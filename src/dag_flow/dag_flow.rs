//! Dagger - A library for executing directed acyclic graphs (DAGs) with custom actions.
//!
//! This library provides a way to define and execute DAGs with custom actions. It supports
//! loading graph definitions from YAML files, validating the graph structure, and executing
//! custom actions associated with each node in the graph.

use anyhow::anyhow;
use anyhow::{Error, Result};
use dashmap::DashMap;
use petgraph::visit::IntoNodeReferences;

use async_trait::async_trait;
use chrono::NaiveDateTime;
use cuid2;
use petgraph::algo::is_cyclic_directed;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use petgraph::visit::Topo;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs::File;

use std::io::Read;
use std::sync::Arc;
use std::sync::RwLock;
use tokio::sync::oneshot;

// use tokio::sync::RwLock;
use serde_json::error::Error as JsonError;
use tokio::time::{sleep, timeout, Duration};
use tracing::{debug, error, info, trace, warn, Level}; // Assuming you're using Tokio for async runtime // Add at top with other imports

// Add these imports at the top with other imports
use serde::de::Error as SerdeError;
use std::io::Error as IoError;

use super::any::{DynAny, IntoAny};

// Add near the top with other type definitions



#[macro_export]
macro_rules! register_action {
    ($executor:expr, $action_name:expr, $action_func:path) => {{
        struct Action;
        #[async_trait::async_trait]
        impl NodeAction for Action {
            fn name(&self) -> String {
                $action_name.to_string()
            }
            async fn execute(
                &self,
                executor: &mut DagExecutor,
                node: &Node,
                cache: &Cache,
            ) -> Result<()> {
                $action_func(executor, node, cache).await
            }
            fn schema(&self) -> serde_json::Value {
                serde_json::json!({
                    "name": $action_name,
                    "description": "Manually registered action",
                    "parameters": { "type": "object", "properties": {} },
                    "returns": { "type": "object" }
                })
            }
        }
        $executor.register_action(Arc::new(Action));
    }};
}

/// Specifies how to execute a workflow (DEPRECATED - use ExecutionMode)
#[derive(Debug, Clone)]
pub enum WorkflowSpec {
    /// Execute a static, pre-loaded DAG by name
    Static { name: String },
    /// Start an agent-driven flow with a given task
    Agent { task: String },
}

impl WorkflowSpec {
    /// Convert to execution mode and name
    pub fn into_parts(self) -> (ExecutionMode, String) {
        match self {
            WorkflowSpec::Static { name } => (ExecutionMode::Static, name),
            WorkflowSpec::Agent { task } => (ExecutionMode::Agent, task),
        }
    }
}

/// Configuration for retry behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RetryStrategy {
    /// Exponential backoff with configurable parameters
    Exponential {
        initial_delay_secs: u64,
        max_delay_secs: u64,
        multiplier: f64,
    },
    /// Linear backoff with fixed delay
    Linear { delay_secs: u64 },
    /// No delay between retries
    Immediate,
}

impl Default for RetryStrategy {
    fn default() -> Self {
        Self::Exponential {
            initial_delay_secs: 2,
            max_delay_secs: 60,
            multiplier: 2.0,
        }
    }
}

/// Configuration for DAG execution behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagConfig {
    /// Maximum number of retry attempts
    pub max_attempts: Option<u8>,
    /// What to do when a node fails
    pub on_failure: OnFailure,
    /// Maximum runtime in seconds
    pub timeout_seconds: Option<u64>,
    /// How long to wait for human input (None = indefinite)
    pub human_wait_minutes: Option<u32>,
    /// What to do when human input times out
    pub human_timeout_action: HumanTimeoutAction,
    /// Maximum tokens allowed
    pub max_tokens: Option<u64>,
    /// Maximum iterations allowed
    pub max_iterations: Option<u32>,
    /// How often to trigger human review (in iterations)
    pub review_frequency: Option<u32>,
    /// Retry strategy configuration
    pub retry_strategy: RetryStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OnFailure {
    Continue,
    Pause,
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HumanTimeoutAction {
    Autopilot,
    Pause,
}

impl Default for DagConfig {
    fn default() -> Self {
        Self {
            max_attempts: Some(3),
            on_failure: OnFailure::Pause,
            timeout_seconds: Some(3600),
            human_wait_minutes: None,
            human_timeout_action: HumanTimeoutAction::Pause,
            max_tokens: None,
            max_iterations: Some(50),
            review_frequency: Some(5),
            retry_strategy: RetryStrategy::default(),
        }
    }
}

impl DagConfig {
    /// Validates configuration values
    pub fn validate(&self) -> Result<(), Error> {
        // Validate max_attempts
        if let Some(attempts) = self.max_attempts {
            if attempts == 0 {
                return Err(anyhow!("max_attempts must be greater than 0"));
            }
        }

        // Validate timeout_seconds
        if let Some(timeout) = self.timeout_seconds {
            if timeout == 0 {
                return Err(anyhow!("timeout_seconds must be greater than 0"));
            }
            if timeout > 86400 {
                // 24 hours
                return Err(anyhow!("timeout_seconds cannot exceed 24 hours"));
            }
        }

        // Validate human_wait_minutes
        if let Some(wait) = self.human_wait_minutes {
            if wait > 1440 {
                // 24 hours
                return Err(anyhow!("human_wait_minutes cannot exceed 24 hours"));
            }
        }

        // Validate max_iterations
        if let Some(iterations) = self.max_iterations {
            if iterations == 0 {
                return Err(anyhow!("max_iterations must be greater than 0"));
            }
            if iterations > 1000 {
                return Err(anyhow!("max_iterations cannot exceed 1000"));
            }
        }

        // Validate review_frequency
        if let Some(freq) = self.review_frequency {
            if freq == 0 {
                return Err(anyhow!("review_frequency must be greater than 0"));
            }
        }

        Ok(())
    }

    /// Merges two configurations, with override_with taking precedence
    pub fn merge(base: &Self, override_with: &Self) -> Result<Self, Error> {
        let merged = Self {
            max_attempts: override_with.max_attempts.or(base.max_attempts),
            on_failure: override_with.on_failure.clone(),
            timeout_seconds: override_with.timeout_seconds.or(base.timeout_seconds),
            human_wait_minutes: override_with.human_wait_minutes.or(base.human_wait_minutes),
            human_timeout_action: override_with.human_timeout_action.clone(),
            max_tokens: override_with.max_tokens.or(base.max_tokens),
            max_iterations: override_with.max_iterations.or(base.max_iterations),
            review_frequency: override_with.review_frequency.or(base.review_frequency),
            retry_strategy: override_with.retry_strategy.clone(),
        };

        // Validate merged configuration
        merged.validate()?;
        Ok(merged)
    }
}

/// Metadata about a DAG's execution state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagMetadata {
    pub status: String,
    pub task: String,
}

/// Represents a graph of nodes.
#[derive(Debug, Deserialize)]
pub struct Graph {
    /// The nodes in the graph.
    pub nodes: Vec<Node>,
    pub name: String,
    pub description: String,
    pub instructions: Option<Vec<String>>,
    pub tags: Vec<String>,
    pub author: String,
    pub version: String,
    pub signature: String,
    pub config: Option<DagConfig>,
}

/// A trait for converting values between Rust types and `DynAny` enum.
pub trait Convertible {
    /// Converts a Rust type to a `DynAny` enum.
    fn to_value(&self) -> DynAny;

    /// Converts a `DynAny` enum to a Rust type.
    fn from_value(value: &DynAny) -> Option<Self>
    where
        Self: Sized;
}

/// An input or output field of a node.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct IField {
    /// The name of the field.
    pub name: String,
    /// The description of the field.
    pub description: Option<String>,

    /// The data type of the field.
    // pub data_type: String, // Changed to String for simplicity in this example
    /// The reference to another node's output.
    pub reference: String,
    // pub default: Option<DynAny>,
}

/// An input or output field of a node.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct OField {
    /// The name of the field.
    pub name: String,
    /// The description of the field.
    pub description: Option<String>,
}

/// A node in the graph.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Node {
    /// The unique identifier of the node.
    ///
    pub id: String,
    /// The dependencies of the node (other nodes that must be executed before this node).
    pub dependencies: Vec<String>,
    /// The inputs of the node.
    pub inputs: Vec<IField>,
    /// The outputs of the node.
    pub outputs: Vec<OField>,
    /// The action to be executed by the node.
    pub action: String,
    /// The failure action to be executed if the node's action fails.
    pub failure: String,
    /// The on-failure behavior (continue or terminate).
    pub onfailure: bool,
    /// The description of the node.
    pub description: String,
    /// The timeout for the node's action in seconds.
    pub timeout: u64,
    /// The number of times to retry the node's action if it fails.
    pub try_count: u8,
    pub instructions: Option<Vec<String>>,
}

/// Type alias for a cache of input and output values.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SerializableData {
    pub value: Value,
}

#[derive(Debug, Default)]
pub struct Cache {
    pub data: Arc<DashMap<String, DashMap<String, SerializableData>>>,
}

impl Cache {
    pub fn new() -> Self {
        Cache {
            data: Arc::new(DashMap::new()),
        }
    }

    pub fn append_global_value<T: Serialize + for<'de> Deserialize<'de>>(
        &self,
        dag_name: &str,
        key: &str,
        value: T,
    ) -> Result<()> {
        let global_key = format!("global:{}", dag_name);
        let json_value = serde_json::to_value(value)?;

        // Get or insert the inner DashMap for the global key.
        let inner_map = self
            .data
            .entry(global_key)
            .or_insert_with(|| DashMap::new());

        // Use `entry` API for efficient updates.
        inner_map
            .entry(key.to_string())
            .and_modify(|existing_data| {
                if let Some(existing_vec) = existing_data.value.as_array_mut() {
                    existing_vec.push(json_value.clone()); // Clone for the modify closure.
                } else {
                    existing_data.value =
                        Value::Array(vec![existing_data.value.clone(), json_value.clone()]);
                }
            })
            .or_insert(SerializableData {
                value: Value::Array(vec![json_value]),
            }); // No clone needed here

        Ok(())
    }

    pub fn get_global_value<T: for<'de> Deserialize<'de>>(
        &self,
        dag_name: &str,
        key: &str,
    ) -> Result<Option<T>> {
        let global_key = format!("global:{}", dag_name);

        if let Some(inner_map) = self.data.get(&global_key) {
            if let Some(serializable_data) = inner_map.get(key) {
                let value: T = serde_json::from_value(serializable_data.value.clone())?;
                Ok(Some(value))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    pub fn insert_value<T: Serialize>(&self, node_id: &str, key: &str, value: &T) -> Result<()> {
        let value = serde_json::to_value(value)?;
        let map = self
            .data
            .entry(node_id.to_string())
            .or_insert_with(|| DashMap::new());
        map.insert(key.to_string(), SerializableData { value });
        Ok(())
    }
}

// Clone is already handled correctly by Arc<DashMap>.
impl Clone for Cache {
    fn clone(&self) -> Self {
        Cache {
            data: self.data.clone(),
        }
    }
}

pub fn serialize_cache_to_json(cache: &Cache) -> Result<String> {
    // Create a serializable representation of the cache.
    let serializable_cache: HashMap<String, HashMap<String, SerializableData>> = cache
        .data
        .iter()
        .map(|m| (m.key().clone(), m.value().clone().into_iter().collect()))
        .collect();

    serde_json::to_string(&serializable_cache).map_err(|e| e.into())
}

pub fn serialize_cache_to_prettyjson(cache: &Cache) -> Result<String> {
    // Create a serializable representation of the cache.
    let serializable_cache: HashMap<String, HashMap<String, SerializableData>> = cache
        .data
        .iter()
        .map(|m| (m.key().clone(), m.value().clone().into_iter().collect()))
        .collect();

    serde_json::to_string_pretty(&serializable_cache).map_err(|e| e.into())
}

/// Function to load the cache from JSON
/// Function to load the cache from JSON
pub fn load_cache_from_json(json_data: &str) -> Result<Cache> {
    let deserialized_cache: HashMap<String, DashMap<String, SerializableData>> =
        serde_json::from_str(json_data)?;

    let cache: HashMap<String, DashMap<String, SerializableData>> = deserialized_cache
        .into_iter()
        .map(|(node_id, category_map)| {
            let converted_category: DashMap<String, SerializableData> = category_map
                .into_iter()
                .map(|(output_name, value)| (output_name, value))
                .collect();
            (node_id, converted_category)
        })
        .collect(); // This collect needs the type hint

    Ok(Cache {
        data: Arc::new(DashMap::from_iter(cache)),
    })
}
// pub fn insert_value<T: IntoAny + 'static>(cache: &Cache, category: String, key: String, value: T) {
//     let mut cache_write = cache.write().unwrap();
//     let category_map = cache_write.entry(category).or_insert_with(HashMap::new);
//     category_map.insert(key, Box::new(value));
// }

pub fn insert_value<T>(cache: &Cache, node_id: &str, output_name: &str, value: T) -> Result<()>
where
    T: Serialize + std::fmt::Debug,
{
    let cache_data = &cache.data; // Get a reference to the DashMap

    let json_value = serde_json::to_value(value)?; // Serialize to serde_json::Value

    let serializable_data = SerializableData { value: json_value }; // Use Value

    // Use entry API for efficient insertion/modification.
    let inner_map = cache_data
        .entry(node_id.to_string())
        .or_insert_with(|| DashMap::new());
    inner_map.insert(output_name.to_string(), serializable_data);

    Ok(())
}

pub fn generate_node_id(action_name: &str) -> String {
    let timestamp = chrono::Utc::now().timestamp_millis() % 1_000_000; // Last 6 digits
    format!("{}_{}", action_name, timestamp)
}

pub fn get_input<T: for<'de> Deserialize<'de>>(
    cache: &Cache,
    node_id: &str,
    key: &str,
) -> Result<T> {
    let cache_read = &cache.data;

    if let Some(category_map) = cache_read.get(node_id) {
        if let Some(serializable_data) = category_map.get(key) {
            let value: T = serde_json::from_value(serializable_data.value.clone())?; // Deserialize from Value
            Ok(value)
        } else {
            Err(anyhow!("Key '{}' not found in node '{}'", key, node_id).into())
        }
    } else {
        Err(anyhow!("Node '{}' not found in cache", node_id).into())
    }
}

pub fn parse_input_from_name<T: for<'de> Deserialize<'de>>(
    cache: &Cache,
    input_name: String,
    inputs: &[IField],
) -> Result<T> {
    // Find the input field that matches the given name
    let input_field = inputs
        .iter()
        .find(|input| input.name == input_name)
        .ok_or_else(|| anyhow!("Input field '{}' not found", input_name))?;

    // Extract the reference from the input field
    let reference = &input_field.reference;

    // Split the reference into node ID and output name
    let parts: Vec<&str> = reference.split('.').collect();
    if parts.len() != 2 {
        return Err(anyhow!("Invalid reference format: '{}'", reference).into());
    }
    let node_id = parts[0];
    let output_name = parts[1];
    // Use get_input to retrieve the value from the cache
    get_input(cache, node_id, output_name)
}

pub fn get_global_input<T: for<'de> Deserialize<'de>>(
    cache: &Cache,
    dag_name: &str,
    key: &str,
) -> Result<T, DagError> {
    let cache_read = &cache.data;

    let global_key = format!("global:{}", dag_name); // Consistent global key

    if let Some(global_map) = cache_read.get(&global_key) {
        if let Some(data) = global_map.get(key) {
            let value: T = serde_json::from_value(data.value.clone())?; // Use Value
            Ok(value)
        } else {
            Err(DagError::InvalidState(format!(
                "Key '{}' not found in global inputs for DAG '{}'",
                key, dag_name
            )))
        }
    } else {
        Err(DagError::InvalidState(format!(
            "Global inputs not found for DAG '{}'",
            dag_name
        )))
    }
}

pub fn insert_global_value<T: Serialize>(
    cache: &Cache,
    dag_name: &str,
    key: &str,
    value: T,
) -> Result<()> {
    let mut cache_write = &cache.data;

    let global_key = format!("global:{}", dag_name); // Consistent global key
    let json_value = serde_json::to_value(value)?; // Serialize to Value
    let serializable_data = SerializableData { value: json_value }; // Use Value

    if let Some(global_map) = cache_write.get_mut(&global_key) {
        global_map.insert(key.to_string(), serializable_data);
    } else {
        let mut new_global_map = DashMap::new();
        new_global_map.insert(key.to_string(), serializable_data);
        cache_write.insert(global_key, new_global_map);
    }

    Ok(())
}

pub fn append_global_value<T: Serialize + for<'de> Deserialize<'de>>(
    cache: &Cache,
    dag_name: &str,
    key: &str,
    value: T,
) -> Result<()> {
    let global_key = format!("global:{}", dag_name);
    let json_value = serde_json::to_value(value)?;

    // Get or insert the inner DashMap for the global key.
    let inner_map = cache
        .data
        .entry(global_key)
        .or_insert_with(|| DashMap::new());

    // Use `entry` API for efficient updates.
    inner_map
        .entry(key.to_string())
        .and_modify(|existing_data| {
            if let Some(existing_vec) = existing_data.value.as_array_mut() {
                existing_vec.push(json_value.clone()); // Clone for the modify closure.
            } else {
                existing_data.value =
                    Value::Array(vec![existing_data.value.clone(), json_value.clone()]);
            }
        })
        .or_insert(SerializableData {
            value: Value::Array(vec![json_value]),
        }); // No clone needed here

    Ok(())
}
/// A trait for custom actions associated with nodes.
#[async_trait]
pub trait NodeAction: Send + Sync {
    /// Returns the name of the action.
    fn name(&self) -> String;

    /// Executes the action with the given node and inputs, and returns the outputs.
    async fn execute(&self, executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()>;
    fn schema(&self) -> Value;
}

// Add ExecutionTree type definition before DagExecutor
pub type ExecutionTree = DiGraph<NodeSnapshot, ExecutionEdge>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSnapshot {
    pub node_id: String,
    pub outcome: NodeExecutionOutcome,
    pub cache_ref: String,
    pub timestamp: NaiveDateTime,
    pub channel: Option<String>, // New field for tracking message channel
    pub message_id: Option<String>, // New field for tracking message ID
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEdge {
    pub parent: String,
    pub label: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DagError {
    #[error("Lock error: {0}")]
    LockError(String),

    #[error("Node not found: {0}")]
    NodeNotFound(String),

    #[error("Action not found: {0}")]
    ActionNotFound(String),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Database error: {0}")]
    DatabaseError(#[from] sled::Error),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Execution error: {0}")]
    ExecutionError(String),

    #[error("Invalid graph: {0}")]
    InvalidGraph(String),

    #[error("Cancelled")]
    Cancelled,

    #[error("Action not registered: {0}")]
    ActionNotRegistered(String),

    #[error("Invalid state: {0}")]
    InvalidState(String),

    #[error("Dependency not found: {0}")]
    DependencyNotFound(String),

    #[error("Node already exists: {0}")]
    NodeAlreadyExists(String),

    #[error("Unknown database error occurred")]
    UnknownDatabaseError,
}

type ActionRegistry = Arc<RwLock<HashMap<String, Arc<dyn NodeAction>>>>;
/// The main executor for DAGs.
pub struct DagExecutor {
    /// A registry of custom actions.
    pub function_registry: ActionRegistry,
    /// The graphs to be executed.
    pub graphs: Arc<RwLock<HashMap<String, Graph>>>,
    /// The prebuilt DAGs.
    pub prebuilt_dags:
        Arc<RwLock<HashMap<String, (DiGraph<Node, ()>, HashMap<String, NodeIndex>)>>>,
    pub config: DagConfig,
    pub sled_db: sled::Db,
    pub stopped: Arc<RwLock<bool>>,
    pub paused: Arc<RwLock<bool>>,
    pub start_time: NaiveDateTime,
    pub tree: Arc<RwLock<HashMap<String, ExecutionTree>>>,
    /// Track bootstrapped agent DAGs
    pub bootstrapped_agents: Arc<RwLock<HashSet<String>>>,
}

impl DagExecutor {
    /// Creates a new `DagExecutor` with optional configuration
    pub fn new(
        config: Option<DagConfig>,
        registry: ActionRegistry,
        sled_path: &str,
    ) -> Result<Self, Error> {
        let sled_db = sled::open(sled_path)?;

        Ok(DagExecutor {
            function_registry: registry,
            graphs: Arc::new(RwLock::new(HashMap::new())),
            prebuilt_dags: Arc::new(RwLock::new(HashMap::new())),
            config: config.unwrap_or_default(),
            sled_db,
            stopped: Arc::new(RwLock::new(false)),
            paused: Arc::new(RwLock::new(false)),
            start_time: chrono::Local::now().naive_local(),
            tree: Arc::new(RwLock::new(HashMap::new())),
            bootstrapped_agents: Arc::new(RwLock::new(HashSet::new())),
        })
    }

    /// Registers a custom action with the `DagExecutor`.
    pub fn register_action(&mut self, action: Arc<dyn NodeAction>) -> Result<(), DagError> {
        let mut registry = self
            .function_registry
            .write()
            .map_err(|e| DagError::LockError(e.to_string()))?;

        info!("Registered action: {:#?}", action.name());
        registry.insert(action.name(), action);
        Ok(())
    }
    pub fn get_tool_schemas(&self) -> Vec<Value> {
        let registry = self.function_registry.read().unwrap();
        registry.values().map(|action| action.schema()).collect()
    }
    /// Loads a graph definition from a YAML file with proper config merging
    pub fn load_yaml_file(&mut self, file_path: &str) -> Result<(), Error> {
        let mut file = File::open(file_path)
            .map_err(|e| anyhow!("Failed to open file {}: {}", file_path, e))?;

        let mut yaml_content = String::new();
        file.read_to_string(&mut yaml_content)
            .map_err(|e| anyhow!("Failed to read file {}: {}", file_path, e))?;

        let mut graph: Graph = serde_yaml::from_str(&yaml_content)
            .map_err(|e| anyhow!("Failed to parse YAML file {}: {}", file_path, e))?;

        // Merge and validate configurations
        if let Some(graph_config) = &graph.config {
            let merged_config = DagConfig::merge(&self.config, graph_config)?;
            graph.config = Some(merged_config);
        }

        // Build DAG
        let (dag, node_indices) = self.build_dag_internal(&graph)?;
        let name = graph.name.clone();

        // Acquire write locks and update both structures atomically
        let mut graphs = self
            .graphs
            .write()
            .map_err(|e| anyhow!("Failed to acquire graphs write lock: {}", e))?;
        let mut dags = self
            .prebuilt_dags
            .write()
            .map_err(|e| anyhow!("Failed to acquire DAGs write lock: {}", e))?;

        graphs.insert(name.clone(), graph);
        dags.insert(name, (dag, node_indices));

        Ok(())
    }

    // extend above to load all yaml files in a directory

    pub fn load_yaml_dir(&mut self, dir_path: &str) {
        match std::fs::read_dir(dir_path) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => {
                            if let Ok(file_type) = entry.file_type() {
                                if file_type.is_file() {
                                    if let Some(file_path) = entry.path().to_str() {
                                        self.load_yaml_file(file_path);
                                    } else {
                                        error!(
                                            "Failed to convert file path to string: {:?}",
                                            entry.path()
                                        );
                                    }
                                }
                            } else {
                                error!(
                                    "Failed to determine file type for entry: {:?}",
                                    entry.path()
                                );
                            }
                        }
                        Err(e) => {
                            error!("Error reading directory entry: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                error!("Failed to read directory {}: {}", dir_path, e);
            }
        }
    }

    /// Executes the DAG and returns a `DagExecutionReport`.
    pub async fn execute_dag(
        &mut self,
        spec: WorkflowSpec,
        cache: &Cache,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<DagExecutionReport, DagError> {
        let (mode, name) = spec.into_parts();
        self.execute_dag_with_mode(mode, &name, cache, cancel_rx).await
    }

    /// Executes a static DAG by name (simplified interface)
    pub async fn execute_static_dag(
        &mut self,
        name: &str,
        cache: &Cache,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<DagExecutionReport, DagError> {
        self.execute_dag_with_mode(ExecutionMode::Static, name, cache, cancel_rx).await
    }

    /// Executes an agent-driven DAG (simplified interface)
    pub async fn execute_agent_dag(
        &mut self,
        task_name: &str,
        cache: &Cache,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<DagExecutionReport, DagError> {
        self.execute_dag_with_mode(ExecutionMode::Agent, task_name, cache, cancel_rx).await
    }

    /// Executes the DAG with the new simplified interface
    pub async fn execute_dag_with_mode(
        &mut self,
        mode: ExecutionMode,
        name: &str,
        cache: &Cache,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<DagExecutionReport, DagError> {
        match mode {
            ExecutionMode::Static => {
                // Get read lock to access prebuilt DAG
                let prebuilt_dag = {
                    let dags = self
                        .prebuilt_dags
                        .read()
                        .map_err(|e| DagError::LockError(e.to_string()))?;
                    dags.get(name)
                        .ok_or_else(|| {
                            DagError::NodeNotFound(format!("Graph '{}' not found", name))
                        })?
                        .clone()
                };

                let (dag, _) = prebuilt_dag;
                if dag.node_count() == 0 {
                    return Err(DagError::InvalidGraph(format!(
                        "Graph '{}' contains no nodes",
                        name
                    )));
                }

                self.start_time = chrono::Local::now().naive_local();

                tokio::select! {
                    result = execute_dag_async(self, &dag, cache) => {
                        let (report, needs_human_check) = result;
                        if needs_human_check {
                            self.add_node(name, format!("human_check_{}", cuid2::create_id()),
                                         "human_interrupt".to_string(), vec![])?;
                        }
                        Ok(report)
                    },
                    _ = cancel_rx => Err(DagError::Cancelled),
                }
            }
            ExecutionMode::Agent => {
                // Verify supervisor action is registered first
                {
                    let registry = self.function_registry.read().unwrap();
                    if !registry.contains_key("supervisor_step") {
                        return Err(DagError::ActionNotFound("supervisor_step".to_string()));
                    }
                    info!("Supervisor action registered");
                } // registry lock is dropped here

                // Attempt to bootstrap agent DAG if not already done
                let needs_bootstrap = {
                    let mut bootstrapped = self
                        .bootstrapped_agents
                        .write()
                        .map_err(|e| DagError::LockError(e.to_string()))?;
                    if !bootstrapped.contains(name) {
                        bootstrapped.insert(name.to_string());
                        true
                    } else {
                        false
                    }
                };

                if needs_bootstrap {
                    // Bootstrap agent DAG with write lock
                    let mut dags = self
                        .prebuilt_dags
                        .write()
                        .map_err(|e| DagError::LockError(e.to_string()))?;

                    if !dags.contains_key(name) {
                        // Create bootstrap graph
                        let graph = Graph {
                            name: name.to_string(),
                            nodes: vec![Node {
                                id: "supervisor_start".to_string(),
                                action: "supervisor_step".to_string(),
                                dependencies: Vec::new(),
                                inputs: Vec::new(),
                                outputs: Vec::new(),
                                failure: String::new(),
                                onfailure: true,
                                description: "Supervisor node".to_string(),
                                timeout: self.config.timeout_seconds.unwrap_or(3600),
                                try_count: self.config.max_attempts.unwrap_or(3),
                                instructions: None,
                            }],
                            description: format!("Agent-driven DAG for task: {}", name),
                            instructions: None,
                            tags: vec!["agent".to_string()],
                            author: "system".to_string(),
                            version: "1.0".to_string(),
                            signature: "auto-generated".to_string(),
                            config: Some(self.config.clone()),
                        };

                        let (mut dag, indices) = self
                            .build_dag_internal(&graph)
                            .map_err(|e| DagError::InvalidGraph(e.to_string()))?;
                        self.graphs.write().unwrap().insert(name.to_string(), graph);
                        dags.insert(name.to_string(), (dag, indices));
                    }
                }

                // Get DAG with read lock
                let (dag, _) = {
                    let dags = self
                        .prebuilt_dags
                        .read()
                        .map_err(|e| DagError::LockError(e.to_string()))?;
                    dags.get(name)
                        .ok_or_else(|| {
                            DagError::NodeNotFound(format!("Agent DAG '{}' not found", name))
                        })?
                        .clone()
                };

                self.start_time = chrono::Local::now().naive_local();

                // Record active DAG in Sled
                let active_tree = self.sled_db.open_tree("active")?;
                active_tree.insert(
                    name.as_bytes(),
                    serde_json::to_vec(&DagMetadata {
                        status: "Running".to_string(),
                        task: name.to_string(),
                    })?,
                )?;

                tokio::select! {
                    result = execute_dag_async(self, &dag, cache) => {
                        let (report, needs_human_check) = result;
                        if needs_human_check {
                            self.add_node(name, format!("human_check_{}", cuid2::create_id()),
                                      "human_interrupt".to_string(), vec![])?;
                        }
                        Ok(report)
                    },
                    _ = cancel_rx => Err(DagError::Cancelled),
                }
            }
        }
    }

    fn build_dag_internal(
        &self,
        graph: &Graph,
    ) -> Result<(DiGraph<Node, ()>, HashMap<String, NodeIndex>), Error> {
        let mut dag = DiGraph::<Node, ()>::new();
        let mut node_indices = HashMap::new();

        for node in &graph.nodes {
            let node_index = dag.add_node(node.clone());
            node_indices.insert(node.id.clone(), node_index);
        }

        validate_dag_structure(&dag)?;
        validate_node_dependencies(&graph.nodes, &node_indices)?;
        validate_node_actions(self, &graph.nodes)?;
        // validate_io_data_types(&graph.nodes)?;

        for node in &graph.nodes {
            let dependent_node_index = node_indices[&node.id];
            for dependency_id in &node.dependencies {
                let dependency_node_index = node_indices[dependency_id];
                dag.add_edge(dependency_node_index, dependent_node_index, ());
            }
        }

        Ok((dag, node_indices))
    }

    pub fn list_dags(&self) -> Result<Vec<(String, String)>> {
        let graphs = self
            .graphs
            .read()
            .map_err(|e| anyhow!("Failed to acquire graphs read lock: {}", e))?;

        Ok(graphs
            .iter()
            .map(|(name, graph)| (name.clone(), graph.description.clone()))
            .collect())
    }

    pub fn list_dag_filtered_tag(&self, filter: &str) -> Result<Vec<(String, String)>> {
        let graphs = self
            .graphs
            .read()
            .map_err(|e| anyhow!("Failed to acquire graphs read lock: {}", e))?;

        Ok(graphs
            .iter()
            .filter(|(_, graph)| graph.tags.iter().any(|tag| tag.contains(filter)))
            .map(|(name, graph)| (name.clone(), graph.description.clone()))
            .collect())
    }

    pub fn list_dag_multiple_tags(&self, tags: Vec<String>) -> Result<Vec<(String, String)>> {
        let graphs = self
            .graphs
            .read()
            .map_err(|e| anyhow!("Failed to acquire graphs read lock: {}", e))?;

        Ok(graphs
            .iter()
            .filter(|(_, graph)| tags.iter().all(|tag| graph.tags.contains(tag)))
            .map(|(name, graph)| (name.clone(), graph.description.clone()))
            .collect())
    }

    pub fn list_dags_metadata(&self) -> Result<Vec<(String, String, String, String, String)>> {
        let graphs = self
            .graphs
            .read()
            .map_err(|e| anyhow!("Failed to acquire graphs read lock: {}", e))?;

        Ok(graphs
            .iter()
            .map(|(name, graph)| {
                (
                    name.clone(),
                    graph.description.clone(),
                    graph.author.clone(),
                    graph.version.clone(),
                    graph.signature.clone(),
                )
            })
            .collect())
    }

    /// Saves only changed cache entries since last save
    pub fn save_cache(&self, dag_id: &str, cache: &Cache) -> Result<(), DagError> {
        let start = std::time::Instant::now();
        let cache_read = &cache.data;

        // Get last saved state from Sled
        let cache_tree = self.sled_db.open_tree("cache")?;

        let previous_state: HashMap<String, DashMap<String, SerializableData>> =
            match cache_tree.get(dag_id.as_bytes())? {
                Some(compressed) => zstd::decode_all(&compressed[..])
                    .map_err(|e| {
                        DagError::SerializationError(serde_json::Error::io(IoError::new(
                            std::io::ErrorKind::Other,
                            e.to_string(),
                        )))
                    })
                    .and_then(|bytes| serde_json::from_slice(&bytes).map_err(DagError::from))?,
                None => HashMap::new(),
            };

        // Calculate delta by comparing with previous state
        let mut delta = HashMap::new();
        for dashmap_ref in cache_read.iter() {
            // Changed: No tuple destructuring
            let node_id = dashmap_ref.key(); // Use .key()
            let current_values = dashmap_ref.value(); // Use .value()
            match previous_state.get(node_id) {
                Some(prev_values) => {
                    let mut node_delta = HashMap::new();
                    for inner_ref in current_values.iter() {
                        // Iterate over inner DashMap
                        let key = inner_ref.key();
                        let value = inner_ref.value();
                        if !prev_values.contains_key(key)
                            || prev_values
                                .get(key)
                                .map_or(true, |prev_value| prev_value.value != value.value)
                        {
                            node_delta.insert(key.clone(), value.clone());
                        }
                    }
                    if !node_delta.is_empty() {
                        delta.insert(node_id.clone(), node_delta);
                    }
                }
                None => {
                    // If no previous state, the entire inner map is a delta.
                    let node_delta: HashMap<String, SerializableData> = current_values
                        .iter()
                        .map(|inner_ref| (inner_ref.key().clone(), inner_ref.value().clone()))
                        .collect();
                    delta.insert(node_id.clone(), node_delta);
                }
            }
        }

        // Only save if there are changes
        if !delta.is_empty() {
            let serialized = serde_json::to_vec(&delta)?;
            let compressed = zstd::encode_all(&*serialized, 3).map_err(|e| {
                DagError::SerializationError(serde_json::Error::custom(e.to_string()))
            })?;

            // Retry logic with exponential backoff
            let mut last_error = None;
            for attempt in 0..3 {
                match cache_tree.insert(dag_id.as_bytes(), compressed.clone()) {
                    Ok(_) => {
                        let duration = start.elapsed();
                        info!(
                            "Cache delta saved for DAG {}: {} nodes updated, {} bytes, took {:?}",
                            dag_id,
                            delta.len(),
                            compressed.len(),
                            duration
                        );
                        return Ok(());
                    }
                    Err(e) => {
                        last_error = Some(e);
                        if attempt < 2 {
                            let delay =
                                std::time::Duration::from_millis(100 * 2u64.pow(attempt as u32));
                            warn!(
                                "Cache save attempt {} failed, retrying in {:?}",
                                attempt + 1,
                                delay
                            );
                            std::thread::sleep(delay);
                        }
                    }
                }
            }
            Err(DagError::DatabaseError(last_error.unwrap_or_else(|| {
                sled::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Unknown database error occurred",
                ))
            })))
        } else {
            trace!("No changes detected in cache for DAG {}", dag_id);
            Ok(())
        }
    }

    /// Saves a specific delta update to the cache
    pub fn save_cache_delta(
        &self,
        dag_id: &str,
        delta: HashMap<String, HashMap<String, SerializableData>>,
    ) -> Result<(), DagError> {
        let start = std::time::Instant::now();
        let cache_tree = self.sled_db.open_tree("cache")?;

        // Load and merge with existing cache
        let mut current = match cache_tree.get(dag_id.as_bytes())? {
            Some(compressed) => zstd::decode_all(&compressed[..])
                .map_err(|e| DagError::SerializationError(serde_json::Error::io(e)))
                .and_then(|bytes| serde_json::from_slice(&bytes).map_err(DagError::from))
                .unwrap_or_else(|e| {
                    warn!("Failed to load existing cache, starting fresh: {}", e);
                    HashMap::new()
                }),
            None => HashMap::new(),
        };

        // Apply delta updates
        for (node_id, updates) in delta {
            current
                .entry(node_id)
                .or_insert_with(HashMap::new)
                .extend(updates);
        }

        // Serialize and compress
        let serialized = serde_json::to_vec(&current)?;
        let compressed = zstd::encode_all(&*serialized, 3)
            .map_err(|e| DagError::SerializationError(serde_json::Error::custom(e.to_string())))?;

        // Retry logic
        let mut last_error = None;
        for attempt in 0..3 {
            match cache_tree.insert(dag_id.as_bytes(), compressed.clone()) {
                Ok(_) => {
                    let duration = start.elapsed();
                    info!(
                        "Cache delta merged for DAG {}: {} total nodes, {} bytes, took {:?}",
                        dag_id,
                        current.len(),
                        compressed.len(),
                        duration
                    );
                    return Ok(());
                }
                Err(e) => {
                    last_error = Some(e);
                    if attempt < 2 {
                        let delay =
                            std::time::Duration::from_millis(100 * 2u64.pow(attempt as u32));
                        warn!(
                            "Delta save attempt {} failed, retrying in {:?}",
                            attempt + 1,
                            delay
                        );
                        std::thread::sleep(delay);
                    }
                }
            }
        }

        Err(DagError::DatabaseError(last_error.unwrap_or_else(|| {
            sled::Error::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Unknown database error occurred",
            ))
        })))
    }

    /// Adds a new node to an existing DAG with validation and transactional safety
    pub fn add_node(
        &mut self,
        name: &str,
        node_id: String,
        action_name: String, // Changed from Arc<dyn NodeAction> to String
        dependencies: Vec<String>,
    ) -> Result<(), DagError> {
        // Verify the action is registered
        let registry = self.function_registry.read().unwrap();
        if !registry.contains_key(&action_name) {
            return Err(DagError::ActionNotRegistered(action_name));
        }

        // Acquire write locks for both graphs and prebuilt_dags
        let mut graphs = self
            .graphs
            .write()
            .map_err(|e| DagError::LockError(e.to_string()))?;
        let mut dags = self
            .prebuilt_dags
            .write()
            .map_err(|e| DagError::LockError(e.to_string()))?;

        // Validate node ID uniqueness and dependencies within the lock
        let (dag, indices) = dags
            .get_mut(name)
            .ok_or_else(|| DagError::NodeNotFound(format!("DAG not found: {}", name)))?;

        if indices.contains_key(&node_id) {
            return Err(DagError::NodeAlreadyExists(node_id));
        }

        // Validate all dependencies exist
        for dep_id in &dependencies {
            if !indices.contains_key(dep_id) {
                return Err(DagError::DependencyNotFound(dep_id.to_string()));
            }
        }

        // Create new node
        let node = Node {
            id: node_id.clone(),
            dependencies: dependencies.clone(),
            action: action_name, // Use the action name directly
            inputs: Vec::new(),
            outputs: Vec::new(),
            failure: String::new(),
            onfailure: true,
            description: format!("Dynamically added node: {}", node_id),
            timeout: self.config.timeout_seconds.unwrap_or(3600),
            try_count: self.config.max_attempts.unwrap_or(3),
            instructions: None,
        };

        // Create temporary DAG for validation
        let mut temp_dag = dag.clone();
        let node_index = temp_dag.add_node(node.clone());

        // Add edges in temporary DAG
        for dep_id in &dependencies {
            let dep_index = indices[dep_id];
            temp_dag.add_edge(dep_index, node_index, ());
        }

        // Validate the temporary DAG
        if let Err(e) = validate_dag_structure(&temp_dag) {
            return Err(DagError::InvalidGraph(e.to_string()));
        }

        // If validation passed, commit changes to actual DAG
        let node_index = dag.add_node(node.clone());
        indices.insert(node_id.clone(), node_index);

        for dep_id in &dependencies {
            let dep_index = indices[dep_id];
            dag.add_edge(dep_index, node_index, ());
        }

        // Update the graph definition
        if let Some(graph) = graphs.get_mut(name) {
            graph.nodes.push(node);
        }

        debug!("Successfully added node {} to DAG {}", node_id, name);
        Ok(())
    }

    /// Updates the cache with ad-hoc instructions
    pub fn update_cache(
        &self,
        dag_id: &str,
        key: String,
        value: SerializableData,
    ) -> Result<(), DagError> {
        let mut cache = self
            .load_cache(dag_id)
            .map_err(|e| DagError::ExecutionError(e.to_string()))?;
        {
            let mut write_cache = &cache.data;

            if let Some(node_map) = write_cache.get_mut(dag_id) {
                node_map.insert(key, value);
            } else {
                let mut new_map = DashMap::new();
                new_map.insert(key, value);
                write_cache.insert(dag_id.to_string(), new_map);
            }
        }
        self.save_cache(dag_id, &cache)?;
        Ok(())
    }

    /// Resumes execution of a paused DAG
    pub async fn resume_from_pause(
        &mut self,
        dag_id: &str,
        input: Option<String>,
    ) -> Result<DagExecutionReport, DagError> {
        // Verify DAG is in pending state
        let pending_tree = self.sled_db.open_tree("pending")?;
        if pending_tree.get(dag_id.as_bytes())?.is_none() {
            return Err(DagError::InvalidState(format!(
                "DAG {} is not in paused state",
                dag_id
            )));
        }

        // Load saved cache state
        let cache = self
            .load_cache(dag_id)
            .map_err(|e| DagError::ExecutionError(e.to_string()))?;

        // Update cache with new input if provided
        if let Some(input_value) = input {
            self.update_cache(
                dag_id,
                "pending_instructions".to_string(),
                SerializableData {
                    value: serde_json::to_value(&input_value).unwrap(),
                },
            )?;
        }

        // Update Sled trees
        pending_tree.remove(dag_id.as_bytes())?;
        let active_tree = self.sled_db.open_tree("active")?;
        active_tree.insert(
            dag_id.as_bytes(),
            serde_json::to_vec(&DagMetadata {
                status: "Running".to_string(),
                task: dag_id.to_string(),
            })?,
        )?;

        // Clear paused flag
        *self.paused.write().unwrap() = false;

        // Resume execution
        let (tx, rx) = oneshot::channel();
        self.execute_static_dag(dag_id, &cache, rx).await
    }

    /// Serializes an execution tree to JSON format
    pub fn serialize_tree_to_json(&self, dag_name: &str) -> Result<String> {
        let trees = self.tree.read().unwrap();
        let tree = trees
            .get(dag_name)
            .ok_or_else(|| anyhow!("No execution tree found for DAG: {}", dag_name))?;

        #[derive(Serialize)]
        struct SerializedTree<'a> {
            nodes: Vec<(usize, &'a NodeSnapshot)>,
            edges: Vec<(usize, usize, &'a ExecutionEdge)>,
        }

        let serialized = SerializedTree {
            nodes: tree
                .node_references()
                .map(|(i, n)| (i.index(), n))
                .collect(),
            edges: tree
                .edge_references()
                .map(|e| (e.source().index(), e.target().index(), e.weight()))
                .collect(),
        };

        serde_json::to_string_pretty(&serialized)
            .map_err(|e| anyhow!("Failed to serialize execution tree: {}", e))
    }

    pub fn debug_tree_state(&self, dag_id: &str) {
        let trees = self.tree.read().unwrap();
        info!("Current execution trees:");
        for (name, tree) in trees.iter() {
            info!("  DAG '{}': {} nodes", name, tree.node_count());
        }

        // Also check Sled
        if let Ok(snapshots) = self.sled_db.open_tree("snapshots") {
            info!("Sled snapshots:");
            for result in snapshots.iter() {
                if let Ok((key, _)) = result {
                    if let Ok(key_str) = String::from_utf8(key.to_vec()) {
                        info!("  Found snapshot for DAG: {}", key_str);
                    }
                }
            }
        }
    }

    /// Serializes an execution tree to DOT format for visualization
    pub fn serialize_tree_to_dot(&self, dag_id: &str) -> Result<String> {
        let normalized_id = normalize_dag_name(dag_id);
        info!("Generating DOT graph for DAG '{}'", normalized_id);

        let trees = self.tree.read().unwrap();
        let tree = match trees.get(&normalized_id) {
            Some(t) => {
                info!(
                    "Using in-memory execution tree with {} nodes",
                    t.node_count()
                );
                t
            }
            None => {
                info!("In-memory tree not found, attempting to load from Sled");
                match self.load_execution_tree(&normalized_id)? {
                    Some(t) => {
                        info!(
                            "Loaded execution tree from Sled with {} nodes",
                            t.node_count()
                        );
                        std::mem::drop(trees);
                        let mut trees_write = self.tree.write().unwrap();
                        trees_write.insert(normalized_id.clone(), t.clone());
                        &trees_write.clone()[&normalized_id]
                    }
                    None => {
                        return Err(anyhow!(
                            "No execution tree found for DAG: {}",
                            normalized_id
                        ))
                    }
                }
            }
        };

        // Add this helper function to ensure consistent DAG name handling
      

        let cache = self.load_cache(&normalized_id)?;
        let cache_read = &cache.data;
        info!("Loaded cache with {} entries", cache_read.len());

        let mut dot = String::from("digraph ExecutionFlow {\n");
        dot.push_str("  graph [rankdir=LR, nodesep=0.5, ranksep=1.0];\n");
        dot.push_str("  node [shape=box, style=rounded, fontname=\"Helvetica\", width=1.5];\n");
        dot.push_str("  edge [fontsize=10];\n\n");

        let mut processed_nodes = HashSet::new();
        let mut processed_edges = HashSet::new();

        let flatten_value = |value: &str| -> String {
            let trimmed =
                value.trim_matches(|c| c == '"' || c == '[' || c == ']' || c == '{' || c == '}');
            if trimmed.len() > 50 {
                format!("{}...", &trimmed[..47])
            } else {
                trimmed.to_string()
            }
        };

        for node_idx in tree.node_indices() {
            let node = &tree[node_idx];
            if processed_nodes.contains(&node.node_id) {
                continue;
            }
            processed_nodes.insert(node.node_id.clone());

            let mut label = format!(
                "<<TABLE BORDER=\"0\" CELLBORDER=\"1\" CELLSPACING=\"0\" CELLPADDING=\"4\">\n\
             <TR><TD BGCOLOR=\"#E8E8E8\"><B>{}</B></TD></TR>\n",
                node.node_id
            );

            let action_name = self
                .graphs
                .read()
                .unwrap()
                .iter()
                .find_map(|(_, g)| g.nodes.iter().find(|n| n.id == node.node_id))
                .map(|n| n.action.clone())
                .unwrap_or_else(|| "unknown".to_string());
            label.push_str(&format!("<TR><TD>Action: {}</TD></TR>\n", action_name));

            if let Some(node_cache) = cache_read.get(&node.node_id) {
                let mut inputs = Vec::new();
                let mut outputs = Vec::new();

                // Get the *inner* DashMap and iterate over *it*.
                for inner_ref in node_cache.value().iter() {
                    // Changed: .value().iter()
                    let key = inner_ref.key(); // Use .key()
                    let data = inner_ref.value(); // Use .value()
                    let value_str = flatten_value(&data.value.to_string());
                    if key.starts_with("input_") || node_cache.len() == 1 {
                        inputs.push((key.clone(), value_str));
                    } else if key.starts_with("output_") || key == "retrieved_data" {
                        outputs.push((key.clone(), value_str));
                    }
                }

                if !inputs.is_empty() {
                    label.push_str("<TR><TD BGCOLOR=\"#E8F0FE\"><B>Inputs</B></TD></TR>\n");
                    for (key, value) in inputs {
                        label.push_str(&format!("<TR><TD><I>{}</I>: {}</TD></TR>\n", key, value));
                    }
                }

                if !outputs.is_empty() {
                    label.push_str("<TR><TD BGCOLOR=\"#E8F0FE\"><B>Outputs</B></TD></TR>\n");
                    for (key, value) in outputs {
                        label.push_str(&format!("<TR><TD><I>{}</I>: {}</TD></TR>\n", key, value));
                    }
                }
            }

            label.push_str(&format!(
                "<TR><TD BGCOLOR=\"{}\">{}</TD></TR>\n",
                if node.outcome.success {
                    "#E6FFE6"
                } else {
                    "#FFE6E6"
                },
                if node.outcome.success {
                    "✓ Success"
                } else {
                    "✗ Failed"
                }
            ));

            label.push_str(&format!(
                "<TR><TD>Time: {}</TD></TR>\n",
                node.timestamp.format("%H:%M:%S")
            ));

            if !node.outcome.success {
                if let Some(error) = &node.outcome.final_error {
                    label.push_str(&format!(
                        "<TR><TD><FONT COLOR=\"red\">Error: {}</FONT></TD></TR>\n",
                        flatten_value(error)
                    ));
                }
            }

            label.push_str("</TABLE>>");

            let color = if node.outcome.success { "green" } else { "red" };
            dot.push_str(&format!(
                "  \"{}\" [label={}, color={}, fontcolor=black];\n",
                node.node_id, label, color
            ));
        }

        dot.push_str("\n  // Edges\n");
        for edge in tree.edge_references() {
            let source = &tree[edge.source()].node_id;
            let target = &tree[edge.target()].node_id;
            let edge_key = format!("{}->{}", source, target);

            if !processed_edges.contains(&edge_key) {
                processed_edges.insert(edge_key.clone());
                let label = edge.weight().label.clone();
                info!("Recording edge: {} -> {}", source, target);
                dot.push_str(&format!(
                    "  \"{}\" -> \"{}\" [label=\"{}\"];\n",
                    source, target, label
                ));
            }
        }

        dot.push_str("}\n");
        info!("Generated DOT graph successfully for '{}'", normalized_id);
        Ok(dot)
    }

    /// Loads cache from Sled for a given DAG
    fn load_cache(&self, dag_id: &str) -> Result<Cache> {
        let normalized_id = normalize_dag_name(dag_id);
        info!("Loading cache for DAG: {}", normalized_id);

        let cache_tree = self.sled_db.open_tree("cache")?;
        let snapshots_tree = self.sled_db.open_tree("snapshots")?;

        // Prioritize cache tree (final state)
        if let Some(compressed) = cache_tree.get(normalized_id.as_bytes())? {
            let bytes = zstd::decode_all(&compressed[..])?;
            let json_str = String::from_utf8(bytes)?;
            info!("Loaded cache JSON: {}", json_str);
            let cache = load_cache_from_json(&json_str)?;
            info!("Loaded {} entries from cache", cache.data.len());
            return Ok(cache);
        }

        // Fallback to snapshots tree
        if let Some(compressed) = snapshots_tree.get(normalized_id.as_bytes())? {
            let bytes = zstd::decode_all(&compressed[..])?;
            let json_str = String::from_utf8(bytes)?;
            info!("Loaded snapshots JSON: {}", json_str);
            let cache = load_cache_from_json(&json_str)?;
            info!("Loaded {} entries from snapshots", cache.data.len());
            return Ok(cache);
        }

        info!(
            "No cache found in cache or snapshots trees for DAG {}",
            normalized_id
        );
        Ok(Cache::new())
    }

    /// Helper method to check if execution is stopped
    async fn check_stopped(&self) -> bool {
        *self.stopped.read().unwrap()
    }

    /// Stops execution and cancels any waiting human interrupts
    pub fn stop(&mut self) {
        *self.stopped.write().unwrap() = true;

        // Cancel any waiting human interrupts
        for (_, graph) in self.graphs.read().unwrap().iter() {
            for node in &graph.nodes {
                if node.action == "human_interrupt" {
                    if let Some(action) = self.function_registry.read().unwrap().get(&node.action) {
                        if let Some(human_interrupt) =
                            action.as_any().downcast_ref::<HumanInterrupt>()
                        {
                            human_interrupt.cancel();
                        }
                    }
                }
            }
        }
    }

    /// Saves the execution tree to Sled
    pub fn save_execution_tree(&self, dag_id: &str) -> Result<(), DagError> {
        let trees = self
            .tree
            .read()
            .map_err(|e| DagError::LockError(e.to_string()))?;
        if let Some(tree) = trees.get(dag_id) {
            // Convert to serializable format
            let serializable_tree = SerializableExecutionTree::from_execution_tree(tree);
            let serialized = serde_json::to_vec(&serializable_tree)?;
            let compressed = zstd::encode_all(&*serialized, 3).map_err(|e| {
                DagError::SerializationError(serde_json::Error::custom(e.to_string()))
            })?;
            let tree_store = self.sled_db.open_tree("execution_trees")?;
            tree_store.insert(normalize_dag_name(dag_id).as_bytes(), compressed)?;
            info!("Saved execution tree for '{}'", dag_id);
        } else {
            warn!("No execution tree found to save for '{}'", dag_id);
        }
        Ok(())
    }

    /// Loads the execution tree from Sled
    pub fn load_execution_tree(&self, dag_id: &str) -> Result<Option<ExecutionTree>> {
        let normalized_id = normalize_dag_name(dag_id);
        let tree_store = self.sled_db.open_tree("execution_trees")?;
        if let Some(compressed) = tree_store.get(normalized_id.as_bytes())? {
            let bytes = zstd::decode_all(&compressed[..])?;
            let serializable_tree: SerializableExecutionTree = serde_json::from_slice(&bytes)?;
            let tree = serializable_tree.to_execution_tree();
            Ok(Some(tree))
        } else {
            Ok(None)
        }
    }

    /// Debug utility to print Sled DB contents
    pub fn debug_print_sled_db(&self) -> Result<(), DagError> {
        info!("=== Sled DB Contents ===");
        for tree_name in ["cache", "snapshots", "execution_trees", "active", "pending"].iter() {
            if let Ok(tree) = self.sled_db.open_tree(*tree_name) {
                info!("Tree '{}' contents:", tree_name);
                for result in tree.iter() {
                    match result {
                        Ok((key, value)) => {
                            let key_str = String::from_utf8_lossy(&key);
                            info!("  Key: {}, Value size: {} bytes", key_str, value.len());
                            if let Ok(bytes) = zstd::decode_all(&value[..]) {
                                if let Ok(json_str) = String::from_utf8(bytes) {
                                    info!("    Decoded: {}", json_str);
                                }
                            }
                        }
                        Err(e) => error!("Error reading tree entry: {}", e),
                    }
                }
            }
        }
        info!("=====================");
        Ok(())
    }
}

/// New types for reporting node and DAG execution outcomes.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NodeExecutionOutcome {
    pub node_id: String,
    /// Whether the node's execution succeeded.
    pub success: bool,
    /// Each retry's error message (if any).
    pub retry_messages: Vec<String>,
    /// The final error message recorded (if any) when the node ultimately fails.
    pub final_error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DagExecutionReport {
    /// The outcome for each node executed.
    pub node_outcomes: Vec<NodeExecutionOutcome>,
    /// Indicates the overall DAG status (false if any critical node failed).
    pub overall_success: bool,
    /// A consolidated overall error message (if any). This should make it easy to
    /// quickly know *why* the DAG failed.
    pub error: Option<String>,
}

/// Updates the execution tree with a new node snapshot and its dependencies
fn update_execution_tree(
    tree: &mut ExecutionTree,
    snapshot: NodeSnapshot,
    parent_id: Option<String>,
    label: Option<String>,
) -> NodeIndex {
    // Add the new node
    let node_idx = tree.add_node(snapshot);

    // If there's a parent, validate and add edge
    if let Some(parent) = parent_id {
        // Explicitly check for parent existence
        if let Some(parent_idx) = tree.node_indices().find(|i| tree[*i].node_id == parent) {
            tree.add_edge(
                parent_idx,
                node_idx,
                ExecutionEdge {
                    parent,
                    label: label.unwrap_or_else(|| "executed_after".to_string()),
                },
            );
        } else {
            warn!("Parent node {} not found in execution tree", parent);
        }
    }

    node_idx
}

/// Helper function to record execution snapshots in the tree
fn record_execution_snapshot(
    executor: &DagExecutor,
    node: &Node,
    outcome: &NodeExecutionOutcome,
    cache: &Cache,
) {
    if let Some((dag_name, _)) = executor
        .graphs
        .read()
        .unwrap()
        .iter()
        .find(|(_, g)| g.nodes.iter().any(|n| n.id == node.id))
    {
        info!(
            "Recording snapshot for DAG '{}', node '{}'",
            dag_name, node.id
        );

        let snapshot = NodeSnapshot {
            node_id: node.id.clone(),
            outcome: outcome.clone(),
            cache_ref: generate_cache_ref(&node.id),
            timestamp: chrono::Local::now().naive_local(),
            channel: None,    // New field for tracking message channel
            message_id: None, // New field for tracking message ID
        };

        // Update in-memory execution tree
        let mut trees = executor.tree.write().unwrap();
        let tree = trees.entry(dag_name.clone()).or_insert_with(DiGraph::new);

        // Add the node to the tree
        let node_idx = tree.add_node(snapshot.clone());

        // Add edges for ALL dependencies
        info!("Dependencies for {}: {:?}", node.id, node.dependencies);
        for parent_id in &node.dependencies {
            if let Some(parent_idx) = tree.node_indices().find(|i| tree[*i].node_id == *parent_id) {
                tree.add_edge(
                    parent_idx,
                    node_idx,
                    ExecutionEdge {
                        parent: parent_id.clone(),
                        label: "executed_after".to_string(),
                    },
                );
                info!("Added edge: {} -> {}", parent_id, node.id);
            } else {
                warn!(
                    "Parent node {} not found in execution tree for {}",
                    parent_id, node.id
                );
            }
        }

        // Store full cache state in Sled "snapshots" tree
        let full_cache = {
            let cache_read = &cache.data;
            info!("Cache contents for node {}:", node.id);
            for dashmap_ref in cache_read.iter() {
                // Changed: No tuple destructuring
                let node_id = dashmap_ref.key(); // Use .key()
                let node_cache = dashmap_ref.value(); // Use .value()
                info!("  Node {}:", node_id);
                for inner_ref in node_cache.iter() {
                    // Iterate over inner DashMap
                    let key = inner_ref.key(); // Use .key()
                    let value = inner_ref.value(); // Use .value()
                    info!("    {} = {}", key, value.value.to_string());
                }
            }
            cache_read.clone()
        };

        // Create a serializable representation of the cache.
        let serializable_cache: HashMap<String, HashMap<String, SerializableData>> = full_cache
            .iter()
            .map(|m| (m.key().clone(), m.value().clone().into_iter().collect()))
            .collect();

        match serde_json::to_vec(&serializable_cache) {
            Ok(serialized) => {
                info!(
                    "Serialized cache for '{}': {} bytes",
                    dag_name,
                    serialized.len()
                );
                match zstd::encode_all(&*serialized, 3) {
                    Ok(compressed) => {
                        info!(
                            "Compressed cache for '{}': {} bytes",
                            dag_name,
                            compressed.len()
                        );
                        if let Ok(tree) = executor.sled_db.open_tree("snapshots") {
                            match tree.insert(dag_name.as_bytes(), compressed) {
                                Ok(_) => {
                                    info!("Successfully saved full snapshot for '{}'", dag_name)
                                }
                                Err(e) => {
                                    error!("Failed to save snapshot for '{}': {}", dag_name, e)
                                }
                            }
                        }
                    }
                    Err(e) => error!("Failed to compress cache for '{}': {}", dag_name, e),
                }
            }
            Err(e) => error!("Failed to serialize cache for '{}': {}", dag_name, e),
        }
    }
}

/// Executes the DAG asynchronously and produces a `DagExecutionReport` summarizing the outcomes.
pub async fn execute_dag_async(
    executor: &mut DagExecutor,
    dag: &DiGraph<Node, ()>,
    cache: &Cache,
) -> (DagExecutionReport, bool) {
    // Get DAG name and validate it exists
    let dag_name = {
        let graphs = match executor.graphs.read() {
            Ok(guard) => guard,
            Err(e) => {
                error!("Failed to acquire read lock: {}", e);
                return (
                    create_execution_report(
                        Vec::new(),
                        false,
                        Some("Failed to acquire lock".to_string()),
                    ),
                    false,
                );
            }
        };

        match dag
            .node_references()
            .next()
            .and_then(|(_, node)| graphs.iter().find(|(_, g)| g.nodes.contains(node)))
        {
            Some((name, _)) if !name.is_empty() => name.clone(),
            _ => {
                return (
                    create_execution_report(
                        Vec::new(),
                        false,
                        Some("Unable to determine DAG name - execution aborted".to_string()),
                    ),
                    false,
                );
            }
        }
    };

    let mut node_outcomes = Vec::new();
    let mut overall_success = true;
    let mut executed_nodes = std::collections::HashSet::new();

    // Main execution loop
    while !*executor.stopped.read().unwrap() {
        // Get the latest DAG state
        let current_dag = {
            let dags = match executor.prebuilt_dags.read() {
                Ok(dags) => dags,
                Err(e) => {
                    return (
                        create_execution_report(Vec::new(), false, Some(e.to_string())),
                        false,
                    )
                }
            };

            match dags.get(&dag_name) {
                Some((dag, _)) => dag.clone(),
                None => break, // Exit if we can't get the DAG
            }
        };

        // Get supervisor iteration count for more accurate tracking in dynamic DAGs
        let supervisor_iteration: usize = if let Some(supervisor_node) = current_dag
            .node_references()
            .find(|(_, node)| node.action == "supervisor_step")
        {
            parse_input_from_name(cache, "iteration".to_string(), &supervisor_node.1.inputs)
                .unwrap_or(0)
        } else {
            executed_nodes.len()
        };

        // Check iteration limit using supervisor count for dynamic DAGs
        if let Some(max_iter) = executor.config.max_iterations {
            if supervisor_iteration >= max_iter as usize {
                return (
                    create_execution_report(
                        node_outcomes,
                        false,
                        Some(format!("Maximum iterations ({}) reached", max_iter)),
                    ),
                    true,
                );
            }
        }

        // Check timeout
        let elapsed = chrono::Local::now().naive_local() - executor.start_time;
        if elapsed.num_seconds() > executor.config.timeout_seconds.unwrap_or(3600) as i64 {
            return (
                create_execution_report(
                    node_outcomes,
                    false,
                    Some("DAG timeout exceeded".to_string()),
                ),
                false,
            );
        }

        let mut topo = Topo::new(&current_dag);
        let mut has_new_nodes = false;

        // Process all available nodes in topological order
        while let Some(node_index) = topo.next(&current_dag) {
            let node = &current_dag[node_index];

            // Skip already executed nodes
            if executed_nodes.contains(&node.id) {
                continue;
            }

            has_new_nodes = true;
            let outcome = execute_node_async(executor, node, cache).await;
            executed_nodes.insert(node.id.clone());

            if !outcome.success {
                match executor.config.on_failure {
                    OnFailure::Stop => {
                        overall_success = false;
                        node_outcomes.push(outcome);
                        return (create_execution_report(node_outcomes, false, None), false);
                    }
                    OnFailure::Pause => {
                        // Save state before pausing
                        if let Err(e) = executor.save_cache(&node.id, cache) {
                            error!("Failed to save cache before pause: {}", e);
                        }
                        overall_success = false;
                        node_outcomes.push(outcome);
                        return (create_execution_report(node_outcomes, false, None), false);
                    }
                    OnFailure::Continue => {
                        overall_success = false;
                        node_outcomes.push(outcome);
                    }
                }
            } else {
                node_outcomes.push(outcome);
            }

            // Check if we need to pause for dynamic updates
            if *executor.paused.read().unwrap() {
                return (
                    create_execution_report(node_outcomes, overall_success, None),
                    false,
                );
            }
        }

        // Break if no new nodes were found in this iteration
        if !has_new_nodes {
            break;
        }

        // Small delay to prevent tight loop
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Save execution tree before returning
    if let Err(e) = executor.save_execution_tree(&dag_name) {
        error!("Failed to save execution tree: {}", e);
    }

    // Save the full cache state before returning
    if let Err(e) = executor.save_cache(&dag_name, cache) {
        error!("Failed to save cache for '{}': {}", dag_name, e);
    }

    (
        create_execution_report(node_outcomes, overall_success, None),
        false,
    )
}

/// Modified execute_node_async with configurable retry strategy
async fn execute_node_async(
    executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache,
) -> NodeExecutionOutcome {
    let mut outcome = NodeExecutionOutcome {
        node_id: node.id.clone(),
        success: false,
        retry_messages: Vec::with_capacity(node.try_count as usize),
        final_error: None,
    };

    let action = match executor.function_registry.read() {
        Ok(registry) => match registry.get(&node.action) {
            Some(action) => action.clone(),
            None => {
                let error = format!(
                    "Action '{}' not registered for node '{}'",
                    node.action, node.id
                );
                error!("{}", error);
                outcome.final_error = Some(error);
                record_execution_snapshot(executor, node, &outcome, cache);
                return outcome;
            }
        },
        Err(e) => {
            let error = format!("Failed to acquire registry lock: {}", e);
            error!("{}", error);
            outcome.final_error = Some(error);
            record_execution_snapshot(executor, node, &outcome, cache);
            return outcome;
        }
    };

    let mut retries_left = node.try_count;

    while retries_left > 0 {
        let attempt_number = node.try_count - retries_left + 1;
        info!(
            "Executing node '{}' (attempt {}/{})",
            node.id, attempt_number, node.try_count
        );

        // Add per-node timeout enforcement
        let node_start = chrono::Local::now().naive_local();
        let node_timeout = Duration::from_secs(node.timeout);
        let global_remaining = Duration::from_secs(executor.config.timeout_seconds.unwrap_or(3600))
            .saturating_sub(
                node_start
                    .signed_duration_since(executor.start_time)
                    .to_std()
                    .unwrap_or_default(),
            );

        let effective_timeout = node_timeout.min(global_remaining);

        // Execute with timeout and handle errors
        let execution_result =
            timeout(effective_timeout, action.execute(executor, node, cache)).await;

        match execution_result {
            Ok(Ok(_)) => {
                info!(
                    "Node '{}' execution succeeded on attempt {}",
                    node.id, attempt_number
                );
                outcome.success = true;
                record_execution_snapshot(executor, node, &outcome, cache);
                return outcome;
            }
            Ok(Err(e)) => {
                // Handle regular execution error
                let err_message = e.to_string();
                outcome.retry_messages.push(format!(
                    "Attempt {} failed: {}",
                    attempt_number, err_message
                ));

                if !handle_failure(
                    executor,
                    node,
                    &mut outcome,
                    &mut retries_left,
                    attempt_number,
                    err_message,
                    cache,
                )
                .await
                {
                    record_execution_snapshot(executor, node, &outcome, cache);
                    return outcome;
                }
            }
            Err(elapsed) => {
                // Handle timeout error
                let err_message = format!("Timeout after {:?}", effective_timeout);
                outcome.retry_messages.push(format!(
                    "Attempt {} failed: {}",
                    attempt_number, err_message
                ));

                error!(
                    "Node '{}' execution timed out (attempt {}/{}): {}",
                    node.id, attempt_number, node.try_count, err_message
                );

                if !handle_failure(
                    executor,
                    node,
                    &mut outcome,
                    &mut retries_left,
                    attempt_number,
                    err_message,
                    cache,
                )
                .await
                {
                    record_execution_snapshot(executor, node, &outcome, cache);
                    return outcome;
                }
            }
        }
    }

    outcome.final_error = Some(format!(
        "Node '{}' failed after {} attempts",
        node.id, node.try_count
    ));
    record_execution_snapshot(executor, node, &outcome, cache);
    outcome
}

// Helper function to handle failure cases
async fn handle_failure(
    executor: &DagExecutor,
    node: &Node,
    outcome: &mut NodeExecutionOutcome,
    retries_left: &mut u8,
    attempt_number: u8,
    err_message: String,
    cache: &Cache,
) -> bool {
    if node.onfailure && *retries_left > 1 {
        // Calculate retry delay based on strategy
        let delay = match &executor.config.retry_strategy {
            RetryStrategy::Exponential {
                initial_delay_secs,
                max_delay_secs,
                multiplier,
            } => {
                let attempt = (node.try_count - *retries_left) as f64;
                let delay = (*initial_delay_secs as f64 * multiplier.powf(attempt)).round() as u64;
                delay.min(*max_delay_secs)
            }
            RetryStrategy::Linear { delay_secs } => *delay_secs,
            RetryStrategy::Immediate => 0,
        };

        if delay > 0 {
            info!(
                "Waiting {} seconds before retry {} for node '{}'",
                delay,
                attempt_number + 1,
                node.id
            );
            sleep(Duration::from_secs(delay)).await;
        }

        *retries_left -= 1;
        true
    } else {
        outcome.final_error = Some(format!(
            "Failed after {} attempts. Last error: {}",
            attempt_number, err_message
        ));
        record_execution_snapshot(executor, node, outcome, cache);
        false
    }
}

/// Helper function to create execution reports
fn create_execution_report(
    node_outcomes: Vec<NodeExecutionOutcome>,
    overall_success: bool,
    error: Option<String>,
) -> DagExecutionReport {
    let error = error.or_else(|| {
        let error_messages: Vec<String> = node_outcomes
            .iter()
            .filter_map(|o| o.final_error.clone())
            .collect();
        if !error_messages.is_empty() {
            Some(error_messages.join("\n"))
        } else {
            None
        }
    });

    DagExecutionReport {
        node_outcomes,
        overall_success,
        error,
    }
}

/// Validates the structure of the DAG.
pub fn validate_dag_structure(dag: &DiGraph<Node, ()>) -> Result<(), Error> {
    if is_cyclic_directed(dag) {
        return Err(anyhow!("The graph is not a DAG as it contains cycles."));
    }
    Ok(())
}

/// Validates the dependencies of the nodes.
pub fn validate_node_dependencies(
    nodes: &[Node],
    node_indices: &HashMap<String, NodeIndex>,
) -> Result<(), Error> {
    for node in nodes {
        for dependency_id in &node.dependencies {
            if !node_indices.contains_key(dependency_id) {
                return Err(anyhow!(format!(
                    "Dependency '{}' for node '{}' not found.",
                    dependency_id, node.id
                )));
            }
        }
    }
    Ok(())
}

/// Validates the actions of the nodes.
pub fn validate_node_actions(executor: &DagExecutor, nodes: &[Node]) -> Result<(), Error> {
    let registry = executor.function_registry.clone();
    for node in nodes {
        if !registry.read().unwrap().contains_key(&node.action) {
            return Err(anyhow!(format!(
                "Action '{}' for node '{}' is not registered.",
                node.action, node.id
            )));
        }
    }
    Ok(())
}

/// Human intervention action that can pause execution for input
///
/// This action allows for human review and intervention during DAG execution.
/// It can:
/// - Wait for a configured duration for human input
/// - Handle timeout scenarios based on configuration
/// - Resume execution when input is received
/// - Integrate with the supervisor for dynamic DAG updates
#[derive(Default)]
pub struct HumanInterrupt {
    /// Channel sender for signaling when input is received
    input_tx: Arc<RwLock<Option<tokio::sync::mpsc::Sender<()>>>>,
    /// Channel for cancellation
    cancel_tx: Arc<RwLock<Option<tokio::sync::mpsc::Sender<()>>>>,
}

impl HumanInterrupt {
    pub fn new() -> Self {
        Self {
            input_tx: Arc::new(RwLock::new(None)),
            cancel_tx: Arc::new(RwLock::new(None)),
        }
    }

    /// Cancels any ongoing wait operations
    pub fn cancel(&self) {
        if let Ok(cancel_tx) = self.cancel_tx.read() {
            if let Some(tx) = &*cancel_tx {
                let _ = tx.try_send(());
            }
        }
    }
}


  fn normalize_dag_name(dag_id: &str) -> String {
            dag_id.to_lowercase()
        }


        // When creating new nodes, initialize cache_ref with a unique identifier:
pub fn generate_cache_ref(node_id: &str) -> String {
    format!(
        "cache_{}_{}",
        node_id,
        chrono::Utc::now().timestamp_millis()
    )
}

/// Serializable representation of ExecutionTree for storage
#[derive(Serialize, Deserialize)]
struct SerializableExecutionTree {
    nodes: Vec<(usize, NodeSnapshot)>,         // (index, node data)
    edges: Vec<(usize, usize, ExecutionEdge)>, // (source index, target index, edge data)
}

impl SerializableExecutionTree {
    // Convert from ExecutionTree to SerializableExecutionTree
    fn from_execution_tree(tree: &ExecutionTree) -> Self {
        let nodes = tree
            .node_references()
            .map(|(idx, node)| (idx.index(), node.clone()))
            .collect();
        let edges = tree
            .edge_references()
            .map(|edge| {
                (
                    edge.source().index(),
                    edge.target().index(),
                    edge.weight().clone(),
                )
            })
            .collect();
        SerializableExecutionTree { nodes, edges }
    }

    // Convert back from SerializableExecutionTree to ExecutionTree
    fn to_execution_tree(&self) -> ExecutionTree {
        let mut tree = DiGraph::new();
        let mut index_map = HashMap::new();

        // Add nodes
        for (idx, node) in &self.nodes {
            let new_idx = tree.add_node(node.clone());
            index_map.insert(*idx, new_idx);
        }

        // Add edges
        for (src_idx, tgt_idx, edge) in &self.edges {
            let src = index_map[src_idx];
            let tgt = index_map[tgt_idx];
            tree.add_edge(src, tgt, edge.clone());
        }

        tree
    }
}

/// Execution mode for workflows
#[derive(Debug, Clone)]
pub enum ExecutionMode {
    /// Execute a static, pre-loaded DAG
    Static,
    /// Start an agent-driven flow 
    Agent,
}

impl Default for ExecutionMode {
    fn default() -> Self {
        ExecutionMode::Static
    }
}
