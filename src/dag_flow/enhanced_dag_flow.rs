use crate::errors::{DaggerError, Result};
use crate::memory::Cache as ImprovedCache;
use crate::limits::{ResourceTracker, ResourceLimits};
use crate::concurrency::{ConcurrentTaskRegistry, TaskNotificationSystem, AtomicTaskState, TaskStatus as ConcurrentTaskStatus};
use crate::performance::{PerformanceMonitor, AsyncSerializationService};
use crate::dag_builder::{DagConfig, NodeDefinition, DagExecutionContext, DagExecutionState, DagStatus, NodeStatus, NodeExecutionState, ErrorHandling, RetryPolicy, BackoffStrategy};

use async_trait::async_trait;
use dashmap::DashMap;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::algo::{is_cyclic_directed, toposort};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore};
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn, instrument};
use uuid::Uuid;
use chrono::Utc;

/// Enhanced DAG executor with improved infrastructure
#[derive(Debug)]
pub struct EnhancedDagExecutor {
    /// Execution context with all shared resources
    context: Arc<DagExecutionContext>,
    /// Node action registry
    actions: Arc<DashMap<String, Arc<dyn EnhancedNodeAction>>>,
    /// Execution graph
    graph: Arc<RwLock<DiGraph<String, ()>>>,
    /// Node index mapping
    node_indices: Arc<RwLock<HashMap<String, NodeIndex>>>,
    /// Execution semaphore for concurrency control
    execution_semaphore: Arc<Semaphore>,
}

/// Enhanced node action trait with performance monitoring
#[async_trait]
pub trait EnhancedNodeAction: Send + Sync {
    /// Action name for identification
    fn name(&self) -> String;
    
    /// Action schema for validation
    fn schema(&self) -> Value;
    
    /// Execute the action with enhanced context
    async fn execute(
        &self,
        executor: &EnhancedDagExecutor,
        node: &NodeDefinition,
        context: &DagExecutionContext,
    ) -> crate::errors::Result<Value>;
    
    /// Validate input before execution (optional)
    async fn validate_input(&self, _input: &Value) -> crate::errors::Result<()> {
        Ok(())
    }
    
    /// Cleanup after execution (optional)
    async fn cleanup(&self, _node: &NodeDefinition, _context: &DagExecutionContext) -> crate::errors::Result<()> {
        Ok(())
    }
}

/// Enhanced execution result with detailed metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedExecutionResult {
    /// Node ID
    pub node_id: String,
    /// Execution status
    pub status: NodeStatus,
    /// Result data (if successful)
    pub result: Option<Value>,
    /// Error information (if failed)
    pub error: Option<String>,
    /// Execution metrics
    pub metrics: NodeExecutionMetrics,
    /// Start time
    pub start_time: Instant,
    /// End time
    pub end_time: Option<Instant>,
    /// Retry count
    pub retry_count: u32,
}

/// Detailed execution metrics for nodes
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeExecutionMetrics {
    /// Total execution duration
    pub execution_duration: Option<Duration>,
    /// Queue time before execution
    pub queue_duration: Option<Duration>,
    /// Memory used during execution
    pub memory_used_bytes: u64,
    /// Whether result was cached
    pub cache_hit: bool,
    /// Serialization time
    pub serialization_duration: Option<Duration>,
    /// Number of dependencies resolved
    pub dependencies_resolved: usize,
    /// Custom metrics
    pub custom_metrics: HashMap<String, f64>,
}

impl EnhancedDagExecutor {
    /// Create a new enhanced DAG executor
    pub fn new(context: Arc<DagExecutionContext>) -> Self {
        let max_parallel = context.config.max_parallel_nodes;
        
        Self {
            context,
            actions: Arc::new(DashMap::new()),
            graph: Arc::new(RwLock::new(DiGraph::new())),
            node_indices: Arc::new(RwLock::new(HashMap::new())),
            execution_semaphore: Arc::new(Semaphore::new(max_parallel)),
        }
    }
    
    /// Register an enhanced action
    pub fn register_action(&self, action: Arc<dyn EnhancedNodeAction>) {
        let name = action.name();
        self.actions.insert(name, action);
    }
    
