use crate::registry::GLOBAL_REGISTRY;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bincode;
use chrono::{NaiveDateTime, Utc};
use dashmap::{DashMap, DashSet};
use linkme;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
// Removed sled import - persistence is now handled by newer task-core system
use std::fmt::Debug;
use std::ops::Deref;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use thiserror;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, trace, warn};

use crate::taskagent::errors;

// Reused Components from Existing Library
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutput {
    pub success: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}

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
    Accepted,
    Rejected,
}

// Cache (Reused from Lines 3673-3826)
#[derive(Debug, Default)]
pub struct Cache {
    pub data: Arc<DashMap<String, DashMap<String, Value>>>,
}

impl Cache {
    pub fn new() -> Self {
        Cache {
            data: Arc::new(DashMap::new()),
        }
    }

    pub fn insert_value<T: Serialize>(&self, node_id: &str, key: &str, value: &T) -> Result<()> {
        let value = serde_json::to_value(value)?;
        let map = self
            .data
            .entry(node_id.to_string())
            .or_insert_with(DashMap::new);
        map.insert(key.to_string(), value);
        Ok(())
    }

    pub fn get_value(&self, node_id: &str, key: &str) -> Option<Value> {
        self.data
            .get(node_id)
            .and_then(|map| map.get(key).map(|v| v.clone()))
    }
}

impl Clone for Cache {
    fn clone(&self) -> Self {
        Cache {
            data: self.data.clone(),
        }
    }
}

#[async_trait]
pub trait TaskAgent: Send + Sync + 'static {
    /// Returns the agent's unique name.
    fn name(&self) -> String;

    /// Returns a human-readable description of the agent's purpose.
    fn description(&self) -> String {
        "No description provided".to_string() // Default implementation
    }

    /// Returns the JSON schema describing input parameters and output format.
    /// Should include "parameters" and "returns" fields.
    fn input_schema(&self) -> Value;
    fn output_schema(&self) -> Value;

    /// Executes the task with the given task ID and input.
    ///
    /// # Arguments
    /// - `task_id`: The unique identifier of the task.
    /// - `input`: The input data as a JSON Value.
    /// - `task_manager`: A reference to the task manager for accessing cache and other resources.
    ///
    /// # Returns
    /// A `TaskOutput` indicating success or failure.
    async fn execute(
        &self,
        task_id: &str,
        input: Value,
        task_manager: &TaskManager,
    ) -> Result<TaskOutput>;

    fn validate_input(&self, message: &Value) -> Result<()> {
        let schema = self.input_schema();
        let compiled_schema = jsonschema::validator_for(&schema)
            .map_err(|e| anyhow::anyhow!("Failed to compile input schema: {}", e))?;
        if let Err(errors) = compiled_schema.validate(message) {
            warn!(
                "Input validation failed for agent {}: {}",
                self.name(),
                errors
            );
            return Err(anyhow::anyhow!("Invalid input: {}", errors));
        }
        Ok(())
    }
    fn validate_output(&self, message: &Value) -> Result<()> {
        let schema = self.output_schema();
        let compiled_schema = jsonschema::validator_for(&schema)
            .map_err(|e| anyhow::anyhow!("Failed to compile output schema: {}", e))?;
        if let Err(errors) = compiled_schema.validate(message) {
            warn!(
                "Output validation failed for agent {}: {}",
                self.name(),
                errors
            );
            return Err(anyhow::anyhow!("Invalid output: {}", errors));
        }
        Ok(())
    }
}

/// TaskContext provides all the information an agent needs
pub struct TaskContext {
    pub task_id: String,
    pub job_id: String,
    pub input: Value,
    pub parent_task_id: Option<String>,
    pub dependencies: Vec<String>,
    pub task_manager: Arc<TaskManager>,
}

impl TaskContext {
    /// Create a new TaskContext
    pub fn new(
        task_id: String,
        job_id: String,
        input: Value,
        parent_task_id: Option<String>,
        dependencies: Vec<String>,
        task_manager: Arc<TaskManager>,
    ) -> Self {
        Self {
            task_id,
            job_id,
            input,
            parent_task_id,
            dependencies,
            task_manager,
        }
    }

    /// Get a value from the cache
    pub fn get_cache_value(&self, key: &str) -> Option<Value> {
        self.task_manager.cache.get_value(&self.task_id, key)
    }

    /// Store a value in the cache
    pub fn store_cache_value<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.task_manager
            .cache
            .insert_value(&self.task_id, key, value)
    }

    /// Get parent task information
    pub fn get_parent_task(&self) -> Option<Task> {
        self.parent_task_id
            .as_ref()
            .and_then(|id| self.task_manager.get_task_by_id(id))
    }

    /// Get dependency task information
    pub fn get_dependency_tasks(&self) -> Vec<Task> {
        self.dependencies
            .iter()
            .filter_map(|id| self.task_manager.get_task_by_id(id))
            .collect()
    }

    /// Get the objective for this job
    pub fn get_objective(&self) -> Option<Task> {
        self.task_manager.get_objective(&self.job_id)
    }
}

// TaskExecutor
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

impl TaskExecutor {
    pub fn new(
        task_manager: Arc<TaskManager>,
        agent_registry: Arc<TaskAgentRegistry>,
        cache: Arc<Cache>,
        config: TaskConfiguration,
        job_id: String,
        allowed_agents: Option<Vec<String>>,
    ) -> Self {
        Self {
            task_manager,
            agent_registry,
            cache,
            config: Arc::new(config),
            job_id,
            stopped: Arc::new(tokio::sync::RwLock::new(false)),
            start_time: chrono::Utc::now().naive_utc(),
            allowed_agents,
        }
    }

