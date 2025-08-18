// use crate::core::builders::DaggerConfig;
use crate::core::errors::{DaggerError, Result};
use crate::core::limits::ResourceTracker;
use crate::core::memory::Cache;
// use crate::core::concurrency::ConcurrentTaskRegistry;
// use crate::core::performance::{PerformanceMonitor, AsyncSerializationService};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

/// DAG execution configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagConfig {
    /// DAG name/identifier
    pub name: String,
    /// Description of the DAG
    pub description: Option<String>,
    /// Maximum execution time for the entire DAG
    pub max_execution_time: Duration,
    /// Maximum number of parallel node executions
    pub max_parallel_nodes: usize,
    /// Retry policy for failed nodes
    pub retry_policy: RetryPolicy,
    /// Error handling strategy
    pub error_handling: ErrorHandling,
    /// Whether to enable caching for this DAG
    pub enable_caching: bool,
    /// Whether to enable detailed metrics
    pub enable_metrics: bool,
    /// Custom metadata
    pub metadata: HashMap<String, Value>,
}

impl Default for DagConfig {
    fn default() -> Self {
        Self {
            name: format!("dag_{}", Uuid::new_v4()),
            description: None,
            max_execution_time: Duration::from_secs(3600), // 1 hour
            max_parallel_nodes: 10,
            retry_policy: RetryPolicy::default(),
            error_handling: ErrorHandling::StopOnError,
            enable_caching: true,
            enable_metrics: true,
            metadata: HashMap::new(),
        }
    }
}

/// Retry policy for DAG nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of retries
    pub max_retries: u32,
    /// Base delay between retries
    pub base_delay: Duration,
    /// Maximum delay between retries
    pub max_delay: Duration,
    /// Backoff strategy
    pub backoff: BackoffStrategy,
    /// Which errors should trigger retries
    pub retry_on: Vec<String>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            backoff: BackoffStrategy::Exponential,
            retry_on: vec![
                "timeout".to_string(),
                "resource".to_string(),
                "concurrency".to_string(),
            ],
        }
    }
}

/// Backoff strategy for retries
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackoffStrategy {
    /// Fixed delay
    Fixed,
    /// Linear increase
    Linear,
    /// Exponential backoff
    Exponential,
    /// Exponential with jitter
    ExponentialJitter,
}

/// Error handling strategy for DAG execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorHandling {
    /// Stop entire DAG on first error
    StopOnError,
    /// Continue executing independent nodes
    ContinueOnError,
    /// Skip failed nodes and continue
    SkipOnError,
    /// Custom error handling function
    Custom(String),
}

/// Node definition in a DAG
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDefinition {
    /// Unique node identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Node description
    pub description: Option<String>,
    /// Node type/operation
    pub node_type: String,
    /// Node configuration
    pub config: Value,
    /// Dependencies (other node IDs)
    pub dependencies: Vec<String>,
    /// Maximum execution time for this node
    pub timeout: Option<Duration>,
    /// Node-specific retry policy
    pub retry_policy: Option<RetryPolicy>,
    /// Whether this node can be cached
    pub cacheable: bool,
    /// Node metadata
    pub metadata: HashMap<String, Value>,
}

impl NodeDefinition {
    pub fn new<S: Into<String>>(id: S, node_type: S) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            description: None,
            node_type: node_type.into(),
            config: Value::Null,
            dependencies: Vec::new(),
            timeout: None,
            retry_policy: None,
            cacheable: true,
            metadata: HashMap::new(),
        }
    }
}

/// DAG execution context
#[derive(Debug)]
pub struct DagExecutionContext {
    /// DAG configuration
    pub config: DagConfig,
    /// Shared cache
    pub cache: Arc<Cache>,
    /// Resource tracker
    pub resource_tracker: Arc<ResourceTracker>,
    /// Execution state
    pub state: Arc<RwLock<DagExecutionState>>,
}

/// Current state of DAG execution
#[derive(Debug, Clone)]
pub struct DagExecutionState {
    /// Current status
    pub status: DagStatus,
    /// Node execution states
    pub node_states: HashMap<String, NodeExecutionState>,
    /// Start time
    pub start_time: Option<std::time::Instant>,
    /// End time
    pub end_time: Option<std::time::Instant>,
    /// Error information if failed
    pub error: Option<String>,
    /// Execution metrics
    pub metrics: DagExecutionMetrics,
}