    /// Build the execution graph from node definitions
    pub async fn build_graph(&self, nodes: HashMap<String, NodeDefinition>) -> Result<()> {
        let mut graph = self.graph.write().await;
        let mut node_indices = self.node_indices.write().await;
        
        // Clear existing graph
        graph.clear();
        node_indices.clear();
        
        // Add all nodes first
        for (node_id, _node_def) in &nodes {
            let index = graph.add_node(node_id.clone());
            node_indices.insert(node_id.clone(), index);
        }
        
        // Add edges based on dependencies
        for (node_id, node_def) in &nodes {
            let node_index = node_indices[node_id];
            
            for dep_id in &node_def.dependencies {
                if let Some(&dep_index) = node_indices.get(dep_id) {
                    graph.add_edge(dep_index, node_index, ());
                } else {
                    return Err(DaggerError::validation(format!(
                        "Node '{}' depends on non-existent node '{}'",
                        node_id, dep_id
                    )));
                }
            }
        }
        
        // Check for cycles
        if is_cyclic_directed(&*graph) {
            return Err(DaggerError::validation("DAG contains cycles"));
        }
        
        info!("Built execution graph with {} nodes", nodes.len());
        Ok(())
    }
    
    /// Execute the DAG with enhanced monitoring and error handling
    #[instrument(skip(self, nodes))]
    pub async fn execute_dag(
        &self,
        nodes: HashMap<String, NodeDefinition>,
    ) -> Result<HashMap<String, EnhancedExecutionResult>> {
        // Build the execution graph
        self.build_graph(nodes.clone()).await?;
        
        // Start execution time tracking
        self.context.resource_tracker.start_execution().await;
        
        // Update DAG state to running
        {
            let mut state = self.context.state.write().await;
            state.status = DagStatus::Running;
            state.start_time = Some(Instant::now());
        }
        
        let operation_start = Instant::now();
        
        // Execute nodes in topological order with concurrency
        let result = self.execute_nodes_parallel(nodes).await;
        
        // Record overall execution time
        let execution_duration = operation_start.elapsed();
        self.context.performance_monitor.record_operation("dag_execution", execution_duration);
        
        // Update final DAG state
        {
            let mut state = self.context.state.write().await;
            state.end_time = Some(Instant::now());
            state.status = match &result {
                Ok(_) => DagStatus::Completed,
                Err(e) => {
                    state.error = Some(e.to_string());
                    DagStatus::Failed
                }
            };
            state.metrics.total_execution_time = Some(execution_duration);
        }
        
        result
    }
    
    /// Execute nodes in parallel while respecting dependencies
    async fn execute_nodes_parallel(
        &self,
        nodes: HashMap<String, NodeDefinition>,
    ) -> Result<HashMap<String, EnhancedExecutionResult>> {
        let mut results = HashMap::new();
        let mut executing = HashSet::new();
        let mut completed = HashSet::new();
        let total_nodes = nodes.len();
        
        // Continue until all nodes are processed
        while completed.len() < total_nodes {
            // Check execution timeout
            self.context.resource_tracker.check_execution_time().await?;
            
            // Find ready nodes (all dependencies completed)
            let ready_nodes: Vec<String> = nodes
                .iter()
                .filter_map(|(node_id, node_def)| {
                    if completed.contains(node_id) || executing.contains(node_id) {
                        return None;
                    }
                    
                    // Check if all dependencies are completed
                    let deps_ready = node_def.dependencies.iter().all(|dep| completed.contains(dep));
                    if deps_ready {
                        Some(node_id.clone())
                    } else {
                        None
                    }
                })
                .collect();
            
            if ready_nodes.is_empty() && executing.is_empty() {
                return Err(DaggerError::execution(
                    "dag_executor",
                    "No ready nodes and no executing nodes - possible dependency deadlock"
                ));
            }
            
            // Launch ready nodes
            let mut tasks = Vec::new();
            let ready_nodes_count = ready_nodes.len();
            for node_id in ready_nodes {
                executing.insert(node_id.clone());
                let node_def = nodes[&node_id].clone();
                let executor = self.clone();
                
                let task = tokio::spawn(async move {
                    let result = executor.execute_single_node(node_def).await;
                    (node_id, result)
                });
                
                tasks.push(task);
            }
            
            // Wait for at least one task to complete
            if !tasks.is_empty() {
                let (node_id, execution_result) = tasks
                    .into_iter()
                    .next()
                    .unwrap()
                    .await
                    .map_err(|e| DaggerError::internal(format!("Task join error: {}", e)))?;
                
                executing.remove(&node_id);
                
                match execution_result {
                    Ok(result) => {
                        completed.insert(node_id.clone());
                        results.insert(node_id, result);
                    }
                    Err(e) => {
                        // Handle error based on error handling strategy
                        let should_continue = self.handle_node_error(&node_id, &e).await;
                        if !should_continue {
                            return Err(e);
                        }
                        completed.insert(node_id.clone());
                    }
                }
            }
            
            // Small delay to prevent busy waiting
            if executing.is_empty() && ready_nodes_count == 0 {
                sleep(Duration::from_millis(10)).await;
            }
        }
        
        Ok(results)
    }
    
