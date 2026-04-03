//! # Task-Core: High-Performance Task Execution System
//!
//! A lock-free, persistent task execution system with dependency management,
//! crash recovery, and dynamic task creation.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use bytes::Bytes;
//! use task_core::{AgentRegistry, Durability, NewTaskSpec, TaskSystemBuilder, TaskType};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Build system
//!     let system = TaskSystemBuilder::new()
//!         .with_storage_path("tasks.db")
//!         .build(Arc::new(AgentRegistry::new())).await?;
//!
//!     // Submit a task
//!     let agent_id = system.agent_id("example").unwrap_or(1);
//!     let _task_id = system.submit_task(NewTaskSpec {
//!         job: None,
//!         agent: agent_id,
//!         public_id: None,
//!         thread_id: Some(Arc::from("thread-1")),
//!         subject: Some(Arc::from("Example task")),
//!         description: Arc::from("Run the example task"),
//!         owner: Some(Arc::from("example")),
//!         metadata: serde_json::json!({ "surface": "docs" }),
//!         source: None,
//!         acceptance_criteria: None,
//!         input: Bytes::from_static(b"{}"),
//!         dependencies: Vec::new().into(),
//!         durability: Durability::BestEffort,
//!         task_type: TaskType::Task,
//!         timeout: None,
//!         max_retries: Some(3),
//!         parent: None,
//!     }).await?;
//!
//!     // Run system
//!     system.run().await?;
//!     Ok(())
//! }
//! ```

// Module declarations
pub mod config;
pub mod error;
pub mod executor;
#[cfg(feature = "metrics")]
pub mod metrics;
pub mod model;
pub mod ready_queue;
pub mod recovery;
pub mod scheduler;
pub mod sqlite_storage;
pub mod storage;
pub mod util;

// Re-exports for convenience
pub use bytes::Bytes;
pub use config::{TaskConfig, TaskConfigBuilder};
pub use error::{Result, TaskError};
pub use executor::{Agent, AgentRegistry, SharedState, TaskContext, TaskHandle};
pub use model::{
    AgentError, AgentId, Durability, JobId, NewTaskEvent, NewTaskSpec, PublicTaskStatus, Task,
    TaskEventRecord, TaskId, TaskOutputRecord, TaskSourceMetadata, TaskStatus, TaskType,
};
pub use ready_queue::ReadyQueue;
pub use recovery::{Recovery, RecoveryConfig};
pub use scheduler::Scheduler;
pub use sqlite_storage::{SharedTree, SqliteSharedTree, SqliteStorage};
pub use storage::Storage;
pub use util::IntoBytes;

use sqlx::{Pool, Sqlite};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{error, info};

// Global agent registration using linkme
#[linkme::distributed_slice]
pub static AGENTS: [fn(&mut AgentRegistry)] = [..];

/// Main task system that coordinates all components
pub struct TaskSystem {
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) scheduler: Arc<Scheduler>,
    pub(crate) executor: Arc<executor::Executor>,
    pub(crate) ready_queue: Arc<ReadyQueue<TaskId>>,
    pub(crate) shared_state: Arc<SharedState>,
    pub(crate) config: Arc<TaskConfig>,
    recovery_stats: Arc<RwLock<recovery::RecoveryStats>>,
    shutdown_flag: Arc<AtomicBool>,
    executor_handle: Mutex<Option<JoinHandle<()>>>,
}

impl TaskSystem {
    /// Start the task system
    pub async fn start(
        storage_path: impl AsRef<Path>,
        config: TaskConfig,
        mut agent_registry: AgentRegistry,
    ) -> Result<Arc<Self>> {
        info!(
            "Starting task system with storage at: {:?}",
            storage_path.as_ref()
        );

        let storage = Arc::new(SqliteStorage::open(storage_path).await?);
        Self::start_with_storage(storage, config, &mut agent_registry).await
    }

    /// Start the task system against a caller-provided SQLite pool.
    pub async fn start_with_pool(
        pool: Pool<Sqlite>,
        config: TaskConfig,
        mut agent_registry: AgentRegistry,
    ) -> Result<Arc<Self>> {
        info!("Starting task system with caller-provided SQLite pool");

        let storage = Arc::new(SqliteStorage::open_with_pool(pool).await?);
        Self::start_with_storage(storage, config, &mut agent_registry).await
    }

