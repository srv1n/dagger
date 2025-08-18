# Complete API Reference

This document provides a comprehensive reference for ALL public APIs in the Dagger crate.

## Table of Contents

1. [Core Types](#core-types)
2. [DagExecutor Methods](#dagexecutor-methods)
3. [Cache Operations](#cache-operations)
4. [Task Agent System](#task-agent-system)
5. [Pub/Sub System](#pubsub-system)
6. [Coordinator System](#coordinator-system)
7. [Storage Layer](#storage-layer)
8. [Utility Functions](#utility-functions)
9. [Error Types](#error-types)
10. [Configuration Types](#configuration-types)

## Core Types

### DAG Flow Types

```rust
// Node definition for DAG workflows
pub struct Node {
    pub id: String,
    pub dependencies: Vec<String>,
    pub inputs: Vec<IField>,
    pub outputs: Vec<OField>,
    pub action: String,
    pub failure: String,
    pub onfailure: bool,
    pub description: String,
    pub timeout: u64,
    pub try_count: u32,
    pub instructions: Option<String>,
}

// Input field definition
pub struct IField {
    pub name: String,
    pub description: Option<String>,
    pub reference: String,  // Format: "node_id.output_name"
}

// Output field definition
pub struct OField {
    pub name: String,
    pub description: Option<String>,
}

// Graph definition (from YAML)
pub struct Graph {
    pub name: String,
    pub description: String,
    pub author: String,
    pub version: String,
    pub signature: String,
    pub tags: Vec<String>,
    pub nodes: Vec<Node>,
    pub config: Option<DagConfig>,
}

// Cache for data sharing
pub struct Cache {
    pub data: Arc<DashMap<String, Value>>,
}

// Workflow specification
pub enum WorkflowSpec {
    Static { name: String },  // Static DAG by name
    Agent { task: String },    // Agent-driven flow
}

// Execution mode (simplified API)
pub enum ExecutionMode {
    Static,  // Execute pre-loaded DAG
    Agent,   // Start agent-driven flow
}
```

## DagExecutor Methods

### Creation and Configuration

```rust
impl DagExecutor {
    // Create new executor with optional config
    pub async fn new(
        config: Option<DagConfig>,
        registry: Arc<RwLock<HashMap<String, Arc<dyn NodeAction>>>>,
        database_url: &str,  // "sqlite::memory:" or "sqlite:file.db"
    ) -> Result<Self, Error>
    
    // Register an action
    pub async fn register_action(&mut self, action: Arc<dyn NodeAction>) -> Result<(), DagError>
    
    // Set services for dependency injection
    pub fn set_services<T: Any + Send + Sync + 'static>(&mut self, services: Arc<T>)
    
    // Get services
    pub fn get_services<T: Any + Send + Sync + 'static>(&self) -> Option<Arc<T>>
}
```

### Loading Workflows

```rust
impl DagExecutor {
    // Load YAML workflow file
    pub async fn load_yaml_file(&mut self, file_path: &str) -> Result<(), Error>
    
    // Load all YAML files in directory
    pub fn load_yaml_dir(&mut self, dir_path: &str)
    
    // Load from Graph struct
    pub async fn build_dag(&mut self, graph: Graph) -> Result<(), Error>
    
    // Add node dynamically
    pub async fn add_node(
        &mut self,
        dag_name: &str,
        node_id: String,
        action: String,
        dependencies: Vec<String>,
    ) -> Result<()>
    
    // Set node inputs as JSON
    pub fn set_node_inputs_json(
        &mut self,
        dag_name: &str,
        node_id: &str,
        inputs: Value,
    ) -> Result<()>
}
```

### Workflow Execution

```rust
impl DagExecutor {
    // Execute static DAG (simplified API)
    pub async fn execute_static_dag(
        &mut self,
        dag_name: &str,
        cache: &Cache,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<DagExecutionReport>
    
    // Execute agent-driven flow (simplified API)
    pub async fn execute_agent_dag(
        &mut self,
        task_description: &str,
        cache: &Cache,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<DagExecutionReport>
    
    // Execute with mode enum
    pub async fn execute_dag_with_mode(
        &mut self,
        mode: ExecutionMode,
        name_or_task: &str,
        cache: &Cache,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<DagExecutionReport>
    
    // Legacy API (still supported)
    pub async fn execute_dag(
        &mut self,
        spec: WorkflowSpec,
        cache: &Cache,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<DagExecutionReport>
    
    // Stop execution
    pub async fn stop(&mut self)
    
    // Pause execution
    pub fn pause(&mut self)
    
    // Resume execution
    pub fn resume(&mut self)
}
```

### Workflow Query Methods

```rust
impl DagExecutor {
    // List all DAGs with descriptions
    pub async fn list_dags(&self) -> Result<Vec<(String, String)>>
    
    // Filter by single tag
    pub async fn list_dag_filtered_tag(&self, filter: &str) -> Result<Vec<(String, String)>>
    
    // Filter by multiple tags (must have ALL)
    pub async fn list_dag_multiple_tags(&self, tags: Vec<String>) -> Result<Vec<(String, String)>>
    
    // Get detailed metadata for all workflows
    pub async fn list_dags_metadata(&self) -> Result<Vec<(String, String, String, String, String)>>
    // Returns: (name, description, author, version, signature)
}
```

### Persistence and Recovery

```rust
impl DagExecutor {
    // Save cache to SQLite
    pub async fn save_cache(&self, dag_name: &str, cache: &Cache) -> Result<()>
    
    // Load cache from SQLite
    pub async fn load_cache(&self, dag_name: &str) -> Result<Cache>
    
    // Save execution tree
    pub async fn save_execution_tree(&self, dag_name: &str) -> Result<()>
    
    // Load execution tree
    pub async fn load_execution_tree(&self, dag_name: &str) -> Result<Option<ExecutionTree>>
}
```

### Visualization

```rust
impl DagExecutor {
    // Generate DOT graph for visualization
    pub async fn serialize_tree_to_dot(&self, dag_name: &str) -> Result<String>
    
    // Export execution tree as JSON
    pub async fn serialize_tree_to_json(&self, dag_name: &str) -> Result<String>
    
    // Get tool schemas (for documentation)
    pub async fn get_tool_schemas(&self) -> Result<Vec<Value>>
}
```

## Cache Operations

### Writing to Cache

```rust
// Insert value into node namespace
pub fn insert_value<T>(
    cache: &Cache,
    node_id: &str,
    output_name: &str,
    value: T
) -> Result<()>
where T: Serialize

// Insert global value (two-level namespace)
pub fn insert_global_value<T: Serialize>(
    cache: &Cache,
    namespace: &str,
    key: &str,
    value: T
) -> Result<()>

// Append to array/vector (creates if not exists)
pub fn append_global_value<T>(
    cache: &Cache,
    namespace: &str,
    key: &str,
    value: T
) -> Result<()>
where T: Serialize + for<'de> Deserialize<'de>
```

### Reading from Cache

```rust
// Get value by node_id and key
pub fn get_input<T>(
    cache: &Cache,
    node_id: &str,
    key: &str,
) -> Result<T>
where T: for<'de> Deserialize<'de>

// Parse input using node's input field references
pub fn parse_input_from_name<T>(
    cache: &Cache,
    input_name: String,  // Note: String, not &str
    inputs: &[IField],
) -> Result<T>
where T: for<'de> Deserialize<'de>

// Get global value by namespace and key
pub fn get_global_input<T>(
    cache: &Cache,
    namespace: &str,
    key: &str,
) -> Result<T>
where T: for<'de> Deserialize<'de>
```

### Cache Serialization

```rust
// Export cache as JSON
pub fn serialize_cache_to_json(cache: &Cache) -> Result<String>

// Export cache as pretty JSON
pub fn serialize_cache_to_prettyjson(cache: &Cache) -> Result<String>

// Load cache from JSON
pub fn load_cache_from_json(json_data: &str) -> Result<Cache>
```

## Task Agent System

### Core Task Types

```rust
pub struct Task {
    pub id: String,
    pub input: Value,
    pub output: Option<Value>,
    pub status: TaskStatus,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub dependencies: Vec<String>,
    pub agent_id: Option<String>,
}

pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Waiting,
    Retrying,
}

pub struct TaskManager {
    // Main task orchestration system
}
```

### TaskManager Methods

```rust
impl TaskManager {
    // Create new task manager
    pub fn new(
        registry: Arc<RwLock<HashMap<String, Arc<dyn TaskAgent>>>>,
        db_path: &str,
    ) -> Result<Self>
    
    // Start a job with tasks
    pub fn start_job(
        &mut self,
        job_id: String,
        tasks: Vec<Task>,
        cancel_rx: Option<oneshot::Receiver<()>>,
        completion_hook: Option<CompletionCallback>,
    ) -> Result<JobHandle>
    
    // Get job status
    pub fn get_job_status(&self, job_id: &str) -> Option<JobStatus>
    
    // Cancel a job
    pub fn cancel_job(&self, job_id: &str) -> Result<()>
    
    // Get execution report
    pub fn get_execution_report(&self, job_id: &str) -> Option<TaskExecutionReport>
    
    // Register an agent
    pub fn register_agent(&mut self, name: String, agent: Arc<dyn TaskAgent>)
    
    // Get DOT graph for visualization
    pub fn get_dot_graph(&self, job_id: Option<&str>) -> Result<String>
}
```

### TaskAgent Trait

```rust
#[async_trait]
pub trait TaskAgent: Send + Sync + 'static {
    // Agent name
    fn name(&self) -> String;
    
    // Can this agent handle this task?
    fn can_handle(&self, task: &Task) -> bool;
    
    // Execute the task
    async fn execute(&self, task: Task) -> Result<Value, String>;
    
    // Get agent capabilities
    fn capabilities(&self) -> Vec<String>;
    
    // Validate task input
    fn validate_input(&self, input: &Value) -> Result<(), String>;
}
```

## Pub/Sub System

### Core Types

```rust
pub struct Message {
    pub id: String,
    pub channel: String,
    pub payload: Value,
    pub timestamp: i64,
    pub sender: String,
    pub headers: HashMap<String, String>,
}

pub struct PubSubExecutor {
    // Main pub/sub orchestration
}

#[async_trait]
pub trait PubSubAgent: Send + Sync + 'static {
    fn name(&self) -> String;
    fn subscriptions(&self) -> Vec<String>;
    fn publications(&self) -> Vec<String>;
    async fn handle_message(&self, msg: Message) -> Result<()>;
    fn input_schema(&self) -> Option<Value>;
    fn output_schema(&self) -> Option<Value>;
}
```

### PubSubExecutor Methods

```rust
impl PubSubExecutor {
    // Create new executor
    pub fn new(config: Option<PubSubConfig>) -> Self
    
    // Register an agent
    pub async fn register_agent(&mut self, agent: Arc<dyn PubSubAgent>) -> Result<()>
    
    // Publish a message
    pub async fn publish(&self, channel: &str, payload: Value) -> Result<()>
    
    // Subscribe to channel
    pub async fn subscribe(&self, channel: &str) -> Result<Receiver<Message>>
    
    // Start processing
    pub async fn start(&mut self, cancel_rx: oneshot::Receiver<()>) -> Result<()>
    
    // Get channel statistics
    pub async fn get_channel_stats(&self, channel: &str) -> Option<ChannelStats>
}
```

## Coordinator System

### Core Types

```rust
pub struct Coordinator {
    // Orchestrates parallel execution
}

pub struct NodeRef {
    pub dag_name: String,
    pub node_id: String,
}

pub enum ExecutionEvent {
    NodeStarted { node: NodeRef },
    NodeCompleted { node: NodeRef, outcome: NodeOutcome },
    NodeFailed { node: NodeRef, error: String },
    NodeSkipped { node: NodeRef, reason: String },
}

pub enum ExecutorCommand {
    AddNode { dag_name: String, spec: NodeSpec },
    AddNodes { dag_name: String, specs: Vec<NodeSpec> },
    SetNodeInputs { dag_name: String, node_id: String, inputs: Value },
    PauseBranch { branch_id: String, reason: Option<String> },
    ResumeBranch { branch_id: String },
    CancelBranch { branch_id: String },
    EmitEvent { event: Value },
}
```

### Coordinator Methods

```rust
impl Coordinator {
    // Create new coordinator
    pub fn new(
        hooks: Vec<Arc<dyn EventHook>>,
        cap_events: usize,
        cap_cmds: usize,
    ) -> Self
    
    // Run parallel execution
    pub async fn run_parallel(
        mut self,
        exec: &mut DagExecutor,
        cache: &Cache,
        dag_name: &str,
        run_id: &str,
        cancel_rx: oneshot::Receiver<()>,
    ) -> Result<()>
    
    // Get event sender (for workers)
    pub fn event_sender(&self) -> mpsc::Sender<ExecutionEvent>
    
    // Get command sender (for hooks)
    pub fn command_sender(&self) -> mpsc::Sender<ExecutorCommand>
}
```

### EventHook Trait

```rust
#[async_trait]
pub trait EventHook: Send + Sync {
    // Handle execution events
    async fn handle(
        &self,
        ctx: &HookContext,
        event: &ExecutionEvent,
    ) -> Vec<ExecutorCommand>;
    
    // Called at start
    async fn on_start(&self, ctx: &HookContext) -> Vec<ExecutorCommand>;
    
    // Called at completion
    async fn on_complete(
        &self,
        ctx: &HookContext,
        success: bool,
    ) -> Vec<ExecutorCommand>;
}
```

## Storage Layer

### SqliteStorage

```rust
pub struct SqliteStorage {
    // SQLite backend for persistence
}

impl SqliteStorage {
    // Create new storage
    pub async fn new(database_url: &str) -> Result<Self>
    
    // Store artifact
    pub async fn store_artifact(&self, key: &str, data: &[u8]) -> Result<()>
    
    // Retrieve artifact
    pub async fn get_artifact(&self, key: &str) -> Result<Option<Vec<u8>>>
    
    // Store execution snapshot
    pub async fn store_snapshot(&self, dag_id: &str, snapshot: &Value) -> Result<()>
    
    // Get execution history
    pub async fn get_execution_history(&self, dag_id: &str) -> Result<Vec<Value>>
    
    // Clean old data
    pub async fn cleanup_old_data(&self, days: i32) -> Result<u64>
}
```

## Utility Functions

```rust
// Generate unique node ID
pub fn generate_node_id(action_name: &str) -> String

// Generate cache reference
pub fn generate_cache_ref(node_id: &str) -> String

// Validate DAG structure
pub fn validate_dag_structure(dag: &DiGraph<Node, ()>) -> Result<(), Error>

// Validate node dependencies
pub fn validate_node_dependencies(
    nodes: &[Node],
    node_indices: &HashMap<String, NodeIndex>,
) -> Result<(), Error>

// Validate node actions
pub async fn validate_node_actions(
    executor: &DagExecutor,
    nodes: &[Node],
) -> Result<(), Error>
```

## Error Types

```rust
pub enum DagError {
    NodeNotFound(String),
    CyclicDependency,
    InvalidGraph(String),
    ExecutionFailed(String),
    Timeout,
    Cancelled,
    ConfigError(String),
    IoError(String),
    SerializationError(String),
    DatabaseError(String),
}

pub enum TaskError {
    TaskNotFound(String),
    AgentNotFound(String),
    ExecutionFailed(String),
    ValidationFailed(String),
    DependencyFailed(String),
}

pub enum PubSubError {
    ChannelNotFound(String),
    SchemaValidationFailed(String),
    MessageDeliveryFailed(String),
    AgentError(String),
}
```

## Configuration Types

### DagConfig

```rust
pub struct DagConfig {
    pub max_iterations: Option<u32>,
    pub timeout_seconds: Option<u64>,
    pub enable_parallel_execution: bool,
    pub max_parallel_nodes: usize,
    pub enable_incremental_cache: bool,
    pub cache_snapshot_interval: u64,
    pub on_failure: OnFailure,
    pub retry_strategy: RetryStrategy,
    pub enable_partial_execution: bool,
    pub enable_debug_logging: bool,
    pub enable_metrics: bool,
    pub enable_tracing: bool,
    pub human_timeout_seconds: Option<u64>,
    pub human_timeout_action: HumanTimeoutAction,
}

pub enum OnFailure {
    Stop,      // Stop execution on first failure
    Pause,     // Pause for manual intervention
    Continue,  // Continue with other nodes
}

pub enum RetryStrategy {
    Immediate,
    ExponentialBackoff { base_delay_ms: u64, max_delay_ms: u64, factor: f32 },
    FixedDelay { delay_ms: u64 },
    LinearBackoff { initial_delay_ms: u64, increment_ms: u64 },
}

pub enum HumanTimeoutAction {
    Cancel,   // Cancel the operation
    Continue, // Continue without human input
    UseDefault(Value), // Use default value
}
```

## NodeAction Trait

```rust
#[async_trait]
pub trait NodeAction: Send + Sync {
    // Action name
    fn name(&self) -> String;
    
    // Execute the action
    async fn execute(
        &self,
        executor: &mut DagExecutor,
        node: &Node,
        cache: &Cache,
    ) -> Result<()>;
    
    // Get action schema
    fn schema(&self) -> Value;
    
    // As any for downcasting
    fn as_any(&self) -> &dyn Any;
}
```

## Execution Reports

```rust
pub struct DagExecutionReport {
    pub success: bool,
    pub nodes_executed: usize,
    pub nodes_failed: usize,
    pub nodes_skipped: usize,
    pub total_duration_ms: u64,
    pub node_outcomes: Vec<NodeExecutionOutcome>,
    pub error_message: Option<String>,
}

pub struct NodeExecutionOutcome {
    pub node_id: String,
    pub success: bool,
    pub duration_ms: u64,
    pub attempts: u32,
    pub outputs: HashMap<String, Value>,
    pub error: Option<String>,
    pub retry_messages: Vec<String>,
    pub final_error: Option<String>,
}

pub struct TaskExecutionReport {
    pub job_id: String,
    pub success: bool,
    pub tasks_completed: usize,
    pub tasks_failed: usize,
    pub total_duration_ms: u64,
    pub task_outcomes: Vec<TaskOutcome>,
}
```

## Macros

### register_action!

```rust
// Register an action with the executor
register_action!(executor, "action_name", action_function).await?;

// Expands to:
// - Creates NodeAction implementation
// - Registers with executor
```

### Procedural Macros (from dagger-macros crate)

```rust
// Define an action with metadata
#[action(
    description = "Description",
    timeout = 30,
    retries = 3
)]
async fn my_action(executor: &mut DagExecutor, node: &Node, cache: &Cache) -> Result<()>

// Define a task agent
#[task_agent(
    name = "agent_name",
    description = "Agent description"
)]
async fn my_agent(task: Task) -> Result<Value>

// Define a pub/sub agent
#[pubsub_agent(
    subscribe = "input_channel",
    publish = "output_channel",
    schema_in = json!({...}),
    schema_out = json!({...})
)]
async fn my_pubsub_agent(msg: Message) -> Result<()>
```

## Complete Example

```rust
use dagger::*;
use tokio::sync::RwLock;
use std::sync::Arc;
use std::collections::HashMap;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Initialize
    let registry = Arc::new(RwLock::new(HashMap::new()));
    let config = DagConfig {
        enable_parallel_execution: true,
        max_parallel_nodes: 4,
        ..Default::default()
    };
    let mut executor = DagExecutor::new(
        Some(config),
        registry,
        "sqlite::memory:"
    ).await?;
    
    // 2. Register actions
    register_action!(executor, "fetch", fetch_data).await?;
    register_action!(executor, "process", process_data).await?;
    
    // 3. Load workflow
    executor.load_yaml_file("workflow.yaml").await?;
    
    // 4. Setup cache
    let cache = Cache::new();
    insert_value(&cache, "inputs", "api_key", "secret123")?;
    
    // 5. Execute
    let (_tx, rx) = tokio::sync::oneshot::channel();
    let report = executor.execute_static_dag(
        "my_workflow",
        &cache,
        rx
    ).await?;
    
    // 6. Check results
    println!("Success: {}", report.success);
    println!("Nodes executed: {}", report.nodes_executed);
    
    // 7. Export visualization
    let dot = executor.serialize_tree_to_dot("my_workflow").await?;
    std::fs::write("workflow.dot", dot)?;
    
    Ok(())
}

async fn fetch_data(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    let api_key = get_global_input::<String>(cache, "inputs", "api_key")?;
    // Fetch data...
    insert_value(cache, &node.id, "data", vec![1, 2, 3])?;
    Ok(())
}

async fn process_data(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache
) -> Result<()> {
    let data = parse_input_from_name::<Vec<i32>>(
        cache,
        "input_data".to_string(),
        &node.inputs
    )?;
    let result = data.iter().sum::<i32>();
    insert_value(cache, &node.id, "result", result)?;
    Ok(())
}
```

## Notes

1. **String vs &str**: Many functions take `String` instead of `&str` (e.g., `parse_input_from_name`)
2. **Async APIs**: Most executor methods are async and require `.await`
3. **Send + Sync**: All traits require Send + Sync for thread safety
4. **Arc/RwLock**: Registry uses `Arc<RwLock<HashMap>>` for thread-safe sharing
5. **SQLite URLs**: Use `"sqlite::memory:"` for in-memory or `"sqlite:file.db"` for persistent storage