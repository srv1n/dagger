use crate::{
    error::{Result, TaskError},
    model::{Task, TaskId, TaskStatus, TaskType, Durability, AgentId, JobId},
    storage::Storage,
};
use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde_json;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    ConnectOptions, Pool, Row, Sqlite,
};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, trace, warn};

/// SQLite-based storage implementation
pub struct SqliteStorage {
    pool: Pool<Sqlite>,
    next_id: AtomicU64,
}

impl SqliteStorage {
    /// Open SQLite storage at path
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let database_url = format!("sqlite:{}", path.as_ref().display());
        
        let mut options = SqliteConnectOptions::new()
            .filename(&database_url[7..]) // Remove "sqlite:" prefix
            .journal_mode(SqliteJournalMode::Wal)
            .create_if_missing(true);
        
        options.disable_statement_logging();

        let pool = SqlitePoolOptions::new()
            .max_connections(20)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(options)
            .await
            .map_err(|e| TaskError::Storage(format!("Failed to connect to database: {}", e)))?;

        // Run schema initialization
        Self::initialize_schema(&pool).await?;

        // Initialize next ID from existing tasks
        let max_id: i64 = sqlx::query_scalar!(
            "SELECT COALESCE(MAX(CAST(id AS INTEGER)), 0) FROM tasks"
        )
        .fetch_one(&pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to get max task ID: {}", e)))?
        .unwrap_or(0);

        Ok(Self {
            pool,
            next_id: AtomicU64::new((max_id + 1) as u64),
        })
    }