    /// Execute a single node with comprehensive monitoring
    #[instrument(skip(self, node_def))]
    async fn execute_single_node(&self, node_def: NodeDefinition) -> Result<EnhancedExecutionResult> {
        let node_id = node_def.id.clone();
        let queue_start = Instant::now();
        
        // Acquire execution semaphore
        let _permit = self.execution_semaphore.acquire().await
            .map_err(|e| DaggerError::concurrency(format!("Failed to acquire execution permit: {}", e)))?;
        
        let queue_duration = queue_start.elapsed();
        let execution_start = Instant::now();
        
        // Allocate memory tracking for this node
        let estimated_memory = 1024 * 1024; // 1MB default estimate
        let _memory_allocation = self.context.resource_tracker
            .allocate_memory(estimated_memory)?;
        
        // Start task execution tracking
        let _task_execution = self.context.resource_tracker.start_task()?;
        
        // Update node state to running
        self.update_node_state(&node_id, NodeStatus::Running, None).await;
        
        let mut result = EnhancedExecutionResult {
            node_id: node_id.clone(),
            status: NodeStatus::Running,
            result: None,
            error: None,
            metrics: NodeExecutionMetrics {
                queue_duration: Some(queue_duration),
                dependencies_resolved: node_def.dependencies.len(),
                ..Default::default()
            },
            start_time: execution_start,
            end_time: None,
            retry_count: 0,
        };
        
        // Execute with retries
        let execution_result = self.execute_with_retries(&node_def, &mut result).await;
        
        // Record final timing
        let end_time = Instant::now();
        result.end_time = Some(end_time);
        result.metrics.execution_duration = Some(end_time.duration_since(execution_start));
        
        // Record performance metrics
        let operation_name = format!("node_execution_{}", node_def.node_type);
        let duration = result.metrics.execution_duration.unwrap_or_default();
        
        match &execution_result {
            Ok(_) => {
                self.context.performance_monitor.record_operation(&operation_name, duration);
                result.status = NodeStatus::Completed;
            }
            Err(e) => {
                self.context.performance_monitor.record_error(&operation_name);
                result.status = NodeStatus::Failed;
                result.error = Some(e.to_string());
            }
        }
        
        // Update final node state
        self.update_node_state(&node_id, result.status.clone(), result.error.clone()).await;
        
        // Store result in cache if successful and cacheable
        if execution_result.is_ok() && node_def.cacheable {
            if let Some(ref result_data) = result.result {
                let cache_key = format!("node_result_{}", node_id);
                if let Err(e) = self.context.cache.insert_value(&node_id, &cache_key, result_data).await {
                    warn!("Failed to cache result for node {}: {}", node_id, e);
                } else {
                    result.metrics.cache_hit = false; // This is a cache store, not hit
                    debug!("Cached result for node {}", node_id);
                }
            }
        }
        
        execution_result.map(|_| result)
    }
    
    /// Execute node with retry logic
    async fn execute_with_retries(
        &self,
        node_def: &NodeDefinition,
        result: &mut EnhancedExecutionResult,
    ) -> crate::errors::Result<Value> {
        let retry_policy = node_def.retry_policy.as_ref()
            .unwrap_or(&self.context.config.retry_policy);
        
        let mut last_error = None;
        let max_retries = retry_policy.max_retries;
        
        for attempt in 0..=max_retries {
            result.retry_count = attempt;
            
            // Check if we should retry this error
            if attempt > 0 {
                if let Some(ref error) = last_error {
                    let should_retry = self.should_retry_error(error, retry_policy);
                    if !should_retry {
                        return Err(crate::errors::DaggerError::execution("node_executor", "Retry limit exceeded"));
                    }
                    
                    // Apply backoff delay
                    let delay = self.calculate_backoff_delay(attempt, retry_policy);
                    sleep(delay).await;
                }
            }
            
            // Try to get cached result first
            if node_def.cacheable && attempt == 0 {
                if let Some(cached_result) = self.get_cached_result(&node_def.id).await {
                    result.metrics.cache_hit = true;
                    return Ok(cached_result);
                }
            }
            
            // Execute the node action
            match self.execute_node_action(node_def).await {
                Ok(execution_result) => {
                    result.result = Some(execution_result.clone());
                    return Ok(execution_result);
                }
                Err(e) => {
                    last_error = Some(e);
                    warn!("Node {} execution attempt {} failed: {}", 
                          node_def.id, attempt + 1, last_error.as_ref().unwrap());
                }
            }
        }
        
        Err(last_error.unwrap_or_else(|| 
            DaggerError::execution("node_executor", "Unknown execution error")))
    }
    