    pub async fn execute(
        &mut self,
        job_id: String,
        initial_tasks: Vec<Task>,
        cancel_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<TaskExecutionReport> {
        println!("Executing job: {}", job_id);

        // Add initial tasks if any were provided
        for task in initial_tasks {
            self.task_manager.add_task(
                task.job_id.clone(),
                task.task_id.clone(),
                task.parent_task_id.clone(),
                task.acceptance_criteria.clone(),
                task.description.clone(),
                task.agent.clone(),
                task.dependencies.clone(),
                task.input.clone(),
                task.timeout,
                task.max_retries,
                task.task_type,
                task.summary.clone(),
                task.created_at,
                task.updated_at,
                task.retry_count,
            )?;
        }

        // Create a channel for task outcomes
        let (outcome_tx, mut outcome_rx) = mpsc::channel(100);
        let self_clone = self.clone();
        tokio::spawn(async move {
            self_clone
                .run_task_loop(job_id.clone(), outcome_tx, cancel_rx)
                .await;
        });
        let mut outcomes = Vec::new();
        while let Some(outcome) = outcome_rx.recv().await {
            outcomes.push(outcome.clone());
            info!("Received outcome for task: {}", outcome.task_id);
        }
        let overall_success = outcomes.iter().all(|o| o.success);
        Ok(TaskExecutionReport {
            outcomes,
            overall_success,
        })
    }

    async fn run_task_loop(
        &self,
        job_id: String,
        outcome_tx: mpsc::Sender<TaskOutcome>,
        mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
    ) {
        let mut interval = tokio::time::interval(Duration::from_millis(2000));
        loop {
            tokio::select! {
                _ = &mut cancel_rx => {
                    info!("Job {} cancelled", job_id);
                    break;
                }
                _ = interval.tick() => {
                    info!("Checking for ready tasks in job {}", job_id);
                    if *self.stopped.read().await { break; }
                    if let Some(max_time) = self.config.max_execution_time {
                        let elapsed = Utc::now().naive_utc() - self.start_time;
                        if elapsed > chrono::Duration::from_std(max_time).unwrap() {
                            warn!("Job {} exceeded maximum execution time", job_id);
                            *self.stopped.write().await = true;
                            break;
                        }
                    }
                    let ready_tasks = self.task_manager.get_ready_tasks_for_job(&job_id);
                    println!("🔄 Found {} ready tasks for job {}", ready_tasks.len(), job_id);
                    for task in &ready_tasks {
                        println!("🔄 Ready task: {} (agent: {})", task.task_id, task.agent);
                    }
                    if ready_tasks.is_empty() && self.task_manager.tasks_by_job.get(&job_id).map_or(false, |tasks| {
                        tasks.iter().all(|t| {
                            self.task_manager.tasks_by_id.get(t.key()).map_or(false, |task| {
                                matches!(task.status, TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Rejected)
                            })
                        })
                    }) {
                        info!("All tasks for job {} completed", job_id);
                        break;
                    }
                    for task in ready_tasks {
                        println!("▶️ Executing task {} with agent {}", task.task_id, task.agent);
                        let outcome = self.execute_single_task(task).await;
                        if outcome_tx.send(outcome).await.is_err() {
                            error!("Failed to send task outcome");
                            break;
                        }
                    }
                }
            }
        }
        drop(outcome_tx);
    }

    async fn execute_single_task(&self, task: Task) -> TaskOutcome {
        let task_id = task.task_id.clone();

        // Add logging for task execution start
        info!(
            "TaskExecutor: Starting execution of task {} (agent: {})",
            task_id, task.agent
        );

        // Claim the task
        if let Err(e) = self.task_manager.claim_task(&task_id, task.agent.clone()) {
            error!("Failed to claim task {}: {}", task_id, e);
            return TaskOutcome {
                task_id,
                success: false,
                error: Some(format!("Failed to claim task: {}", e)),
            };
        }

        // Log successful claim
        info!(
            "TaskExecutor: Successfully claimed task {} for agent {}",
            task_id, task.agent
        );

        // Get the agent
        let agent = match self.agent_registry.get_agent(&task.agent) {
            Some(agent) => agent,
            None => {
                error!("Agent {} not found for task {}", task.agent, task_id);
                if let Err(e) = self
                    .task_manager
                    .update_task_status(&task_id, TaskStatus::Failed)
                {
                    error!("Failed to update task status: {}", e);
                }
                return TaskOutcome {
                    task_id,
                    success: false,
                    error: Some(format!("Agent {} not found", task.agent)),
                };
            }
        };

        // Log agent found
        info!(
            "TaskExecutor: Found agent {} for task {}",
            task.agent, task_id
        );

        // Execute the task with the agent
        info!(
            "TaskExecutor: Executing task {} with agent {}",
            task_id, task.agent
        );
        match agent
            .execute(&task_id, task.input.clone(), &self.task_manager)
            .await
        {
            Ok(output) => {
                // Check the task's current status rather than overriding it
                let status = self
                    .task_manager
                    .get_task_by_id(&task_id)
                    .map(|t| t.status)
                    .unwrap_or(TaskStatus::Failed);
                TaskOutcome {
                    task_id,
                    success: matches!(status, TaskStatus::Completed),
                    error: output.error,
                }
            }
            Err(e) => {
                // Log execution error
                error!("TaskExecutor: Error executing task {}: {}", task_id, e);

                // Update task status in task manager
                if let Err(update_err) = self
                    .task_manager
                    .update_task_status(&task_id, TaskStatus::Failed)
                {
                    error!("Failed to update task status: {}", update_err);
                }

                // Return outcome
                TaskOutcome {
                    task_id,
                    success: false,
                    error: Some(e.to_string()),
                }
            }
        }
    }

    pub fn get_dot_graph(&self, job_id: Option<&str>) -> String {
        let job_id = job_id.unwrap_or(&self.job_id);
        self.task_manager.generate_dot_graph(job_id)
    }

    /// Generates a detailed DOT graph visualization that includes both task structure and cache data
    pub fn generate_detailed_dot_graph(&self) -> String {
        self.task_manager
            .generate_detailed_dot_graph(&self.job_id, &self.cache)
    }

    /// Generates a detailed DOT graph visualization for a specific job ID
    pub fn generate_detailed_dot_graph_for_job(&self, job_id: &str) -> String {
        self.task_manager
            .generate_detailed_dot_graph(job_id, &self.cache)
    }

    pub fn clone(&self) -> Self {
        Self {
            task_manager: Arc::clone(&self.task_manager),
            agent_registry: Arc::clone(&self.agent_registry),
            cache: Arc::clone(&self.cache),
            config: Arc::clone(&self.config),
            job_id: self.job_id.clone(),
            stopped: Arc::clone(&self.stopped),
            start_time: self.start_time,
            allowed_agents: self.allowed_agents.clone(),
        }
    }

    /// Persists the current state of the job (no-op - use newer task-core system for persistence)
    pub fn persist_state(&self) -> Result<()> {
        warn!("persist_state is deprecated - use the newer task-core system for persistence");
        Ok(())
    }

    /// Rehydrates the state of the job (no-op - use newer task-core system for persistence)
    pub fn rehydrate_state(&self) -> Result<()> {
        warn!("rehydrate_state is deprecated - use the newer task-core system for persistence");
        Ok(())
    }

    /// Resumes a previously paused job (deprecated - use newer task-core system)
    pub async fn resume_job(
        &mut self,
        job_id: String,
        cancel_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<TaskExecutionReport> {
        warn!("resume_job is deprecated - use the newer task-core system for job resumption");
        
        // Update the job ID
        self.job_id = job_id.clone();

        // Skip sled rehydration - deprecated functionality

        // Reset the stopped flag
        *self.stopped.write().await = false;

        // Just reuse the execute method's implementation
        // This is simpler and avoids the issues with task handles
        let (outcome_tx, mut outcome_rx) = mpsc::channel::<TaskOutcome>(100);

        // Spawn the task loop
        let executor_clone = self.clone();
        tokio::spawn(async move {
            executor_clone
                .run_task_loop(job_id, outcome_tx, cancel_rx)
                .await;
        });

        // Collect outcomes
        let mut outcomes = Vec::new();
        let mut overall_success = true;

        // We'll use a timeout to check if we're done
        // This is a bit of a hack, but it avoids the issues with moved values
        let mut consecutive_empty_polls = 0;
        const MAX_EMPTY_POLLS: usize = 10; // After 10 empty polls (1 second), we'll consider the job done

        loop {
            match outcome_rx.recv().await {
                Some(outcome) => {
                    consecutive_empty_polls = 0; // Reset counter when we get an outcome
                    if !outcome.success {
                        overall_success = false;
                    }
                    outcomes.push(outcome);
                }
                None => {
                    // Channel closed, all tasks are done
                    break;
                }
            }

            // Check if there are any ready tasks
            let ready_tasks = self.task_manager.get_ready_tasks_for_job(&self.job_id);
            if ready_tasks.is_empty() {
                consecutive_empty_polls += 1;
                if consecutive_empty_polls >= MAX_EMPTY_POLLS {
                    // No ready tasks for a while, we're probably done
                    break;
                }
                // Wait a bit before checking again
                sleep(Duration::from_millis(100)).await;
            } else {
                consecutive_empty_polls = 0; // Reset counter if there are ready tasks
            }
        }

        Ok(TaskExecutionReport {
            outcomes,
            overall_success,
        })
    }
}

// TaskManager (Enhanced from Previous Discussions)
#[derive(Clone)]
pub struct TaskManager {
    pub tasks_by_id: Arc<DashMap<String, Task>>,
    pub tasks_by_job: Arc<DashMap<String, DashSet<String>>>,
    pub tasks_by_assignee: Arc<DashMap<String, DashSet<String>>>,
    pub tasks_by_status: Arc<DashMap<TaskStatus, DashSet<String>>>,
    pub tasks_by_parent: Arc<DashMap<String, DashSet<String>>>,
    pub heartbeat_interval: Duration,
    pub stall_action: StallAction,
    pub last_activity: Arc<DashMap<String, Instant>>,
    pub cache: Arc<Cache>,
    pub agent_registry: Arc<TaskAgentRegistry>,
    // Deprecated: sled_db_path - use newer task-core system for persistence
    pub sled_db_path: Option<PathBuf>,
    pub jobs: Arc<DashMap<String, JobHandle>>,
}

#[derive(Clone)]
pub enum StallAction {
    NotifyPlanningAgent,
    TerminateJob,
    Custom(fn(&str)),
}

/// JobHandle provides a way to monitor or cancel jobs
#[derive(Clone)]
pub struct JobHandle {
    pub job_id: String,
    pub cancel_tx: Arc<tokio::sync::RwLock<Option<tokio::sync::oneshot::Sender<()>>>>,
    pub status: Arc<tokio::sync::RwLock<JobStatus>>,
}

/// JobStatus represents the current state of a job
#[derive(Debug, Clone, PartialEq)]
pub enum JobStatus {
    Running,
    Completed(bool), // Success or failure
    Cancelled,
}

impl JobHandle {
    /// Cancel the job
    pub async fn cancel(&mut self) -> Result<()> {
        let mut tx_guard = self.cancel_tx.write().await;
        if let Some(tx) = tx_guard.take() {
            tx.send(())
                .map_err(|_| anyhow!("Failed to send cancellation signal"))?;
            *self.status.write().await = JobStatus::Cancelled;
        }
        Ok(())
    }

    /// Get the current status of the job
    pub async fn get_status(&self) -> JobStatus {
        self.status.read().await.clone()
    }
}

impl TaskManager {
    pub fn new(
        heartbeat_interval: Duration,
        stall_action: StallAction,
        cache: Cache,
        agent_registry: TaskAgentRegistry,
        sled_db_path: Option<PathBuf>,
    ) -> Self {
        Self {
            tasks_by_id: Arc::new(DashMap::new()),
            tasks_by_job: Arc::new(DashMap::new()),
            tasks_by_assignee: Arc::new(DashMap::new()),
            tasks_by_status: Arc::new(DashMap::new()),
            tasks_by_parent: Arc::new(DashMap::new()),

            heartbeat_interval,
            stall_action,
            last_activity: Arc::new(DashMap::new()),
            cache: Arc::new(cache),
            agent_registry: Arc::new(agent_registry),
            sled_db_path,
            jobs: Arc::new(DashMap::new()),
        }
    }

    /// Start a new job with the given initial tasks
    pub fn start_job(
        &self,
        job_id: String,
        initial_tasks: Vec<Task>,
        allowed_agents: Option<Vec<String>>,
        config: Option<TaskConfiguration>,
    ) -> Result<JobHandle> {
        // Add initial tasks
        for task in initial_tasks {
            self.add_task(
                task.job_id.clone(),
                task.task_id.clone(),
                task.parent_task_id.clone(),
                task.acceptance_criteria.clone(),
                task.description.clone(),
                task.agent.clone(),
                task.dependencies.clone(),
                task.input.clone(),
                task.timeout,
                task.max_retries,
                task.task_type,
                task.summary.clone(),
                task.created_at,
                task.updated_at,
                task.retry_count,
            )?;
        }

        // Create cancellation channel
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

        // Create job handle
        let job_handle = JobHandle {
            job_id: job_id.clone(),
            cancel_tx: Arc::new(tokio::sync::RwLock::new(Some(cancel_tx))),
            status: Arc::new(tokio::sync::RwLock::new(JobStatus::Running)),
        };

        // Store job handle
        self.jobs.insert(job_id.clone(), job_handle.clone());

        // Track job activity
        self.last_activity.insert(job_id.clone(), Instant::now());

        // Create and spawn executor
        let config = config.unwrap_or_else(|| TaskConfiguration {
            max_execution_time: Some(Duration::from_secs(3600)),
            retry_strategy: RetryStrategy::FixedRetry(3),
            human_timeout_action: HumanTimeoutAction::TimeoutAfter(Duration::from_secs(30)),
            sled_db_path: self.sled_db_path.clone(),
        });

        let mut executor = TaskExecutor::new(
            Arc::new(self.clone()),
            self.agent_registry.clone(),
            self.cache.clone(),
            config,
            job_id.clone(),
            allowed_agents,
        );

        let job_handle_clone = job_handle.clone();
        let job_id_clone = job_id.clone();
        tokio::spawn(async move {
            let result = executor
                .execute(job_id_clone.clone(), Vec::new(), cancel_rx)
                .await;
            let success = result.is_ok() && result.unwrap().overall_success;
            let mut status = job_handle_clone.status.write().await;
            *status = JobStatus::Completed(success);
        });

        // Start background task for heartbeat checking and state persistence
        let tm = Arc::new(self.clone());
        let job_id_clone = job_id.clone();
        tokio::spawn(async move {
            let heartbeat_interval = tm.heartbeat_interval;
            loop {
                // Check if job is still running
                let status = tm.get_job_status(&job_id_clone).await;
                match status {
                    Ok(JobStatus::Running) => {
                        // Check heartbeats
                        tm.check_heartbeats();

                        // Persist state if path is available
                        if let Some(path) = &tm.sled_db_path {
                            if let Err(e) = tm.persist_job_state(&job_id_clone, path) {
                                warn!("Failed to persist job state: {}", e);
                            }
                        }

                        // Sleep for the heartbeat interval
                        tokio::time::sleep(heartbeat_interval).await;
                    }
                    _ => {
                        // Job is no longer running, exit the loop
                        break;
                    }
                }
            }
        });

        Ok(job_handle)
    }

    /// Stop a running job
    pub async fn stop_job(&self, job_id: &str) -> Result<()> {
        if let Some(mut job_handle) = self.jobs.get_mut(job_id) {
            job_handle.cancel().await?;
            Ok(())
        } else {
            Err(anyhow!("Job not found: {}", job_id))
        }
    }

    /// Get the status of a job
    pub async fn get_job_status(&self, job_id: &str) -> Result<JobStatus> {
        if let Some(job_handle) = self.jobs.get(job_id) {
            Ok(job_handle.get_status().await)
        } else {
            Err(anyhow!("Job not found: {}", job_id))
        }
    }

    /// Get all running jobs
    pub fn get_running_jobs(&self) -> Vec<String> {
        self.jobs.iter().map(|entry| entry.key().clone()).collect()
    }

    pub fn add_task(
        &self,
        job_id: String,
        task_id: String,
        parent_task_id: Option<String>,
        acceptance_criteria: Option<String>,
        description: String,
        agent: String,
        dependencies: Vec<String>,
        input: Value,
        timeout: Option<Duration>,
        max_retries: u32,
        task_type: TaskType,
        summary: Option<String>,
        created_at: NaiveDateTime,
        updated_at: NaiveDateTime,
        retry_count: u32,
    ) -> Result<String> {
        // Validate inputs
        if task_id.is_empty() {
            return Err(anyhow!("Task ID cannot be empty"));
        }
        if job_id.is_empty() {
            return Err(anyhow!("Job ID cannot be empty"));
        }

        // Prevent self-dependencies
        if dependencies.contains(&task_id) {
            return Err(anyhow!("Task cannot depend on itself"));
        }

        // Check if task already exists
        if self.tasks_by_id.contains_key(&task_id) {
            return Err(anyhow!("Task with ID {} already exists", task_id));
        }

        // Create the task
        let task = Task {
            job_id: job_id.clone(),
            task_id: task_id.clone(),
            parent_task_id,
            acceptance_criteria,
            description,
            status: TaskStatus::Pending,
            status_reason: None,
            agent: agent.clone(),
            dependencies: dependencies.clone(),
            input,
            output: TaskOutput {
                success: false,
                data: None,
                error: None,
            },
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            timeout,
            max_retries,
            retry_count: 0,
            task_type: TaskType::Task,
            summary: None,
        };

        self.tasks_by_id.insert(task_id.clone(), task.clone());

        // Update tasks_by_job
        self.tasks_by_job
            .entry(job_id.clone())
            .or_insert_with(DashSet::new)
            .insert(task_id.clone());

        // Update tasks_by_assignee
        self.tasks_by_assignee
            .entry(agent.clone())
            .or_insert_with(DashSet::new)
            .insert(task_id.clone());

        // Update tasks_by_status
        self.tasks_by_status
            .entry(TaskStatus::Pending)
            .or_insert_with(DashSet::new)
            .insert(task_id.clone());

        // Update tasks_by_parent if parent_task_id exists
        if let Some(parent_id) = &task.parent_task_id {
            self.tasks_by_parent
                .entry(parent_id.clone())
                .or_insert_with(DashSet::new)
                .insert(task_id.clone());
        }

        // Tasks are always added with Pending status initially
        // If they have dependencies, they'll be processed when dependencies complete
        tracing::info!("TaskManager: Task {} added with status Pending", task_id);
        println!("🚀 Task {} added with status Pending", task_id);

        // Debug: Print the task status
        if let Some(task) = self.tasks_by_id.get(&task_id) {
            println!("🔍 Task {} status: {:?}", task_id, task.status);
        }

        Ok(task_id)
    }

    pub fn claim_task(&self, task_id: &str, agent_name: String) -> Result<()> {
        tracing::info!(
            "TaskManager: Claiming task {} for agent {}",
            task_id,
            agent_name
        );

        {
            let mut task = self
                .tasks_by_id
                .get_mut(task_id)
                .ok_or_else(|| anyhow!("Task not found"))?;
            tracing::debug!("TaskManager: Got mutable reference for task {}", task_id);
            if task.status != TaskStatus::Pending {
                let err = anyhow!("Task is not claimable");
                tracing::error!(
                    "TaskManager: Failed to claim task {}: {} (current status: {:?})",
                    task_id,
                    err,
                    task.status
                );
                return Err(err);
            }
            tracing::info!(
                "TaskManager: Updating task {} status to InProgress",
                task_id
            );
            task.agent = agent_name.clone();
            task.status = TaskStatus::InProgress;
            task.updated_at = Utc::now().naive_utc();
            tracing::debug!("TaskManager: Task {} modified in tasks_by_id", task_id);
        }

        tracing::info!(
            "TaskManager: Before calling update_task_status_map for task {}",
            task_id
        );
        // Get the current status before updating
        let current_status = if let Some(task) = self.tasks_by_id.get(task_id) {
            task.status.clone()
        } else {
            // Default to Pending if task not found (shouldn't happen)
            TaskStatus::Pending
        };
        self.update_task_status_map(task_id, current_status, TaskStatus::InProgress);
        tracing::info!(
            "TaskManager: After calling update_task_status_map for task {}",
            task_id
        );

        tracing::debug!("TaskManager: Task {} status updated to InProgress", task_id);
        tracing::debug!(
            "TaskManager: Updating last activity for job {}",
            self.tasks_by_id.get(task_id).unwrap().job_id
        );
        self.last_activity.insert(
            self.tasks_by_id.get(task_id).unwrap().job_id.clone(),
            Instant::now(),
        );

        tracing::debug!(
            "TaskManager: Updating assignee for task {} to {}",
            task_id,
            agent_name
        );
        if let Some(assignee_set) = self.tasks_by_assignee.get_mut(&agent_name) {
            assignee_set.insert(task_id.to_string());
        } else {
            let mut set = DashSet::new();
            set.insert(task_id.to_string());
            self.tasks_by_assignee.insert(agent_name.clone(), set);
        }

        tracing::info!(
            "TaskManager: Successfully claimed task {} for agent {}",
            task_id,
            agent_name
        );
        Ok(())
    }

    async fn execute_single_task(&self, task: Task) -> TaskOutcome {
        let task_id = task.task_id.clone();

        if let Err(e) = self.claim_task(&task_id, task.agent.clone()) {
            return TaskOutcome {
                task_id,
                success: false,
                error: Some(e.to_string()),
            };
        }

        let agent = match self.agent_registry.get_agent(&task.agent) {
            Some(agent) => agent,
            None => {
                self.complete_task(
                    &task_id,
                    TaskOutput {
                        success: false,
                        data: None,
                        error: Some(format!("Agent {} not found", task.agent)),
                    },
                )
                .unwrap_or_else(|e| error!("Failed to update task status: {}", e));
                return TaskOutcome {
                    task_id,
                    success: false,
                    error: Some(format!("Agent {} not found", task.agent)),
                };
            }
        };

        match agent.execute(&task_id, task.input.clone(), &self).await {
            Ok(output) => {
                self.complete_task(&task_id, output.clone())
                    .unwrap_or_else(|e| error!("Failed to complete task: {}", e));
                TaskOutcome {
                    task_id,
                    success: output.success,
                    error: output.error,
                }
            }
            Err(e) => {
                self.complete_task(
                    &task_id,
                    TaskOutput {
                        success: false,
                        data: None,
                        error: Some(e.to_string()),
                    },
                )
                .unwrap_or_else(|e| error!("Failed to complete task: {}", e));
                TaskOutcome {
                    task_id,
                    success: false,
                    error: Some(e.to_string()),
                }
            }
        }
    }

    pub fn complete_task(&self, task_id: &str, output: TaskOutput) -> Result<()> {
        tracing::info!("TaskManager: Completing task: {}", task_id);

        let new_status;
        let job_id;

        // Use a block to limit the scope of the mutable reference
        {
            let mut task = match self.tasks_by_id.get_mut(task_id) {
                Some(task) => task,
                None => return Err(anyhow!("Task not found: {}", task_id)),
            };

            new_status = if output.success {
                TaskStatus::Completed
            } else {
                TaskStatus::Failed
            };
            job_id = task.job_id.clone();

            task.output = output;
            task.updated_at = Utc::now().naive_utc();
        } // The mutable reference is dropped here when task goes out of scope

        // Now we can safely call update_task_status without holding the lock
        self.update_task_status(task_id, new_status)?;
        Ok(())
    }

    fn update_task_status_map(
        &self,
        task_id: &str,
        old_status: TaskStatus,
        new_status: TaskStatus,
    ) {
        tracing::info!(
            "update_task_status_map: Updating task status map for task {} from {:?} to {:?}",
            task_id,
            old_status,
            new_status
        );

        // Remove from old status set
        if let Some(mut tasks_set) = self.tasks_by_status.get_mut(&old_status) {
            tasks_set.remove(task_id);
            tracing::debug!(
                "update_task_status_map: Removed task {} from status {:?}",
                task_id,
                old_status
            );
        }

        // Add to new status set using entry API
        self.tasks_by_status
            .entry(new_status)
            .or_insert_with(DashSet::new)
            .insert(task_id.to_string());

        tracing::info!("update_task_status_map: Task status map updated");
    }

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

    pub fn get_dependency_outputs(&self, task_id: &str) -> Result<Vec<Value>> {
        let task = self
            .tasks_by_id
            .get(task_id)
            .ok_or_else(|| anyhow!("Task not found: {}", task_id))?;
        let outputs = task
            .dependencies
            .iter()
            .map(|dep_id| {
                let dep_task = self
                    .tasks_by_id
                    .get(dep_id)
                    .ok_or_else(|| anyhow!("Dependency task not found: {}", dep_id))?;
                if dep_task.status != TaskStatus::Completed {
                    Err(anyhow!("Dependency task {} not completed", dep_id))
                } else {
                    Ok(dep_task.output.data.clone().unwrap_or_default())
                }
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(outputs)
    }

    pub fn check_dependencies(&self, job_id: &str, completed_task_id: &str) {
        tracing::info!(
            "TaskManager: Checking dependencies after completion of task {}",
            completed_task_id
        );

        if let Some(task_ids) = self.tasks_by_job.get(job_id) {
            for task_id_ref in task_ids.iter() {
                let task_id = task_id_ref.key();
                if task_id == completed_task_id {
                    continue;
                }

                if let Some(mut task) = self.tasks_by_id.get_mut(task_id) {
                    if task.status != TaskStatus::Pending && task.status != TaskStatus::Blocked {
                        continue;
                    }

                    let all_dependencies_met = task.dependencies.iter().all(|dep_id| {
                        self.tasks_by_id
                            .get(dep_id)
                            .map_or(false, |dep_task| dep_task.status == TaskStatus::Completed)
                    });

                    if all_dependencies_met && task.status != TaskStatus::Pending {
                        tracing::info!(
                            "TaskManager: Task {} dependencies met, setting to Pending",
                            task_id
                        );
                        task.status = TaskStatus::Pending;
                        self.update_task_status_map(
                            task_id,
                            TaskStatus::Pending,
                            TaskStatus::Pending,
                        );
                    }
                }
            }
        }
    }
    pub fn check_heartbeats(&self) {
        let now = Instant::now();

        // Check each job's last activity
        for entry in self.last_activity.iter() {
            let job_id = entry.key();
            let last_activity = entry.value();

            // If the job has been inactive for longer than the heartbeat interval
            if now.duration_since(*last_activity) > self.heartbeat_interval {
                // Execute the stall action
                match &self.stall_action {
                    StallAction::NotifyPlanningAgent => {
                        warn!("Job {} has stalled. Notifying planning agent.", job_id);
                    }
                    StallAction::TerminateJob => {
                        warn!("Job {} has stalled. Terminating job.", job_id);
                        // Mark all pending/in-progress tasks as failed
                        if let Some(task_ids) = self.tasks_by_job.get(job_id) {
                            for task_id_ref in task_ids.iter() {
                                let task_id = task_id_ref.key();
                                if let Some(mut task) = self.tasks_by_id.get_mut(task_id) {
                                    if task.status == TaskStatus::Pending
                                        || task.status == TaskStatus::InProgress
                                    {
                                        task.status = TaskStatus::Failed;
                                        task.status_reason =
                                            Some("Job terminated due to inactivity".to_string());
                                        self.update_task_status_map(
                                            task_id,
                                            TaskStatus::Failed,
                                            TaskStatus::Failed,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    StallAction::Custom(action_fn) => {
                        warn!("Job {} has stalled. Executing custom action.", job_id);
                        action_fn(job_id);
                    }
                }

                // Reset the last activity time to avoid repeated actions
                self.last_activity
                    .insert(job_id.to_string(), Instant::now());
            }
        }
    }

    pub fn get_ready_tasks_for_job(&self, job_id: &str) -> Vec<Task> {
        tracing::info!("TaskManager: Getting ready tasks for job {}", job_id);
        let mut ready_tasks = Vec::new();
        if let Some(task_ids) = self.tasks_by_job.get(job_id) {
            tracing::info!(
                "TaskManager: Found {} tasks for job {}",
                task_ids.len(),
                job_id
            );
            for task_id_ref in task_ids.iter() {
                let task_id = task_id_ref.key();
                if let Some(task) = self.tasks_by_id.get(task_id) {
                    tracing::info!(
                        "TaskManager: Checking task {}: status={:?}, dependencies={:?}",
                        task_id,
                        task.status,
                        task.dependencies
                    );
                    if task.status == TaskStatus::Pending {
                        let all_deps_met = task.dependencies.iter().all(|dep_id| {
                            self.tasks_by_id
                                .get(dep_id)
                                .map_or(false, |dep_task| dep_task.status == TaskStatus::Completed)
                        });
                        if all_deps_met {
                            tracing::info!("TaskManager: Task {} is ready", task_id);
                            ready_tasks.push(task.clone());
                        } else {
                            tracing::info!("TaskManager: Task {} has unmet dependencies", task_id);
                        }
                    }
                }
            }
        }
        tracing::info!("TaskManager: Returning {} ready tasks", ready_tasks.len());
        ready_tasks
    }

    pub fn get_task_by_id(&self, task_id: &str) -> Option<Task> {
        self.tasks_by_id.get(task_id).map(|t| t.clone())
    }

    pub fn generate_dot_graph(&self, job_id: &str) -> String {
        let mut dot = String::from("digraph G {\n");

        if let Some(job_tasks) = self.tasks_by_job.get(job_id) {
            for task_id_ref in job_tasks.iter() {
                let task_id = task_id_ref.key();
                if let Some(task) = self.tasks_by_id.get(task_id) {
                    let label = format!(
                        "{} [{}]\\n{}",
                        task.task_id,
                        format!("{:?}", task.status),
                        task.description
                    );
                    dot.push_str(&format!("\"{}\" [label=\"{}\"];\n", task.task_id, label));

                    // Add edges for dependencies
                    for dep_id in &task.dependencies {
                        dot.push_str(&format!("\"{}\" -> \"{}\";\n", dep_id, task.task_id));
                    }
                }
            }
        }

        dot.push_str("}\n");
        dot
    }

    /// Generates a detailed DOT graph visualization that includes both task structure and cache data
    pub fn generate_detailed_dot_graph(&self, job_id: &str, cache: &Cache) -> String {
        let mut dot = String::from("digraph TaskExecution {\n");
        dot.push_str("  graph [rankdir=LR, nodesep=0.5, ranksep=1.0];\n");
        dot.push_str("  node [shape=box, style=rounded, fontname=\"Helvetica\", width=1.5];\n");
        dot.push_str("  edge [fontsize=10];\n\n");

        // Add tooltip and URL support for interactive viewing
        dot.push_str("  // Enable tooltips and URL support\n");
        dot.push_str("  node [tooltip=\"Click to expand/collapse\"];\n");

        let mut processed_nodes = std::collections::HashSet::new();
        let mut processed_edges = std::collections::HashSet::new();

        // Helper function to truncate long strings
        let truncate_string = |s: &str, max_len: usize| -> String {
            if s.len() > max_len {
                format!("{}...", &s[..max_len - 3])
            } else {
                s.to_string()
            }
        };

        // Helper function to escape special characters in DOT labels
        let escape_label = |s: &str| -> String {
            s.replace("\"", "\\\"")
                .replace("\n", "\\n")
                .replace("\r", "\\r")
                .replace("\t", "\\t")
        };

        // Helper function to extract key fields from JSON
        let extract_json_summary = |json_value: &Value, max_fields: usize| -> String {
            if let Value::Object(map) = json_value {
                let mut fields = Vec::new();
                for (i, (key, value)) in map.iter().enumerate() {
                    if i >= max_fields {
                        fields.push("...".to_string());
                        break;
                    }

                    let value_str = match value {
                        Value::String(s) => truncate_string(s, 30),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        Value::Null => "null".to_string(),
                        Value::Array(a) => format!("[{} items]", a.len()),
                        Value::Object(o) => format!("{{...}} ({} fields)", o.len()),
                    };

                    fields.push(format!("{}: {}", key, value_str));
                }
                fields.join("\\n")
            } else {
                truncate_string(&json_value.to_string(), 50)
            }
        };

        // Get all tasks for the job
        if let Some(job_tasks) = self.tasks_by_job.get(job_id) {
            for task_id_ref in job_tasks.iter() {
                let task_id = task_id_ref.key();
                if let Some(task) = self.tasks_by_id.get(task_id) {
                    if processed_nodes.contains(task_id) {
                        continue;
                    }
                    processed_nodes.insert(task_id.clone());

                    // Create HTML-like label with table structure
                    let mut label = format!(
                        "<<TABLE BORDER=\"0\" CELLBORDER=\"1\" CELLSPACING=\"0\" CELLPADDING=\"4\">\n\
                         <TR><TD COLSPAN=\"2\" BGCOLOR=\"#E8E8E8\"><B>{}</B></TD></TR>\n\
                         <TR><TD COLSPAN=\"2\">Agent: {}</TD></TR>\n\
                         <TR><TD COLSPAN=\"2\">{}</TD></TR>\n",
                        task_id,
                        task.agent,
                        escape_label(&truncate_string(&task.description, 50))
                    );

                    // Add status information
                    let status_color = match task.status {
                        TaskStatus::Completed => "green",
                        TaskStatus::Failed => "red",
                        TaskStatus::InProgress => "blue",
                        TaskStatus::Blocked => "orange",
                        TaskStatus::Pending => "gray",
                        TaskStatus::Accepted => "purple",
                        TaskStatus::Rejected => "brown",
                    };

                    label.push_str(&format!(
                        "<TR><TD COLSPAN=\"2\" BGCOLOR=\"{}\">Status: {:?}</TD></TR>\n",
                        status_color, task.status
                    ));

                    // Add task input summary (key fields only)
                    let input_summary = extract_json_summary(&task.input, 3);
                    label.push_str(&format!(
                        "<TR><TD COLSPAN=\"2\"><FONT COLOR=\"blue\">Input: {}</FONT></TD></TR>\n",
                        escape_label(&input_summary)
                    ));

                    // Add task output if available
                    if task.status == TaskStatus::Completed || task.status == TaskStatus::Failed {
                        if task.output.success {
                            if let Some(data) = &task.output.data {
                                let output_summary = extract_json_summary(data, 3);
                                label.push_str(&format!(
                                    "<TR><TD COLSPAN=\"2\"><FONT COLOR=\"green\">Output: {}</FONT></TD></TR>\n",
                                    escape_label(&output_summary)
                                ));
                            }
                        } else if let Some(error) = &task.output.error {
                            label.push_str(&format!(
                                "<TR><TD COLSPAN=\"2\"><FONT COLOR=\"red\">Error: {}</FONT></TD></TR>\n",
                                escape_label(&truncate_string(error, 100))
                            ));
                        }
                    }

                    // Add cache data summary if available
                    if let Some(node_cache) = cache.data.get(task_id) {
                        let cache_count = node_cache.len();
                        if cache_count > 0 {
                            label.push_str(&format!(
                                "<TR><TD COLSPAN=\"2\" BGCOLOR=\"#E8E8E8\"><B>Cache Data ({} entries)</B></TD></TR>\n",
                                cache_count
                            ));

                            // Show only first few cache entries
                            let mut count = 0;
                            for entry in node_cache.iter() {
                                if count >= 2 {
                                    label.push_str("<TR><TD COLSPAN=\"2\">...</TD></TR>\n");
                                    break;
                                }

                                let key = entry.key();
                                let value = entry.value();
                                let value_str = truncate_string(&value.to_string(), 30);
                                label.push_str(&format!(
                                    "<TR><TD>{}</TD><TD>{}</TD></TR>\n",
                                    escape_label(key),
                                    escape_label(&value_str)
                                ));
                                count += 1;
                            }
                        }
                    }

                    // Add timestamps
                    label.push_str(&format!(
                        "<TR><TD>Created</TD><TD>{}</TD></TR>\n",
                        task.created_at.format("%Y-%m-%d %H:%M:%S")
                    ));

                    label.push_str("</TABLE>>");

                    // Set node color based on status
                    let node_color = match task.status {
                        TaskStatus::Completed => "green",
                        TaskStatus::Failed => "red",
                        TaskStatus::InProgress => "blue",
                        TaskStatus::Blocked => "orange",
                        TaskStatus::Pending => "gray",
                        TaskStatus::Accepted => "purple",
                        TaskStatus::Rejected => "brown",
                    };

                    // Create a detailed tooltip with full input/output data
                    let mut tooltip = format!("{} - {}", task_id, escape_label(&task.description));

                    // Add full input to tooltip
                    tooltip.push_str(&format!(
                        "\\nInput: {}",
                        escape_label(&truncate_string(&task.input.to_string(), 500))
                    ));

                    // Add full output to tooltip if available
                    if let Some(data) = &task.output.data {
                        tooltip.push_str(&format!(
                            "\\nOutput: {}",
                            escape_label(&truncate_string(&data.to_string(), 500))
                        ));
                    }

                    if let Some(error) = &task.output.error {
                        tooltip.push_str(&format!(
                            "\\nError: {}",
                            escape_label(&truncate_string(error, 500))
                        ));
                    }

                    // Add the node with tooltip and URL for interactivity
                    dot.push_str(&format!(
                        "  \"{}\" [label={}, color={}, penwidth=2.0, tooltip=\"{}\", URL=\"javascript:alert('Task Details for {}');\"];\n",
                        task_id, label, node_color, tooltip, task_id
                    ));

                    // Add edges for dependencies
                    for dep_id in &task.dependencies {
                        let edge_key = format!("{}->{}", dep_id, task_id);
                        if !processed_edges.contains(&edge_key) {
                            processed_edges.insert(edge_key);
                            dot.push_str(&format!("  \"{}\" -> \"{}\";\n", dep_id, task_id));
                        }
                    }
                }
            }
        }

        // Add a legend
        dot.push_str("\n  // Legend\n");
        dot.push_str("  subgraph cluster_legend {\n");
        dot.push_str("    label=\"Legend\";\n");
        dot.push_str("    style=filled;\n");
        dot.push_str("    color=lightgrey;\n");
        dot.push_str("    node [style=filled, shape=box];\n");
        dot.push_str("    \"Completed\" [color=green, label=\"Completed\"];\n");
        dot.push_str("    \"Failed\" [color=red, label=\"Failed\"];\n");
        dot.push_str("    \"InProgress\" [color=blue, label=\"In Progress\"];\n");
        dot.push_str("    \"Blocked\" [color=orange, label=\"Blocked\"];\n");
        dot.push_str("    \"Pending\" [color=gray, label=\"Pending\"];\n");
        dot.push_str("    \"Accepted\" [color=purple, label=\"Accepted\"];\n");
        dot.push_str("    \"Rejected\" [color=brown, label=\"Rejected\"];\n");
        dot.push_str("  }\n");

        // Add a note about interactivity
        dot.push_str("\n  // Note about interactivity\n");
        dot.push_str("  subgraph cluster_note {\n");
        dot.push_str("    label=\"Usage Notes\";\n");
        dot.push_str("    style=filled;\n");
        dot.push_str("    color=lightyellow;\n");
        dot.push_str("    \"note\" [shape=note, label=\"Hover over nodes to see details\\nClick on nodes to expand/collapse\", style=filled, fillcolor=lightyellow];\n");
        dot.push_str("  }\n");

        dot.push_str("}\n");
        dot
    }

    /// Persists the entire state of a job (deprecated - use newer task-core system)
    pub fn persist_job_state(&self, _job_id: &str, _sled_path: &Path) -> Result<()> {
        warn!("persist_job_state is deprecated - use the newer task-core system for persistence");
        Ok(())
    }

    /// Rehydrates a job's state (deprecated - use newer task-core system)
    pub fn rehydrate_job_state(&self, _job_id: &str, _sled_path: &Path) -> Result<()> {
        warn!("rehydrate_job_state is deprecated - use the newer task-core system for persistence");
        Ok(())
    }

    /// Clears all state for a specific job
    fn clear_job_state(&self, job_id: &str) {
        // Remove from tasks_by_job
        if let Some(task_ids) = self.tasks_by_job.remove(job_id) {
            // For each task in this job
            for task_id_ref in task_ids.1.iter() {
                let task_id = task_id_ref.key();

                // Get the task to access its properties
                if let Some(task) = self.tasks_by_id.get(task_id) {
                    // Remove from tasks_by_assignee
                    if let Some(assignee_tasks) = self.tasks_by_assignee.get_mut(&task.agent) {
                        assignee_tasks.remove(task_id);
                    }

                    // Remove from tasks_by_status
                    if let Some(status_tasks) = self.tasks_by_status.get_mut(&task.status) {
                        status_tasks.remove(task_id);
                    }

                    // Remove from tasks_by_parent if applicable
                    if let Some(parent_id) = &task.parent_task_id {
                        if let Some(parent_tasks) = self.tasks_by_parent.get_mut(parent_id) {
                            parent_tasks.remove(task_id);
                        }
                    }

                    // We don't remove from ready_tasks anymore
                    // Just log that we're clearing the job state
                    tracing::debug!("TaskManager: Clearing state for task {}", task_id);
                }

                // Remove from tasks_by_id
                self.tasks_by_id.remove(task_id);
            }
        }

        // Remove from last_activity
        self.last_activity.remove(job_id);
    }

    // Create a new task with specified type
    pub fn add_task_with_type(
        &self,
        job_id: String,
        description: String,
        agent: String,
        dependencies: Vec<String>,
        input: Value,
        parent_task_id: Option<String>,
        acceptance_criteria: Option<String>,
        task_type: TaskType,
        timeout: Option<Duration>,
        max_retries: u32,
        summary: Option<String>,

        retry_count: u32,
    ) -> Result<String> {
        println!("Adding task with type: {:?}", task_type);
        let task_id = cuid2::create_id();

        self.add_task(
            job_id,
            task_id.clone(),
            parent_task_id,
            acceptance_criteria,
            description,
            agent,
            dependencies,
            input,
            timeout,
            max_retries,
            task_type,
            summary,
            Utc::now().naive_utc(),
            Utc::now().naive_utc(),
            retry_count,
        )?;

        Ok(task_id)
    }

    // Helper methods for specific task types
    pub fn add_objective(
        &self,
        job_id: String,
        description: String,
        agent: String,
        input: Value,
    ) -> Result<String> {
        self.add_task_with_type(
            job_id,
            description,
            agent,
            vec![],
            input,
            None,
            None,
            TaskType::Objective,
            None,
            0,
            None,
            0,
        )
    }

    pub fn add_story(
        &self,
        job_id: String,
        description: String,
        agent: String,
        parent_objective_id: String,
        acceptance_criteria: Option<String>,
        dependencies: Vec<String>,
        input: Value,
    ) -> Result<String> {
        self.add_task_with_type(
            job_id,
            description,
            agent,
            dependencies,
            input,
            Some(parent_objective_id),
            acceptance_criteria,
            TaskType::Story,
            None,
            0,
            None,
            0,
        )
    }

    // Similar methods for add_task and add_subtask

    // Get all child tasks of a given parent
    pub fn get_child_tasks(&self, parent_id: &str) -> Vec<Task> {
        self.tasks_by_id
            .iter()
            .filter_map(|entry| {
                let task = entry.value();
                if task.parent_task_id.as_deref() == Some(parent_id) {
                    Some(task.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    // Get tasks by type
    pub fn get_tasks_by_type(&self, job_id: &str, task_type: TaskType) -> Vec<Task> {
        self.tasks_by_id
            .iter()
            .filter_map(|entry| {
                let task = entry.value();
                if task.job_id == job_id
                    && std::mem::discriminant(&task.task_type) == std::mem::discriminant(&task_type)
                {
                    Some(task.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    // Get the objective for a job
    pub fn get_objective(&self, job_id: &str) -> Option<Task> {
        self.get_tasks_by_type(job_id, TaskType::Objective)
            .into_iter()
            .next()
    }

    // Update a task's summary (for information distillation)
    pub fn update_task_summary(&self, task_id: &str, summary: String) -> Result<()> {
        if let Some(mut task_ref) = self.tasks_by_id.get_mut(task_id) {
            task_ref.summary = Some(summary);
            task_ref.updated_at = Utc::now().naive_utc();
            Ok(())
        } else {
            Err(anyhow!("Task not found: {}", task_id))
        }
    }

    /// Update the status of a task
    pub fn update_task_status(&self, task_id: &str, new_status: TaskStatus) -> Result<()> {
        tracing::info!(
            "TaskManager: Updating task {} status to {:?}",
            task_id,
            new_status
        );

        // Get all the information we need in one go
        let (old_status, job_id) = {
            println!("🫡 Updating task {} status to {:?}", task_id, new_status);
            // let shatask = self.tasks_by_id.get(task_id);
            // println!("🫡 ShatTask: {:#?}", shatask);
            let mut task = self
                .tasks_by_id
                .get_mut(task_id)
                .ok_or_else(|| anyhow!("Task not found"))?;
            println!("🫡 Task found");
            let old_status = task.status.clone();
            let job_id = task.job_id.clone();
            println!("🫡 Job id: {}", job_id);
            tracing::info!(
                "TaskManager: Task {} status changing from {:?} to {:?}",
                task_id,
                old_status,
                new_status
            );

            task.status = new_status;
            task.updated_at = Utc::now().naive_utc();

            (old_status, job_id)
        }; // Release lock here

        // Update the status maps
        self.update_task_status_map(task_id, old_status, new_status);

        // Update last activity
        self.last_activity.insert(job_id.clone(), Instant::now());

        // Check dependencies if task is completed
        if new_status == TaskStatus::Completed {
            tracing::info!(
                "TaskManager: Task {} is now completed, checking dependencies",
                task_id
            );
            self.check_dependencies(&job_id, task_id);
        }

        tracing::info!(
            "TaskManager: Task {} status successfully updated to {:?}",
            task_id,
            new_status
        );

        Ok(())
    }

    pub fn get_tasks_by_job(&self, job_id: &str) -> Result<Vec<Task>> {
        println!("🫡 Getting tasks by job {}", job_id);
        // println!("🫡 Tasks by id: {:#?}", self.tasks_by_id);
        let tasks = self
            .tasks_by_id
            .iter()
            .filter_map(|entry| {
                println!(
                    "🫡 Entry key: {}, value job_id: {}",
                    entry.key(),
                    entry.value().job_id
                );

                let task = entry.value();
                if task.job_id == job_id {
                    Some(task.clone())
                } else {
                    None
                }
            })
            .collect();

        println!("Fucl you munde");
        println!("🫡 Tasks: {:#?}", tasks);
        Ok(tasks)
    }
}

// Task Execution Report
#[derive(Debug)]
pub struct TaskExecutionReport {
    pub outcomes: Vec<TaskOutcome>,
    pub overall_success: bool,
}

#[derive(Debug, Clone)]
pub struct TaskOutcome {
    pub task_id: String,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct TaskConfiguration {
    pub max_execution_time: Option<Duration>,
    pub retry_strategy: RetryStrategy,
    pub human_timeout_action: HumanTimeoutAction,
    // Deprecated: sled_db_path - use newer task-core system for persistence
    pub sled_db_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub enum RetryStrategy {
    NoRetry,
    FixedRetry(u32),
    ExponentialBackoff {
        max_retries: u32,
        initial_delay: Duration,
        max_delay: Duration,
    },
}
impl Default for RetryStrategy {
    fn default() -> Self {
        Self::NoRetry
    }
}
#[derive(Clone, Debug)]
pub enum HumanTimeoutAction {
    Overwrite,              // Don't ask human, make decision
    Pause,                  // Pause execution and wait indefinitely
    TimeoutAfter(Duration), // Wait for specified duration, then continue
}

impl Default for HumanTimeoutAction {
    fn default() -> Self {
        Self::Overwrite
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskType {
    Objective,
    Story,
    Task,
    Subtask,
}

#[derive(Clone)]
pub struct TaskAgentRegistry {
    agents: Arc<DashMap<String, Box<dyn TaskAgent>>>,
}

// Global registry for task agents
#[linkme::distributed_slice]
pub static TASK_AGENTS: [fn() -> Box<dyn TaskAgent>] = [..];

impl TaskAgentRegistry {
    pub fn new() -> Self {
        Self {
            agents: Arc::new(DashMap::new()),
        }
    }

    pub fn register(&self, name: &str, agent: Box<dyn TaskAgent>) -> Result<()> {
        if self.agents.contains_key(name) {
            return Err(anyhow!("Agent with name '{}' already registered", name));
        }
        self.agents.insert(name.to_string(), agent);
        Ok(())
    }

    pub fn get_agent(&self, name: &str) -> Option<impl Deref<Target = Box<dyn TaskAgent>> + '_> {
        self.agents.get(name)
    }

    pub fn get_agent_descriptions(&self) -> Vec<AgentDescription> {
        self.agents
            .iter()
            .map(|entry| {
                let agent = entry.value();
                AgentDescription {
                    name: agent.name(),
                    description: agent.description(),
                    input_schema: agent.input_schema(),
                    output_schema: agent.output_schema(),
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDescription {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Value,
}
