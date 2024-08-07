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
pub type Cache = RwLock<HashMap<String, HashMap<String, DynAny>>>;

pub fn insert_value<T: IntoAny + 'static>(cache: &Cache, category: String, key: String, value: T) {
    let mut cache_write = cache.write().unwrap();
    let category_map = cache_write.entry(category).or_insert_with(HashMap::new);
    category_map.insert(key, Box::new(value));
}

pub fn get_value<T: 'static>(cache: &Cache, category: &str, key: &str) -> Option<T> {
    let cache_read = cache.read().unwrap();
    if let Some(category_map) = cache_read.get(category) {
        if let Some(value) = category_map.get(key) {
            if let Ok(downcasted_value) = downcast::<T>(value.clone()) {
                return Some(downcasted_value);
            }
        }
    }
    None
}
pub fn parse_input<T: 'static>(cache: &Cache, input: IField) -> Result<T> {
    let parts: Vec<&str> = input.reference.split('.').collect();
    if parts.len() != 2 {
        error!("Invalid reference format: {}", input.reference);
        return Err(anyhow::anyhow!(format!(
            "Invalid reference format. needs to be node.reference on field: {}",
            input.reference
        )));
    }

    let node_id = parts[0];
    let output_name = parts[1];
    let cache_read = cache.read().unwrap();
    if let Some(category_map) = cache_read.get(node_id) {
        if let Some(value) = category_map.get(output_name) {
            if let Ok(downcasted_value) = downcast::<T>(value.clone()) {
                return Ok(downcasted_value);
            }
        }
    }
    Err(anyhow::anyhow!(format!(
        "Value not found {}",
        input.reference
    )))
}