    /// Execute the actual node action
    async fn execute_node_action(&self, node_def: &NodeDefinition) -> crate::errors::Result<Value> {
        // Find the appropriate action
        let action = self.actions.get(&node_def.node_type)
            .ok_or_else(|| crate::errors::DaggerError::execution(
                "node_executor", 
                "No action registered for node type"
            ))?;
        
        // Validate input if the action supports it
        action.validate_input(&node_def.config).await?;
        
        // Apply timeout if specified
        let execution_future = action.execute(self, node_def, &self.context);
        
        let result = if let Some(timeout_duration) = node_def.timeout {
            timeout(timeout_duration, execution_future).await
                .map_err(|_| DaggerError::timeout("node_execution", timeout_duration.as_millis() as u64))?
        } else {
            execution_future.await
        }?;
        
        // Cleanup after execution
        action.cleanup(node_def, &self.context).await?;
        
        Ok(result)
    }
    
    /// Check if an error should trigger a retry
    fn should_retry_error(&self, error: &DaggerError, retry_policy: &RetryPolicy) -> bool {
        let error_category = error.category();
        retry_policy.retry_on.iter().any(|pattern| pattern == error_category)
    }
    
    /// Calculate backoff delay for retries
    fn calculate_backoff_delay(&self, attempt: u32, retry_policy: &RetryPolicy) -> Duration {
        let base_delay = retry_policy.base_delay;
        let max_delay = retry_policy.max_delay;
        
        let delay = match retry_policy.backoff {
            BackoffStrategy::Fixed => base_delay,
            BackoffStrategy::Linear => base_delay * attempt,
            BackoffStrategy::Exponential => {
                let exponential_delay = base_delay * (2_u64.pow(attempt.saturating_sub(1)));
                Duration::from_millis(exponential_delay.as_millis().min(max_delay.as_millis()) as u64)
            }
            BackoffStrategy::ExponentialJitter => {
                let exponential_delay = base_delay * (2_u64.pow(attempt.saturating_sub(1)));
                let jitter = (fastrand::u32(..) as f64 / u32::MAX as f64) * 0.2 - 0.1; // ±10% jitter
                let jittered_delay = exponential_delay.as_millis() as f64 * (1.0 + jitter);
                Duration::from_millis(jittered_delay.min(max_delay.as_millis() as f64) as u64)
            }
        };
        
        delay.min(max_delay)
    }
    
    /// Get cached result for a node
    async fn get_cached_result(&self, node_id: &str) -> Option<Value> {
        let cache_key = format!("node_result_{}", node_id);
        self.context.cache.get_value(node_id, &cache_key).await
    }
    
