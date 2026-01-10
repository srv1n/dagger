use crate::{
    error::TaskError,
    model::{AgentError, AgentId, Durability, JobId, NewTaskSpec, Task, TaskId, TaskStatus},
    ready_queue::ReadyQueue,
    scheduler::Scheduler,
    storage::Storage,
};
use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, Semaphore};
use tokio::time::{sleep, timeout};
use tracing::{error, info, warn};

/// Agent trait for task execution
#[async_trait]
pub trait Agent: Send + Sync + 'static {
    /// Execute the task with given input and context
    async fn execute(&self, task: Task, ctx: Arc<TaskContext>) -> Result<Bytes, AgentError>;
}

/// Task execution context
pub struct TaskContext {
    pub task_id: TaskId,
    pub job_id: JobId,
    pub agent_id: AgentId,
    pub handle: Arc<dyn TaskHandle>,
    pub shared: Arc<SharedState>,
    pub storage: Arc<dyn Storage>,
    pub parent: Option<TaskId>,
    pub dependencies: Arc<[TaskId]>,
}

impl TaskContext {
    /// Get dependency outputs
    pub async fn dependency_output(&self, dep_id: TaskId) -> Result<Option<Bytes>, TaskError> {
        self.storage.get_output(dep_id).await
    }
}

/// Shared state for inter-task communication
pub struct SharedState {
    tree: Arc<dyn crate::sqlite_storage::SharedTree>,
}

impl SharedState {
    pub fn new_from_trait(tree: Arc<dyn crate::sqlite_storage::SharedTree>) -> Self {
        Self { tree }
    }

    pub async fn put(&self, scope: &str, key: &str, val: &[u8]) -> Result<(), TaskError> {
        self.tree.put(scope, key, val).await
    }

    pub async fn get(&self, scope: &str, key: &str) -> Result<Option<Bytes>, TaskError> {
        self.tree.get(scope, key).await
    }

    pub async fn delete(&self, scope: &str, key: &str) -> Result<bool, TaskError> {
        self.tree.delete(scope, key).await
    }
}

/// Task handle for dynamic task creation
#[async_trait]
pub trait TaskHandle: Send + Sync {
    async fn spawn_task(&self, spec: NewTaskSpec) -> Result<TaskId, TaskError>;
    async fn spawn_dependent(&self, parent: TaskId, spec: NewTaskSpec)
        -> Result<TaskId, TaskError>;
}

/// Agent registry
pub struct AgentRegistry {
    by_id: Vec<Option<Arc<dyn Agent>>>,
    by_name: DashMap<&'static str, AgentId>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            by_id: vec![None; u16::MAX as usize],
            by_name: DashMap::new(),
        }
    }

    pub fn register(
        &mut self,
        id: AgentId,
        name: &'static str,
        agent: Arc<dyn Agent>,
    ) -> Result<(), TaskError> {
        if self.by_id[id as usize].is_some() {
            return Err(TaskError::AgentAlreadyRegistered(id));
        }
        self.by_id[id as usize] = Some(agent);
        self.by_name.insert(name, id);
        Ok(())
    }

    pub fn get(&self, id: AgentId) -> Option<Arc<dyn Agent>> {
        self.by_id.get(id as usize)?.clone()
    }

    pub fn get_by_name(&self, name: &str) -> Option<Arc<dyn Agent>> {
        let id = *self.by_name.get(name)?;
        self.get(id)
    }

    pub fn id_for_name(&self, name: &str) -> Option<AgentId> {
        self.by_name.get(name).map(|id| *id)
    }
}

impl Clone for AgentRegistry {
    fn clone(&self) -> Self {
        let by_id = self.by_id.clone();
        let by_name = DashMap::new();
        for entry in self.by_name.iter() {
            by_name.insert(*entry.key(), *entry.value());
        }
        Self { by_id, by_name }
    }
}

/// Worker pool executor
pub struct Executor {
    storage: Arc<dyn Storage>,
    ready: Arc<ReadyQueue<TaskId>>,
    workers: usize,
    agent_registry: Arc<AgentRegistry>,
    scheduler: Arc<Scheduler>,
    shared_state: Arc<SharedState>,
}

impl Executor {
    pub fn new(
        storage: Arc<dyn Storage>,
        ready: Arc<ReadyQueue<TaskId>>,
        workers: usize,
        agent_registry: Arc<AgentRegistry>,
        scheduler: Arc<Scheduler>,
        shared_state: Arc<SharedState>,
    ) -> Self {
        Self {
            storage,
            ready,
            workers,
            agent_registry,
            scheduler,
            shared_state,
        }
    }

