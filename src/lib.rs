//! Dagger - A library for executing directed acyclic graphs (DAGs) with custom actions.
//!
//! This library provides a way to define and execute DAGs with custom actions. It supports
//! loading graph definitions from YAML files, validating the graph structure, and executing
//! custom actions associated with each node in the graph.

use anyhow::anyhow;
use anyhow::{Error, Result};

pub mod any;
pub use any::*;
use async_trait::async_trait;
use chrono::NaiveDateTime;
use core::any::type_name;
use petgraph::algo::is_cyclic_directed;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::Topo;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use std::collections::HashMap;

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

            async fn execute(&self, node: &Node, cache: &Cache) -> Result<()> {
                $action_func(node, cache).await
            }
        }

        $executor.register_action(Arc::new(Action));
    }};
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
#[derive(Debug, Clone, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
pub struct OField {
    /// The name of the field.
    pub name: String,
    /// The description of the field.
    pub description: Option<String>,
}

/// A node in the graph.
#[derive(Debug, Clone, Deserialize)]
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
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SerializableData {
    pub value: String, // Use String or a custom serializable type
}

// Update your cache to use the new SerializableData
pub type Cache = RwLock<HashMap<String, HashMap<String, SerializableData>>>;

pub fn serialize_cache_to_json(cache: &Cache) -> Result<String> {
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
    serde_json::to_string(&serialized_cache)
        .map_err(|e| anyhow::anyhow!(format!("Serialization error: {}", e)))
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

/// A trait for custom actions associated with nodes.
#[async_trait]
pub trait NodeAction: Send + Sync {
    /// Returns the name of the action.
    fn name(&self) -> String {
        type_name::<Self>().to_string()
    }

    /// Executes the action with the given node and inputs, and returns the outputs.
    async fn execute(&self, node: &Node, cache: &Cache) -> Result<()>;
}

/// The main executor for DAGs.
pub struct DagExecutor {
    /// A registry of custom actions.
    function_registry: HashMap<String, Arc<dyn NodeAction>>,
    /// The graphs to be executed.
    graphs: HashMap<String, Graph>,
    /// The prebuilt DAGs.
    prebuilt_dags: HashMap<String, (DiGraph<Node, ()>, HashMap<String, NodeIndex>)>,
}

impl DagExecutor {
    /// Creates a new `DagExecutor`.
    pub fn new() -> Self {
        DagExecutor {
            function_registry: HashMap::new(),
            graphs: HashMap::new(),
            prebuilt_dags: HashMap::new(),
        }
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

    /// Loads a graph definition from a YAML file.
    pub fn load_yaml_file(&mut self, file_path: &str) {
        match File::open(file_path) {
            Ok(mut file) => {
                let mut yaml_content = String::new();
                if let Err(e) = file.read_to_string(&mut yaml_content) {
                    error!("Failed to read file {}: {}", file_path, e);
                    return;
                }

                match serde_yaml::from_str::<Graph>(&yaml_content) {
                    Ok(graph) => match self.build_dag_internal(&graph) {
                        Ok((dag, node_indices)) => {
                            let name = graph.name.clone();
                            self.graphs.insert(name.clone(), graph);
                            self.prebuilt_dags.insert(name, (dag, node_indices));
                        }
                        Err(e) => {
                            error!("Failed to build DAG for file {}: {}", file_path, e);
                        }
                    },
                    Err(e) => {
                        error!("Failed to parse YAML file {}: {}", file_path, e);
                    }
                }
            }
            Err(e) => {
                error!("Failed to open file {}: {}", file_path, e);
            }
        }
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
    /// In case of cancellation, an error is returned.
    pub async fn execute_dag(
        &self,
        name: &str,
        cache: &Cache,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<DagExecutionReport, Error> {
        let (dag, _node_indices) = self
            .prebuilt_dags
            .get(name)
            .ok_or_else(|| anyhow!("Graph '{}' not found", name))?;

        let report = tokio::select! {
            report = execute_dag_async(self, dag, cache) => report,
            _ = cancel_rx => {
                return Err(anyhow!("DAG execution was cancelled."));
            }
        };

        Ok(report)
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

    pub fn list_dags(&self) -> Vec<(String, String)> {
        // return name and description of all graphs
        let response = self
            .graphs
            .iter()
            .map(|(name, graph)| (name.clone(), graph.description.clone()))
            .collect();
        response
    }

    pub fn list_dag_filtered_tag(&self, filter: &str) -> Vec<(String, String)> {
        // return name and description of all graphs that match a substring within any of the tags (tags is a vec<String>)
        let response = self
            .graphs
            .iter()
            .filter(|(name, graph)| graph.tags.iter().any(|tag| tag.contains(filter)))
            .map(|(name, graph)| (name.clone(), graph.description.clone()))
            .collect();
        response
    }

    pub fn list_dag_multiple_tags(&self, tags: Vec<String>) -> Vec<(String, String)> {
        // return name and description of all graphs that match all tags
        let response = self
            .graphs
            .iter()
            .filter(|(name, graph)| tags.iter().all(|tag| graph.tags.contains(tag)))
            .map(|(name, graph)| (name.clone(), graph.description.clone()))
            .collect();
        response
    }

    // list all metadata of avaialble dags name, description, signature, author, version
    pub fn list_dags_metadata(&self) -> Vec<(String, String, String, String, String)> {
        // return name and description of all graphs
        let response = self
            .graphs
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
            .collect();
        response
    }
}

/// Modified `execute_node_async` that captures the error message from the final retry attempt.
async fn execute_node_async(
    executor: &DagExecutor,
    node: &Node,
    cache: &Cache,
) -> NodeExecutionOutcome {
    let mut outcome = NodeExecutionOutcome {
        node_id: node.id.clone(),
        success: false,
        retry_messages: Vec::new(),
        final_error: None,
    };

    info!("Executing action for node: {}", node.id);
    let action = match executor.function_registry.get(&node.action).cloned() {
        Some(a) => a,
        None => {
            outcome.final_error =
                Some(format!("Unknown action {} for node {}", node.action, node.id));
            return outcome;
        }
    };

    let timeout_duration = Duration::from_secs(node.timeout as u64);
    let mut retries_left = node.try_count;

    while retries_left > 0 {
        info!(
            "Trying to execute node: {} ({} retries left)...",
            node.id, retries_left
        );
        match timeout(timeout_duration, action.execute(node, cache)).await {
            Ok(Ok(_)) => {
                info!("Node '{}' execution succeeded", node.id);
                outcome.success = true;
                return outcome;
            }
            Ok(Err(e)) => {
                let err_message = e.to_string();
                outcome.retry_messages.push(err_message.clone());
                error!("Node '{}' execution failed: {}", node.id, e);

                if node.onfailure {
                    warn!("Retrying node '{}'...", node.id);
                    sleep(Duration::from_secs(1)).await;
                    retries_left -= 1;
                } else {
                    // When not retrying further, immediately record the error.
                    outcome.final_error = Some(err_message);
                    return outcome;
                }
            }
            Err(e) => {
                let err_message = e.to_string();
                outcome.retry_messages.push(err_message.clone());
                error!("Node '{}' execution timed out: {}", node.id, e);

                if node.onfailure {
                    warn!("Retrying node '{}' after timeout...", node.id);
                    sleep(Duration::from_secs(1)).await;
                    retries_left -= 1;
                } else {
                    outcome.final_error = Some(err_message);
                    return outcome;
                }
            }
        }
    }

    // After all retries are exhausted, use the final error message produced in the last attempt.
    if let Some(last_err) = outcome.retry_messages.last() {
        outcome.final_error = Some(format!(
            "Failed after {} retries: {}",
            node.try_count, last_err
        ));
    } else {
        outcome.final_error = Some(format!("Node '{}' execution failed", node.id));
    }

    outcome
}

/// Executes the DAG asynchronously and produces a `DagExecutionReport` summarizing the outcomes.
pub async fn execute_dag_async(
    executor: &DagExecutor,
    dag: &DiGraph<Node, ()>,
    cache: &Cache,
) -> DagExecutionReport {
    let mut node_outcomes = Vec::new();
    let mut overall_success = true;
    let mut topo = Topo::new(dag);

    while let Some(node_index) = topo.next(dag) {
        let node = &dag[node_index];

        let outcome = execute_node_async(executor, node, cache).await;

        // If a node did not succeed and its onfailure flag is false, we abort immediately.
        if !outcome.success && !node.onfailure {
            overall_success = false;
            node_outcomes.push(outcome);
            break;
        } else if !outcome.success {
            overall_success = false;
        }

        node_outcomes.push(outcome);
    }

    // Build a consolidated error message if there is any node failure.
    let error_messages: Vec<String> = node_outcomes
        .iter()
        .filter_map(|outcome| outcome.final_error.clone())
        .collect();

    let aggregated_error = if !error_messages.is_empty() {
        Some(error_messages.join("\n"))
    } else {
        None
    };

    DagExecutionReport {
        node_outcomes,
        overall_success,
        error: aggregated_error,
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

// pub fn get_input_values(
//     cache: &Cache,
//     node_inputs: &Vec<IField>,
// ) -> Result<HashMap<String, DynAny>, anyhow::Error> {
//     let mut input_values = HashMap::new();
//     println!("node_inputs: {:#?}", node_inputs);
//     for input in node_inputs {
//         let reference = input.reference.clone();
//         let parts: Vec<&str> = reference.split('.').collect();
//         if parts.len() != 2 {
//             error!("Invalid reference format: {}", reference);
//             return Err(anyhow::anyhow!("Invalid reference format"));
//         }
//         let node_id = parts[0];
//         let output_name = parts[1];
//         println!("node_id: {}, output_name: {}", node_id, output_name);
//         let val: DynAny = get_value(cache, "inputs", "num1").unwrap();
//         println!("val: {:#?}", val);

//         get_value(cache, node_id, output_name)
//             .map(|value| input_values.insert(input.name.clone(), value))
//             .unwrap();
//     }
//     println!("input_values: {:#?}", input_values);
//     Ok(input_values)
// }

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

