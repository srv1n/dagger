//! # Task-Core: High-Performance Task Execution System
//! 
//! A lock-free, persistent task execution system with dependency management,
//! crash recovery, and dynamic task creation.
//! 
//! ## Quick Start
//! 
//! ```rust,no_run
//! use task_core::{TaskSystem, TaskSystemBuilder, NewTaskSpec};
//! 
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Build system
//!     let system = TaskSystemBuilder::new()
//!         .with_storage_path("tasks.db")
//!         .build_with_registry().await?;
//!     
//!     // Submit a task
//!     let task_id = system.submit_task(NewTaskSpec {
//!         // ... task specification
//!     }).await?;
//!     
//!     // Run system
//!     system.run().await?;
//!     Ok(())
//! }
//! ```

// Module declarations
pub mod config;
pub mod model;
pub mod storage;
pub mod ready_queue;
pub mod scheduler;
pub mod executor;
pub mod recovery;
pub mod error;
pub mod util;
#[cfg(feature = "metrics")]
pub mod metrics;

// Re-exports for convenience
pub use config::{TaskConfig, TaskConfigBuilder};
pub use model::{
    Task, TaskId, JobId, AgentId, TaskStatus, TaskType,
    Durability, NewTaskSpec, AgentError,
};
pub use storage::{Storage, SledStorage};
pub use ready_queue::ReadyQueue;
pub use scheduler::Scheduler;
pub use executor::{Agent, AgentRegistry, TaskContext, SharedState, TaskHandle};
pub use recovery::{recover, RecoveryConfig};
pub use error::{TaskError, Result};

use std::sync::{Arc, Weak};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, RwLock, oneshot};
use tokio::task::JoinHandle;
use tracing::{info, warn, error};

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
        info!("Starting task system with storage at: {:?}", storage_path.as_ref());
        
        // Create storage
        let storage = Arc::new(SledStorage::open(storage_path)?);
        
        // Create ready queue
        let ready_queue = Arc::new(ReadyQueue::new(config.queue_capacity));
        
        // Create scheduler
        let scheduler = Arc::new(Scheduler::new(
            storage.clone(),
            ready_queue.clone(),
        ));
        
        // Initialize scheduler from storage
        scheduler.initialize_from_storage().await?;
        
        // Create shared state
        let shared_state = Arc::new(SharedState(storage.shared_tree()?));
        
        // Register all agents from linkme
        for register_fn in AGENTS {
            register_fn(&mut agent_registry);
        }
        
        // Create executor
        let executor = Arc::new(executor::Executor::new(
            storage.clone(),
            ready_queue.clone(),
            config.max_parallel,
            Arc::new(agent_registry),
            scheduler.clone(),
            shared_state.clone(),
        ));
        
        // Run recovery
        let recovery_config = RecoveryConfig::default();
        let recovery_stats = recovery::recover(
            storage.clone(),
            ready_queue.clone(),
            recovery_config,
        ).await?;
        
        info!("Recovery complete: {:?}", recovery_stats);
        
        // Create system
        let system = Arc::new(Self {
            storage,
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
        let shutdown_flag = system.shutdown_flag.clone();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        
        let handle = tokio::spawn(async move {
            if let Err(e) = executor_clone.run(shutdown_rx).await {
                error!("Executor error: {}", e);
            }
        });
        
        *system.executor_handle.lock().await = Some(handle);
        
        // Store shutdown sender
        system.shared_state.0.insert(b"__shutdown_tx", bincode::serialize(&shutdown_tx).unwrap())?;
        
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
        self.scheduler.add_task(task).await?;
        
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
        
        // Send shutdown signal to executor
        if let Ok(shutdown_tx_bytes) = self.shared_state.0.get(b"__shutdown_tx") {
            if let Ok(shutdown_tx) = bincode::deserialize::<oneshot::Sender<()>>(&shutdown_tx_bytes) {
                let _ = shutdown_tx.send(());
            }
        }
        
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
    
    /// Get a task by ID
    pub async fn get_task(&self, id: TaskId) -> Result<Option<Task>> {
        self.storage.get(id).await
    }
    
    /// Update task status
    pub async fn update_task_status(&self, id: TaskId, old: TaskStatus, new: TaskStatus) -> Result<()> {
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
    config: Option<TaskConfig>,
}

impl TaskSystemBuilder {
    pub fn new() -> Self {
        Self {
            storage_path: PathBuf::from("tasks.db"),
            config: None,
        }
    }
    
    pub fn with_storage_path(mut self, path: impl AsRef<Path>) -> Self {
        self.storage_path = path.as_ref().to_path_buf();
        self
    }
    
    pub fn with_config(mut self, config: TaskConfig) -> Self {
        self.config = Some(config);
        self
    }
    
    pub async fn build(self, registry: Arc<AgentRegistry>) -> Result<Arc<TaskSystem>> {
        let config = self.config.unwrap_or_default();
        TaskSystem::start(self.storage_path, config, Arc::try_unwrap(registry).unwrap_or_else(|arc| (*arc).clone())).await
    }
    
    pub async fn build_with_registry(self) -> Result<Arc<TaskSystem>> {
        let registry = AgentRegistry::new();
        self.build(Arc::new(registry)).await
    }
}

// Helper implementations for forward references
impl storage::Storage for SledStorage {
    // Implementation is in storage.rs
}

impl executor::SharedState {
    pub(crate) fn new(tree: sled::Tree) -> Self {
        Self(tree)
    }
}