/// DAG execution status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DagStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Paused,
}

/// Node execution state
#[derive(Debug, Clone)]
pub struct NodeExecutionState {
    pub status: NodeStatus,
    pub start_time: Option<std::time::Instant>,
    pub end_time: Option<std::time::Instant>,
    pub attempts: u32,
    pub error: Option<String>,
    pub result: Option<Value>,
}

/// Node execution status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeStatus {
    Pending,
    Ready,
    Running,
    Completed,
    Failed,
    Skipped,
    Cancelled,
}

/// DAG execution metrics
#[derive(Debug, Clone, Default)]
pub struct DagExecutionMetrics {
    pub total_nodes: usize,
    pub completed_nodes: usize,
    pub failed_nodes: usize,
    pub skipped_nodes: usize,
    pub total_execution_time: Option<Duration>,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub retries: u64,
}

/// Fluent builder for DAG configuration and execution
#[derive(Debug)]
pub struct DagFlowBuilder {
    config: DagConfig,
    nodes: HashMap<String, NodeDefinition>,
    edges: Vec<(String, String)>, // (from, to) relationships
}

impl DagFlowBuilder {
    /// Create a new DAG builder
    pub fn new<S: Into<String>>(name: S) -> Self {
        Self {
            config: DagConfig {
                name: name.into(),
                ..Default::default()
            },
            nodes: HashMap::new(),
            edges: Vec::new(),
        }
    }

    /// Set DAG description
    pub fn description<S: Into<String>>(mut self, description: S) -> Self {
        self.config.description = Some(description.into());
        self
    }

    /// Set maximum execution time
    pub fn max_execution_time(mut self, duration: Duration) -> Self {
        self.config.max_execution_time = duration;
        self
    }

    /// Set maximum parallel node executions
    pub fn max_parallel_nodes(mut self, count: usize) -> Self {
        self.config.max_parallel_nodes = count;
        self
    }

    /// Configure retry policy
    pub fn retry_policy<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(RetryPolicyBuilder) -> RetryPolicyBuilder,
    {
        let builder = RetryPolicyBuilder::new(self.config.retry_policy);
        self.config.retry_policy = configure(builder).build();
        self
    }

    /// Set error handling strategy
    pub fn error_handling(mut self, strategy: ErrorHandling) -> Self {
        self.config.error_handling = strategy;
        self
    }

    /// Enable or disable caching
    pub fn enable_caching(mut self, enabled: bool) -> Self {
        self.config.enable_caching = enabled;
        self
    }

    /// Enable or disable metrics
    pub fn enable_metrics(mut self, enabled: bool) -> Self {
        self.config.enable_metrics = enabled;
        self
    }

    /// Add metadata
    pub fn metadata<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Serialize,
    {
        if let Ok(json_value) = serde_json::to_value(value) {
            self.config.metadata.insert(key.into(), json_value);
        }
        self
    }

    /// Add a node to the DAG
    pub fn node<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(NodeBuilder) -> NodeBuilder,
    {
        let builder = NodeBuilder::new();
        let node = configure(builder).build();
        self.nodes.insert(node.id.clone(), node);
        self
    }

    /// Add multiple nodes
    pub fn nodes<I, F>(mut self, nodes: I) -> Self
    where
        I: IntoIterator<Item = F>,
        F: FnOnce(NodeBuilder) -> NodeBuilder,
    {
        for configure in nodes {
            let builder = NodeBuilder::new();
            let node = configure(builder).build();
            self.nodes.insert(node.id.clone(), node);
        }
        self
    }

    /// Add a dependency between nodes
    pub fn depends_on<S1, S2>(mut self, node: S1, dependency: S2) -> Self
    where
        S1: Into<String>,
        S2: Into<String>,
    {
        let node_id = node.into();
        let dep_id = dependency.into();

        if let Some(node_def) = self.nodes.get_mut(&node_id) {
            if !node_def.dependencies.contains(&dep_id) {
                node_def.dependencies.push(dep_id.clone());
            }
        }

        self.edges.push((dep_id, node_id));
        self
    }

