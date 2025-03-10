# Dagger:TaskAgent | A Hierarchical, Agent-Based Task Management Framework

## Introduction

Dagger:TaskAgent is a Rust-based framework designed for building and managing complex, multi-agent workflows.  It draws inspiration from project management methodologies (specifically Agile, with concepts like stories, tasks, and subtasks) and aims to address the limitations of existing agent frameworks (like ReAct) when dealing with larger, more intricate problems.  Dagger focuses on efficient context management, state persistence, and flexible agent interaction.  The core idea is to break down a large objective into a hierarchy of manageable units of work, allowing agents to operate with the appropriate level of context and reducing the need to repeatedly pass large amounts of data to LLMs.

## Conversational Context & Design Rationale

The development of Dagger was driven by a series of design discussions and iterations.  The key points that shaped the framework are:

* **Limitations of ReAct:** The ReAct (Reason-Act-Observe) framework, while useful for simpler tasks, becomes inefficient for complex problems.  In ReAct, each decision point often requires sending the entire context (all scraped data, previous interactions, etc.) to an LLM.  This leads to high latency, increased costs, and potential context window limitations.

* **Agile-Inspired Hierarchy:** Dagger adopts a hierarchical structure similar to Agile project management:
    - **Objective:** The top-level goal (e.g., "Write a report on X").
    - **Stories:** Mid-level breakdowns of the objective (e.g., "Gather data on X", "Analyze X").
    - **Tasks:** Specific actions within a story (e.g., "Scrape web for X data").
    - **Subtasks:** Granular steps within a task (e.g., "Clean scraped data").

* **Distilled Context:** A crucial design principle is "distilled context."  As you move up the hierarchy (from subtasks to tasks to stories to the objective), the amount of *detailed* information decreases, while the *summarized* information increases.
    *   **Subtask Level:** Agents at this level have very specific operational knowledge (e.g., "scrape this webpage").
    *   **Task Level:** Agents have more context about the task's goal but don't need to know the minutiae of every subtask.
    *   **Story Level:** Agents deal with summarized information from all tasks within a story, allowing them to assess progress and make higher-level decisions.
    *   **Objective Level:** The "planner" agent operates with high-level summaries from all stories, focusing on overall progress and plan adjustments.

* **Stateless Agents and Explicit Context:** Agents are generally stateless.  When an agent is invoked, it receives only the necessary context for its operation.  This context can be the full history (like in ReAct) or a carefully curated subset, providing flexibility and efficiency.  The Task Manager plays a crucial role in managing this context.

* **Caching:** A robust caching system is integrated to avoid redundant work.  The cache is structured hierarchically, mirroring the task structure.  This allows for efficient retrieval of previously computed results and facilitates "distilled context" by storing summaries at different levels.

* **Flexibility and Extensibility:** Dagger is designed to be flexible.  The framework provides core components (Task Manager, Agent Registry, Cache), but allows users to define custom agents, retry strategies, timeout actions, and even stall handling behavior.  The use of traits (like `TaskAgent`) and macros (`#[pubsub_agent]`, `#[task_agent]`) promotes extensibility.

* **Persistence:**  Dagger leverages `sled`, a lightweight embedded database, to persist the state of jobs. This enables resuming jobs after interruptions, ensures durability, and provides a mechanism for long-running workflows.

* **Dynamic Dependency Creation:** Agents can request more information during execution by creating new tasks that become dependencies for the current task. This allows for adaptive workflows where agents can dynamically expand the task graph based on discovered needs, pausing their execution until the new dependencies are resolved.

## Key Components (overview)

### TaskManager
The TaskManager is the heart of the system, responsible for:
- Task tracking: Maintains a map of tasks by ID, job, assignee, status, and parent.
- Dependency resolution: Checks if a task's dependencies are complete before marking it as ready.
- State management: Updates task statuses (e.g., Pending, InProgress, Completed) and handles heartbeats to detect stalls.
- Persistence: Optionally saves state to Sled for recovery.

