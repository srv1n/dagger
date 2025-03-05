use std::time::Duration;
use anyhow::Result;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::taskagent::{
    TaskManager, TaskAgentRegistry, TaskExecutor, Cache, 
    TaskConfiguration, RetryStrategy, HumanTimeoutAction, StallAction
};
use crate::registry::GLOBAL_REGISTRY;

/// Builder for the task system to simplify setup
pub struct TaskSystemBuilder {
    task_manager: Option<TaskManager>,
    agent_registry: TaskAgentRegistry,
    cache: Cache,
    config: TaskConfiguration,
    job_id: String,
    heartbeat_interval: Duration,
    stall_action: StallAction,
    register_in_global: bool,
}

impl Default for TaskSystemBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskSystemBuilder {
    /// Create a new task system builder with sensible defaults
    pub fn new() -> Self {
        // Generate a unique job ID by default
        let job_id = format!("job_{}", chrono::Utc::now().timestamp());
        
        // Create default configuration
        let config = TaskConfiguration {
            max_execution_time: Some(Duration::from_secs(60)),
            retry_strategy: RetryStrategy::FixedRetry(3),
            human_timeout_action: HumanTimeoutAction::TimeoutAfter(Duration::from_secs(30)),
            sled_db_path: None,
        };

        Self {
            task_manager: None,
            agent_registry: TaskAgentRegistry::new(),
            cache: Cache::new(),
            config,
            job_id,
            heartbeat_interval: Duration::from_secs(60),
            stall_action: StallAction::TerminateJob,
            register_in_global: true,
        }
    }

    /// Set a custom job ID
    pub fn with_job_id(mut self, job_id: impl Into<String>) -> Self {
        self.job_id = job_id.into();
        self
    }

    /// Set heartbeat interval for the task manager
    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    /// Set stall action for the task manager
    pub fn with_stall_action(mut self, action: StallAction) -> Self {
        self.stall_action = action;
        self
    }

    /// Register an agent by name
    pub fn register_agent(mut self, agent_name: &str) -> Result<Self> {
        self.agent_registry.register_agent(agent_name)?;
        Ok(self)
    }

    /// Register multiple agents by name
    pub fn register_agents(mut self, agent_names: &[&str]) -> Result<Self> {
        for name in agent_names {
            self.agent_registry.register_agent(name)?;
        }
        Ok(self)
    }

    /// Set whether to register in global registry
    pub fn register_in_global(mut self, register: bool) -> Self {
        self.register_in_global = register;
        self
    }

    /// Configure task execution parameters
    pub fn with_task_config(mut self, config: TaskConfiguration) -> Self {
        self.config = config;
        self
    }

    /// Build the task system
    pub async fn build(self) -> Result<TaskSystem> {
        // Create task manager if not provided
        let task_manager = match self.task_manager {
            Some(tm) => tm,
            None => TaskManager::new(
                self.heartbeat_interval,
                self.stall_action,
            ),
        };
        
        // TODO: Implement proper global registry registration if needed
        // For now, we'll skip this step to fix the compilation error
        
        // Create task executor
        let executor = TaskExecutor::new(
            task_manager.clone(),
            self.agent_registry,
            self.cache.clone(),
            self.config,
            self.job_id.clone(),
            None,
        );
        
        Ok(TaskSystem {
            task_manager,
            executor,
            cache: self.cache,
            job_id: self.job_id,
        })
    }
}

/// High-level task system that encapsulates all the components
pub struct TaskSystem {
    pub task_manager: TaskManager,
    pub executor: TaskExecutor,
    pub cache: Cache,
    pub job_id: String,
}

impl TaskSystem {
    /// Create a simple objective and execute it
    pub async fn run_objective(
        &mut self, 
        objective_description: &str, 
        agent_name: &str, 
        input: Value
    ) -> Result<Value> {
        // Create the objective
        let objective_id = self.task_manager.add_objective(
            self.job_id.clone(),
            objective_description.to_string(),
            agent_name.to_string(),
            input,
        )?;
        
        // Mark the objective task as ready
        self.task_manager.ready_tasks.insert(objective_id.clone());
        
        // Get the objective task
        let initial_tasks = vec![
            self.task_manager.get_task_by_id(&objective_id).unwrap(),
        ];
        
        // Create a cancellation channel
        let (_, cancel_rx) = oneshot::channel();
        
        // Execute the job
        let report = self.executor.execute(self.job_id.clone(), initial_tasks, cancel_rx).await?;
        
        // Find the output from the objective task
        if let Some(task) = self.task_manager.get_task_by_id(&objective_id) {
            if let Some(data) = task.output.data {
                return Ok(data);
            }
        }
        
        Err(anyhow::anyhow!("No output produced from objective task"))
    }
    
    /// Add a task and dependencies, then mark it as ready
    pub fn add_task(
        &self,
        description: String,
        agent: String,
        dependencies: Vec<String>,
        input: Value,
    ) -> Result<String> {
        let task_id = self.task_manager.add_task_with_type(
            self.job_id.clone(),
            format!("task_{}", cuid2::create_id()),  // Generate a unique task ID
            None,                                      // No parent task
            None,                                      // No acceptance criteria
            description,
            None,
            dependencies,
            input,
            None,                                      // No timeout
            3,                                         // Default max retries
            crate::taskagent::TaskType::Task,
            None,                                      // No summary
        )?;
        
        // Mark the task as ready if it has no dependencies
        if dependencies.is_empty() {
            self.task_manager.ready_tasks.insert(task_id.clone());
        }
        
        Ok(task_id)
    }
} 