    pub async fn run(
        self: Arc<Self>,
        mut shutdown_rx: oneshot::Receiver<()>,
    ) -> Result<(), TaskError> {
        let semaphore = Arc::new(Semaphore::new(self.workers));

        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    info!("Executor shutting down");
                    break;
                }
                _ = self.run_iteration(&semaphore) => {
                    // Continue processing
                }
            }
        }

        // Ensure storage is flushed
        self.storage.flush().await?;
        Ok(())
    }

    pub fn agent_id(&self, name: &str) -> Option<AgentId> {
        self.agent_registry.id_for_name(name)
    }

    async fn run_iteration(&self, semaphore: &Arc<Semaphore>) {
        if let Some(task_id) = self.ready.pop() {
            let permit = match semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    // Queue is full, push back and wait
                    self.ready.push(task_id);
                    sleep(Duration::from_millis(10)).await;
                    return;
                }
            };

            let storage = self.storage.clone();
            let agents = self.agent_registry.clone();
            let scheduler = self.scheduler.clone();
            let shared_state = self.shared_state.clone();

            tokio::spawn(async move {
                if let Err(e) = run_one(task_id, storage, agents, scheduler, shared_state).await {
                    error!(task_id = %task_id, error = %e, "Task execution failed");
                }
                drop(permit);
            });
        } else {
            // No tasks ready, sleep briefly
            sleep(Duration::from_millis(10)).await;
        }
    }
}

/// Execute a single task
async fn run_one(
    id: TaskId,
    storage: Arc<dyn Storage>,
    agents: Arc<AgentRegistry>,
    scheduler: Arc<Scheduler>,
    shared_state: Arc<SharedState>,
) -> Result<(), TaskError> {
    // Update status to running
    storage
        .update_status(id, TaskStatus::Pending, TaskStatus::Running)
        .await?;
    storage.flush().await?;

    // Get task
    let task = storage
        .get(id)
        .await?
        .ok_or_else(|| TaskError::TaskNotFound(id))?;

    // Get agent
    let agent = agents
        .get(task.agent)
        .ok_or_else(|| TaskError::AgentNotFound(task.agent))?;

    // Create context
    let context = Arc::new(TaskContext {
        task_id: task.id,
        job_id: task.job,
        agent_id: task.agent,
        handle: Arc::new(TaskHandleImpl {
            storage: storage.clone(),
            scheduler: scheduler.clone(),
        }),
        shared: shared_state,
        storage: storage.clone(),
        parent: task.parent,
        dependencies: Arc::from(task.dependencies.as_slice()),
    });

    // Set timeout
    let timeout_duration = task.timeout.unwrap_or(Duration::from_secs(300));

    // Execute with timeout
    let result = timeout(timeout_duration, agent.execute(task.clone(), context)).await;

    match result {
        Ok(Ok(output)) => {
            // Success
            storage.update_output(id, output).await?;
            storage
                .update_status(id, TaskStatus::Running, TaskStatus::Completed)
                .await?;
            scheduler
                .on_status_change(id, TaskStatus::Completed)
                .await?;
        }
        Ok(Err(e)) => {
            // Agent error
            let should_retry = e.is_retryable()
                && task.durability == Durability::BestEffort
                && task.retry_count.load(std::sync::atomic::Ordering::Relaxed) < task.max_retries;

            if should_retry {
                // Increment retry count and re-queue
                let task = task.clone();
                task.retry_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                storage.put(&task).await?;
                storage
                    .update_status(id, TaskStatus::Running, TaskStatus::Pending)
                    .await?;
                if !scheduler.push_ready(id) {
                    warn!(task_id = %id, "Retry enqueue failed (ready queue full)");
                }
            } else {
                // Final failure
                let mut task = task.clone();
                task.record_error(&e);
                storage.put(&task).await?;
                storage
                    .update_status(id, TaskStatus::Running, TaskStatus::Failed)
                    .await?;
                scheduler.on_status_change(id, TaskStatus::Failed).await?;
            }
        }
        Err(_) => {
            // Timeout
            let e = AgentError::Timeout(timeout_duration);
            let mut task = task.clone();
            task.record_error(&e);
            storage.put(&task).await?;
            storage
                .update_status(id, TaskStatus::Running, TaskStatus::Failed)
                .await?;
            scheduler.on_status_change(id, TaskStatus::Failed).await?;
        }
    }

    Ok(())
}

/// TaskHandle implementation
struct TaskHandleImpl {
    storage: Arc<dyn Storage>,
    scheduler: Arc<Scheduler>,
}

#[async_trait]
impl TaskHandle for TaskHandleImpl {
    async fn spawn_task(&self, spec: NewTaskSpec) -> Result<TaskId, TaskError> {
        let task_id = self.storage.next_task_id().await?;
        let task = Task::from_spec(task_id, spec);

        self.storage.put(&task).await?;
        self.scheduler.add_task(&task).await?;

        Ok(task_id)
    }

    async fn spawn_dependent(
        &self,
        parent: TaskId,
        mut spec: NewTaskSpec,
    ) -> Result<TaskId, TaskError> {
        spec.parent = Some(parent);
        self.spawn_task(spec).await
    }
}