It uses DashMap for concurrent access, ensuring thread-safety in a multi-agent environment.

### TaskExecutor
The TaskExecutor drives task execution:
- Runs task loops: Polls for ready tasks and assigns them to agents.
- Handles cancellation: Supports job termination via a oneshot channel.
- Visualizes workflows: Generates DOT graphs for debugging and monitoring.

It integrates with the TaskManager and GlobalAgentRegistry to fetch agents and execute tasks.

### TaskAgent
The TaskAgent trait defines the interface for agents:
- Name and description: Identifies the agent and its purpose.
- Schemas: Specifies input/output JSON schemas for validation.
- Execution: Implements the agent's logic, interacting with the TaskManager and Cache.
- Agents are registered globally via the GlobalAgentRegistry and can be invoked dynamically.

### Cache
The Cache (backed by DashMap) stores task outputs and intermediate data:
- Key-value store: Maps task IDs to key-value pairs (e.g., task123:output → JSON value).
- Distillation: Stores summaries passed up the hierarchy.

Concurrency: Uses Arc for shared access across agents.

This reduces redundant computation and supports efficient context passing.

### GlobalAgentRegistry
The GlobalAgentRegistry maintains a thread-safe map of agent factories:
- Registration: Agents are registered with a name and factory function.
- Dynamic creation: Creates agent instances on demand.
- Listing: Provides a list of available agents for planning or debugging.

It uses tokio::sync::RwLock for safe concurrent access.


## Key Components (detailed)

### 1. `TaskAgent` Trait

The foundation for all agents.  It defines the core interface:

```rust
#[async_trait]
pub trait TaskAgent: Send + Sync + 'static  {
    fn name(&self) -> String;
    fn description(&self) -> String {
        "No description provided".to_string() // Default implementation
    }
    fn input_schema(&self) -> Value;
    fn output_schema(&self) -> Value;
    async fn execute(
        &self,
        task_id: &str,
        input: Value,
        task_manager: &TaskManager,
    ) -> Result<TaskOutput>;

    // Input and output validation helpers (using jsonschema)
    fn validate_input(&self, message: &Value) -> Result<()>;
    fn validate_output(&self, message: &Value) -> Result<()>;
}
```

*   **`name()`:**  A unique identifier for the agent.
*   **`description()`:** A human-readable explanation.
*   **`input_schema()` & `output_schema()`:**  JSON Schemas defining the expected input and output formats.  This is crucial for agent interoperability and validation.
*   **`execute()`:** The core logic of the agent.  It receives a `task_id`, input `Value`, and a reference to the `TaskManager`.
*  **`validate_input` & `validate_output`** Helpers to ensure input and output conforms to the schema

**Key Design Choices:**

*   **`Send + Sync + 'static`:**  These trait bounds ensure that agents can be safely shared across threads and have a static lifetime, important for asynchronous execution.
*   **JSON Schema:**  Using JSON Schema for input/output definition provides a standard, well-defined way to specify data structures, enabling validation and automatic documentation generation.
*   **`TaskManager` Access:**  Passing a reference to the `TaskManager` allows agents to interact with the task hierarchy, access the cache, and potentially create new tasks (though careful design is needed to avoid uncontrolled task creation).

**Dynamic Dependency Creation:**

Agents can now request more information during execution by creating new tasks and adding them as dependencies. Here's how an agent implementation might handle this:

```rust
#[task_agent(name = "ExampleAgent", 
    input_schema = r#"{"type": "object"}"#,
    output_schema = r#"{"type": "object"}"#)]
async fn example_agent(
    input: serde_json::Value,
    task_id: &str,
    job_id: &str,
    task_manager: &TaskManager,
) -> Result<serde_json::Value, String> {
    // Check if this is a retry after dependencies were added
    let task = task_manager.get_task_by_id(task_id)
        .ok_or_else(|| "Task not found".to_string())?;
    
    if !task.dependencies.is_empty() {
        // This is a retry after dependencies were resolved
        match task_manager.get_dependency_outputs(task_id) {
            Ok(outputs) => {
                // Use the dependency outputs to complete the task
                let combined_data = process_dependency_outputs(input, outputs);
                return Ok(combined_data);
            },
            Err(e) => return Err(format!("Failed to get dependency outputs: {}", e)),
        }
    }
    
    // First execution - check if we need more information
    if input.get("required_data").is_none() {
        // Create a new task to gather the required data
        let new_task_id = match task_manager.add_task_with_type(
            job_id.to_string(),
            "Gather additional data".to_string(),
            "DataGatherer".to_string(),
            vec![],
            serde_json::json!({"context": "needed for task"}),
            Some(task_id.to_string()),
            None,
            TaskType::Task,
            None,
            0,
            None,
            0,
        ) {
            Ok(id) => id,
            Err(e) => return Err(format!("Failed to create dependency task: {}", e)),
        };
        
        // Add the new task as a dependency
        if let Err(e) = task_manager.add_dependency(task_id, &new_task_id) {
            return Err(format!("Failed to add dependency: {}", e));
        }
        
        // Set the current task to Blocked
        if let Err(e) = task_manager.update_task_status(task_id, TaskStatus::Blocked) {
            return Err(format!("Failed to update task status: {}", e));
        }
        
        // Return a special error to indicate we need more information
        return Err("needs_more_info".to_string());
    }
    
    // Normal execution path with all required information
    Ok(serde_json::json!({"result": "Task completed successfully"}))
}

fn process_dependency_outputs(input: Value, outputs: Vec<Value>) -> Value {
    // Combine the original input with the dependency outputs
    // This is just an example - actual implementation would depend on your needs
    let mut result = input.as_object().unwrap().clone();
    for (i, output) in outputs.iter().enumerate() {
        result.insert(format!("dependency_{}", i), output.clone());
    }
    serde_json::Value::Object(result)
}
```

In this example:
1. The agent first checks if it's being executed after dependencies were added
2. If dependencies exist, it retrieves their outputs and uses them to complete the task
3. If this is the first execution and required data is missing, it:
   - Creates a new task to gather the needed information
   - Adds that task as a dependency
   - Sets its own status to Blocked
   - Returns a special error indicating it needs more information
4. The TaskExecutor will respect the Blocked status and not mark the task as Failed
5. When the dependency completes, the task will be set back to Pending and executed again

This pattern allows for adaptive workflows where agents can dynamically expand the task graph based on discovered needs during execution.

### 2. `TaskManager`

The central coordinator of the entire system.  It manages tasks, their statuses, dependencies, and relationships.

```rust
#[derive(Clone)]
pub struct TaskManager {
    pub tasks_by_id: Arc<DashMap<String, Task>>,
    pub tasks_by_job: Arc<DashMap<String, DashSet<String>>>,
    pub tasks_by_assignee: Arc<DashMap<String, DashSet<String>>>,
    pub tasks_by_status: Arc<DashMap<TaskStatus, DashSet<String>>>,
    pub tasks_by_parent: Arc<DashMap<String, DashSet<String>>>,
    pub ready_tasks: Arc<DashSet<String>>, // No longer used as a queue.
    pub heartbeat_interval: Duration,
    pub stall_action: StallAction,
    pub last_activity: Arc<DashMap<String, Instant>>,
    pub cache: Arc<Cache>,
    pub agent_registry: Arc<TaskAgentRegistry>,
    pub sled_db_path: Option<PathBuf>,
    pub jobs: Arc<DashMap<String, JobHandle>>,
}
```

