use crate::{
    model::{Task, TaskId, TaskStatus, TaskType, AgentError},
    error::{TaskError, Result},
};
use async_trait::async_trait;
use bytes::Bytes;
use sled::{Db, Tree};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, info, warn};

/// Storage trait for task persistence
#[async_trait]
pub trait Storage: Send + Sync {
    /// Store a task
    async fn put(&self, task: &Task) -> Result<()>;
    
    /// Get a task by ID
    async fn get(&self, id: TaskId) -> Result<Option<Task>>;
    
    /// Update task status with CAS
    async fn update_status(&self, id: TaskId, old: TaskStatus, new: TaskStatus) -> Result<()>;
    
    /// List all running tasks
    async fn list_running(&self) -> Result<Vec<TaskId>>;
    
    /// Get task output
    async fn get_output(&self, id: TaskId) -> Result<Option<Bytes>>;
    
    /// Update task output
    async fn update_output(&self, id: TaskId, output: Bytes) -> Result<()>;
    
    /// Get task status
    async fn get_status(&self, id: TaskId) -> Result<TaskStatus>;
    
    /// Flush pending writes
    async fn flush(&self) -> Result<()>;
    
    /// Get next task ID
    async fn next_task_id(&self) -> Result<TaskId>;
    
    /// Get shared state tree
    fn shared_tree(&self) -> Result<Tree>;
}

/// Sled-based storage implementation
pub struct SledStorage {
    db: Db,
    tasks: Tree,
    outputs: Tree,
    shared: Tree,
    next_id: AtomicU64,
}

impl SledStorage {
    /// Open storage at path
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = sled::open(path)?;
        let tasks = db.open_tree("tasks")?;
        let outputs = db.open_tree("outputs")?;
        let shared = db.open_tree("shared")?;
        
        // Initialize next ID from existing tasks
        let mut max_id = 0u64;
        for item in tasks.iter() {
            if let Ok((key, _)) = item {
                if let Ok(id_bytes) = key.as_ref().try_into() {
                    let id = u64::from_be_bytes(id_bytes);
                    max_id = max_id.max(id);
                }
            }
        }
        
        Ok(Self {
            db,
            tasks,
            outputs,
            shared,
            next_id: AtomicU64::new(max_id + 1),
        })
    }
}

#[async_trait]
impl Storage for SledStorage {
    async fn put(&self, task: &Task) -> Result<()> {
        let key = task.id.to_be_bytes();
        let value = bincode::serialize(task)?;
        self.tasks.insert(key, value)?;
        Ok(())
    }
    
    async fn get(&self, id: TaskId) -> Result<Option<Task>> {
        let key = id.to_be_bytes();
        match self.tasks.get(key)? {
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => Ok(None),
        }
    }
    
    async fn update_status(&self, id: TaskId, old: TaskStatus, new: TaskStatus) -> Result<()> {
        let key = id.to_be_bytes();
        
        loop {
            match self.tasks.get(&key)? {
                Some(current_bytes) => {
                    let mut task: Task = bincode::deserialize(&current_bytes)?;
                    
                    // Check current status
                    let current = TaskStatus::from_u8(task.status.load(Ordering::Relaxed))
                        .ok_or_else(|| TaskError::InvalidStatus)?;
                    
                    if current != old {
                        return Err(TaskError::StatusMismatch { expected: old, found: current });
                    }
                    
                    // Update status
                    task.status.store(new as u8, Ordering::Release);
                    let new_bytes = bincode::serialize(&task)?;
                    
                    // CAS update
                    match self.tasks.compare_and_swap(&key, Some(current_bytes), Some(new_bytes))? {
                        Ok(_) => {
                            debug!("Updated task {} status from {:?} to {:?}", id, old, new);
                            return Ok(());
                        }
                        Err(_) => {
                            // Retry on CAS failure
                            continue;
                        }
                    }
                }
                None => return Err(TaskError::TaskNotFound(id)),
            }
        }
    }
    
    async fn list_running(&self) -> Result<Vec<TaskId>> {
        let mut running = Vec::new();
        
        for item in self.tasks.iter() {
            if let Ok((key, value)) = item {
                if let Ok(task) = bincode::deserialize::<Task>(&value) {
                    let status = TaskStatus::from_u8(task.status.load(Ordering::Relaxed))
                        .unwrap_or(TaskStatus::Pending);
                    
                    if status == TaskStatus::Running {
                        if let Ok(id_bytes) = key.as_ref().try_into() {
                            let id = u64::from_be_bytes(id_bytes);
                            running.push(id);
                        }
                    }
                }
            }
        }
        
        Ok(running)
    }
    
    async fn get_output(&self, id: TaskId) -> Result<Option<Bytes>> {
        let key = id.to_be_bytes();
        Ok(self.outputs.get(key)?.map(|ivec| Bytes::copy_from_slice(&ivec)))
    }
    
    async fn update_output(&self, id: TaskId, output: Bytes) -> Result<()> {
        let key = id.to_be_bytes();
        self.outputs.insert(key, output.as_ref())?;
        Ok(())
    }
    
    async fn get_status(&self, id: TaskId) -> Result<TaskStatus> {
        let task = self.get(id).await?.ok_or(TaskError::TaskNotFound(id))?;
        TaskStatus::from_u8(task.status.load(Ordering::Relaxed))
            .ok_or_else(|| TaskError::InvalidStatus)
    }
    
    async fn flush(&self) -> Result<()> {
        self.db.flush_async().await?;
        Ok(())
    }
    
    async fn next_task_id(&self) -> Result<TaskId> {
        Ok(self.next_id.fetch_add(1, Ordering::Relaxed))
    }
    
    fn shared_tree(&self) -> Result<Tree> {
        Ok(self.shared.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewTaskSpec, Durability};
    use tempfile::TempDir;
    use std::sync::Arc;
    
    #[tokio::test]
    async fn test_storage_operations() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let storage = SledStorage::open(temp_dir.path())?;
        
        // Create task
        let task_id = storage.next_task_id().await?;
        let spec = NewTaskSpec {
            agent: 1,
            input: Bytes::from("test"),
            dependencies: vec![],
            durability: Durability::BestEffort,
            task_type: TaskType::Task,
            description: Arc::from("test task"),
            timeout: None,
            max_retries: None,
            parent: None,
        };
        let task = Task::from_spec(task_id, spec);
        
        // Put and get
        storage.put(&task).await?;
        let retrieved = storage.get(task_id).await?;
        assert!(retrieved.is_some());
        
        // Update status
        storage.update_status(task_id, TaskStatus::Pending, TaskStatus::Running).await?;
        assert_eq!(storage.get_status(task_id).await?, TaskStatus::Running);
        
        // Output
        let output = Bytes::from("result");
        storage.update_output(task_id, output.clone()).await?;
        let retrieved_output = storage.get_output(task_id).await?;
        assert_eq!(retrieved_output, Some(output));
        
        Ok(())
    }
}