    async fn start_with_storage(
        storage: Arc<SqliteStorage>,
        config: TaskConfig,
        agent_registry: &mut AgentRegistry,
    ) -> Result<Arc<Self>> {
        let storage_dyn: Arc<dyn Storage> = storage.clone();

        // Create ready queue
        let ready_queue = Arc::new(ReadyQueue::new(config.queue_capacity));

        // Create scheduler
        let scheduler = Arc::new(Scheduler::new(storage_dyn.clone(), ready_queue.clone()));

        // Initialize scheduler from storage
        scheduler.initialize_from_storage().await?;

        // Create shared state
        let shared_tree = Arc::new(SqliteSharedTree::new(storage.clone()));
        let shared_state = Arc::new(SharedState::new_from_trait(shared_tree));

        // Register all agents from linkme
        for register_fn in AGENTS {
            register_fn(agent_registry);
        }

        // Create executor
        let executor = Arc::new(executor::Executor::new(
            storage_dyn.clone(),
            ready_queue.clone(),
            config.max_workers,
            Arc::new(agent_registry.clone()),
            scheduler.clone(),
            shared_state.clone(),
        ));

        // Run recovery
        let recovery_config = RecoveryConfig::default();
        let recovery = Recovery::new(storage_dyn.clone(), ready_queue.clone(), recovery_config);
        let recovery_stats = recovery.recover().await?;

        info!("Recovery complete: {:?}", recovery_stats);

        // Create system
        let system = Arc::new(Self {
            storage: storage_dyn,
            scheduler,
            executor,
            ready_queue,
            shared_state,
            config: Arc::new(config),
            recovery_stats: Arc::new(RwLock::new(recovery_stats)),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            executor_handle: Mutex::new(None),
        });

        // Start executor
        let executor_clone = system.executor.clone();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let handle = tokio::spawn(async move {
            if let Err(e) = executor_clone.run(shutdown_rx).await {
                error!("Executor error: {}", e);
            }
        });

        *system.executor_handle.lock().await = Some(handle);

        // Note: shutdown_tx will be handled differently in the new architecture
        let _ = shutdown_tx;

        Ok(system)
    }

    /// Submit a task to the system
    pub async fn submit_task(&self, spec: NewTaskSpec) -> Result<TaskId> {
        if self.shutdown_flag.load(Ordering::Relaxed) {
            return Err(TaskError::SystemShutdown);
        }

        let task_id = self.storage.next_task_id().await?;
        let task = Task::from_spec(task_id, spec);

        // Store task
        self.storage.put(&task).await?;

        // Add to scheduler
        self.scheduler.add_task(&task).await?;

        Ok(task_id)
    }

    /// Run the task system (blocks until shutdown)
    pub async fn run(self: Arc<Self>) -> Result<()> {
        info!("Task system running");

        // Wait for shutdown signal
        while !self.shutdown_flag.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        info!("Task system shutting down");
        Ok(())
    }

    /// Shutdown the system gracefully
    pub async fn shutdown(&self) -> Result<()> {
        info!("Initiating shutdown");
        self.shutdown_flag.store(true, Ordering::Relaxed);

        // Note: shutdown signaling will be handled differently with the new architecture

        // Wait for executor to finish
        if let Some(handle) = self.executor_handle.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }

        // Flush storage
        self.storage.flush().await?;