*   **`tasks_by_id`, `tasks_by_job`, etc.:**  `DashMap`s provide concurrent, lock-free access to task data, indexed by various criteria.  This allows for efficient lookups and updates.
*   **`ready_tasks`**: This DashSet stores Ids of tasks that are eligible to run.
*   **`heartbeat_interval` & `stall_action`:**  Mechanisms for detecting and handling stalled jobs.
*   **`last_activity`:** Tracks when each job was last active (used for heartbeat checks).
*   **`cache`:**  A reference to the shared `Cache` instance.
*   **`agent_registry`:**  A reference to the `TaskAgentRegistry`, allowing the `TaskManager` to retrieve agent instances.
*   **`sled_db_path`:**  An optional path to a `sled` database for persistence.
*  **`jobs`**: Stores job handles for cancelling and monitoring.

**Key Methods for Dynamic Dependencies:**

```rust
impl TaskManager {
    // Add a new dependency to an existing task
    pub fn add_dependency(&self, task_id: &str, new_dep_id: &str) -> Result<()> {
        if let Some(mut task) = self.tasks_by_id.get_mut(task_id) {
            if !task.dependencies.contains(&new_dep_id.to_string()) {
                task.dependencies.push(new_dep_id.to_string());
            }
            Ok(())
        } else {
            Err(anyhow!("Task not found: {}", task_id))
        }
    }

    // Retrieve outputs from all dependencies of a task
    pub fn get_dependency_outputs(&self, task_id: &str) -> Result<Vec<Value>> {
        let task = self.tasks_by_id.get(task_id)
            .ok_or_else(|| anyhow!("Task not found: {}", task_id))?;
        let outputs = task.dependencies.iter().map(|dep_id| {
            let dep_task = self.tasks_by_id.get(dep_id)
                .ok_or_else(|| anyhow!("Dependency task not found: {}", dep_id))?;
            if dep_task.status != TaskStatus::Completed {
                Err(anyhow!("Dependency task {} not completed", dep_id))
            } else {
                Ok(dep_task.output.data.clone().unwrap_or_default())
            }
        }).collect::<Result<Vec<_>>>()?;
        Ok(outputs)
    }
}
```

**Key Design Choices:**

*   **DashMap:**  The use of `DashMap` is critical for performance and concurrency. It allows multiple agents and the `TaskManager` itself to access and modify task data without blocking.
*   **Multiple Indexes:** Indexing tasks by ID, job, assignee, status, and parent allows for efficient queries needed for different operations (e.g., finding all tasks for a specific job, finding all tasks assigned to a particular agent, etc.).
*   **Stall Handling:**  The `heartbeat_interval` and `stall_action` provide a configurable way to deal with jobs that become unresponsive.  The `StallAction` enum allows for different strategies (notify a planning agent, terminate the job, or execute a custom function).
* **Sled Integration:**  The use of sled allows for storing tasks and persist them on disk.
* **Dynamic Dependencies:** The ability to add dependencies to a task during execution enables adaptive workflows where agents can request more information as needed.

### 3. `Cache`

A hierarchical, key-value store for caching intermediate results.

```rust
#[derive(Debug, Default)]
pub struct Cache {
    pub data: Arc<DashMap<String, DashMap<String, Value>>>,
}

impl Cache {
    pub fn new() -> Self { /* ... */ }
    pub fn insert_value<T: Serialize>(&self, node_id: &str, key: &str, value: &T) -> Result<()> { /* ... */ }
    pub fn get_value(&self, node_id: &str, key: &str) -> Option<Value> { /* ... */ }
}
```

*   **`data`:**  A nested `DashMap`.  The outer `DashMap` is keyed by `node_id` (which corresponds to a task ID), and the inner `DashMap` is keyed by arbitrary strings (allowing for storing multiple values per task).
*   **`insert_value` & `get_value`:**  Methods for storing and retrieving values.

**Key Design Choices:**

*   **Hierarchical Structure:**  The cache mirrors the task hierarchy, making it easy to associate cached data with specific tasks.  This is essential for the "distilled context" approach.
*   **`node_id` as Key:** Using the task ID as the primary key allows for efficient retrieval of all cached data related to a particular task.
*   **`serde_json::Value`:**  Storing values as `serde_json::Value` provides flexibility to cache any serializable data.

