//! Dagger - A library for executing directed acyclic graphs (DAGs) with custom actions.
//!
//! This library provides a way to define and execute DAGs with custom actions. It supports
//! loading graph definitions from YAML files, validating the graph structure, and executing
//! custom actions associated with each node in the graph.

use anyhow::anyhow;
use anyhow::{Context, Error, Result};
use async_trait::async_trait;
use chrono::Utc;
use core::any::type_name;
use petgraph::algo::is_cyclic_directed;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::Topo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::sync::{Arc, Mutex};
use tokio::time::{error as TimeoutError, sleep, timeout, Duration};
use tracing::{debug, error, info, trace, warn, Level};

/// Macro for registering an action with the `DagExecutor`.
///
/// # Examples
///
/// ```
/// use dagger::register_action;
/// use dagger::DagExecutor;
/// use dagger::NodeAction;
///
/// struct MyAction;
///
/// #[async_trait::async_trait]
/// impl NodeAction for MyAction {
///     fn name(&self) -> String {
///         "my_action".to_string()
///     }
///
///     async fn execute(&self, node: &Node, inputs: &Cache) -> Result<Cache> {
///         // Implementation of the action
///     }
/// }
///
/// let executor = DagExecutor::new();
/// register_action!(executor, "My Action", MyAction);
/// ```
#[macro_export]
macro_rules! register_action {
    ($executor:expr, $action_name:expr, $action_func:path) => {{
        struct Action;

        #[async_trait::async_trait]
        impl NodeAction for Action {
            fn name(&self) -> String {
                $action_name.to_string()
            }

            async fn execute(&self, node: &Node, inputs: &Cache) -> Result<Cache> {
                $action_func(node, inputs).await
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
}

/// Represents a value that can be used as input or output in a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Value {
    /// A floating-point value.
    Float(f64),
    /// An integer value.
    Integer(i64),
    /// A string value.
    String(String),

    /// A vector of strings.
    VecString(Vec<String>),
    /// A vector of integers.
    VecInt(Vec<i64>),
    /// A vector of floating-point values.
    VecFloat(Vec<f64>),

    /// A boolean value.
    Bool(bool),

    /// An object with string keys and values.
    Object(HashMap<String, Value>),

    /// A null value.
    Null,

    /// A datetime value in UTC.
    DateTime(chrono::DateTime<chrono::Utc>),
    // Add more variants as needed
}

/// A trait for converting values between Rust types and `Value` enum.
pub trait Convertible {
    /// Converts a Rust type to a `Value` enum.
    fn to_value(&self) -> Value;

    /// Converts a `Value` enum to a Rust type.
    fn from_value(value: &Value) -> Option<Self>
    where
        Self: Sized;
}

impl Convertible for f64 {
    fn to_value(&self) -> Value {
        Value::Float(*self)
    }

    fn from_value(value: &Value) -> Option<Self> {
        if let Value::Float(f) = value {
            Some(*f)
        } else {
            None
        }
    }
}

impl Convertible for i64 {
    fn to_value(&self) -> Value {
        Value::Integer(*self)
    }

    fn from_value(value: &Value) -> Option<Self> {
        if let Value::Integer(i) = value {
            Some(*i)
        } else {
            None
        }
    }
}

impl Convertible for String {
    fn to_value(&self) -> Value {
        Value::String(self.clone())
    }

    fn from_value(value: &Value) -> Option<Self> {
        if let Value::String(s) = value {
            Some(s.clone())
        } else {
            None
        }
    }
}

impl Convertible for bool {
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }

    fn from_value(value: &Value) -> Option<Self> {
        if let Value::Bool(b) = value {
            Some(*b)
        } else {
            None
        }
    }
}

impl Convertible for HashMap<String, Value> {
    fn to_value(&self) -> Value {
        Value::Object(self.clone())
    }

    fn from_value(value: &Value) -> Option<Self> {
        if let Value::Object(obj) = value {
            Some(obj.clone())
        } else {
            None
        }
    }
}

impl Convertible for chrono::DateTime<Utc> {
    fn to_value(&self) -> Value {
        Value::DateTime(*self)
    }

    fn from_value(value: &Value) -> Option<Self> {
        if let Value::DateTime(dt) = value {
            Some(*dt)
        } else {
            None
        }
    }
}

/// An input or output field of a node.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct IOField {
    /// The name of the field.
    pub name: String,
    /// The description of the field.
    pub description: Option<String>,
    /// The data type of the field.
    #[serde(rename = "type")]
    pub data_type: String, // Changed to String for simplicity in this example
    /// The reference to another node's output.
    pub reference: Option<String>,
    pub default: Option<Value>,
}

/// The type of a variable.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub enum VariableType {
    /// A floating-point variable.
    Float,
    /// An integer variable.
    Integer,
    /// A string variable.
    String,
    /// A vector of strings.
    VecString,
    /// A vector of integers.
    VecInt,
    /// A vector of floating-point values.
    VecFloat,

    /// A boolean value.
    Bool,

    /// An object with string keys and values.
    Object,

    /// A null value.
    Null,

    /// A datetime value in UTC.
    DateTime,
}