    /// Add multiple dependencies
    pub fn depends_on_all<S, I>(mut self, node: S, dependencies: I) -> Self
    where
        S: Into<String>,
        I: IntoIterator<Item = S>,
    {
        let node_id = node.into();

        for dep in dependencies {
            let dep_id = dep.into();
            if let Some(node_def) = self.nodes.get_mut(&node_id) {
                if !node_def.dependencies.contains(&dep_id) {
                    node_def.dependencies.push(dep_id.clone());
                }
            }
            self.edges.push((dep_id, node_id.clone()));
        }

        self
    }

    /// Validate the DAG structure
    pub fn validate(&self) -> Result<()> {
        // Check for cycles
        if self.has_cycles()? {
            return Err(DaggerError::validation("DAG contains cycles"));
        }

        // Check that all dependencies exist
        for node in self.nodes.values() {
            for dep in &node.dependencies {
                if !self.nodes.contains_key(dep) {
                    return Err(DaggerError::validation(format!(
                        "Node '{}' depends on non-existent node '{}'",
                        node.id, dep
                    )));
                }
            }
        }

        // Validate node configurations
        for node in self.nodes.values() {
            if node.id.is_empty() {
                return Err(DaggerError::validation("Node ID cannot be empty"));
            }
            if node.node_type.is_empty() {
                return Err(DaggerError::validation("Node type cannot be empty"));
            }
        }

        Ok(())
    }