### 4. `TaskExecutor`

Responsible for executing tasks within a job.

```rust
pub struct TaskExecutor {
    task_manager: Arc<TaskManager>,
    agent_registry: Arc<TaskAgentRegistry>,
    cache: Arc<Cache>,
    config: Arc<TaskConfiguration>,
    job_id: String,
    stopped: Arc<tokio::sync::RwLock<bool>>,
    start_time: NaiveDateTime,
    allowed_agents: Option<Vec<String>>,
}
```

*  **`task_manager`:** A reference to TaskManager instance.
*  **`agent_registry`:** Reference to a registry of registered agents.
*  **`cache`:** Shared cache between tasks.
*  **`config`:** Configuration settings for the executor.
*  **`job_id`:** ID of the job being executed.
*  **`stopped`:** Flag for stopping execution.
*  **`start_time`:** Timestamp when job started.
*  **`allowed_agents`:** Optional list of allowed agent names.

**Key Design Choices:**
*   **Job-Specific:**  A `TaskExecutor` is created for each job, ensuring isolation.
*   **Configuration:** The `TaskConfiguration` struct allows for setting parameters like maximum execution time, retry strategy, and human timeout behavior.
*   **`allowed_agents`**: This field gives you the opportunity to filter and run only specific types of agents.

**Handling Dynamic Dependencies:**

The TaskExecutor has been updated to properly handle tasks that create dynamic dependencies during execution. Here's the modified execution logic:

```rust
async fn execute_single_task(&self, task: Task) -> TaskOutcome {
    let task_id = task.task_id.clone();

    if let Err(e) = self.task_manager.claim_task(&task_id, task.agent.clone()) {
        return TaskOutcome {
            task_id,
            success: false,
            error: Some(e.to_string()),
        };
    }

    let agent = match self.agent_registry.get_agent(&task.agent) {
        Some(agent) => agent,
        None => {
            self.task_manager.update_task_status(&task_id, TaskStatus::Failed)?;
            return TaskOutcome {
                task_id,
                success: false,
                error: Some(format!("Agent {} not found", task.agent)),
            };
        }
    };

    match agent.execute(&task_id, task.input.clone(), &self.task_manager).await {
        Ok(output) => {
            // Check the task's current status rather than overriding it
            // This allows agents to set tasks to Blocked status
            let status = self.task_manager.get_task_by_id(&task_id)
                .map(|t| t.status)
                .unwrap_or(TaskStatus::Failed);
            
            TaskOutcome {
                task_id,
                success: matches!(status, TaskStatus::Completed),
                error: output.error,
            }
        },
        Err(e) => {
            self.task_manager.update_task_status(&task_id, TaskStatus::Failed)?;
            TaskOutcome {
                task_id,
                success: false,
                error: Some(e.to_string()),
            }
        }
    }
}
```

The key change is that the executor now respects the task status set by the agent rather than automatically setting it based on the `TaskOutput.success` field. This allows agents to set a task to `Blocked` status when they need more information, and the executor won't override it.

When a task's dependencies are completed, the `TaskManager`'s `check_dependencies` method will automatically transition the task from `Blocked` to `Pending`, making it eligible for execution again. The executor will then pick it up for another execution attempt, and the agent can retrieve the outputs from its dependencies to complete the task.

### 5. `Task`, `TaskStatus`, `TaskOutput`