/// An input to a node.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Input {
    /// The name of the input.
    pub name: String,
    /// The description of the input.
    pub description: String,
    /// The input type.
    #[serde(rename = "type")]
    pub input_type: VariableType,
    /// The reference to another node's output.
    pub reference: String,
    /// An optional prompt for the input.
    pub prompt: Option<String>,
}

/// An output of a node.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Output {
    /// The name of the output.
    pub name: String,
    /// The output type.
    #[serde(rename = "type")]
    pub output_type: VariableType,
}

/// A node in the graph.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Node {
    /// The unique identifier of the node.
    ///
    pub id: String,
    /// The dependencies of the node (other nodes that must be executed before this node).
    pub dependencies: Vec<String>,
    /// The inputs of the node.
    pub inputs: Vec<IOField>,
    /// The outputs of the node.
    pub outputs: Vec<IOField>,
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
}

/// Type alias for a cache of input and output values.
pub type Cache = HashMap<String, Value>;

/// A trait for custom actions associated with nodes.
#[async_trait]
pub trait NodeAction {
    /// Returns the name of the action.
    fn name(&self) -> String {
        type_name::<Self>().to_string()
    }

    /// Executes the action with the given node and inputs, and returns the outputs.
    async fn execute(&self, node: &Node, inputs: &Cache) -> Result<Cache>;
}

/// The main executor for DAGs.
pub struct DagExecutor {
    /// A registry of custom actions.
    function_registry: Mutex<HashMap<String, Arc<dyn NodeAction>>>,
    /// The graphs to be executed.
    graphs: HashMap<String, Graph>,
    /// The prebuilt DAGs.
    prebuilt_dags: HashMap<String, (DiGraph<Node, ()>, HashMap<String, NodeIndex>)>,
}

impl DagExecutor {
    /// Creates a new `DagExecutor`.
    pub fn new() -> Self {
        DagExecutor {
            function_registry: Mutex::new(HashMap::new()),
            graphs: HashMap::new(),
            prebuilt_dags: HashMap::new(),
        }
    }

    /// Registers a custom action with the `DagExecutor`.
    pub fn register_action(&self, action: Arc<dyn NodeAction>) {
        info!("Registered action: {:#?}", action.name());
        let action_name = action.name().clone(); // Get the name from the action itself
        self.function_registry
            .lock()
            .unwrap()
            .insert(action_name, action);
    }

    /// Loads a graph definition from a YAML file.
    pub fn load_yaml_file(&mut self, file_path: &str) -> Result<(), Error> {
        let mut file = match File::open(file_path) {
            Ok(file) => file,
            Err(e) => {
                error!("Failed to open file: {}", e);
                return Err(e.into());
            }
        };
        let mut yaml_content = String::new();
        file.read_to_string(&mut yaml_content)
            .context("Failed to read file")?;

        let graph: Graph = serde_yaml::from_str(&yaml_content).context("Failed to parse YAML")?;

        let (dag, node_indices) = self.build_dag_internal(&graph)?;
        let name = graph.name.clone();
        self.graphs.insert(name.clone(), graph);
        self.prebuilt_dags.insert(name, (dag, node_indices));
        Ok(())
    }

    /// Builds a directed acyclic graph (DAG) from the loaded graph definition.
    // pub fn build_dag(&mut self, name: &str) -> Result<(), Error> {
    //     let graph = self
    //         .graphs
    //         .get(name)
    //         .ok_or_else(|| anyhow!("Graph '{}' not found", name))?;
    //     let (dag, node_indices) = self.build_dag_internal(graph)?;
    //     self.prebuilt_dags
    //         .insert(name.to_string(), (dag, node_indices));
    //     Ok(())
    // }

    /// Executes the DAG with the given inputs and returns the outputs.
    pub async fn execute_dag(&self, name: &str, inputs: &Cache) -> Result<Cache, Error> {
        let (dag, node_indices) = self
            .prebuilt_dags
            .get(name)
            .ok_or_else(|| anyhow!("Graph '{}' not found", name))?;
        let mut updated_inputs = inputs.clone();
        execute_dag_async(self, &dag, &mut updated_inputs).await?;
        Ok(updated_inputs)
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
        validate_io_data_types(&graph.nodes)?;

        for node in &graph.nodes {
            let dependent_node_index = node_indices[&node.id];
            for dependency_id in &node.dependencies {
                let dependency_node_index = node_indices[dependency_id];
                dag.add_edge(dependency_node_index, dependent_node_index, ());
            }
        }

        Ok((dag, node_indices))
    }
}

