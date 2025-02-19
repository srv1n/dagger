//! Dagger - A library for executing directed acyclic graphs (DAGs) with custom actions.
//!
//! This library provides a way to define and execute DAGs with custom actions. It supports
//! loading graph definitions from YAML files, validating the graph structure, and executing
//! custom actions associated with each node in the graph.

use anyhow::anyhow;
use anyhow::{Error, Result};
use petgraph::visit::IntoNodeReferences;
pub mod any;
pub use any::*;
use async_trait::async_trait;
use chrono::NaiveDateTime;
use core::any::type_name;
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
use tokio::time::{sleep, timeout, Duration};
use tracing::{debug, error, info, trace, warn, Level}; // Assuming you're using Tokio for async runtime

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
        }

        $executor.register_action(Arc::new(Action));
    }};
}

/// Specifies how to execute a workflow
#[derive(Debug, Clone)]
pub enum WorkflowSpec {
    /// Execute a static, pre-loaded DAG by name
    Static { name: String },
    /// Start an agent-driven flow with a given task
    Agent { task: String },
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
    Linear {
        delay_secs: u64,
    },
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
            if timeout > 86400 { // 24 hours
                return Err(anyhow!("timeout_seconds cannot exceed 24 hours"));
            }
        }

        // Validate human_wait_minutes
        if let Some(wait) = self.human_wait_minutes {
            if wait > 1440 { // 24 hours
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
    pub value: String,
}

// Update your cache to use the new SerializableData
pub type Cache = RwLock<HashMap<String, HashMap<String, SerializableData>>>;

pub fn serialize_cache_to_json(cache: &Cache) -> Result<String> {
    let cache_read = cache.read().map_err(|e| 
        anyhow!("Failed to acquire cache read lock: {}", e))?;
    
    let serialized_cache: HashMap<String, HashMap<String, String>> = cache_read
        .iter()
        .map(|(node_id, category_map)| {
            let serialized_category: HashMap<String, String> = category_map
                .iter()
                .map(|(output_name, data)| (output_name.clone(), data.value.clone()))
                .collect();
            (node_id.clone(), serialized_category)
        })
        .collect();

    serde_json::to_string(&serialized_cache)
        .map_err(|e| anyhow!("Failed to serialize cache to JSON: {}", e))
}

pub fn serialize_cache_to_prettyjson(cache: &Cache) -> Result<String> {
    let cache_read = cache.read().unwrap();
    let serialized_cache: HashMap<String, HashMap<String, String>> = cache_read
        .iter()
        .map(|(node_id, category_map)| {
            let serialized_category: HashMap<String, String> = category_map
                .iter()
                .map(|(output_name, data)| (output_name.clone(), data.value.clone()))
                .collect();
            (node_id.clone(), serialized_category)
        })
        .collect();

    // Convert to JSON string
    serde_json::to_string_pretty(&serialized_cache)
        .map_err(|e| anyhow::anyhow!(format!("Serialization error: {}", e)))
}

/// Function to load the cache from JSON
/// Function to load the cache from JSON
pub fn load_cache_from_json(json_data: &str) -> Result<Cache> {
    // Create a new cache instance
    let cache = Cache::new(HashMap::new());

    // Deserialize the JSON string into a HashMap
    let parsed_cache: HashMap<String, HashMap<String, String>> = serde_json::from_str(json_data)
        .map_err(|e| anyhow::anyhow!(format!("Deserialization error: {}", e)))?;

    // Lock the cache for writing
    {
        let mut cache_write = cache.write().unwrap();

        // Populate the cache with the deserialized values
        for (node_id, category_map) in parsed_cache {
            let serialized_category: HashMap<String, SerializableData> = category_map
                .into_iter()
                .map(|(output_name, value)| (output_name, SerializableData { value }))
                .collect();
            cache_write.insert(node_id, serialized_category);
        }
    } // The write lock is released here

    Ok(cache) // Return the cache without any borrow issues
}
// pub fn insert_value<T: IntoAny + 'static>(cache: &Cache, category: String, key: String, value: T) {
//     let mut cache_write = cache.write().unwrap();
//     let category_map = cache_write.entry(category).or_insert_with(HashMap::new);
//     category_map.insert(key, Box::new(value));
// }

pub fn insert_value<T: Serialize>(
    cache: &Cache,
    node_id: &str,
    output_name: &str,
    value: T,
) -> Result<()> {
    let mut cache_write = cache.write().unwrap();

    // Serialize the value to a JSON string
    let serialized_value = serde_json::to_string(&value)
        .map_err(|e| anyhow::anyhow!(format!("Serialization error: {}", e)))?;

    // Store the serialized value in the cache
    cache_write
        .entry(node_id.to_string())
        .or_insert_with(HashMap::new)
        .insert(
            output_name.to_string(),
            SerializableData {
                value: serialized_value,
            },
        );

    Ok(())
}

pub fn generate_node_id( action_name: &str) -> String {
    let timestamp = chrono::Utc::now().timestamp_millis() % 1_000_000; // Last 6 digits
    format!("{}_{}", action_name, timestamp)
}

pub fn get_input<T: for<'de> Deserialize<'de>>(cache: &Cache, node_id: &str, key: &str) -> Result<T> {
    let cache_read = cache.read().unwrap();
    let node_map = cache_read.get(node_id).ok_or_else(|| anyhow!("Node '{}' not found", node_id))?;
    let serialized_value = node_map.get(key).ok_or_else(|| anyhow!("Input '{}' not found", key))?;
    serde_json::from_str(&serialized_value.value).map_err(|e| anyhow!("Deserialization error: {}", e))
}
pub fn parse_input_from_name<T: for<'de> Deserialize<'de>>(
    cache: &Cache,
    input_name: String,
    inputs: &[IField],
) -> Result<T> {
    let input = inputs
        .iter()
        .find(|input| input.name == input_name)
        .ok_or_else(|| anyhow::anyhow!("Input not found: {}", input_name))?;

    let parts: Vec<&str> = input.reference.split('.').collect();
    if parts.len() != 2 {
        return Err(anyhow::anyhow!(
            "Invalid reference format: {}",
            input.reference
        ));
    }

    let node_id = parts[0];
    let output_name = parts[1];

    let cache_read = cache.read().unwrap();
    let category_map = cache_read
        .get(node_id)
        .ok_or_else(|| anyhow::anyhow!("Node not found: {}", node_id))?;

    let serialized_value = category_map
        .get(output_name)
        .ok_or_else(|| anyhow::anyhow!("Output not found: {}", output_name))?;

    serde_json::from_str(&serialized_value.value)
        .map_err(|e| anyhow::anyhow!("Deserialization error: {}", e))
}

pub fn get_global_input<T: for<'de> Deserialize<'de>>(
    cache: &Cache,
    dag_name: &str,
    key: &str,
) -> Result<T> {
    let cache_read = cache.read().unwrap();
    let dag_map = cache_read.get(dag_name).ok_or_else(|| anyhow!("DAG '{}' not found", dag_name))?;
    let serialized_value = dag_map.get(key).ok_or_else(|| anyhow!("Key '{}' not found", key))?;
    serde_json::from_str(&serialized_value.value).map_err(|e| anyhow!("Deserialization error: {}", e))
}

pub fn insert_global_value<T: Serialize>(
    cache: &Cache,
    dag_name: &str,
    key: &str,
    value: T,
) -> Result<()> {
    let mut cache_write = cache.write().unwrap();
    let dag_map = cache_write.entry(dag_name.to_string()).or_insert_with(HashMap::new);
    dag_map.insert(key.to_string(), SerializableData { value: serde_json::to_string(&value)? });
    Ok(())
}

pub fn append_global_value<T: Serialize + for<'de> Deserialize<'de>>(
    cache: &Cache,
    dag_name: &str,
    key: &str,
    value: T,
) -> Result<()> {
    let mut cache_write = cache.write().unwrap();
    let dag_map = cache_write.entry(dag_name.to_string()).or_insert_with(HashMap::new);
    let existing: Vec<T> = dag_map.get(key).map(|v| serde_json::from_str(&v.value).unwrap_or(vec![])).unwrap_or(vec![]);
    let mut updated = existing;
    updated.push(value);
    dag_map.insert(key.to_string(), SerializableData { value: serde_json::to_string(&updated)? });
    Ok(())
}
/// A trait for custom actions associated with nodes.
#[async_trait]
pub trait NodeAction: Send + Sync {
    /// Returns the name of the action.
    fn name(&self) -> String;

    /// Executes the action with the given node and inputs, and returns the outputs.
    async fn execute(&self, executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()>;
}

// Add ExecutionTree type definition before DagExecutor
pub type ExecutionTree = DiGraph<NodeSnapshot, ExecutionEdge>;

#[derive(Debug, Clone, Serialize)]
pub struct NodeSnapshot {
    pub node_id: String,
    pub outcome: NodeExecutionOutcome,
    pub cache_ref: String,  // Changed from cache_snapshot to cache_ref
    pub timestamp: NaiveDateTime,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionEdge {
    pub parent: String,
    pub label: String,
}

/// The main executor for DAGs.
pub struct DagExecutor {
    /// A registry of custom actions.
    function_registry: HashMap<String, Arc<dyn NodeAction>>,
    /// The graphs to be executed.
   pub graphs: Arc<RwLock<HashMap<String, Graph>>>,
    /// The prebuilt DAGs.
    prebuilt_dags: Arc<RwLock<HashMap<String, (DiGraph<Node, ()>, HashMap<String, NodeIndex>)>>>,
    config: DagConfig,
    sled_db: sled::Db,
    stopped: Arc<RwLock<bool>>,
    paused: Arc<RwLock<bool>>,
    start_time: NaiveDateTime,
    tree: Arc<RwLock<HashMap<String, ExecutionTree>>>,
    /// Track bootstrapped agent DAGs
    bootstrapped_agents: Arc<RwLock<HashSet<String>>>,
}

impl DagExecutor {
    /// Creates a new `DagExecutor` with optional configuration
    pub fn new(config: Option<DagConfig>) -> Result<Self, Error> {
        let sled_db = sled::open("dagger_db")?;

        Ok(DagExecutor {
            function_registry: HashMap::new(),
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
    pub fn register_action(&mut self, action: Arc<dyn NodeAction>) -> Result<(), Error> {
        info!("Registered action: {:#?}", action.name());
        let action_name = action.name().clone(); // Get the name from the action itself
        self.function_registry
            // .lock()
            // .map_err(|e| anyhow!("Failed to acquire lock: {}", e))?
            .insert(action_name, action);
        Ok(())
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
        let mut graphs = self.graphs.write()
            .map_err(|e| anyhow!("Failed to acquire graphs write lock: {}", e))?;
        let mut dags = self.prebuilt_dags.write()
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
    ) -> Result<DagExecutionReport, Error> {
        match spec {
            WorkflowSpec::Static { name } => {
                // Get read lock to access prebuilt DAG
                let prebuilt_dag = {
                    let dags = self.prebuilt_dags.read().map_err(|e| 
                        anyhow!("Failed to acquire read lock: {}", e))?;
                    dags.get(&name)
                        .ok_or_else(|| anyhow!("Graph '{}' not found", name))?
                        .clone()
                };

                let (dag, _) = prebuilt_dag;
                if dag.node_count() == 0 {
                    return Err(anyhow!("Graph '{}' contains no nodes", name));
                }

                self.start_time = chrono::Local::now().naive_local();
                
                tokio::select! {
                    result = execute_dag_async(self, &dag, cache) => {
                        let (report, needs_human_check) = result;
                        if needs_human_check {
                            self.add_node(&name, format!("human_check_{}", cuid2::create_id()),
                                         "human_interrupt".to_string(), vec![])?;
                        }
                        Ok(report)
                    },
                    _ = cancel_rx => Err(anyhow!("DAG execution was cancelled")),
                }
            }
            WorkflowSpec::Agent { task } => {
                // Verify supervisor action is registered
                if !self.function_registry.contains_key("supervisor_step") {
                    return Err(anyhow!("Supervisor action not registered"));
                } else {
                    info!("Supervisor action registered");
                }

                // Attempt to bootstrap agent DAG if not already done
                let needs_bootstrap = {
                    let mut bootstrapped = self.bootstrapped_agents.write()
                        .map_err(|e| anyhow!("Failed to acquire bootstrap lock: {}", e))?;
                    if !bootstrapped.contains(&task) {
                        bootstrapped.insert(task.clone());
                        true
                    } else {
                        false
                    }
                };

                if needs_bootstrap {
                    // Bootstrap agent DAG with write lock
                    let mut dags = self.prebuilt_dags.write()
                        .map_err(|e| anyhow!("Failed to acquire write lock: {}", e))?;
                    
                    if !dags.contains_key(&task) {
                        // Create bootstrap graph
                        let graph = Graph {
                            name: task.clone(),
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
                            description: format!("Agent-driven DAG for task: {}", task),
                            instructions: None,
                            tags: vec!["agent".to_string()],
                            author: "system".to_string(),
                            version: "1.0".to_string(),
                            signature: "auto-generated".to_string(),
                            config: Some(self.config.clone()),
                        };

                        let (mut dag, indices) = self.build_dag_internal(&graph)?;
                        self.graphs.write().unwrap().insert(task.clone(), graph);
                        dags.insert(task.clone(), (dag, indices));
                    }
                }

                // Get DAG with read lock
                let (dag, _) = {
                    let dags = self.prebuilt_dags.read()
                        .map_err(|e| anyhow!("Failed to acquire read lock: {}", e))?;
                    dags.get(&task)
                        .ok_or_else(|| anyhow!("Agent DAG '{}' not found", task))?
                        .clone()
                };

                self.start_time = chrono::Local::now().naive_local();

                // Record active DAG in Sled
                let active_tree = self.sled_db.open_tree("active")?;
                active_tree.insert(
                    task.as_bytes(),
                    serde_json::to_vec(&DagMetadata {
                        status: "Running".to_string(),
                        task: task.clone(),
                    })?,
                )?;

                tokio::select! {
                    result = execute_dag_async(self, &dag, cache) => {
                        let (report, needs_human_check) = result;
                        if needs_human_check {
                            self.add_node(&task, format!("human_check_{}", cuid2::create_id()),
                                         "human_interrupt".to_string(), vec![])?;
                        }
                        Ok(report)
                    },
                    _ = cancel_rx => Err(anyhow!("DAG execution was cancelled")),
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
        let graphs = self.graphs.read()
            .map_err(|e| anyhow!("Failed to acquire graphs read lock: {}", e))?;
            
        Ok(graphs
            .iter()
            .map(|(name, graph)| (name.clone(), graph.description.clone()))
            .collect())
    }

    pub fn list_dag_filtered_tag(&self, filter: &str) -> Result<Vec<(String, String)>> {
        let graphs = self.graphs.read()
            .map_err(|e| anyhow!("Failed to acquire graphs read lock: {}", e))?;
            
        Ok(graphs
            .iter()
            .filter(|(_, graph)| graph.tags.iter().any(|tag| tag.contains(filter)))
            .map(|(name, graph)| (name.clone(), graph.description.clone()))
            .collect())
    }

    pub fn list_dag_multiple_tags(&self, tags: Vec<String>) -> Result<Vec<(String, String)>> {
        let graphs = self.graphs.read()
            .map_err(|e| anyhow!("Failed to acquire graphs read lock: {}", e))?;
            
        Ok(graphs
            .iter()
            .filter(|(_, graph)| tags.iter().all(|tag| graph.tags.contains(tag)))
            .map(|(name, graph)| (name.clone(), graph.description.clone()))
            .collect())
    }

    pub fn list_dags_metadata(&self) -> Result<Vec<(String, String, String, String, String)>> {
        let graphs = self.graphs.read()
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
    pub fn save_cache(&self, dag_id: &str, cache: &Cache) -> Result<()> {
        let start = std::time::Instant::now();
        let cache_read = cache.read().unwrap();
        
        // Get last saved state from Sled
        let cache_tree = self.sled_db.open_tree("cache")?;
        let previous_state: HashMap<String, HashMap<String, SerializableData>> = if let Some(compressed) = cache_tree.get(dag_id.as_bytes())? {
            match zstd::decode_all(&compressed[..])
                .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.into()))
            {
                Ok(state) => state,
                Err(e) => {
                    warn!("Failed to load previous cache state: {}", e);
                    HashMap::new()
                }
            }
        } else {
            HashMap::new()
        };

        // Calculate delta by comparing with previous state
        let mut delta = HashMap::new();
        for (node_id, current_values) in cache_read.iter() {
            match previous_state.get(node_id) {
                Some(prev_values) => {
                    let mut node_delta = HashMap::new();
                    for (key, value) in current_values {
                        if !prev_values.contains_key(key) || prev_values[key] != *value {
                            node_delta.insert(key.clone(), value.clone());
                        }
                    }
                    if !node_delta.is_empty() {
                        delta.insert(node_id.clone(), node_delta);
                    }
                }
                None => {
                    delta.insert(node_id.clone(), current_values.clone());
                }
            }
        }

        // Only save if there are changes
        if !delta.is_empty() {
            let serialized = serde_json::to_vec(&delta)?;
            let compressed = zstd::encode_all(&*serialized, 3)?;
            
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
                            let delay = std::time::Duration::from_millis(100 * 2u64.pow(attempt as u32));
                            warn!("Cache save attempt {} failed, retrying in {:?}", attempt + 1, delay);
                            std::thread::sleep(delay);
                        }
                    }
                }
            }
            Err(anyhow!("Failed to save cache after retries: {}", last_error.unwrap()))
        } else {
            trace!("No changes detected in cache for DAG {}", dag_id);
            Ok(())
        }
    }

    /// Saves a specific delta update to the cache
    pub fn save_cache_delta(&self, dag_id: &str, delta: HashMap<String, HashMap<String, SerializableData>>) -> Result<()> {
        let start = std::time::Instant::now();
        let cache_tree = self.sled_db.open_tree("cache")?;
        
        // Load and merge with existing cache
        let mut current = match cache_tree.get(dag_id.as_bytes())? {
            Some(compressed) => {
                zstd::decode_all(&compressed[..])
                    .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.into()))
                    .unwrap_or_else(|e| {
                        warn!("Failed to load existing cache, starting fresh: {}", e);
                        HashMap::new()
                    })
            }
            None => HashMap::new(),
        };

        // Apply delta updates
        for (node_id, updates) in delta {
            current.entry(node_id)
                .or_insert_with(HashMap::new)
                .extend(updates);
        }

        // Serialize and compress
        let serialized = serde_json::to_vec(&current)?;
        let compressed = zstd::encode_all(&*serialized, 3)?;
        
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
                        let delay = std::time::Duration::from_millis(100 * 2u64.pow(attempt as u32));
                        warn!("Delta save attempt {} failed, retrying in {:?}", attempt + 1, delay);
                        std::thread::sleep(delay);
                    }
                }
            }
        }
        
        Err(anyhow!("Failed to save delta after retries: {}", last_error.unwrap()))
    }

    /// Adds a new node to an existing DAG with validation and transactional safety
    pub fn add_node(
        &mut self,
        name: &str,
        node_id: String,
        action_name: String,  // Changed from Arc<dyn NodeAction> to String
        dependencies: Vec<String>,
    ) -> Result<()> {
        // Verify the action is registered
        if !self.function_registry.contains_key(&action_name) {
            return Err(anyhow!("Action '{}' not registered", action_name));
        }

        // Acquire write locks for both graphs and prebuilt_dags
        let mut graphs = self.graphs.write()
            .map_err(|e| anyhow!("Failed to acquire graphs write lock: {}", e))?;
        let mut dags = self.prebuilt_dags.write()
            .map_err(|e| anyhow!("Failed to acquire DAGs write lock: {}", e))?;

        // Validate node ID uniqueness and dependencies within the lock
        let (dag, indices) = dags.get_mut(name)
            .ok_or_else(|| anyhow!("DAG not found: {}", name))?;

        if indices.contains_key(&node_id) {
            return Err(anyhow!("Node ID already exists: {}", node_id));
        }

        // Validate all dependencies exist
        for dep_id in &dependencies {
            if !indices.contains_key(dep_id) {
                return Err(anyhow!("Dependency not found: {}", dep_id));
            }
        }

        // Create new node
        let node = Node {
            id: node_id.clone(),
            dependencies: dependencies.clone(),
            action: action_name,  // Use the action name directly
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
            return Err(anyhow!("Invalid DAG after adding node: {}", e));
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
    pub fn update_cache(&self, dag_id: &str, key: String, value: SerializableData) -> Result<()> {
        let cache = self.load_cache(dag_id)?;
        let mut cache_write = cache.write().unwrap();

        let instructions = cache_write
            .entry("global".to_string())
            .or_insert_with(HashMap::new);

        instructions.insert("pending_instructions".to_string(), value);
        drop(cache_write);
        
        // Signal any waiting HumanInterrupt actions
        if let Some(graph) = self.graphs.read().unwrap().get(dag_id) {
            for node in &graph.nodes {
                if node.action == "human_interrupt" {
                    if let Some(action) = self.function_registry.get(&node.action) {
                        if let Some(human_interrupt) = action.as_any().downcast_ref::<HumanInterrupt>() {
                            if let Some(tx) = human_interrupt.input_tx.read().unwrap().as_ref() {
                                let _ = tx.try_send(());
                            }
                        }
                    }
                }
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
    ) -> Result<DagExecutionReport> {
        // Verify DAG is in pending state
        let pending_tree = self.sled_db.open_tree("pending")?;
        if pending_tree.get(dag_id.as_bytes())?.is_none() {
            return Err(anyhow!("DAG {} is not in paused state", dag_id));
        }

        // Load saved cache state
        let cache = self.load_cache(dag_id)?;

        // Update cache with new input if provided
        if let Some(input_value) = input {
            self.update_cache(
                dag_id,
                "pending_instructions".to_string(),
                SerializableData { value: input_value },
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
        self.execute_dag(
            WorkflowSpec::Static {
                name: dag_id.to_string(),
            },
            &cache,
            rx,
        )
        .await
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

    /// Serializes an execution tree to DOT format for visualization
    pub fn serialize_tree_to_dot(&self, dag_id: &str) -> Result<String> {
        let trees = self.tree.read().unwrap();
        let tree = trees
            .get(dag_id)
            .ok_or_else(|| anyhow!("No tree for {}", dag_id))?;
        let mut dot = String::from("digraph ExecutionTree {\n");
        for node_idx in tree.node_indices() {
            let node = &tree[node_idx];
            dot.push_str(&format!(
                "  \"{}\" [label=\"{}: {}\"];\n",
                node.node_id,
                node.node_id,
                if node.outcome.success {
                    "Success"
                } else {
                    "Failed"
                }
            ));
        }
        for edge in tree.edge_indices() {
            let (from, to) = tree.edge_endpoints(edge).unwrap();
            let edge_data = &tree[edge];
            dot.push_str(&format!(
                "  \"{}\" -> \"{}\" [label=\"{}\"];\n",
                tree[from].node_id, tree[to].node_id, edge_data.label
            ));
        }
        dot.push_str("}\n");
        Ok(dot)
    }

    /// Loads cache from Sled for a given DAG
    fn load_cache(&self, dag_id: &str) -> Result<Cache> {
        let cache_tree = self.sled_db.open_tree("cache")?;
        if let Some(compressed) = cache_tree.get(dag_id.as_bytes())? {
            let bytes = zstd::decode_all(&compressed[..])?;
            load_cache_from_json(&String::from_utf8(bytes)?)
        } else {
            Ok(Cache::new(HashMap::new()))
        }
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
                    if let Some(action) = self.function_registry.get(&node.action) {
                        if let Some(human_interrupt) = action.as_any().downcast_ref::<HumanInterrupt>() {
                            human_interrupt.cancel();
                        }
                    }
                }
            }
        }
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
        if let Some(parent_idx) = tree.node_indices()
            .find(|i| tree[*i].node_id == parent) 
        {
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
    // Find the DAG name this node belongs to
    if let Some((dag_name, _)) = executor
        .graphs
        .read()
        .map_err(|e| anyhow!("Failed to acquire graphs read lock: {}", e))
        .unwrap()
        .iter()
        .find(|(_, g)| g.nodes.iter().any(|n| n.id == node.id))
    {
        // Create cache delta and store in Sled
        let cache_ref = format!("snapshot_{}_{}", node.id, chrono::Utc::now().timestamp());
        
        // Store only the relevant cache entries for this node
        let delta_cache = {
            let cache_read = cache.read().unwrap();
            let mut delta = HashMap::new();
            
            // Include node's own cache entries
            if let Some(node_cache) = cache_read.get(&node.id) {
                delta.insert(node.id.clone(), node_cache.clone());
            }
            
            // Include referenced inputs' cache entries
            for input in &node.inputs {
                let parts: Vec<&str> = input.reference.split('.').collect();
                if parts.len() == 2 {
                    let ref_node_id = parts[0];
                    if let Some(ref_cache) = cache_read.get(ref_node_id) {
                        delta.insert(ref_node_id.to_string(), ref_cache.clone());
                    }
                }
            }
            
            delta
        };

        // Store delta in Sled
        if let Ok(serialized) = serde_json::to_vec(&delta_cache) {
            if let Ok(compressed) = zstd::encode_all(&*serialized, 3) {
                if let Err(e) = executor.sled_db
                    .open_tree("snapshots")
                    .and_then(|tree| tree.insert(cache_ref.as_bytes(), compressed))
                {
                    error!("Failed to store cache snapshot: {}", e);
                }
            }
        }

        // Create snapshot with cache reference
        let snapshot = NodeSnapshot {
            node_id: node.id.clone(),
            outcome: outcome.clone(),
            cache_ref,
            timestamp: chrono::Local::now().naive_local(),
        };

        // Update execution tree
        if let Ok(mut trees) = executor.tree.write() {
            let tree = trees
                .entry(dag_name.to_string())
                .or_insert_with(DiGraph::new);
            let parent_id = node.dependencies.first().cloned();
            update_execution_tree(tree, snapshot, parent_id, None);
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

        match dag.node_references().next().and_then(|(_, node)| {
            graphs.iter()
                .find(|(_, g)| g.nodes.contains(node))
        }) {
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
            let dags = executor.prebuilt_dags.read()
                .map_err(|e| error!("Failed to acquire DAGs read lock: {}", e))
                .ok();
            
            match dags.and_then(|dags| dags.get(&dag_name).map(|(dag, _)| dag.clone())) {
                Some(dag) => dag,
                None => break, // Exit if we can't get the DAG
            }
        };

        // Get supervisor iteration count for more accurate tracking in dynamic DAGs
        let supervisor_iteration: usize = if let Some(supervisor_node) = current_dag.node_references()
            .find(|(_, node)| node.action == "supervisor_step") {
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

    let action = match executor.function_registry.get(&node.action).cloned() {
        Some(a) => a,
        None => {
            let error = format!("Action '{}' not registered for node '{}'", node.action, node.id);
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
            .saturating_sub(node_start.signed_duration_since(executor.start_time).to_std().unwrap_or_default());
        
        let effective_timeout = node_timeout.min(global_remaining);
        debug!(
            "Node '{}' timeout: {:?} (node) vs {:?} (global remaining)",
            node.id, node_timeout, global_remaining
        );

        // Record attempt in execution tree
        let attempt_outcome = NodeExecutionOutcome {
            node_id: node.id.clone(),
            success: false,
            retry_messages: outcome.retry_messages.clone(),
            final_error: None,
        };
        record_execution_snapshot(executor, node, &attempt_outcome, cache);

        // Execute with timeout and handle errors
        let execution_result = timeout(effective_timeout, action.execute(executor, node, cache)).await;
        
        match execution_result {
            Ok(Ok(_)) => {
                info!("Node '{}' execution succeeded on attempt {}", node.id, attempt_number);
                outcome.success = true;
                record_execution_snapshot(executor, node, &outcome, cache);
                return outcome;
            }
            Ok(Err(e)) => {
                // Handle regular execution error
                let err_message = e.to_string();
                outcome.retry_messages.push(format!(
                    "Attempt {} failed: {}",
                    attempt_number,
                    err_message
                ));
                
                error!(
                    "Node '{}' execution failed (attempt {}/{}): {}",
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
                ).await {
                    return outcome;
                }
            }
            Err(elapsed) => {
                // Handle timeout error
                let err_message = format!("Timeout after {:?}", effective_timeout);
                outcome.retry_messages.push(format!(
                    "Attempt {} failed: {}",
                    attempt_number,
                    err_message
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
                ).await {
                    return outcome;
                }
            }
        }
    }

    outcome.final_error = Some(format!(
        "Node '{}' failed after {} attempts",
        node.id,
        node.try_count
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
            RetryStrategy::Exponential { initial_delay_secs, max_delay_secs, multiplier } => {
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
            attempt_number,
            err_message
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
        if !registry.contains_key(&node.action) {
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


#[async_trait]
impl NodeAction for HumanInterrupt {
    fn name(&self) -> String {
        "human_interrupt".to_string()
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

        if let Some(wait_minutes) = executor.config.human_wait_minutes {
            info!(
                "Node {} waiting for input for {} minutes",
                node.id, wait_minutes
            );

            let wait_duration = Duration::from_secs(wait_minutes as u64 * 60);
            let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(1);
            let (cancel_tx, mut cancel_rx) = tokio::sync::mpsc::channel(1);
            
            // Store senders with proper cleanup
            {
                let mut tx_write = self.input_tx.write().unwrap();
                let mut cancel_write = self.cancel_tx.write().unwrap();
                *tx_write = Some(input_tx);
                *cancel_write = Some(cancel_tx);
            }

            // Ensure cleanup of channels on function exit
            struct ChannelCleanup<'a>(&'a HumanInterrupt);
            impl<'a> Drop for ChannelCleanup<'a> {
                fn drop(&mut self) {
                    if let Ok(mut tx_write) = self.0.input_tx.write() {
                        *tx_write = None;
                    }
                    if let Ok(mut cancel_write) = self.0.cancel_tx.write() {
                        *cancel_write = None;
                    }
                }
            }
            let _cleanup = ChannelCleanup(self);

            let result = tokio::select! {
                _ = sleep(wait_duration) => {
                    let cache_read = cache.read().unwrap();
                    let has_input = cache_read
                        .get("global")
                        .and_then(|m| m.get("pending_instructions"))
                        .is_some();

                    if !has_input {
                        match executor.config.human_timeout_action {
                            HumanTimeoutAction::Autopilot => {
                                info!("No human input received for {}, proceeding in autopilot", node.id);
                                Ok(())
                            }
                            HumanTimeoutAction::Pause => {
                                info!("No human input received for {}, pausing DAG {}", node.id, dag_name);
                                *executor.paused.write().unwrap() = true;
                                executor.save_cache(&dag_name, cache)?;
                                
                                let pending_tree = executor.sled_db.open_tree("pending")?;
                                let active_tree = executor.sled_db.open_tree("active")?;
                                
                                if let Some(metadata) = active_tree.remove(dag_name.as_bytes())? {
                                    pending_tree.insert(dag_name.as_bytes(), metadata)?;
                                }
                                
                                Err(anyhow!("Paused for human input"))
                            }
                        }
                    } else {
                        info!("Human input received for {}, continuing", node.id);
                        Ok(())
                    }
                }
                _ = input_rx.recv() => {
                    info!("Received immediate input notification for {}", node.id);
                    Ok(())
                }
                _ = cancel_rx.recv() => {
                    info!("Human interrupt cancelled for {}", node.id);
                    Err(anyhow!("Human interrupt cancelled"))
                }
                _ = executor.check_stopped() => {
                    info!("DAG stopped, cancelling human interrupt for {}", node.id);
                    Err(anyhow!("DAG execution stopped"))
                }
            };

            result
        } else {
            Ok(())
        }
    }
}

/// Supervisor action that manages dynamic DAG updates
/// Supervisor action that manages dynamic DAG updates
pub struct SupervisorStep;

impl Default for SupervisorStep {
    fn default() -> Self {
        Self
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

impl SupervisorStep {
    /// Validates and parses instruction JSON
    fn validate_instruction(instruction_str: &str) -> Result<SupervisorInstruction> {
        let instruction_value: Value = serde_json::from_str(instruction_str)
            .map_err(|e| anyhow!("Invalid instruction format: {}", e))?;

        // Validate required fields
        let action = instruction_value.get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing or invalid 'action' field"))?
            .to_string();

        // Validate action is supported
        if !["retrieve_info", "human_review"].contains(&action.as_str()) {
            return Err(anyhow!("Unsupported action: {}", action));
        }

        Ok(SupervisorInstruction {
            action,
            params: instruction_value.get("params").cloned(),
            priority: instruction_value.get("priority")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            timestamp: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// Processes the instruction queue from cache
    async fn process_instruction_queue(
        &self,
        executor: &mut DagExecutor,
        node: &Node,
        cache: &Cache,
        dag_name: &str,
    ) -> Result<()> {
        // Get and sort instructions by priority
        let instructions = {
            let cache_read = cache.read().unwrap();
            if let Some(global) = cache_read.get("global") {
                global.iter()
                    .filter(|(k, _)| k.starts_with("instruction_"))
                    .map(|(_, v)| v.value.clone())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        };

        let mut valid_instructions = Vec::new();
        for instruction_str in instructions {
            match Self::validate_instruction(&instruction_str) {
                Ok(instruction) => valid_instructions.push(instruction),
                Err(e) => {
                    warn!("Skipping invalid instruction: {}", e);
                    // Log invalid instruction for debugging
                    insert_value(
                        cache,
                        "global",
                        &format!("invalid_instruction_{}", chrono::Utc::now().timestamp()),
                        json!({
                            "instruction": instruction_str,
                            "error": e.to_string(),
                            "timestamp": chrono::Utc::now().to_rfc3339(),
                        }),
                    )?;
                }
            }
        }

        // Sort by priority (higher first) then timestamp
        valid_instructions.sort_by(|a, b| {
            b.priority.unwrap_or(0).cmp(&a.priority.unwrap_or(0))
                .then_with(|| a.timestamp.cmp(&b.timestamp))
        });

        // Process sorted instructions
        for instruction in valid_instructions {
            match instruction.action.as_str() {
                "retrieve_info" => {
                    let node_id = format!("info_retrieval_{}", cuid2::create_id());
                    executor.add_node(
                        dag_name,
                        node_id.clone(),
                        "info_retrieval".to_string(),  // Just pass the action name
                        vec![node.id.clone()],
                    )?;

                    // Store params if provided
                    if let Some(params) = instruction.params {
                        insert_value(cache, &node_id, "action_params", params)?;
                    }
                }
                "human_review" => {
                    let node_id = format!("human_check_{}", cuid2::create_id());
                    executor.add_node(
                        dag_name,
                        node_id.clone(),
                        "human_interrupt".to_string(),  // Just pass the action name
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

        // Process instruction queue
        self.process_instruction_queue(executor, node, cache, &dag_name).await?;

        // Get current iteration count
        let current_count: usize = parse_input_from_name(cache, "iteration".to_string(), &node.inputs)
            .unwrap_or(0);
        let next_count = current_count + 1;

        // Get review frequency from config
        let review_frequency = executor
            .graphs
            .read()
            .map_err(|e| anyhow!("Failed to acquire graphs read lock: {}", e))?
            .get(&dag_name)
            .and_then(|g| g.config.as_ref())
            .and_then(|c| c.review_frequency)
            .unwrap_or(5);

        // Add periodic human review based on configured frequency
        if review_frequency > 0 && next_count % review_frequency as usize == 0 {
            let node_id = format!("human_review_{}", cuid2::create_id());
            executor.add_node(
                &dag_name,
                node_id.clone(),
                "human_interrupt".to_string(),  // Just pass the action name
                vec![node.id.clone()],
            )?;
        }

        // Record state
        insert_value(cache, &node.id, "iteration", next_count)?;
        insert_value(
            cache,
            &node.id,
            "timestamp",
            chrono::Utc::now().to_rfc3339(),
        )?;

        Ok(())
    }
}

/// Agent for retrieving information based on instructions
pub struct InfoRetrievalAgent;

impl InfoRetrievalAgent {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl NodeAction for InfoRetrievalAgent {
    fn name(&self) -> String {
        "info_retrieval".to_string()
    }

    async fn execute(&self, executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
        info!("Starting info retrieval for node {}", node.id);

        // Get parameters from cache
        let params: Value =
            parse_input_from_name(cache, "action_params".to_string(), &node.inputs)?;

        // Simulate info retrieval (replace with actual implementation)
        let retrieved_data = match params.get("query") {
            Some(query) => format!("Retrieved data for query: {}", query),
            None => "Retrieved default data".to_string(),
        };

        // Store results
        insert_value(cache, &node.id, "retrieved_data", retrieved_data)?;
        insert_value(
            cache,
            &node.id,
            "retrieval_timestamp",
            chrono::Utc::now().to_rfc3339(),
        )?;

        // // Optionally suggest next steps
        // let should_review = rand::random::<f32>() < 0.3; // 30% chance of suggesting review
        // if should_review {
        //     insert_value(cache, &node.id, "needs_human_review", true)?;
        //     info!("Suggesting human review of retrieved data");
        // }

        Ok(())
    }
}