        info!("Shutdown complete");
        Ok(())
    }

    /// Get scheduler statistics
    pub async fn scheduler_stats(&self) -> Result<scheduler::SchedulerStats> {
        Ok(self.scheduler.get_stats())
    }

    /// Get recovery statistics
    pub async fn recovery_stats(&self) -> Result<recovery::RecoveryStats> {
        Ok(self.recovery_stats.read().await.clone())
    }

    /// Access the shared state store
    pub fn shared_state(&self) -> Arc<SharedState> {
        self.shared_state.clone()
    }

    /// Access the task system configuration
    pub fn config(&self) -> &TaskConfig {
        &self.config
    }

    /// Resolve an agent ID by its registered name
    pub fn agent_id(&self, name: &str) -> Option<AgentId> {
        self.executor.agent_id(name)
    }

    /// Get a task by ID
    pub async fn get_task(&self, id: TaskId) -> Result<Option<Task>> {
        self.storage.get(id).await
    }

    pub async fn get_task_by_public_id(&self, public_id: &str) -> Result<Option<Task>> {
        self.storage.get_by_public_id(public_id).await
    }

    pub async fn list_tasks_by_thread(&self, thread_id: &str) -> Result<Vec<Task>> {
        self.storage.list_tasks_by_thread(thread_id).await
    }

    pub async fn list_child_tasks(&self, parent_id: TaskId) -> Result<Vec<Task>> {
        self.storage.list_child_tasks(parent_id).await
    }

    pub async fn list_task_dependencies(&self, task_id: TaskId) -> Result<Vec<Task>> {
        self.storage.list_task_dependencies(task_id).await
    }

    pub async fn list_task_dependents(&self, task_id: TaskId) -> Result<Vec<Task>> {
        self.storage.list_task_dependents(task_id).await
    }

    pub async fn append_task_output(
        &self,
        task_id: TaskId,
        output: Bytes,
    ) -> Result<TaskOutputRecord> {
        self.storage.append_output(task_id, output).await
    }

    pub async fn list_task_outputs(&self, task_id: TaskId) -> Result<Vec<TaskOutputRecord>> {
        self.storage.list_outputs(task_id).await
    }

    pub async fn append_task_event(
        &self,
        task_id: TaskId,
        event: NewTaskEvent,
    ) -> Result<TaskEventRecord> {
        self.storage.append_event(task_id, event).await
    }

    pub async fn list_task_events(&self, task_id: TaskId) -> Result<Vec<TaskEventRecord>> {
        self.storage.list_events(task_id).await
    }

    pub async fn stop_task(&self, task_id: TaskId, reason: Option<&str>) -> Result<Task> {
        self.storage.request_stop(task_id, reason).await
    }

    pub async fn delete_task(&self, task_id: TaskId, reason: Option<&str>) -> Result<Task> {
        self.storage.soft_delete(task_id, reason).await
    }

    /// Update task status
    pub async fn update_task_status(
        &self,
        id: TaskId,
        old: TaskStatus,
        new: TaskStatus,
    ) -> Result<()> {
        self.storage.update_status(id, old, new).await?;
        self.scheduler.on_status_change(id, new).await?;
        Ok(())
    }

    /// Get queue statistics
    pub fn queue_stats(&self) -> (usize, usize) {
        (self.ready_queue.len(), self.ready_queue.capacity())
    }
}

/// Builder for TaskSystem
pub struct TaskSystemBuilder {
    storage_path: PathBuf,
    storage_pool: Option<Pool<Sqlite>>,
    config: Option<TaskConfig>,
}

impl TaskSystemBuilder {
    pub fn new() -> Self {
        Self {
            storage_path: PathBuf::from("tasks.db"),
            storage_pool: None,
            config: None,
        }
    }

    pub fn with_storage_path(mut self, path: impl AsRef<Path>) -> Self {
        self.storage_path = path.as_ref().to_path_buf();
        self.storage_pool = None;
        self
    }

    pub fn with_sqlite_pool(mut self, pool: Pool<Sqlite>) -> Self {
        self.storage_pool = Some(pool);
        self
    }

    pub fn with_config(mut self, config: TaskConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub async fn build(self, registry: Arc<AgentRegistry>) -> Result<Arc<TaskSystem>> {
        let config = self.config.unwrap_or_default();
        let registry = Arc::try_unwrap(registry).unwrap_or_else(|arc| (*arc).clone());
        match self.storage_pool {
            Some(pool) => TaskSystem::start_with_pool(pool, config, registry).await,
            None => TaskSystem::start(self.storage_path, config, registry).await,
        }
    }

    pub async fn build_with_registry(self) -> Result<Arc<TaskSystem>> {
        let registry = AgentRegistry::new();
        self.build(Arc::new(registry)).await
    }
}