// write another function that takes in cache and the node name and returns the value of the node
pub fn parse_input_from_name<T: 'static>(
    cache: &Cache,
    input_name: String,
    inputs: &Vec<IField>,
) -> Result<T> {
    // let cache_read = cache.read().unwrap();
    if let Some(input) = inputs.iter().find(|input| input.name == input_name) {
        let parts: Vec<&str> = input.reference.split('.').collect();
        if parts.len() != 2 {
            error!("Invalid reference format: {}", input.reference);
            return Err(anyhow::anyhow!(format!(
                "Invalid reference format. needs to be node.reference on field: {}",
                input.reference
            )));
        }

        let node_id = parts[0];
        let output_name = parts[1];
        let cache_read = cache.read().unwrap();
        if let Some(category_map) = cache_read.get(node_id) {
            if let Some(value) = category_map.get(output_name) {
                if let Ok(downcasted_value) = downcast::<T>(value.clone()) {
                    return Ok(downcasted_value);
                }
            }
        }
    }
    Err(anyhow::anyhow!(format!("Value not found {}", input_name)))
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

    /// Executes the DAG with the given inputs and returns the outputs.
    pub async fn execute_dag(
        &self,
        name: &str,
        cache: &Cache,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<(), Error> {
        let (dag, node_indices) = self
            .prebuilt_dags
            .get(name)
            .ok_or_else(|| anyhow!("Graph '{}' not found", name))?;

        tokio::select! {
            res = execute_dag_async(self, dag, cache) => {
                res?
            }
            _ = cancel_rx => {
                // Handle cancellation logic here
                return Err(anyhow!("DAG execution was cancelled."));
            }
        };

        // Handle the result here if needed
        Ok(())
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

/// Executes a single node asynchronously and returns its outputs.

async fn execute_node_async(executor: &DagExecutor, node: &Node, cache: &Cache) -> Result<()> {
    // println!("Executing node: {}", node.id);
    let action = {
        // ...
        executor
            .function_registry
            .get(&node.action)
            .cloned()
            .ok_or_else(|| anyhow!("Unknown action {} for node {}", node.action, node.id))?
    };

    info!("Executing action for node: {}", node.id);

    let timeout_duration = Duration::from_secs(node.timeout as u64);
    let mut retries_left = node.try_count;
    // let inputs_to_function = get_input_values(inputs, &node.inputs)?;
    while retries_left > 0 {
        info!(
            "Trying to execute node: {} ({} retries left)...",
            node.id, retries_left
        );
        match timeout(timeout_duration, action.execute(node, &cache)).await {
            Ok(Ok(result)) => {
                info!("Node '{}' execution succeeded", node.id);
                return Ok(());
            }
            Ok(Err(e)) => {
                error!("Node '{}' execution failed: {}", node.id, e);
                insert_value(
                    cache,
                    node.id.clone(),
                    format!("error_retry{}", retries_left),
                    e.to_string(),
                );
                // let current_error = get_value(cache, node.id, "error")
                if node.onfailure {
                    warn!(
                        "Retrying node '{}' ({} retries left)...",
                        node.id, retries_left
                    );
                    sleep(Duration::from_secs(1)).await; // Wait for 1 second before retrying
                    retries_left -= 1;
                } else {
                    return Err(anyhow!("Node '{}' execution failed: {}", node.id, e));
                }
            }
            Err(e) => {
                error!(
                    "Node '{}' execution timed out after {} seconds due to {}",
                    node.id,
                    node.timeout,
                    e.to_string()
                );
                insert_value(
                    cache,
                    node.id.clone(),
                    format!("error_retry_{}_timeout", retries_left),
                    e.to_string(),
                );
                if node.onfailure {
                    warn!(
                        "Retrying node '{}' ({} retries left)...",
                        node.id, retries_left
                    );
                    sleep(Duration::from_secs(1)).await; // Wait for 1 second before retrying
                    retries_left -= 1;
                    insert_value(
                        cache,
                        node.id.clone(),
                        format!("error_retry_{}_timeout", retries_left),
                        e.to_string(),
                    );
                } else {
                    insert_value(
                        cache,
                        node.id.clone(),
                        format!("error_retry_{}_timeout", retries_left),
                        e.to_string(),
                    );
                    return Err(anyhow!("Node '{}' execution timed out", node.id));
                }
            }
        }
    }
    insert_value(
        cache,
        node.id.clone(),
        format!("error_retry_{}_final", retries_left),
        "Done with all retries",
    );
    Err(anyhow!(
        "Node '{}' failed after {} retries",
        node.id,
        node.try_count
    ))
}
/// Executes the DAG asynchronously and updates the inputs with each node's outputs.
pub async fn execute_dag_async(
    executor: &DagExecutor,
    dag: &DiGraph<Node, ()>,
    cache: &Cache,
) -> Result<(), anyhow::Error> {
    let mut topo = Topo::new(dag);

    // let updated_inputs = Cache::new(inputs);
    while let Some(node_index) = topo.next(dag) {
        let node = &dag[node_index];

        match execute_node_async(executor, node, cache).await {
            Ok(()) => {}
            Err(err) => {
                if !node.onfailure {
                    return Err(anyhow!("Node execution failed: {}", err));
                } else {
                    info!("Node '{}' execution failed: {}", node.id, err);
                    insert_value(cache, node.id.clone(), "error".to_string(), err.to_string());
                }
            }
        }
    }

    Ok(())
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

pub fn get_input_values(
    cache: &Cache,
    node_inputs: &Vec<IField>,
) -> Result<HashMap<String, DynAny>, anyhow::Error> {
    let mut input_values = HashMap::new();
    println!("node_inputs: {:#?}", node_inputs);
    for input in node_inputs {
        let reference = input.reference.clone();
        let parts: Vec<&str> = reference.split('.').collect();
        if parts.len() != 2 {
            error!("Invalid reference format: {}", reference);
            return Err(anyhow::anyhow!("Invalid reference format"));
        }
        let node_id = parts[0];
        let output_name = parts[1];
        println!("node_id: {}, output_name: {}", node_id, output_name);
        let val: DynAny = get_value(cache, "inputs", "num1").unwrap();
        println!("val: {:#?}", val);

        get_value(cache, node_id, output_name)
            .map(|value| input_values.insert(input.name.clone(), value))
            .unwrap();
    }
    println!("input_values: {:#?}", input_values);
    Ok(input_values)
}