These structs and enums represent the core data structures for tasks.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub job_id: String,
    pub task_id: String,
    pub parent_task_id: Option<String>,
    pub acceptance_criteria: Option<String>,
    pub description: String,
    pub status: TaskStatus,
    pub status_reason: Option<String>,
    pub agent: String,
    pub dependencies: Vec<String>,
    pub input: Value,
    pub output: TaskOutput,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub timeout: Option<Duration>,
    pub max_retries: u32,
    pub retry_count: u32,
    pub task_type: TaskType,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Blocked,
    Accepted, // Differentiates between work done and work accepted
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutput {
    pub success: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskType {
    Objective,
    Story,
    Task,
    Subtask,
}
```

**Key Design Choices:**

*   **Comprehensive Task Data:**  The `Task` struct contains all necessary information about a task, including its dependencies, status, input/output, and timestamps.
*   **`TaskStatus`:**  The `TaskStatus` enum provides a clear set of states for tracking task progress.  The distinction between `Completed` and `Accepted` is important for scenarios where a task might be technically finished but not meet the overall objective's requirements.
*   **`TaskOutput`:**  A standardized way to represent the result of a task execution, including success/failure status, data, and potential errors.
*   **`TaskType`**: An enum to represent the task type in the task hierarchy.

### 6. `TaskAgentRegistry` and `GlobalAgentRegistry`

Manages the registration and retrieval of agents.

```rust
#[derive(Clone)]
pub struct TaskAgentRegistry {
    agents: Arc<DashMap<String, Box<dyn TaskAgent>>>,
}

#[derive(Clone)]
pub struct GlobalAgentRegistry {
    agents: Arc<RwLock<HashMap<String, Arc<dyn Fn() -> Arc<dyn TaskAgent> + Send + Sync>>>>,
}

impl TaskAgentRegistry { /* ... */ }
impl GlobalAgentRegistry { /* ... */ }
```

* **`TaskAgentRegistry`:**
    *  Uses `DashMap` to store registered agents and make them accessible for concurrent retrieval.
*   **`GlobalAgentRegistry`:** Stores agent factories for dynamic agent creation.

**Key Design Choices:**

*   **Two Registries:** `TaskAgentRegistry` stores agent instances, while `GlobalAgentRegistry` stores agent factories (functions that create agents). This distinction is important for scenarios where you might want to create new agent instances on demand (e.g., for each task) rather than using singleton agents.
*   **`linkme` Crate:**  The `#[linkme::distributed_slice]` macro (used with `TASK_AGENTS`) provides a way to automatically collect agent registrations across different modules.

### 7. Macros: `#[pubsub_agent]`, `#[task_agent]`, `#[task_workflow]`, `#[task_builder]`

These macros simplify the process of defining agents and workflows.

**`#[pubsub_agent]`:**

*   Generates boilerplate code for creating a `PubSubAgent`.
*   Allows specifying agent name, description, subscriptions, publications, input schema, and output schema.
*   Handles input/output validation using the provided JSON schemas.

**`#[task_agent]`:**

*   Generates code for creating a `TaskAgent` from an async function.
*   Automatically handles input and output validation based on the provided schemas.
*   Provides a consistent interface for task execution and integrates with the `TaskManager`.
*   Registers agent to the global registry

**`#[task_workflow]`:**
* Create new tasks using the `TaskBuilder`.
*   Simplifies workflow creation and execution.
*   Provides methods for creating executors, executing tasks, and visualizing the workflow.

**`#[task_builder]`:**
*  Adds builder pattern for `Task` creation

**Key Design Choices:**

*   **Code Generation:**  Macros significantly reduce boilerplate code, making it easier to define new agents and workflows.
*   **Schema Validation:**  The macros automatically incorporate schema validation, ensuring that agents receive valid input and produce valid output.
*   **Simplified Workflow Definition:**  The `#[task_workflow]` macro provides a high-level way to define and manage complete workflows.

## Getting Started

1.  **Dependencies:** Ensure you have Rust and Cargo installed.  Add Dagger to your `Cargo.toml`:

    ```toml
    [dependencies]
    dagger = { git = "your_dagger_repo_url" } # Replace with the actual URL
    tokio = { version = "1", features = ["full"] }
    anyhow = "1.0"
    serde = { version = "1.0", features = ["derive"] }
    serde_json = "1.0"
    dashmap = "5"
    async-trait = "0.1"
    chrono = "0.4"
    thiserror = "1.0"
    tracing = "0.1"
    tracing-subscriber = "0.3"
    jsonschema = "0.16"
    sled = "0.34"
    bincode = "1.3"
    linkme = "0.3"
    cuid2 = "0.3"
    ```

2.  **Define an Agent:**

    ```rust
    use dagger::taskagent::{TaskAgent, TaskManager, TaskOutput};
    use serde_json::Value;
    use anyhow::Result;
    use async_trait::async_trait;
    
    #[dagger::task_agent(
        name = "WebScraper",
        description = "Scrapes a webpage given a URL",
        input_schema = r#"{"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]}"#,
        output_schema = r#"{"type": "object", "properties": {"content": {"type": "string"}}, "required": ["content"]}"#
    )]
    async fn web_scraper(
        input: Value,
        task_id: &str,
        job_id: &str,
        task_manager: &TaskManager,
    ) -> Result<Value, String> {
        let url = input["url"].as_str().ok_or("Missing URL")?;
        // Implement your scraping logic here...
        let content = format!("Scraped content from {}", url); // Placeholder
        Ok(serde_json::json!({ "content": content }))
    }
    ```

3.  **Create a Task Manager and Register Agents:**

    ```rust
    use dagger::taskagent::{TaskManager, Cache, TaskAgentRegistry, TaskConfiguration, StallAction};
    use std::time::Duration;
    use std::path::PathBuf;

    #[tokio::main]
    async fn main() -> Result<()> {
        // Initialize tracing
        tracing_subscriber::fmt::init();
    
        let cache = Cache::new();
        let agent_registry = TaskAgentRegistry::new();
        let config = TaskConfiguration {
            max_execution_time: Some(Duration::from_secs(3600)),
            retry_strategy: dagger::taskagent::RetryStrategy::FixedRetry(3),
            human_timeout_action: dagger::taskagent::HumanTimeoutAction::TimeoutAfter(Duration::from_secs(30)),
            sled_db_path: Some(PathBuf::from("./my_sled_db")), // Optional: for persistence
        };
        
        let task_manager = TaskManager::new(
            Duration::from_secs(60), // Heartbeat interval
            StallAction::TerminateJob, // Stall action
            cache,
            agent_registry,
            config.sled_db_path.clone(),
        );

       // Register your agents (using generated module from the macro)
        task_manager.agent_registry.register("WebScraper", Box::new(web_scraper::WebScraper_Agent::new()))?;


        Ok(())
    }
    ```

4.  **Create and Execute a Job:**

    ```rust
     // Create an objective
        let job_id = "my_job".to_string();
        let objective_id = task_manager.add_objective(
            job_id.clone(),
            "Get the content of example.com".to_string(),
            "WebScraper".to_string(),
            serde_json::json!({ "url": "https://example.com" }),
        )?;

        // Start the job
        let allowed_agents = Some(vec!["WebScraper".to_string()]);
        let job_handle = task_manager.start_job(job_id, vec![], allowed_agents, Some(config))?;

        // Optionally, wait for the job to complete
        loop {
            match job_handle.get_status().await {
                dagger::taskagent::JobStatus::Running => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                dagger::taskagent::JobStatus::Completed(success) => {
                    println!("Job completed. Success: {}", success);
                    break;
                }
                dagger::taskagent::JobStatus::Cancelled => {
                    println!("Job cancelled.");
                    break;
                }
            }
        }

        // Get results (you'll likely want to iterate through tasks and retrieve from the cache)
        if let Some(objective_task) = task_manager.get_task_by_id(&objective_id) {
            if let Some(output) = objective_task.output.data {
                println!("Objective output: {:?}", output);
            }
        }


        // Generate a DOT graph of the job
        let dot_graph = task_manager.generate_detailed_dot_graph(&"my_job", &task_manager.cache);
        println!("DOT Graph:\n{}", dot_graph);
        std::fs::write("task_graph.dot", dot_graph).expect("Unable to write DOT graph to file");
    
        Ok(())
    }
    ```

5.  **Creating an Agent with Dynamic Dependencies:**

    ```rust
    use dagger::taskagent::{TaskAgent, TaskManager, TaskOutput, TaskStatus, TaskType};
    use serde_json::Value;
    use anyhow::Result;
    use async_trait::async_trait;
    
    #[dagger::task_agent(
        name = "AnalysisAgent",
        description = "Analyzes data and requests more information if needed",
        input_schema = r#"{"type": "object", "properties": {"data": {"type": "string"}}}"#,
        output_schema = r#"{"type": "object", "properties": {"analysis": {"type": "string"}}}"#
    )]
    async fn analysis_agent(
        input: Value,
        task_id: &str,
        job_id: &str,
        task_manager: &TaskManager,
    ) -> Result<Value, String> {
        // Check if this is a retry after dependencies were added
        let task = task_manager.get_task_by_id(task_id)
            .ok_or("Task not found")?;
        
        if !task.dependencies.is_empty() {
            // This is a retry after dependencies were resolved
            let dependency_outputs = task_manager.get_dependency_outputs(task_id)
                .map_err(|e| format!("Failed to get dependency outputs: {}", e))?;
            
            // Process the dependency outputs
            let mut additional_data = Vec::new();
            for output in dependency_outputs {
                if let Some(data) = output.get("content") {
                    additional_data.push(data.as_str().unwrap_or_default());
                }
            }
            
            // Complete the analysis with the additional data
            let analysis = format!(
                "Analysis of {} with additional data: {}",
                input.get("data").and_then(|v| v.as_str()).unwrap_or("unknown"),
                additional_data.join(", ")
            );
            
            return Ok(serde_json::json!({ "analysis": analysis }));
        }
        
        // First execution - check if we need more information
        if !input.get("data").and_then(|v| v.as_str()).unwrap_or_default().contains("complete") {
            // Create a new task to gather additional data
            let new_task_id = task_manager.add_task_with_type(
                job_id.to_string(),
                "Gather additional data".to_string(),
                "WebScraper".to_string(),
                vec![],
                serde_json::json!({ "url": "https://example.org/additional-data" }),
                Some(task_id.to_string()),
                None,
                TaskType::Task,
                None,
                0,
                None,
                0,
            ).map_err(|e| format!("Failed to create dependency task: {}", e))?;
            
            // Add the new task as a dependency
            task_manager.add_dependency(task_id, &new_task_id)
                .map_err(|e| format!("Failed to add dependency: {}", e))?;
            
            // Set the current task to Blocked
            task_manager.update_task_status(task_id, TaskStatus::Blocked)
                .map_err(|e| format!("Failed to update task status: {}", e))?;
            
            // Return a special error to indicate we need more information
            return Err("needs_more_info".to_string());
        }
        
        // Normal execution path with all required information
        let analysis = format!(
            "Analysis of {}",
            input.get("data").and_then(|v| v.as_str()).unwrap_or("unknown")
        );
        
        Ok(serde_json::json!({ "analysis": analysis }))
    }
    ```

    In this example:
    
    1. The `AnalysisAgent` checks if the input data is "complete"
    2. If not, it creates a new `WebScraper` task to gather additional data
    3. It adds this task as a dependency and sets itself to `Blocked`
    4. When the dependency completes, the agent is re-executed
    5. It retrieves the outputs from its dependencies and uses them to complete the analysis

    This demonstrates how agents can dynamically expand the task graph based on their needs during execution.

## Error Handling

Dagger defines a custom error type (`TaskError`) using the `thiserror` crate.  This provides a structured way to handle various error conditions, including agent-specific errors, task management errors, registry errors, and general internal errors.

```rust
#[derive(Error, Debug)]
pub enum TaskError {
    // ... (all the error variants) ...
}
```

The `anyhow` crate is also used for general error handling and propagation.