    async fn initialize_schema(pool: &Pool<Sqlite>) -> Result<()> {
        // Create tasks table compatible with the existing schema
        sqlx::query!(
            r#"
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL,
                agent_id INTEGER NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('pending', 'blocked', 'ready', 'running', 'completed', 'failed', 'paused', 'rejected', 'accepted')),
                durability TEXT NOT NULL CHECK (durability IN ('BestEffort', 'AtMostOnce')),
                retry_count INTEGER NOT NULL DEFAULT 0,
                max_retries INTEGER NOT NULL DEFAULT 3,
                task_type TEXT NOT NULL CHECK (task_type IN ('Objective', 'Story', 'Task', 'Subtask')),
                parent_id TEXT,
                dependencies TEXT, -- JSON array of task IDs
                payload_input BLOB,
                payload_output BLOB,
                payload_description TEXT,
                timeout_secs INTEGER,
                error_data BLOB,
                acceptance_criteria TEXT,
                status_reason TEXT,
                summary TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now', 'utc')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now', 'utc'))
            )
            "#
        )
        .execute(pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to create tasks table: {}", e)))?;

        // Create shared_state table
        sqlx::query!(
            r#"
            CREATE TABLE IF NOT EXISTS shared_state (
                key TEXT PRIMARY KEY,
                value BLOB NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now', 'utc')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now', 'utc'))
            )
            "#
        )
        .execute(pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to create shared_state table: {}", e)))?;

        // Create updated_at trigger for tasks
        sqlx::query!(
            r#"
            CREATE TRIGGER IF NOT EXISTS update_tasks_updated_at
                AFTER UPDATE ON tasks
                BEGIN
                    UPDATE tasks SET updated_at = datetime('now', 'utc') WHERE id = NEW.id;
                END
            "#
        )
        .execute(pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to create tasks trigger: {}", e)))?;

        // Create updated_at trigger for shared_state
        sqlx::query!(
            r#"
            CREATE TRIGGER IF NOT EXISTS update_shared_state_updated_at
                AFTER UPDATE ON shared_state
                BEGIN
                    UPDATE shared_state SET updated_at = datetime('now', 'utc') WHERE key = NEW.key;
                END
            "#
        )
        .execute(pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to create shared_state trigger: {}", e)))?;

        // Create indexes for performance
        sqlx::query!(
            r#"
            CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
            CREATE INDEX IF NOT EXISTS idx_tasks_job_id ON tasks(job_id);
            CREATE INDEX IF NOT EXISTS idx_tasks_agent_id ON tasks(agent_id);
            CREATE INDEX IF NOT EXISTS idx_tasks_parent_id ON tasks(parent_id);
            CREATE INDEX IF NOT EXISTS idx_tasks_created_at ON tasks(created_at);
            "#
        )
        .execute(pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to create indexes: {}", e)))?;

        Ok(())
    }

    fn task_to_db_row(task: &Task) -> Result<TaskRow> {
        let status = match task.status.load(Ordering::Relaxed) {
            0 => "pending",
            1 => "blocked", 
            2 => "ready",
            3 => "running",
            4 => "completed",
            5 => "failed",
            6 => "paused",
            7 => "rejected",
            8 => "accepted",
            _ => return Err(TaskError::InvalidStatus),
        };

        let durability = match task.durability {
            Durability::BestEffort => "BestEffort",
            Durability::AtMostOnce => "AtMostOnce",
        };

        let task_type = match task.task_type {
            TaskType::Objective => "Objective",
            TaskType::Story => "Story", 
            TaskType::Task => "Task",
            TaskType::Subtask => "Subtask",
        };

        let dependencies = serde_json::to_string(&task.dependencies)
            .map_err(|e| TaskError::Serialization(format!("Failed to serialize dependencies: {}", e)))?;

        let output_lock = task.payload.output.try_read()
            .map_err(|_| TaskError::Concurrency("Failed to read task output".into()))?;

        Ok(TaskRow {
            id: task.id.to_string(),
            job_id: task.job.to_string(),
            agent_id: task.agent as i64,
            status: status.to_string(),
            durability: durability.to_string(),
            retry_count: task.retry_count.load(Ordering::Relaxed) as i64,
            max_retries: task.max_retries as i64,
            task_type: task_type.to_string(),
            parent_id: task.parent.map(|p| p.to_string()),
            dependencies,
            payload_input: task.payload.input.to_vec(),
            payload_output: output_lock.as_ref().map(|b| b.to_vec()),
            payload_description: task.payload.description.to_string(),
            timeout_secs: task.timeout.map(|d| d.as_secs() as i64),
            error_data: task.error.as_ref().map(|e| e.to_vec()),
            acceptance_criteria: task.acceptance_criteria.as_ref().map(|c| c.to_string()),
            status_reason: task.status_reason.as_ref().map(|r| r.to_string()),
            summary: task.summary.as_ref().map(|s| s.to_string()),
            created_at: task.created_at.format("%Y-%m-%d %H:%M:%S%.f").to_string(),
            updated_at: task.updated_at.format("%Y-%m-%d %H:%M:%S%.f").to_string(),
        })
    }

    fn db_row_to_task(row: TaskRow) -> Result<Task> {
        use std::sync::atomic::AtomicU8;
        use std::sync::Arc;
        use tokio::sync::RwLock;
        use smallvec::SmallVec;

        let id: TaskId = row.id.parse()
            .map_err(|_| TaskError::InvalidId(row.id.clone()))?;

        let job: JobId = row.job_id.parse()
            .map_err(|_| TaskError::InvalidId(row.job_id.clone()))?;

        let agent: AgentId = row.agent_id as AgentId;

        let status = match row.status.as_str() {
            "pending" => TaskStatus::Pending,
            "blocked" => TaskStatus::Blocked,
            "ready" => TaskStatus::Ready,
            "running" => TaskStatus::Running,
            "completed" => TaskStatus::Completed,
            "failed" => TaskStatus::Failed,
            "paused" => TaskStatus::Paused,
            "rejected" => TaskStatus::Rejected,
            "accepted" => TaskStatus::Accepted,
            _ => return Err(TaskError::InvalidStatus),
        };

        let durability = match row.durability.as_str() {
            "BestEffort" => Durability::BestEffort,
            "AtMostOnce" => Durability::AtMostOnce,
            _ => return Err(TaskError::InvalidStatus),
        };

        let task_type = match row.task_type.as_str() {
            "Objective" => TaskType::Objective,
            "Story" => TaskType::Story,
            "Task" => TaskType::Task,
            "Subtask" => TaskType::Subtask,
            _ => return Err(TaskError::InvalidStatus),
        };

        let dependencies: SmallVec<[TaskId; 4]> = serde_json::from_str(&row.dependencies)
            .map_err(|e| TaskError::Serialization(format!("Failed to deserialize dependencies: {}", e)))?;

        let parent = row.parent_id.and_then(|p| p.parse().ok());

        let created_at = DateTime::parse_from_str(&row.created_at, "%Y-%m-%d %H:%M:%S%.f")
            .map_err(|e| TaskError::Serialization(format!("Failed to parse created_at: {}", e)))?
            .with_timezone(&Utc);

        let updated_at = DateTime::parse_from_str(&row.updated_at, "%Y-%m-%d %H:%M:%S%.f")
            .map_err(|e| TaskError::Serialization(format!("Failed to parse updated_at: {}", e)))?
            .with_timezone(&Utc);

        let timeout = row.timeout_secs.map(|s| Duration::from_secs(s as u64));

        let payload = Arc::new(crate::model::TaskPayload {
            input: Bytes::from(row.payload_input),
            output: RwLock::new(row.payload_output.map(Bytes::from)),
            description: Arc::from(row.payload_description),
        });

        let error = row.error_data.map(Bytes::from);

        Ok(Task {
            id,
            job,
            agent,
            status: AtomicU8::new(status as u8),
            durability,
            retry_count: AtomicU8::new(row.retry_count as u8),
            dependencies,
            payload,
            parent,
            task_type,
            max_retries: row.max_retries as u8,
            timeout,
            created_at,
            updated_at,
            acceptance_criteria: row.acceptance_criteria.map(Arc::from),
            status_reason: row.status_reason.map(Arc::from),
            summary: row.summary.map(Arc::from),
            error,
        })
    }
}

#[derive(Debug)]
struct TaskRow {
    id: String,
    job_id: String,
    agent_id: i64,
    status: String,
    durability: String,
    retry_count: i64,
    max_retries: i64,
    task_type: String,
    parent_id: Option<String>,
    dependencies: String,
    payload_input: Vec<u8>,
    payload_output: Option<Vec<u8>>,
    payload_description: String,
    timeout_secs: Option<i64>,
    error_data: Option<Vec<u8>>,
    acceptance_criteria: Option<String>,
    status_reason: Option<String>,
    summary: Option<String>,
    created_at: String,
    updated_at: String,
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn put(&self, task: &Task) -> Result<()> {
        let row = Self::task_to_db_row(task)?;

        sqlx::query!(
            r#"
            INSERT OR REPLACE INTO tasks (
                id, job_id, agent_id, status, durability, retry_count, max_retries,
                task_type, parent_id, dependencies, payload_input, payload_output,
                payload_description, timeout_secs, error_data, acceptance_criteria,
                status_reason, summary, created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            row.id,
            row.job_id,
            row.agent_id,
            row.status,
            row.durability,
            row.retry_count,
            row.max_retries,
            row.task_type,
            row.parent_id,
            row.dependencies,
            row.payload_input,
            row.payload_output,
            row.payload_description,
            row.timeout_secs,
            row.error_data,
            row.acceptance_criteria,
            row.status_reason,
            row.summary,
            row.created_at,
            row.updated_at
        )
        .execute(&self.pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to store task {}: {}", task.id, e)))?;

        trace!("Stored task {}", task.id);
        Ok(())
    }

    async fn get(&self, id: TaskId) -> Result<Option<Task>> {
        let row = sqlx::query_as!(
            TaskRow,
            r#"
            SELECT 
                id, job_id, agent_id, status, durability, retry_count, max_retries,
                task_type, parent_id, dependencies, payload_input, payload_output,
                payload_description, timeout_secs, error_data, acceptance_criteria,
                status_reason, summary, created_at, updated_at
            FROM tasks WHERE id = ?
            "#,
            id.to_string()
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to get task {}: {}", id, e)))?;

        match row {
            Some(row) => Ok(Some(Self::db_row_to_task(row)?)),
            None => Ok(None),
        }
    }

    async fn update_status(&self, id: TaskId, old: TaskStatus, new: TaskStatus) -> Result<()> {
        let old_str = match old {
            TaskStatus::Pending => "pending",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Ready => "ready",
            TaskStatus::Running => "running",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Paused => "paused",
            TaskStatus::Rejected => "rejected",
            TaskStatus::Accepted => "accepted",
        };

        let new_str = match new {
            TaskStatus::Pending => "pending",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Ready => "ready",
            TaskStatus::Running => "running",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Paused => "paused",
            TaskStatus::Rejected => "rejected",
            TaskStatus::Accepted => "accepted",
        };

        let result = sqlx::query!(
            r#"
            UPDATE tasks 
            SET status = ?
            WHERE id = ? AND status = ?
            "#,
            new_str,
            id.to_string(),
            old_str
        )
        .execute(&self.pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to update task {} status: {}", id, e)))?;

        if result.rows_affected() == 0 {
            // Check if task exists to provide proper error
            let current = self.get(id).await?;
            match current {
                Some(task) => {
                    let current_status = TaskStatus::from_u8(task.status.load(Ordering::Relaxed))
                        .ok_or(TaskError::InvalidStatus)?;
                    return Err(TaskError::StatusMismatch {
                        expected: old,
                        found: current_status,
                    });
                }
                None => return Err(TaskError::TaskNotFound(id)),
            }
        }

        debug!("Updated task {} status from {:?} to {:?}", id, old, new);
        Ok(())
    }

    async fn list_running(&self) -> Result<Vec<TaskId>> {
        let rows = sqlx::query!(
            "SELECT id FROM tasks WHERE status = 'running'"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to list running tasks: {}", e)))?;

        let mut running = Vec::new();
        for row in rows {
            if let Ok(id) = row.id.parse::<TaskId>() {
                running.push(id);
            }
        }

        Ok(running)
    }

    async fn get_output(&self, id: TaskId) -> Result<Option<Bytes>> {
        let row = sqlx::query!(
            "SELECT payload_output FROM tasks WHERE id = ?",
            id.to_string()
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to get output for task {}: {}", id, e)))?;

        Ok(row.and_then(|r| r.payload_output.map(Bytes::from)))
    }

    async fn update_output(&self, id: TaskId, output: Bytes) -> Result<()> {
        sqlx::query!(
            "UPDATE tasks SET payload_output = ? WHERE id = ?",
            output.to_vec(),
            id.to_string()
        )
        .execute(&self.pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to update output for task {}: {}", id, e)))?;

        Ok(())
    }

    async fn get_status(&self, id: TaskId) -> Result<TaskStatus> {
        let row = sqlx::query!(
            "SELECT status FROM tasks WHERE id = ?",
            id.to_string()
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to get status for task {}: {}", id, e)))?;

        match row {
            Some(row) => {
                let status = match row.status.as_str() {
                    "pending" => TaskStatus::Pending,
                    "blocked" => TaskStatus::Blocked,
                    "ready" => TaskStatus::Ready,
                    "running" => TaskStatus::Running,
                    "completed" => TaskStatus::Completed,
                    "failed" => TaskStatus::Failed,
                    "paused" => TaskStatus::Paused,
                    "rejected" => TaskStatus::Rejected,
                    "accepted" => TaskStatus::Accepted,
                    _ => return Err(TaskError::InvalidStatus),
                };
                Ok(status)
            }
            None => Err(TaskError::TaskNotFound(id)),
        }
    }

    async fn flush(&self) -> Result<()> {
        // SQLite with WAL mode handles flushing automatically
        // We could issue a PRAGMA wal_checkpoint if needed
        Ok(())
    }

    async fn next_task_id(&self) -> Result<TaskId> {
        Ok(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    async fn get_shared_state(&self, key: &str) -> Result<Option<Bytes>> {
        let row = sqlx::query!(
            "SELECT value FROM shared_state WHERE key = ?",
            key
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to get shared state '{}': {}", key, e)))?;

        Ok(row.map(|r| Bytes::from(r.value)))
    }

    async fn set_shared_state(&self, key: &str, value: Bytes) -> Result<()> {
        sqlx::query!(
            r#"
            INSERT OR REPLACE INTO shared_state (key, value)
            VALUES (?, ?)
            "#,
            key,
            value.to_vec()
        )
        .execute(&self.pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to set shared state '{}': {}", key, e)))?;

        Ok(())
    }

    async fn delete_shared_state(&self, key: &str) -> Result<bool> {
        let result = sqlx::query!(
            "DELETE FROM shared_state WHERE key = ?",
            key
        )
        .execute(&self.pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to delete shared state '{}': {}", key, e)))?;

        Ok(result.rows_affected() > 0)
    }
}

/// Trait-based implementation for shared state operations
#[async_trait]
pub trait SharedTree: Send + Sync {
    async fn put(&self, scope: &str, key: &str, val: &[u8]) -> Result<()>;
    async fn get(&self, scope: &str, key: &str) -> Result<Option<Bytes>>;
    async fn delete(&self, scope: &str, key: &str) -> Result<bool>;
}

/// SQLite implementation of SharedTree
pub struct SqliteSharedTree {
    storage: Arc<SqliteStorage>,
}

impl SqliteSharedTree {
    pub fn new(storage: Arc<SqliteStorage>) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl SharedTree for SqliteSharedTree {
    async fn put(&self, scope: &str, key: &str, val: &[u8]) -> Result<()> {
        let full_key = format!("{}/{}", scope, key);
        self.storage.set_shared_state(&full_key, Bytes::copy_from_slice(val)).await
    }

    async fn get(&self, scope: &str, key: &str) -> Result<Option<Bytes>> {
        let full_key = format!("{}/{}", scope, key);
        self.storage.get_shared_state(&full_key).await
    }

    async fn delete(&self, scope: &str, key: &str) -> Result<bool> {
        let full_key = format!("{}/{}", scope, key);
        self.storage.delete_shared_state(&full_key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{Durability, NewTaskSpec, Task, TaskStatus, TaskType},
        storage::Storage,
    };
    use bytes::Bytes;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_sqlite_storage_operations() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let db_path = temp_dir.path().join("test.db");
        let storage = SqliteStorage::open(&db_path).await?;

        // Create task
        let task_id = storage.next_task_id().await?;
        let spec = NewTaskSpec {
            agent: 1,
            input: Bytes::from("test input"),
            dependencies: vec![].into(),
            durability: Durability::BestEffort,
            task_type: TaskType::Task,
            description: Arc::from("test task"),
            timeout: None,
            max_retries: Some(3),
            parent: None,
        };
        let task = Task::from_spec(task_id, spec);

        // Put and get
        storage.put(&task).await?;
        let retrieved = storage.get(task_id).await?;
        assert!(retrieved.is_some());
        let retrieved_task = retrieved.unwrap();
        assert_eq!(retrieved_task.id, task_id);

        // Update status
        storage
            .update_status(task_id, TaskStatus::Pending, TaskStatus::Running)
            .await?;
        assert_eq!(storage.get_status(task_id).await?, TaskStatus::Running);

        // List running tasks
        let running_tasks = storage.list_running().await?;
        assert!(running_tasks.contains(&task_id));

        // Output operations
        let output = Bytes::from("test output");
        storage.update_output(task_id, output.clone()).await?;
        let retrieved_output = storage.get_output(task_id).await?;
        assert_eq!(retrieved_output, Some(output));

        // Shared state operations
        let key = "test_key";
        let value = Bytes::from("test_value");
        
        storage.set_shared_state(key, value.clone()).await?;
        let retrieved_value = storage.get_shared_state(key).await?;
        assert_eq!(retrieved_value, Some(value));

        let deleted = storage.delete_shared_state(key).await?;
        assert!(deleted);
        
        let retrieved_after_delete = storage.get_shared_state(key).await?;
        assert_eq!(retrieved_after_delete, None);

        Ok(())
    }

    #[tokio::test]
    async fn test_shared_tree() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let db_path = temp_dir.path().join("test_shared.db");
        let storage = Arc::new(SqliteStorage::open(&db_path).await?);
        let shared_tree = SqliteSharedTree::new(storage);

        let scope = "test_scope";
        let key = "test_key";
        let value = b"test_value";

        // Test put and get
        shared_tree.put(scope, key, value).await?;
        let retrieved = shared_tree.get(scope, key).await?;
        assert_eq!(retrieved, Some(Bytes::copy_from_slice(value)));

        // Test delete
        let deleted = shared_tree.delete(scope, key).await?;
        assert!(deleted);
        
        let retrieved_after_delete = shared_tree.get(scope, key).await?;
        assert_eq!(retrieved_after_delete, None);

        Ok(())
    }
}