/// Executes a single node asynchronously and returns its outputs.

async fn execute_node_async(
    executor: &DagExecutor,
    node: &Node,
    inputs: &Cache,
) -> Result<(String, Result<Cache>), anyhow::Error> {
    println!("Executing node: {}", node.id);
    let action = {
        let registry = executor
            .function_registry
            .lock()
            .map_err(|e| anyhow!("Failed to acquire lock: {}", e))?;
        registry
            .get(&node.action)
            .cloned()
            .ok_or_else(|| anyhow!("Unknown action {} for node {}", node.action, node.id))?
    };

    info!("Executing action for node: {}", node.id);

    let timeout_duration = Duration::from_secs(node.timeout as u64);
    let mut retries_left = node.try_count;

    while retries_left > 0 {
        match timeout(timeout_duration, action.execute(node, inputs)).await {
            Ok(Ok(result)) => return Ok((node.id.clone(), Ok(result))),
            Ok(Err(e)) => {
                error!("Node '{}' execution failed: {}", node.id, e);
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
            Err(_) => {
                error!(
                    "Node '{}' execution timed out after {} seconds",
                    node.id, node.timeout
                );
                if node.onfailure {
                    warn!(
                        "Retrying node '{}' ({} retries left)...",
                        node.id, retries_left
                    );
                    sleep(Duration::from_secs(1)).await; // Wait for 1 second before retrying
                    retries_left -= 1;
                } else {
                    return Err(anyhow!("Node '{}' execution timed out", node.id));
                }
            }
        }
    }

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
    updated_inputs: &mut Cache,
) -> Result<(), Error> {
    let mut topo = Topo::new(&dag);
    while let Some(node_index) = topo.next(&dag) {
        let node = &dag[node_index];
        let (node_id, outputs) = match execute_node_async(executor, node, updated_inputs).await {
            Ok(result) => result,
            Err(err) => {
                if !node.onfailure {
                    // Return an error if onfailure is false
                    let mut error_messages = match updated_inputs.get("error") {
                        Some(Value::VecString(messages)) => messages.clone(),
                        Some(_) => {
                            warn!("Error key already exists with a different type. Overwriting with a new error message.");
                            vec![]
                        }
                        None => vec![],
                    };
                    error_messages.push(format!("{}: {}", node.id, err));
                    updated_inputs.insert("error".to_string(), Value::VecString(error_messages));
                    warn!(
                        "Node '{}' failed, but onfailure is set to true. Continuing...",
                        node.id
                    );
                    break;
                    // return Err(err);
                } else {
                    // Append error message to the 'error' key in updated_inputs
                    let mut error_messages = match updated_inputs.get("error") {
                        Some(Value::VecString(messages)) => messages.clone(),
                        Some(_) => {
                            warn!("Error key already exists with a different type. Overwriting with a new error message.");
                            vec![]
                        }
                        None => vec![],
                    };
                    error_messages.push(format!("{}: {}", node.id, err));
                    updated_inputs.insert("error".to_string(), Value::VecString(error_messages));
                    warn!(
                        "Node '{}' failed, but onfailure is set to true. Continuing...",
                        node.id
                    );
                    continue;
                }
            }
        };

        // Update inputs with outputs for next nodes
        for (name, value) in outputs.unwrap() {
            updated_inputs.insert(format!("{}.{}", node_id, name), value);
        }
        info!("Node {}: {:?}", node.id, updated_inputs);
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
    let registry = match executor.function_registry.lock() {
        Ok(registry) => registry,
        Err(e) => {
            error!("Failed to acquire lock: {}", e);
            return Err(anyhow!(format!("Failed to acquire lock: {}", e)));
        }
    };
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

/// Validates the data types of the inputs and outputs of the nodes.
pub fn validate_io_data_types(nodes: &[Node]) -> Result<(), Error> {
    let valid_types = vec!["Float", "Integer", "String"]; // Extend this list as needed
    for node in nodes {
        for input in &node.inputs {
            if !valid_types.contains(&input.data_type.as_str()) {
                return Err(anyhow!(format!(
                    "Unsupported data type '{}' in inputs for node '{}'.",
                    input.data_type, node.id
                )));
            }
        }
        for output in &node.outputs {
            if !valid_types.contains(&output.data_type.as_str()) {
                return Err(anyhow!(format!(
                    "Unsupported data type '{}' in outputs for node '{}'.",
                    output.data_type, node.id
                )));
            }
        }
    }
    Ok(())
}