    /// Update node execution state
    async fn update_node_state(&self, node_id: &str, status: NodeStatus, error: Option<String>) {
        let mut state = self.context.state.write().await;
        if let Some(node_state) = state.node_states.get_mut(node_id) {
            node_state.status = status;
            node_state.error = error;
            
            match node_state.status {
                NodeStatus::Running => {
                    node_state.start_time = Some(Instant::now());
                }
                NodeStatus::Completed | NodeStatus::Failed | NodeStatus::Cancelled => {
                    node_state.end_time = Some(Instant::now());
                    
                    // Update metrics
                    match node_state.status {
                        NodeStatus::Completed => state.metrics.completed_nodes += 1,
                        NodeStatus::Failed => state.metrics.failed_nodes += 1,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    
    /// Handle node execution error based on error handling strategy
    async fn handle_node_error(&self, node_id: &str, error: &DaggerError) -> bool {
        match self.context.config.error_handling {
            ErrorHandling::StopOnError => false,
            ErrorHandling::ContinueOnError => {
                warn!("Node {} failed, continuing with other nodes: {}", node_id, error);
                true
            }
            ErrorHandling::SkipOnError => {
                info!("Skipping failed node {}: {}", node_id, error);
                self.update_node_state(node_id, NodeStatus::Skipped, Some(error.to_string())).await;
                true
            }
            ErrorHandling::Custom(_) => {
                // TODO: Implement custom error handling
                warn!("Custom error handling not implemented, stopping on error");
                false
            }
        }
    }
    
    /// Get current execution statistics
    pub async fn get_execution_stats(&self) -> DagExecutionState {
        self.context.state.read().await.clone()
    }
}

// Allow cloning for concurrent execution
impl Clone for EnhancedDagExecutor {
    fn clone(&self) -> Self {
        Self {
            context: Arc::clone(&self.context),
            actions: Arc::clone(&self.actions),
            graph: Arc::clone(&self.graph),
            node_indices: Arc::clone(&self.node_indices),
            execution_semaphore: Arc::clone(&self.execution_semaphore),
        }
    }
}

/// Example enhanced action implementation
pub struct ExampleEnhancedAction {
    name: String,
}

impl ExampleEnhancedAction {
    pub fn new(name: String) -> Self {
        Self { name }
    }
}

#[async_trait]
impl EnhancedNodeAction for ExampleEnhancedAction {
    fn name(&self) -> String {
        self.name.clone()
    }
    
    fn schema(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "description": "Example enhanced action with monitoring",
            "parameters": {
                "type": "object",
                "properties": {
                    "message": {"type": "string"},
                    "delay_ms": {"type": "number", "minimum": 0}
                }
            },
            "returns": {
                "type": "object",
                "properties": {
                    "result": {"type": "string"},
                    "processed_at": {"type": "string"}
                }
            }
        })
    }
    
    #[instrument(skip(self, context))]
    async fn execute(
        &self,
        _executor: &EnhancedDagExecutor,
        node: &NodeDefinition,
        context: &DagExecutionContext,
    ) -> crate::errors::Result<Value> {
        let message = node.config["message"].as_str().unwrap_or("No message");
        let delay_ms = node.config["delay_ms"].as_u64().unwrap_or(0);
        
        // Simulate work with optional delay
        if delay_ms > 0 {
            sleep(Duration::from_millis(delay_ms)).await;
        }
        
        // Record custom metric
        context.performance_monitor.record_operation(
            &format!("action_{}", self.name), 
            Duration::from_millis(delay_ms)
        );
        
        let result = serde_json::json!({
            "result": format!("Processed: {}", message),
            "processed_at": Utc::now().to_rfc3339(),
            "node_id": node.id,
            "action": self.name
        });
        
        Ok(result)
    }
    
    async fn validate_input(&self, input: &Value) -> crate::errors::Result<()> {
        if input.get("message").is_none() {
            return Err(DaggerError::validation("Message field is required"));
        }
        
        if let Some(delay) = input.get("delay_ms") {
            if !delay.is_number() || delay.as_u64().unwrap_or(0) > 60000 {
                return Err(DaggerError::validation("delay_ms must be a number ≤ 60000"));
            }
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builders::DaggerBuilder;
    use tokio::test;
    
    #[test]
    async fn test_enhanced_dag_execution() {
        // Create configuration
        let dagger_config = DaggerBuilder::testing().build().unwrap();
        
        // Create shared resources
        let cache = Arc::new(ImprovedCache::new(dagger_config.cache.clone()).unwrap());
        let resource_tracker = Arc::new(ResourceTracker::new(dagger_config.limits.clone()).unwrap());
        let task_registry = Arc::new(ConcurrentTaskRegistry::new(10));
        
        // Create DAG configuration
        let (dag_config, nodes) = crate::dag_builder::DagFlowBuilder::new("test_dag")
            .description("Test DAG for enhanced execution")
            .node(|node| {
                node.id("test_node")
                    .name("Test Node")
                    .node_type("example")
                    .config(serde_json::json!({
                        "message": "Hello, Enhanced DAG!",
                        "delay_ms": 100
                    }))
            })
            .build()
            .unwrap();
        
        // Create execution context
        let context = dag_config.build_execution_context(
            dagger_config,
            cache,
            resource_tracker,
            task_registry,
        ).unwrap();
        
        // Create executor and register action
        let executor = EnhancedDagExecutor::new(Arc::new(context));
        executor.register_action(Arc::new(ExampleEnhancedAction::new("example".to_string())));
        
        // Execute DAG
        let results = executor.execute_dag(nodes).await.unwrap();
        
        // Verify results
        assert_eq!(results.len(), 1);
        let result = &results["test_node"];
        assert!(matches!(result.status, NodeStatus::Completed));
        assert!(result.result.is_some());
        assert!(result.metrics.execution_duration.is_some());
    }
}