    /// Check for cycles using DFS
    fn has_cycles(&self) -> Result<bool> {
        use std::collections::HashSet;

        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();

        for node_id in self.nodes.keys() {
            if !visited.contains(node_id) {
                if self.has_cycle_util(node_id, &mut visited, &mut rec_stack)? {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    fn has_cycle_util(
        &self,
        node_id: &str,
        visited: &mut std::collections::HashSet<String>,
        rec_stack: &mut std::collections::HashSet<String>,
    ) -> Result<bool> {
        visited.insert(node_id.to_string());
        rec_stack.insert(node_id.to_string());

        if let Some(node) = self.nodes.get(node_id) {
            for dep in &node.dependencies {
                if !visited.contains(dep) {
                    if self.has_cycle_util(dep, visited, rec_stack)? {
                        return Ok(true);
                    }
                } else if rec_stack.contains(dep) {
                    return Ok(true);
                }
            }
        }

        rec_stack.remove(node_id);
        Ok(false)
    }

    /// Build the DAG configuration
    pub fn build(self) -> Result<(DagConfig, HashMap<String, NodeDefinition>)> {
        self.validate()?;
        Ok((self.config, self.nodes))
    }

    /// Build and prepare for execution
    pub fn build_execution_context(
        self,
        cache: Arc<Cache>,
        resource_tracker: Arc<ResourceTracker>,
    ) -> Result<DagExecutionContext> {
        let (config, nodes) = self.build()?;

        let mut node_states = HashMap::new();
        for node_id in nodes.keys() {
            node_states.insert(
                node_id.clone(),
                NodeExecutionState {
                    status: NodeStatus::Pending,
                    start_time: None,
                    end_time: None,
                    attempts: 0,
                    error: None,
                    result: None,
                },
            );
        }

        let state = Arc::new(RwLock::new(DagExecutionState {
            status: DagStatus::Pending,
            node_states,
            start_time: None,
            end_time: None,
            error: None,
            metrics: DagExecutionMetrics {
                total_nodes: nodes.len(),
                ..Default::default()
            },
        }));

        Ok(DagExecutionContext {
            config,
            cache,
            resource_tracker,
            state,
        })
    }
}

/// Builder for individual nodes
#[derive(Debug)]
pub struct NodeBuilder {
    node: NodeDefinition,
}

impl NodeBuilder {
    fn new() -> Self {
        Self {
            node: NodeDefinition::new(format!("node_{}", Uuid::new_v4()), "generic".to_string()),
        }
    }

    pub fn id<S: Into<String>>(mut self, id: S) -> Self {
        self.node.id = id.into();
        if self.node.name == format!("node_{}", Uuid::new_v4()) {
            self.node.name = self.node.id.clone();
        }
        self
    }

    pub fn name<S: Into<String>>(mut self, name: S) -> Self {
        self.node.name = name.into();
        self
    }

    pub fn description<S: Into<String>>(mut self, description: S) -> Self {
        self.node.description = Some(description.into());
        self
    }

    pub fn node_type<S: Into<String>>(mut self, node_type: S) -> Self {
        self.node.node_type = node_type.into();
        self
    }

    pub fn config<T: Serialize>(mut self, config: T) -> Self {
        if let Ok(value) = serde_json::to_value(config) {
            self.node.config = value;
        }
        self
    }

    pub fn timeout(mut self, duration: Duration) -> Self {
        self.node.timeout = Some(duration);
        self
    }

    pub fn retry_policy<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(RetryPolicyBuilder) -> RetryPolicyBuilder,
    {
        let builder = RetryPolicyBuilder::new(RetryPolicy::default());
        self.node.retry_policy = Some(configure(builder).build());
        self
    }

    pub fn cacheable(mut self, cacheable: bool) -> Self {
        self.node.cacheable = cacheable;
        self
    }

    pub fn metadata<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Serialize,
    {
        if let Ok(json_value) = serde_json::to_value(value) {
            self.node.metadata.insert(key.into(), json_value);
        }
        self
    }

    fn build(self) -> NodeDefinition {
        self.node
    }
}

/// Builder for retry policies
#[derive(Debug)]
pub struct RetryPolicyBuilder {
    policy: RetryPolicy,
}

impl RetryPolicyBuilder {
    fn new(policy: RetryPolicy) -> Self {
        Self { policy }
    }

    pub fn max_retries(mut self, retries: u32) -> Self {
        self.policy.max_retries = retries;
        self
    }

    pub fn base_delay(mut self, delay: Duration) -> Self {
        self.policy.base_delay = delay;
        self
    }

    pub fn max_delay(mut self, delay: Duration) -> Self {
        self.policy.max_delay = delay;
        self
    }

    pub fn backoff(mut self, strategy: BackoffStrategy) -> Self {
        self.policy.backoff = strategy;
        self
    }

    pub fn retry_on<I, S>(mut self, errors: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.policy.retry_on = errors.into_iter().map(|s| s.into()).collect();
        self
    }

    pub fn exponential_backoff(mut self) -> Self {
        self.policy.backoff = BackoffStrategy::Exponential;
        self
    }

    pub fn exponential_backoff_with_jitter(mut self) -> Self {
        self.policy.backoff = BackoffStrategy::ExponentialJitter;
        self
    }

    pub fn linear_backoff(mut self) -> Self {
        self.policy.backoff = BackoffStrategy::Linear;
        self
    }

    pub fn fixed_backoff(mut self) -> Self {
        self.policy.backoff = BackoffStrategy::Fixed;
        self
    }

    fn build(self) -> RetryPolicy {
        self.policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dag_builder() {
        let (config, nodes) = DagFlowBuilder::new("test_dag")
            .description("A test DAG")
            .max_parallel_nodes(5)
            .enable_caching(true)
            .retry_policy(|retry| {
                retry
                    .max_retries(3)
                    .exponential_backoff()
                    .retry_on(vec!["timeout", "resource"])
            })
            .node(|node| {
                node.id("node1")
                    .name("First Node")
                    .node_type("processor")
                    .config(serde_json::json!({"param": "value"}))
                    .cacheable(true)
            })
            .node(|node| {
                node.id("node2")
                    .name("Second Node")
                    .node_type("transformer")
                    .timeout(Duration::from_secs(30))
            })
            .depends_on("node2", "node1")
            .build()
            .unwrap();

        assert_eq!(config.name, "test_dag");
        assert_eq!(config.max_parallel_nodes, 5);
        assert_eq!(nodes.len(), 2);
        assert!(nodes.contains_key("node1"));
        assert!(nodes.contains_key("node2"));
        assert_eq!(nodes["node2"].dependencies, vec!["node1"]);
    }

    #[test]
    fn test_cycle_detection() {
        let result = DagFlowBuilder::new("cyclic_dag")
            .node(|node| node.id("a").node_type("test"))
            .node(|node| node.id("b").node_type("test"))
            .node(|node| node.id("c").node_type("test"))
            .depends_on("b", "a")
            .depends_on("c", "b")
            .depends_on("a", "c") // Creates a cycle
            .build();

        assert!(result.is_err());
    }

    #[test]
    fn test_validation() {
        // Test missing dependency
        let result = DagFlowBuilder::new("invalid_dag")
            .node(|node| node.id("node1").node_type("test"))
            .depends_on("node1", "nonexistent")
            .build();

        assert!(result.is_err());
    }
}
