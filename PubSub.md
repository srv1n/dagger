# Dagger: A Rust Pub/Sub Multi-Agent System

**Dagger** is a Rust library that provides a publish-subscribe (pub/sub) architecture for building multi-agent systems. It enables developers to create agents that communicate asynchronously through channels, manage state with caching and persistence, and optionally coordinate work using a task management system. This documentation consolidates and refines content from multiple sources to offer a comprehensive, detailed, and user-friendly guide to the system’s components, execution model, and utilities.


## Table of Contents

1.  [Introduction](#introduction)
2.  [Core Concepts](#core-concepts)
    *   [Agents](#agents)
    *   [Channels](#channels)
    *   [Messages](#messages)
    *   [Executor](#executor)
3.  [Pub/Sub Execution Model](#pubsub-execution-model)
4.  [Defining Agents](#defining-agents)
5.  [Working with Channels and Messages](#working-with-channels-and-messages)
6.  [Caching and State Management](#caching-and-state-management)
7.  [Optional Task Management](#optional-task-management)
8.  [Message Queue Mechanics](#message-queue-mechanics)
9.  [Design Decisions](#design-decisions)
10. [Utilities and Helper Functions](#utilities-and-helper-functions)
11. [Configuration](#configuration)
12. [Error Handling](#error-handling)
13. [Examples](#examples-separate-section)

## 1. Introduction

Dagger provides a framework for building multi-agent systems where independent agents communicate by publishing and subscribing to named channels. This decoupled approach can lead to more modular, scalable, and maintainable systems. Key features include:

*   **Asynchronous Communication:** Agents operate concurrently and communicate without blocking, using Rust's `tokio` runtime and `async-broadcast` channels.
*   **Schema Validation:**  Agents define JSON schemas for their input and output messages, which are enforced by the executor to maintain data integrity.
*   **Persistent State:**  Execution state, including message queues, agent metadata, and an execution tree, is persisted using the `sled` embedded database, allowing for recovery and analysis.
*   **Execution Tracing:**  A directed graph (the execution tree) tracks message flow, enabling visualization and debugging (e.g., generating DOT graphs).
*   **Optional Task Management:**  A built-in task manager provides a higher-level abstraction for coordinating work among agents, including task assignment, tracking, and retry mechanisms.
*  **Dynamic Agent Registration:** Support agents to be added at runtime.

## 2. Core Concepts

### Agents

Agents are the fundamental units of computation. They implement the `PubSubAgent` trait:

```rust
#[async_trait]
pub trait PubSubAgent: Send + Sync + 'static {
    fn name(&self) -> String;
    fn subscriptions(&self) -> Vec<String>;
    fn description(&self) -> String;
    fn publications(&self) -> Vec<String>;
    fn input_schema(&self) -> Value;
    fn output_schema(&self) -> Value;
    async fn process_message(
        &self,
        node_id: &str,
        channel: &str,
        message: Message,
        executor: &mut PubSubExecutor,
        cache: Arc<Cache>,
    ) -> Result<()>;
    fn validate_input(&self, message: &Value) -> Result<()>;
    fn validate_output(&self, message: &Value) -> Result<()>;
}
```

*   `name()`:  A unique identifier for the agent.
*   `subscriptions()`:  Channels the agent receives messages from.
*   `publications()`: Channels the agent sends messages to.
* `description()`: String Description of the agent
*   `input_schema()`:  A JSON schema for validating incoming messages.
*   `output_schema()`: A JSON schema for validating outgoing messages.
*   `process_message()`:  The core logic for handling messages. It receives:
    *   `node_id`: A unique ID for this agent instance.
    *   `channel`: The channel the message arrived on.
    *   `message`:  The message itself.
    *   `executor`:  A mutable reference to the `PubSubExecutor` (for publishing new messages).
    *   `cache`: A shared cache for storing and retrieving data.
*    `validate_input(&self, payload: &::serde_json::Value) -> ::anyhow::Result<()>`
*    `validate_output(&self, payload: &::serde_json::Value) -> ::anyhow::Result<()>`

### Channels

Channels are named message queues that facilitate communication between agents.  They are dynamically created and managed by the `PubSubExecutor`.

### Messages

Messages are the data packets exchanged between agents:

```rust
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    pub timestamp: NaiveDateTime,
    pub source: String,
    pub channel: Option<String>,
    pub task_id: Option<String>,
    pub payload: Value,
}
```

*   `timestamp`: Message creation time.
*   `source`:  The `node_id` of the publishing agent.
*   `channel`: The target channel (set by the executor).
*   `task_id`:  An optional ID linking the message to a specific task (used with the `TaskManager`).
*   `payload`: The message data (a `serde_json::Value`).

### Executor

The `PubSubExecutor` is the central component that manages agents, channels, and message routing:

```rust
pub struct PubSubExecutor {
    pub agent_registry: Arc<RwLock<HashMap<String, AgentEntry>>>,
    pub agent_factories: Arc<RwLock<HashMap<String, AgentFactory>>>,
    pub channels: Arc<RwLock<HashMap<String, (Sender<Message>, Receiver<Message>)>>>,
    pub task_manager: TaskManager,
    pub config: PubSubConfig,
    pub sled_db: Db,
    pub execution_tree: Arc<RwLock<ExecutionTree>>,
    pub cache: Arc<Cache>,
    pub stopped: Arc<RwLock<bool>>,
    pub start_time: NaiveDateTime,
    pub current_agent_id: Option<String>,
    pub planned_tasks: Arc<RwLock<usize>>,
}
```

*   `agent_registry`:  Stores registered agents.
*   `agent_factories`: Stores agent factories for dynamic agent creation.
*   `channels`: Maps channel names to their senders and receivers (`async-broadcast`).
*   `task_manager`:  (Optional) Manages tasks.
*   `config`:  Configuration settings.
*   `sled_db`:  The embedded database for persistent state.
*   `execution_tree`:  A graph tracking message flow.
*   `cache`:  A shared, thread-safe cache.
*   `stopped`:  A flag to signal shutdown.
*   `start_time`:  Execution start time.
*    `current_agent_id`:  Current agent.
*    `planned_tasks`: Number of tasks planned.

## 3. Pub/Sub Execution Model

The execution flow is event-driven:

1.  **Initialization:**  The `PubSubExecutor` is created, agents are registered, and an initial message (optional) is published to a channel using `executor.execute()` with a `PubSubWorkflowSpec::EventDriven`.

2.  **Message Routing:**  The executor publishes the message to the specified channel. Subscribed agents receive the message through their associated `async_broadcast::Receiver`.

3.  **Agent Processing:** Each agent's `process_message()` method is invoked (in a separate Tokio task) to handle the incoming message.  The agent can:
    *   Validate the message payload against its input schema.
    *   Perform computations.
    *   Access and update the shared cache.
    *   Publish new messages to other channels (using `executor.publish()`).
    *   Interact with the optional `TaskManager`.

4.  **Iteration:** Steps 2 and 3 repeat until no new messages are generated, a timeout is reached, or the executor is explicitly stopped.

5. **Reporting:**
    * Real-time outcomes for individual agent invocations are available through an mpsc::Receiver<NodeExecutionOutcome>.
    * A final comprehensive PubSubExecutionReport is returned upon completion or cancellation.

## 4. Defining Agents

Agents can be defined by manually implementing the `PubSubAgent` trait or, more conveniently, by using the `#[pubsub_agent]` procedural macro:

```rust
#[pubsub_agent(
    name = "MyAgent",
    subscribe = "input_channel",
    publish = "output_channel",
    input_schema = r#"{"type": "object", "properties": {"data": {"type": "string"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"result": {"type": "string"}}}"#
)]
async fn my_agent(
    node_id: &str,
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: Arc<Cache>,
) -> Result<()> {
    // Agent logic here...
    let input_data = message.payload["data"].as_str().unwrap();
    let result = format!("Processed: {}", input_data);
    let output_message = Message::new(node_id.to_string(), json!({ "result": result }));
    executor.publish("output_channel", output_message, &cache, None).await?;

    Ok(())
}
```

The macro handles:

*   Generating the `PubSubAgent` trait implementation.
*   Creating a struct (e.g., `__MyAgentAgent`) to hold the agent's state.
*   Adding `new` method
*   Adding schema validation code within `process_message()`.
*   Updating the execution tree.
*  Adding `publish_message` method

Agents are registered with the executor:

```rust
executor.register_agent(Arc::new(__MyAgentAgent::new())).await?;

// Or, using the global registry:
GLOBAL_REGISTRY.register("MyAgent".to_string(), Arc::new(|| Arc::new(__MyAgentAgent::new()))).await?;
executor.register_from_global("MyAgent").await?;
```

## 5. Working with Channels and Messages

### Channels

*   **Dynamic Creation:** Channels are automatically created when an agent subscribes to or publishes to a previously non-existent channel.
*   **Listing Channels:** Use `executor.list_channels()` to get a list of active channels and their status (subscriber count, message count, etc.).
* **Channel Status:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelStatus {
    pub name: String,
    pub subscriber_count: usize,
    pub message_count: usize,
    pub created_at: String,
    pub is_active: bool,
}
```
*   **Shutting Down Channels:** Use `executor.shutdown_channel(channel_name, force)` to remove a channel.  The `force` parameter determines whether to close the channel even if there are active subscribers.

### Messages

*   **Creating Messages:** Use `Message::new(source, payload)` or `Message::with_task_id(source, task_id, payload)`.
*   **Publishing Messages:** Use `executor.publish(channel, message, cache, task)`.  The `task` argument is optional and used to integrate with the `TaskManager`.

## 6. Caching and State Management

Dagger provides a two-tiered caching mechanism:

*   **In-Memory Cache:** A `Cache` struct containing a `DashMap` provides fast, concurrent access to data shared between agents within a single execution.
*   **Persistent State:** The `sled` embedded database stores the cache contents, execution tree, and other metadata, enabling persistence across restarts.

```rust
// Cache struct (from lib.rs)
#[derive(Debug, Default)]
pub struct Cache {
    data: Arc<DashMap<String, DashMap<String, SerializableData>>>,
}

// Serializable data for storage
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SerializableData {
    pub value: Value,
}
```

Key functions:

*   `insert_value(&cache, node_id, key, value)`: Inserts a value into the cache.
*   `get_input(&cache, node_id, key)`: Retrieves a value from the cache.
*   `append_global_value(&cache, dag_name, key, value)`:  Appends a value to a list in a global namespace within the cache (useful for accumulating results).
*   `serialize_cache_to_json(&cache)` and `serialize_cache_to_prettyjson(&cache)`:  Serializes the cache to JSON.
*   `load_cache_from_json(json_data)`: Loads the cache from JSON.

The `PubSubExecutor` automatically saves and loads the state (including the cache) using `save_state()` and `load_state()`.

## 7. Optional Task Management

The `TaskManager` provides a structured way to manage units of work:

```rust
#[derive(Clone)]
pub struct TaskManager {
    tasks: Arc<RwLock<Vec<Task>>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Task {
    pub task_id: String,
    pub task_type: String,
    pub status: TaskStatus,
    pub attempts: u32,
    pub max_attempts: u32,
    pub data: Value,
    pub created_at: NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}
```

Key methods:

*   `add_task(task_type, data, max_attempts)`: Adds a new task.
*   `claim_task(task_type, agent_name)`:  Allows an agent to claim a pending task.
*   `complete_task(task_id, success)`: Marks a task as completed or failed.
*   `get_pending_tasks()`: Retrieves a list of pending tasks.
*   `is_complete(planned_tasks)`: Checks if all planned tasks are complete.
*    `update_task_status`
*    `create_or_update_task`
*    `get_task_by_id`
*    `get_tasks_by_type`
*     `get_tasks_by_status`

The `executor.publish()` method can optionally take a task, automatically creating or updating the task in the `TaskManager`.

## 8. Message Queue Mechanics

*   **Underlying Mechanism:**  Channels use `async_broadcast` for efficient MPMC message delivery.
*   **Capacity:**  The maximum number of messages a channel can hold is configurable (via `PubSubConfig`).
*   **Overflow Behavior:**  When a channel is full, the oldest message is dropped to make room for new messages. This behavior is controlled by setting `tx.set_overflow(true)` when the channel is created.
*   **Persistence:**  Messages are stored in `sled` as part of the overall state, enabling replay on restart.

## 9. Design Decisions

*   **Asynchronous and Concurrent:**  Utilizes Rust's `async`/`await` and `tokio` for non-blocking operations and concurrency.
*   **Decoupled Agents:** The pub/sub model promotes loose coupling between agents, making the system more modular and easier to maintain.
*   **Schema Validation:**  JSON schemas for message inputs and outputs help ensure data integrity.
*   **Persistent State:** `sled` is used for its simplicity, performance, and ability to be embedded within the application.
*   **Execution Tree:**  Provides a detailed record of message flow for debugging, analysis, and visualization.
* **Dynamic Channel Creation/Deletion** Channel creation and removal is done on the fly.

## 10. Utilities and Helper Functions

*   `generate_node_id(action_name)`: Generates a unique node ID (e.g., `"action_1678886400000"`).
*   `serialize_cache_to_json(cache)` / `serialize_cache_to_prettyjson(cache)`: Serializes the cache to JSON.
*   `load_cache_from_json(json_data)`: Loads the cache from JSON.
*   `parse_input_from_name(cache, input_name, inputs)`:  Retrieves a value from the cache based on a string reference (e.g., `"node1.output_value"`).
*   `serialize_tree_to_dot(executor, workflow_id, cache)`:  Generates a DOT graph representation of the execution tree.
*   `append_global_value(&cache, dag_name, key, value)`: Appends to a list within a global namespace in the cache.
*  `get_global_input(&cache, dag_name, key)`: Gets an element from the cache

## 11. Configuration

The `PubSubConfig` struct allows you to customize the executor's behavior:

```rust
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
```
* **RetryStrategy:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RetryStrategy {
    Exponential {
        initial_delay_secs: u64,
        max_delay_secs: u64,
        multiplier: f64,
    },
    Linear { delay_secs: u64 },
    Immediate,
}
```
* **HumanTimeoutAction:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HumanTimeoutAction {
    Autopilot,
    Pause,
}
```

## 12. Error Handling

The `PubSubError` enum represents various error conditions:

```rust
#[derive(Debug, thiserror::Error)]
pub enum PubSubError {
    #[error("Lock error: {0}")] LockError(String),
    #[error("Agent not found: {0}")] AgentNotFound(String),
    #[error("Channel not found: {0}")] ChannelNotFound(String),
    #[error("Serialization error: {0}")] SerializationError(#[from] serde_json::Error),
    #[error("Database error: {0}")] DatabaseError(#[from] sled::Error),
    #[error("Validation error: {0}")] ValidationError(String),
    #[error("Execution error: {0}")] ExecutionError(String),
    #[error("Cancelled")] Cancelled,
    #[error("Timeout exceeded")] TimeoutExceeded,
    #[error("No agents registered")] NoAgentsRegistered,
}
```

## 13. Examples (Separate Section)
Detailed examples demonstrating various use cases will be provided in a separate section or directory. This will keep the main README focused on the core concepts and architecture.

```

This revised README provides a comprehensive overview of Dagger's pub/sub system. It clarifies the key concepts, explains the design decisions, and highlights the available utilities, all while maintaining a user-focused perspective. The use of concise code snippets helps illustrate the API without overwhelming the reader with lengthy examples.
