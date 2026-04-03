use crate::{
    error::{Result, TaskError},
    model::{
        AgentId, Durability, JobId, NewTaskEvent, Task, TaskEventRecord, TaskId, TaskOutputRecord,
        TaskPayload, TaskSourceMetadata, TaskStatus, TaskType,
    },
    storage::Storage,
};
use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    ConnectOptions, Pool, Row, Sqlite, Transaction,
};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, trace};

/// SQLite-based storage implementation.
pub struct SqliteStorage {
    pool: Pool<Sqlite>,
    next_id: AtomicU64,
}

impl SqliteStorage {
    /// Open SQLite storage at a standalone path.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let database_url = format!("sqlite:{}", path.as_ref().display());

        let options = SqliteConnectOptions::new()
            .filename(&database_url[7..])
            .journal_mode(SqliteJournalMode::Wal)
            .create_if_missing(true)
            .disable_statement_logging();

        let pool = SqlitePoolOptions::new()
            .max_connections(20)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(options)
            .await
            .map_err(|e| TaskError::Storage(format!("Failed to connect to database: {}", e)))?;

        Self::open_with_pool(pool).await
    }

    /// Open storage against a caller-provided SQLite pool.
    pub async fn open_with_pool(pool: Pool<Sqlite>) -> Result<Self> {
        Self::initialize_schema(&pool).await?;
        Self::migrate_legacy_schema(&pool).await?;

        let max_id: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM dagger_tasks")
            .fetch_one(&pool)
            .await
            .map_err(|e| {
                TaskError::Storage(format!(
                    "Failed to get max task ID from dagger_tasks: {}",
                    e
                ))
            })?;

        Ok(Self {
            pool,
            next_id: AtomicU64::new((max_id + 1) as u64),
        })
    }

    pub fn pool(&self) -> &Pool<Sqlite> {
        &self.pool
    }

    async fn initialize_schema(pool: &Pool<Sqlite>) -> Result<()> {
        let statements = [
            r#"
            CREATE TABLE IF NOT EXISTS dagger_jobs (
                id INTEGER PRIMARY KEY,
                public_id TEXT NOT NULL UNIQUE,
                root_task_id INTEGER,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )
            "#,
            r#"
            CREATE TABLE IF NOT EXISTS dagger_tasks (
                id INTEGER PRIMARY KEY,
                public_id TEXT NOT NULL UNIQUE,
                job_id INTEGER NOT NULL,
                agent_id INTEGER NOT NULL,
                status TEXT NOT NULL CHECK (
                    status IN (
                        'pending', 'blocked', 'ready', 'running', 'completed', 'failed',
                        'paused', 'rejected', 'accepted', 'cancelling', 'cancelled'
                    )
                ),
                durability TEXT NOT NULL CHECK (durability IN ('BestEffort', 'AtMostOnce')),
                retry_count INTEGER NOT NULL DEFAULT 0,
                max_retries INTEGER NOT NULL DEFAULT 3,
                task_type TEXT NOT NULL CHECK (task_type IN ('Objective', 'Story', 'Task', 'Subtask')),
                parent_id INTEGER,
                payload_input BLOB NOT NULL,
                payload_output BLOB,
                timeout_secs INTEGER,
                error_data BLOB,
                thread_id TEXT,
                subject TEXT,
                description TEXT NOT NULL,
                owner TEXT,
                metadata_json TEXT NOT NULL DEFAULT '{}',
                source_metadata_json TEXT,
                acceptance_criteria TEXT,
                status_reason TEXT,
                summary TEXT,
                stop_reason TEXT,
                stop_requested_at TEXT,
                deleted_at TEXT,
                deleted_reason TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )
            "#,
            r#"
            CREATE TABLE IF NOT EXISTS dagger_task_dependencies (
                task_id INTEGER NOT NULL,
                depends_on_task_id INTEGER NOT NULL,
                position INTEGER NOT NULL,
                PRIMARY KEY (task_id, depends_on_task_id)
            )
            "#,
            r#"
            CREATE TABLE IF NOT EXISTS dagger_task_outputs (
                task_id INTEGER NOT NULL,
                sequence INTEGER NOT NULL,
                output BLOB NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (task_id, sequence)
            )
            "#,
            r#"
            CREATE TABLE IF NOT EXISTS dagger_task_events (
                task_id INTEGER NOT NULL,
                sequence INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                status TEXT,
                reason TEXT,
                payload_json TEXT,
                created_at TEXT NOT NULL,
                PRIMARY KEY (task_id, sequence)
            )
            "#,
            r#"
            CREATE TABLE IF NOT EXISTS dagger_shared_state (
                key TEXT PRIMARY KEY,
                value BLOB NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )
            "#,
            "CREATE INDEX IF NOT EXISTS idx_dagger_tasks_job_id ON dagger_tasks(job_id)",
            "CREATE INDEX IF NOT EXISTS idx_dagger_tasks_status ON dagger_tasks(status)",
            "CREATE INDEX IF NOT EXISTS idx_dagger_tasks_thread_id ON dagger_tasks(thread_id)",
            "CREATE INDEX IF NOT EXISTS idx_dagger_tasks_parent_id ON dagger_tasks(parent_id)",
            "CREATE INDEX IF NOT EXISTS idx_dagger_tasks_deleted_at ON dagger_tasks(deleted_at)",
            "CREATE INDEX IF NOT EXISTS idx_dagger_task_dependencies_depends_on ON dagger_task_dependencies(depends_on_task_id)",
            "CREATE INDEX IF NOT EXISTS idx_dagger_task_outputs_task_id ON dagger_task_outputs(task_id, sequence)",
            "CREATE INDEX IF NOT EXISTS idx_dagger_task_events_task_id ON dagger_task_events(task_id, sequence)",
        ];

        for statement in statements {
            sqlx::query(statement).execute(pool).await.map_err(|e| {
                TaskError::Storage(format!("Failed to initialize Dagger schema: {}", e))
            })?;
        }

        Ok(())
    }

    async fn migrate_legacy_schema(pool: &Pool<Sqlite>) -> Result<()> {
        let legacy_tasks_exists = Self::table_exists(pool, "tasks").await?;
        let legacy_state_exists = Self::table_exists(pool, "shared_state").await?;

        if !legacy_tasks_exists && !legacy_state_exists {
            return Ok(());
        }

        let dagger_task_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dagger_tasks")
            .fetch_one(pool)
            .await
            .map_err(|e| {
                TaskError::Storage(format!(
                    "Failed to inspect dagger_tasks during migration: {}",
                    e
                ))
            })?;

        let dagger_state_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM dagger_shared_state")
                .fetch_one(pool)
                .await
                .map_err(|e| {
                    TaskError::Storage(format!(
                        "Failed to inspect dagger_shared_state during migration: {}",
                        e
                    ))
                })?;

        if legacy_tasks_exists && dagger_task_count == 0 {
            let legacy_rows = sqlx::query_as::<_, LegacyTaskRow>(
                r#"
                SELECT
                    id, job_id, agent_id, status, durability, retry_count, max_retries,
                    task_type, parent_id, dependencies, payload_input, payload_output,
                    payload_description, timeout_secs, error_data, acceptance_criteria,
                    status_reason, summary, created_at, updated_at
                FROM tasks
                ORDER BY CAST(id AS INTEGER)
                "#,
            )
            .fetch_all(pool)
            .await
            .map_err(|e| {
                TaskError::Storage(format!(
                    "Failed to read legacy tasks during migration: {}",
                    e
                ))
            })?;

            let mut tx = pool.begin().await?;
            for row in legacy_rows {
                let task_id = row
                    .id
                    .parse::<TaskId>()
                    .map_err(|_| TaskError::InvalidId(row.id.clone()))?;
                let job_id = row.job_id.parse::<JobId>().unwrap_or(task_id);
                let status = TaskStatus::from_str(&row.status).ok_or(TaskError::InvalidStatus)?;
                let now = row.updated_at.clone();

                Self::upsert_job_tx(&mut tx, job_id, Some(task_id), status, &now).await?;

                sqlx::query(
                    r#"
                    INSERT INTO dagger_tasks (
                        id, public_id, job_id, agent_id, status, durability, retry_count, max_retries,
                        task_type, parent_id, payload_input, payload_output, timeout_secs, error_data,
                        thread_id, subject, description, owner, metadata_json, source_metadata_json,
                        acceptance_criteria, status_reason, summary, stop_reason, stop_requested_at,
                        deleted_at, deleted_reason, created_at, updated_at
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    "#,
                )
                .bind(task_id as i64)
                .bind(format!("task-{}", task_id))
                .bind(job_id as i64)
                .bind(row.agent_id)
                .bind(row.status)
                .bind(row.durability)
                .bind(row.retry_count)
                .bind(row.max_retries)
                .bind(row.task_type)
                .bind(optional_i64_from_text(row.parent_id.as_deref()))
                .bind(row.payload_input)
                .bind(row.payload_output.clone())
                .bind(row.timeout_secs)
                .bind(row.error_data)
                .bind(Option::<String>::None)
                .bind(Some(row.payload_description.clone()))
                .bind(row.payload_description)
                .bind(Option::<String>::None)
                .bind("{}")
                .bind(Option::<String>::None)
                .bind(row.acceptance_criteria)
                .bind(row.status_reason)
                .bind(row.summary)
                .bind(Option::<String>::None)
                .bind(Option::<String>::None)
                .bind(Option::<String>::None)
                .bind(Option::<String>::None)
                .bind(row.created_at)
                .bind(row.updated_at)
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    TaskError::Storage(format!("Failed to migrate legacy task {}: {}", task_id, e))
                })?;

                let deps: Vec<TaskId> = if row.dependencies.trim().is_empty() {
                    Vec::new()
                } else {
                    serde_json::from_str(&row.dependencies).map_err(|e| {
                        TaskError::Serialization(format!(
                            "Failed to deserialize legacy dependencies for task {}: {}",
                            task_id, e
                        ))
                    })?
                };

                for (position, dep_id) in deps.iter().enumerate() {
                    sqlx::query(
                        r#"
                        INSERT OR IGNORE INTO dagger_task_dependencies (task_id, depends_on_task_id, position)
                        VALUES (?, ?, ?)
                        "#,
                    )
                    .bind(task_id as i64)
                    .bind(*dep_id as i64)
                    .bind(position as i64)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| {
                        TaskError::Storage(format!(
                            "Failed to migrate dependency {} -> {}: {}",
                            task_id, dep_id, e
                        ))
                    })?;
                }

                if let Some(output) = row.payload_output {
                    sqlx::query(
                        r#"
                        INSERT OR IGNORE INTO dagger_task_outputs (task_id, sequence, output, created_at)
                        VALUES (?, 1, ?, ?)
                        "#,
                    )
                    .bind(task_id as i64)
                    .bind(output)
                    .bind(now)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| {
                        TaskError::Storage(format!(
                            "Failed to migrate output history for task {}: {}",
                            task_id, e
                        ))
                    })?;
                }
            }
            tx.commit().await?;
        }

        if legacy_state_exists && dagger_state_count == 0 {
            let rows = sqlx::query_as::<_, LegacySharedStateRow>(
                "SELECT key, value, created_at, updated_at FROM shared_state",
            )
            .fetch_all(pool)
            .await
            .map_err(|e| {
                TaskError::Storage(format!(
                    "Failed to read legacy shared_state during migration: {}",
                    e
                ))
            })?;

            let mut tx = pool.begin().await?;
            for row in rows {
                sqlx::query(
                    r#"
                    INSERT OR IGNORE INTO dagger_shared_state (key, value, created_at, updated_at)
                    VALUES (?, ?, ?, ?)
                    "#,
                )
                .bind(row.key)
                .bind(row.value)
                .bind(row.created_at)
                .bind(row.updated_at)
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    TaskError::Storage(format!("Failed to migrate shared_state row: {}", e))
                })?;
            }
            tx.commit().await?;
        }

        Ok(())
    }

    async fn table_exists(pool: &Pool<Sqlite>, table_name: &str) -> Result<bool> {
        let exists: Option<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
                .bind(table_name)
                .fetch_optional(pool)
                .await
                .map_err(|e| {
                    TaskError::Storage(format!(
                        "Failed to inspect sqlite_master for {}: {}",
                        table_name, e
                    ))
                })?;
        Ok(exists.is_some())
    }

    async fn upsert_job_tx(
        tx: &mut Transaction<'_, Sqlite>,
        job_id: JobId,
        root_task_id: Option<TaskId>,
        status: TaskStatus,
        now: &str,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO dagger_jobs (id, public_id, root_task_id, status, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                public_id = excluded.public_id,
                root_task_id = COALESCE(dagger_jobs.root_task_id, excluded.root_task_id),
                status = excluded.status,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(job_id as i64)
        .bind(format!("job-{}", job_id))
        .bind(root_task_id.map(|id| id as i64))
        .bind(status.as_str())
        .bind(now)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to upsert job {}: {}", job_id, e)))?;

        Ok(())
    }

    async fn fetch_dependencies(&self, task_id: TaskId) -> Result<smallvec::SmallVec<[TaskId; 4]>> {
        let ids: Vec<i64> = sqlx::query_scalar(
            r#"
            SELECT depends_on_task_id
            FROM dagger_task_dependencies
            WHERE task_id = ?
            ORDER BY position ASC
            "#,
        )
        .bind(task_id as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            TaskError::Storage(format!(
                "Failed to read dependencies for task {}: {}",
                task_id, e
            ))
        })?;

        Ok(ids
            .into_iter()
            .map(|id| id as TaskId)
            .collect::<Vec<_>>()
            .into())
    }

    async fn row_to_task(&self, row: DaggerTaskRow) -> Result<Task> {
        let dependencies = self.fetch_dependencies(row.id as TaskId).await?;
        let created_at = parse_timestamp(&row.created_at)?;
        let updated_at = parse_timestamp(&row.updated_at)?;
        let stop_requested_at = row
            .stop_requested_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?;
        let deleted_at = row.deleted_at.as_deref().map(parse_timestamp).transpose()?;
        let metadata = serde_json::from_str::<Value>(&row.metadata_json).map_err(|e| {
            TaskError::Serialization(format!(
                "Failed to deserialize metadata for task {}: {}",
                row.id, e
            ))
        })?;
        let source = row
            .source_metadata_json
            .as_deref()
            .map(|json| serde_json::from_str::<TaskSourceMetadata>(json))
            .transpose()
            .map_err(|e| {
                TaskError::Serialization(format!(
                    "Failed to deserialize source metadata for task {}: {}",
                    row.id, e
                ))
            })?;

        Ok(Task {
            id: row.id as TaskId,
            public_id: Arc::from(row.public_id),
            job: row.job_id as JobId,
            agent: row.agent_id as AgentId,
            status: AtomicU8::new(
                TaskStatus::from_str(&row.status).ok_or(TaskError::InvalidStatus)? as u8,
            ),
            durability: parse_durability(&row.durability)?,
            retry_count: AtomicU8::new(row.retry_count as u8),
            dependencies,
            payload: Arc::new(TaskPayload {
                input: Bytes::from(row.payload_input),
                output: tokio::sync::RwLock::new(row.payload_output.map(Bytes::from)),
            }),
            parent: row.parent_id.map(|id| id as TaskId),
            task_type: parse_task_type(&row.task_type)?,
            max_retries: row.max_retries as u8,
            timeout: row
                .timeout_secs
                .map(|secs| Duration::from_secs(secs as u64)),
            created_at,
            updated_at,
            thread_id: row.thread_id.map(Arc::from),
            subject: row.subject.map(Arc::from),
            description: Arc::from(row.description),
            owner: row.owner.map(Arc::from),
            metadata,
            source,
            acceptance_criteria: row.acceptance_criteria.map(Arc::from),
            status_reason: row.status_reason.map(Arc::from),
            summary: row.summary.map(Arc::from),
            stop_reason: row.stop_reason.map(Arc::from),
            stop_requested_at,
            deleted_at,
            deleted_reason: row.deleted_reason.map(Arc::from),
            error: row.error_data.map(Bytes::from),
        })
    }

    async fn fetch_task_by_id(&self, id: TaskId) -> Result<Option<Task>> {
        let row = sqlx::query_as::<_, DaggerTaskRow>(
            r#"
            SELECT
                id, public_id, job_id, agent_id, status, durability, retry_count, max_retries,
                task_type, parent_id, payload_input, payload_output, timeout_secs, error_data,
                thread_id, subject, description, owner, metadata_json, source_metadata_json,
                acceptance_criteria, status_reason, summary, stop_reason, stop_requested_at,
                deleted_at, deleted_reason, created_at, updated_at
            FROM dagger_tasks
            WHERE id = ?
            "#,
        )
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to get task {}: {}", id, e)))?;

        match row {
            Some(row) => Ok(Some(self.row_to_task(row).await?)),
            None => Ok(None),
        }
    }

    async fn fetch_task_by_public_id(&self, public_id: &str) -> Result<Option<Task>> {
        let row = sqlx::query_as::<_, DaggerTaskRow>(
            r#"
            SELECT
                id, public_id, job_id, agent_id, status, durability, retry_count, max_retries,
                task_type, parent_id, payload_input, payload_output, timeout_secs, error_data,
                thread_id, subject, description, owner, metadata_json, source_metadata_json,
                acceptance_criteria, status_reason, summary, stop_reason, stop_requested_at,
                deleted_at, deleted_reason, created_at, updated_at
            FROM dagger_tasks
            WHERE public_id = ?
            "#,
        )
        .bind(public_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            TaskError::Storage(format!(
                "Failed to get task by public_id {}: {}",
                public_id, e
            ))
        })?;

        match row {
            Some(row) => Ok(Some(self.row_to_task(row).await?)),
            None => Ok(None),
        }
    }

    async fn fetch_tasks_from_query<'q>(
        &self,
        query: sqlx::query::QueryAs<'q, Sqlite, DaggerTaskRow, sqlx::sqlite::SqliteArguments<'q>>,
    ) -> Result<Vec<Task>> {
        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|e| TaskError::Storage(format!("Failed to list tasks from query: {}", e)))?;

        let mut tasks = Vec::with_capacity(rows.len());
        for row in rows {
            tasks.push(self.row_to_task(row).await?);
        }
        Ok(tasks)
    }

    async fn replace_dependencies_tx(
        tx: &mut Transaction<'_, Sqlite>,
        task_id: TaskId,
        dependencies: &[TaskId],
    ) -> Result<()> {
        sqlx::query("DELETE FROM dagger_task_dependencies WHERE task_id = ?")
            .bind(task_id as i64)
            .execute(&mut **tx)
            .await
            .map_err(|e| {
                TaskError::Storage(format!(
                    "Failed to clear dependencies for task {}: {}",
                    task_id, e
                ))
            })?;

        for (position, dep_id) in dependencies.iter().enumerate() {
            sqlx::query(
                r#"
                INSERT INTO dagger_task_dependencies (task_id, depends_on_task_id, position)
                VALUES (?, ?, ?)
                "#,
            )
            .bind(task_id as i64)
            .bind(*dep_id as i64)
            .bind(position as i64)
            .execute(&mut **tx)
            .await
            .map_err(|e| {
                TaskError::Storage(format!(
                    "Failed to store dependency {} -> {}: {}",
                    task_id, dep_id, e
                ))
            })?;
        }

        Ok(())
    }

    async fn append_event_tx(
        tx: &mut Transaction<'_, Sqlite>,
        task_id: TaskId,
        event: &NewTaskEvent,
    ) -> Result<TaskEventRecord> {
        let next_sequence: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM dagger_task_events WHERE task_id = ?",
        )
        .bind(task_id as i64)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| {
            TaskError::Storage(format!(
                "Failed to allocate event sequence for task {}: {}",
                task_id, e
            ))
        })?;

        let created_at = now_timestamp();
        let payload_json = event
            .payload
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| {
                TaskError::Serialization(format!(
                    "Failed to serialize task event payload for task {}: {}",
                    task_id, e
                ))
            })?;

        sqlx::query(
            r#"
            INSERT INTO dagger_task_events (task_id, sequence, event_type, status, reason, payload_json, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(task_id as i64)
        .bind(next_sequence)
        .bind(&event.event_type)
        .bind(event.status.map(TaskStatus::as_str))
        .bind(event.reason.as_deref())
        .bind(payload_json)
        .bind(&created_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            TaskError::Storage(format!("Failed to append task event for task {}: {}", task_id, e))
        })?;

        Ok(TaskEventRecord {
            sequence: next_sequence as u64,
            event_type: event.event_type.clone(),
            status: event.status,
            reason: event.reason.clone(),
            payload: event.payload.clone(),
            created_at: parse_timestamp(&created_at)?,
        })
    }

    async fn append_output_tx(
        tx: &mut Transaction<'_, Sqlite>,
        task_id: TaskId,
        output: Bytes,
    ) -> Result<TaskOutputRecord> {
        let next_sequence: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM dagger_task_outputs WHERE task_id = ?",
        )
        .bind(task_id as i64)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| {
            TaskError::Storage(format!(
                "Failed to allocate output sequence for task {}: {}",
                task_id, e
            ))
        })?;

        let created_at = now_timestamp();
        let task_updated =
            sqlx::query("UPDATE dagger_tasks SET payload_output = ?, updated_at = ? WHERE id = ?")
                .bind(output.to_vec())
                .bind(&created_at)
                .bind(task_id as i64)
                .execute(&mut **tx)
                .await
                .map_err(|e| {
                    TaskError::Storage(format!(
                        "Failed to update output for task {}: {}",
                        task_id, e
                    ))
                })?;

        if task_updated.rows_affected() == 0 {
            return Err(TaskError::TaskNotFound(task_id));
        }

        sqlx::query(
            r#"
            INSERT INTO dagger_task_outputs (task_id, sequence, output, created_at)
            VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(task_id as i64)
        .bind(next_sequence)
        .bind(output.to_vec())
        .bind(&created_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            TaskError::Storage(format!(
                "Failed to append output history for task {}: {}",
                task_id, e
            ))
        })?;

        let output_record = TaskOutputRecord {
            sequence: next_sequence as u64,
            output: output.clone(),
            created_at: parse_timestamp(&created_at)?,
        };

        let event = NewTaskEvent {
            event_type: "output_appended".to_string(),
            status: None,
            reason: None,
            payload: Some(serde_json::json!({ "sequence": output_record.sequence })),
        };
        let _ = Self::append_event_tx(tx, task_id, &event).await?;

        Ok(output_record)
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn put(&self, task: &Task) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let now = now_timestamp();
        let output_lock = task
            .payload
            .output
            .try_read()
            .map_err(|_| TaskError::Concurrency("Failed to read task output".into()))?;
        let metadata_json = serde_json::to_string(&task.metadata).map_err(|e| {
            TaskError::Serialization(format!("Failed to serialize task metadata: {}", e))
        })?;
        let source_metadata_json = task
            .source
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| {
                TaskError::Serialization(format!("Failed to serialize source metadata: {}", e))
            })?;

        Self::upsert_job_tx(
            &mut tx,
            task.job,
            task.parent.is_none().then_some(task.id),
            task.status(),
            &now,
        )
        .await?;

        sqlx::query(
            r#"
            INSERT INTO dagger_tasks (
                id, public_id, job_id, agent_id, status, durability, retry_count, max_retries,
                task_type, parent_id, payload_input, payload_output, timeout_secs, error_data,
                thread_id, subject, description, owner, metadata_json, source_metadata_json,
                acceptance_criteria, status_reason, summary, stop_reason, stop_requested_at,
                deleted_at, deleted_reason, created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                public_id = excluded.public_id,
                job_id = excluded.job_id,
                agent_id = excluded.agent_id,
                status = excluded.status,
                durability = excluded.durability,
                retry_count = excluded.retry_count,
                max_retries = excluded.max_retries,
                task_type = excluded.task_type,
                parent_id = excluded.parent_id,
                payload_input = excluded.payload_input,
                payload_output = excluded.payload_output,
                timeout_secs = excluded.timeout_secs,
                error_data = excluded.error_data,
                thread_id = excluded.thread_id,
                subject = excluded.subject,
                description = excluded.description,
                owner = excluded.owner,
                metadata_json = excluded.metadata_json,
                source_metadata_json = excluded.source_metadata_json,
                acceptance_criteria = excluded.acceptance_criteria,
                status_reason = excluded.status_reason,
                summary = excluded.summary,
                stop_reason = excluded.stop_reason,
                stop_requested_at = excluded.stop_requested_at,
                deleted_at = excluded.deleted_at,
                deleted_reason = excluded.deleted_reason,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(task.id as i64)
        .bind(task.public_id.as_ref())
        .bind(task.job as i64)
        .bind(task.agent as i64)
        .bind(task.status().as_str())
        .bind(match task.durability {
            Durability::BestEffort => "BestEffort",
            Durability::AtMostOnce => "AtMostOnce",
        })
        .bind(task.retry_count.load(Ordering::Relaxed) as i64)
        .bind(task.max_retries as i64)
        .bind(match task.task_type {
            TaskType::Objective => "Objective",
            TaskType::Story => "Story",
            TaskType::Task => "Task",
            TaskType::Subtask => "Subtask",
        })
        .bind(task.parent.map(|id| id as i64))
        .bind(task.payload.input.to_vec())
        .bind(output_lock.as_ref().map(|bytes| bytes.to_vec()))
        .bind(task.timeout.map(|timeout| timeout.as_secs() as i64))
        .bind(task.error.as_ref().map(|bytes| bytes.to_vec()))
        .bind(task.thread_id.as_ref().map(|v| v.as_ref()))
        .bind(task.subject.as_ref().map(|v| v.as_ref()))
        .bind(task.description.as_ref())
        .bind(task.owner.as_ref().map(|v| v.as_ref()))
        .bind(metadata_json)
        .bind(source_metadata_json)
        .bind(task.acceptance_criteria.as_ref().map(|v| v.as_ref()))
        .bind(task.status_reason.as_ref().map(|v| v.as_ref()))
        .bind(task.summary.as_ref().map(|v| v.as_ref()))
        .bind(task.stop_reason.as_ref().map(|v| v.as_ref()))
        .bind(task.stop_requested_at.map(format_timestamp))
        .bind(task.deleted_at.map(format_timestamp))
        .bind(task.deleted_reason.as_ref().map(|v| v.as_ref()))
        .bind(format_timestamp(task.created_at))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to store task {}: {}", task.id, e)))?;

        Self::replace_dependencies_tx(&mut tx, task.id, &task.dependencies).await?;
        tx.commit().await?;

        trace!("Stored task {}", task.id);
        Ok(())
    }

    async fn get(&self, id: TaskId) -> Result<Option<Task>> {
        self.fetch_task_by_id(id).await
    }

    async fn get_by_public_id(&self, public_id: &str) -> Result<Option<Task>> {
        self.fetch_task_by_public_id(public_id).await
    }

    async fn update_status(&self, id: TaskId, old: TaskStatus, new: TaskStatus) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let now = now_timestamp();
        let result = sqlx::query(
            r#"
            UPDATE dagger_tasks
            SET status = ?, updated_at = ?
            WHERE id = ? AND status = ?
            "#,
        )
        .bind(new.as_str())
        .bind(&now)
        .bind(id as i64)
        .bind(old.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to update task {} status: {}", id, e)))?;

        if result.rows_affected() == 0 {
            drop(tx);
            match self.get(id).await? {
                Some(task) => {
                    return Err(TaskError::StatusMismatch {
                        expected: old,
                        found: task.status(),
                    })
                }
                None => return Err(TaskError::TaskNotFound(id)),
            }
        }

        let event = NewTaskEvent {
            event_type: "status_changed".to_string(),
            status: Some(new),
            reason: None,
            payload: Some(serde_json::json!({
                "from": old.as_str(),
                "to": new.as_str()
            })),
        };
        let _ = Self::append_event_tx(&mut tx, id, &event).await?;
        tx.commit().await?;

        debug!("Updated task {} status from {:?} to {:?}", id, old, new);
        Ok(())
    }

    async fn list_running(&self) -> Result<Vec<TaskId>> {
        let rows: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM dagger_tasks WHERE status IN ('running', 'cancelling') AND deleted_at IS NULL",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to list running tasks: {}", e)))?;

        Ok(rows.into_iter().map(|id| id as TaskId).collect())
    }

    async fn list_tasks(&self) -> Result<Vec<Task>> {
        self.fetch_tasks_from_query(sqlx::query_as::<_, DaggerTaskRow>(
            r#"
            SELECT
                id, public_id, job_id, agent_id, status, durability, retry_count, max_retries,
                task_type, parent_id, payload_input, payload_output, timeout_secs, error_data,
                thread_id, subject, description, owner, metadata_json, source_metadata_json,
                acceptance_criteria, status_reason, summary, stop_reason, stop_requested_at,
                deleted_at, deleted_reason, created_at, updated_at
            FROM dagger_tasks
            ORDER BY created_at ASC, id ASC
            "#,
        ))
        .await
    }

    async fn list_tasks_by_job(&self, job_id: JobId) -> Result<Vec<Task>> {
        self.fetch_tasks_from_query(
            sqlx::query_as::<_, DaggerTaskRow>(
                r#"
                SELECT
                    id, public_id, job_id, agent_id, status, durability, retry_count, max_retries,
                    task_type, parent_id, payload_input, payload_output, timeout_secs, error_data,
                    thread_id, subject, description, owner, metadata_json, source_metadata_json,
                    acceptance_criteria, status_reason, summary, stop_reason, stop_requested_at,
                    deleted_at, deleted_reason, created_at, updated_at
                FROM dagger_tasks
                WHERE job_id = ?
                ORDER BY created_at ASC, id ASC
                "#,
            )
            .bind(job_id as i64),
        )
        .await
    }

    async fn list_tasks_by_thread(&self, thread_id: &str) -> Result<Vec<Task>> {
        self.fetch_tasks_from_query(
            sqlx::query_as::<_, DaggerTaskRow>(
                r#"
                SELECT
                    id, public_id, job_id, agent_id, status, durability, retry_count, max_retries,
                    task_type, parent_id, payload_input, payload_output, timeout_secs, error_data,
                    thread_id, subject, description, owner, metadata_json, source_metadata_json,
                    acceptance_criteria, status_reason, summary, stop_reason, stop_requested_at,
                    deleted_at, deleted_reason, created_at, updated_at
                FROM dagger_tasks
                WHERE thread_id = ?
                ORDER BY created_at ASC, id ASC
                "#,
            )
            .bind(thread_id),
        )
        .await
    }

    async fn list_child_tasks(&self, parent_id: TaskId) -> Result<Vec<Task>> {
        self.fetch_tasks_from_query(
            sqlx::query_as::<_, DaggerTaskRow>(
                r#"
                SELECT
                    id, public_id, job_id, agent_id, status, durability, retry_count, max_retries,
                    task_type, parent_id, payload_input, payload_output, timeout_secs, error_data,
                    thread_id, subject, description, owner, metadata_json, source_metadata_json,
                    acceptance_criteria, status_reason, summary, stop_reason, stop_requested_at,
                    deleted_at, deleted_reason, created_at, updated_at
                FROM dagger_tasks
                WHERE parent_id = ?
                ORDER BY created_at ASC, id ASC
                "#,
            )
            .bind(parent_id as i64),
        )
        .await
    }

    async fn list_task_dependencies(&self, task_id: TaskId) -> Result<Vec<Task>> {
        self.fetch_tasks_from_query(
            sqlx::query_as::<_, DaggerTaskRow>(
                r#"
                SELECT
                    t.id, t.public_id, t.job_id, t.agent_id, t.status, t.durability,
                    t.retry_count, t.max_retries, t.task_type, t.parent_id, t.payload_input,
                    t.payload_output, t.timeout_secs, t.error_data, t.thread_id, t.subject,
                    t.description, t.owner, t.metadata_json, t.source_metadata_json,
                    t.acceptance_criteria, t.status_reason, t.summary, t.stop_reason,
                    t.stop_requested_at, t.deleted_at, t.deleted_reason, t.created_at, t.updated_at
                FROM dagger_tasks t
                JOIN dagger_task_dependencies d
                  ON d.depends_on_task_id = t.id
                WHERE d.task_id = ?
                ORDER BY d.position ASC, t.id ASC
                "#,
            )
            .bind(task_id as i64),
        )
        .await
    }

    async fn list_task_dependents(&self, task_id: TaskId) -> Result<Vec<Task>> {
        self.fetch_tasks_from_query(
            sqlx::query_as::<_, DaggerTaskRow>(
                r#"
                SELECT
                    t.id, t.public_id, t.job_id, t.agent_id, t.status, t.durability,
                    t.retry_count, t.max_retries, t.task_type, t.parent_id, t.payload_input,
                    t.payload_output, t.timeout_secs, t.error_data, t.thread_id, t.subject,
                    t.description, t.owner, t.metadata_json, t.source_metadata_json,
                    t.acceptance_criteria, t.status_reason, t.summary, t.stop_reason,
                    t.stop_requested_at, t.deleted_at, t.deleted_reason, t.created_at, t.updated_at
                FROM dagger_tasks t
                JOIN dagger_task_dependencies d
                  ON d.task_id = t.id
                WHERE d.depends_on_task_id = ?
                ORDER BY t.created_at ASC, t.id ASC
                "#,
            )
            .bind(task_id as i64),
        )
        .await
    }

    async fn get_output(&self, id: TaskId) -> Result<Option<Bytes>> {
        let row = sqlx::query("SELECT payload_output FROM dagger_tasks WHERE id = ?")
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| {
                TaskError::Storage(format!("Failed to get output for task {}: {}", id, e))
            })?;

        Ok(row
            .and_then(|r| r.try_get::<Option<Vec<u8>>, _>("payload_output").ok())
            .flatten()
            .map(Bytes::from))
    }

    async fn append_output(&self, id: TaskId, output: Bytes) -> Result<TaskOutputRecord> {
        let mut tx = self.pool.begin().await?;
        let record = Self::append_output_tx(&mut tx, id, output).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn list_outputs(&self, id: TaskId) -> Result<Vec<TaskOutputRecord>> {
        let rows = sqlx::query(
            r#"
            SELECT sequence, output, created_at
            FROM dagger_task_outputs
            WHERE task_id = ?
            ORDER BY sequence ASC
            "#,
        )
        .bind(id as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            TaskError::Storage(format!("Failed to list outputs for task {}: {}", id, e))
        })?;

        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            records.push(TaskOutputRecord {
                sequence: row
                    .try_get::<i64, _>("sequence")
                    .map_err(TaskError::Sqlite)? as u64,
                output: Bytes::from(
                    row.try_get::<Vec<u8>, _>("output")
                        .map_err(TaskError::Sqlite)?,
                ),
                created_at: parse_timestamp(
                    &row.try_get::<String, _>("created_at")
                        .map_err(TaskError::Sqlite)?,
                )?,
            });
        }
        Ok(records)
    }

    async fn update_output(&self, id: TaskId, output: Bytes) -> Result<()> {
        let _ = self.append_output(id, output).await?;
        Ok(())
    }

    async fn append_event(&self, task_id: TaskId, event: NewTaskEvent) -> Result<TaskEventRecord> {
        let mut tx = self.pool.begin().await?;
        let record = Self::append_event_tx(&mut tx, task_id, &event).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn list_events(&self, task_id: TaskId) -> Result<Vec<TaskEventRecord>> {
        let rows = sqlx::query(
            r#"
            SELECT sequence, event_type, status, reason, payload_json, created_at
            FROM dagger_task_events
            WHERE task_id = ?
            ORDER BY sequence ASC
            "#,
        )
        .bind(task_id as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            TaskError::Storage(format!("Failed to list events for task {}: {}", task_id, e))
        })?;

        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let payload = row
                .try_get::<Option<String>, _>("payload_json")
                .map_err(TaskError::Sqlite)?
                .map(|json| serde_json::from_str::<Value>(&json))
                .transpose()
                .map_err(|e| {
                    TaskError::Serialization(format!(
                        "Failed to deserialize event payload for task {}: {}",
                        task_id, e
                    ))
                })?;
            records.push(TaskEventRecord {
                sequence: row
                    .try_get::<i64, _>("sequence")
                    .map_err(TaskError::Sqlite)? as u64,
                event_type: row
                    .try_get::<String, _>("event_type")
                    .map_err(TaskError::Sqlite)?,
                status: row
                    .try_get::<Option<String>, _>("status")
                    .map_err(TaskError::Sqlite)?
                    .as_deref()
                    .and_then(TaskStatus::from_str),
                reason: row
                    .try_get::<Option<String>, _>("reason")
                    .map_err(TaskError::Sqlite)?,
                payload,
                created_at: parse_timestamp(
                    &row.try_get::<String, _>("created_at")
                        .map_err(TaskError::Sqlite)?,
                )?,
            });
        }

        Ok(records)
    }

    async fn request_stop(&self, id: TaskId, reason: Option<&str>) -> Result<Task> {
        let current_task = self.get(id).await?.ok_or(TaskError::TaskNotFound(id))?;
        let current_status = current_task.status();
        let new_status = match current_status {
            TaskStatus::Running => TaskStatus::Cancelling,
            TaskStatus::Pending
            | TaskStatus::Blocked
            | TaskStatus::Ready
            | TaskStatus::Accepted
            | TaskStatus::Paused => TaskStatus::Cancelled,
            _ => current_status,
        };

        let reason_text = reason
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "Task stop requested".to_string());
        let now = now_timestamp();

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            UPDATE dagger_tasks
            SET
                status = ?,
                status_reason = ?,
                stop_reason = ?,
                stop_requested_at = COALESCE(stop_requested_at, ?),
                updated_at = ?
            WHERE id = ?
            "#,
        )
        .bind(new_status.as_str())
        .bind(&reason_text)
        .bind(&reason_text)
        .bind(&now)
        .bind(&now)
        .bind(id as i64)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            TaskError::Storage(format!("Failed to request stop for task {}: {}", id, e))
        })?;

        let event = NewTaskEvent {
            event_type: "stop_requested".to_string(),
            status: Some(new_status),
            reason: Some(reason_text),
            payload: Some(serde_json::json!({
                "previous_status": current_status.as_str(),
                "next_status": new_status.as_str()
            })),
        };
        let _ = Self::append_event_tx(&mut tx, id, &event).await?;
        tx.commit().await?;

        self.get(id).await?.ok_or(TaskError::TaskNotFound(id))
    }

    async fn soft_delete(&self, id: TaskId, reason: Option<&str>) -> Result<Task> {
        let current_task = self.get(id).await?.ok_or(TaskError::TaskNotFound(id))?;
        let current_status = current_task.status();
        let next_status = match current_status {
            TaskStatus::Running => TaskStatus::Cancelling,
            TaskStatus::Pending
            | TaskStatus::Blocked
            | TaskStatus::Ready
            | TaskStatus::Accepted
            | TaskStatus::Paused => TaskStatus::Cancelled,
            _ => current_status,
        };
        let reason_text = reason
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "Task soft deleted".to_string());
        let now = now_timestamp();

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            UPDATE dagger_tasks
            SET
                status = ?,
                status_reason = ?,
                stop_reason = COALESCE(stop_reason, ?),
                stop_requested_at = COALESCE(stop_requested_at, ?),
                deleted_at = COALESCE(deleted_at, ?),
                deleted_reason = COALESCE(deleted_reason, ?),
                updated_at = ?
            WHERE id = ?
            "#,
        )
        .bind(next_status.as_str())
        .bind(&reason_text)
        .bind(&reason_text)
        .bind(&now)
        .bind(&now)
        .bind(&reason_text)
        .bind(&now)
        .bind(id as i64)
        .execute(&mut *tx)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to soft delete task {}: {}", id, e)))?;

        let event = NewTaskEvent {
            event_type: "task_deleted".to_string(),
            status: Some(next_status),
            reason: Some(reason_text),
            payload: Some(serde_json::json!({ "deleted_at": now })),
        };
        let _ = Self::append_event_tx(&mut tx, id, &event).await?;
        tx.commit().await?;

        self.get(id).await?.ok_or(TaskError::TaskNotFound(id))
    }

    async fn get_status(&self, id: TaskId) -> Result<TaskStatus> {
        let row = sqlx::query("SELECT status FROM dagger_tasks WHERE id = ?")
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| {
                TaskError::Storage(format!("Failed to get status for task {}: {}", id, e))
            })?;

        let status = row
            .and_then(|r| r.try_get::<String, _>("status").ok())
            .as_deref()
            .and_then(TaskStatus::from_str)
            .ok_or(TaskError::TaskNotFound(id))?;

        Ok(status)
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn next_task_id(&self) -> Result<TaskId> {
        Ok(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    async fn get_shared_state(&self, key: &str) -> Result<Option<Bytes>> {
        let row = sqlx::query("SELECT value FROM dagger_shared_state WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| {
                TaskError::Storage(format!("Failed to get shared state '{}': {}", key, e))
            })?;

        Ok(row
            .and_then(|r| r.try_get::<Vec<u8>, _>("value").ok())
            .map(Bytes::from))
    }

    async fn set_shared_state(&self, key: &str, value: Bytes) -> Result<()> {
        let now = now_timestamp();
        sqlx::query(
            r#"
            INSERT INTO dagger_shared_state (key, value, created_at, updated_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(key)
        .bind(value.to_vec())
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to set shared state '{}': {}", key, e)))?;

        Ok(())
    }

    async fn delete_shared_state(&self, key: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM dagger_shared_state WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                TaskError::Storage(format!("Failed to delete shared state '{}': {}", key, e))
            })?;

        Ok(result.rows_affected() > 0)
    }
}

/// Trait-based implementation for shared state operations.
#[async_trait]
pub trait SharedTree: Send + Sync {
    async fn put(&self, scope: &str, key: &str, val: &[u8]) -> Result<()>;
    async fn get(&self, scope: &str, key: &str) -> Result<Option<Bytes>>;
    async fn delete(&self, scope: &str, key: &str) -> Result<bool>;
}

/// SQLite implementation of SharedTree.
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
        self.storage
            .set_shared_state(&full_key, Bytes::copy_from_slice(val))
            .await
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

#[derive(Debug, sqlx::FromRow)]
struct DaggerTaskRow {
    id: i64,
    public_id: String,
    job_id: i64,
    agent_id: i64,
    status: String,
    durability: String,
    retry_count: i64,
    max_retries: i64,
    task_type: String,
    parent_id: Option<i64>,
    payload_input: Vec<u8>,
    payload_output: Option<Vec<u8>>,
    timeout_secs: Option<i64>,
    error_data: Option<Vec<u8>>,
    thread_id: Option<String>,
    subject: Option<String>,
    description: String,
    owner: Option<String>,
    metadata_json: String,
    source_metadata_json: Option<String>,
    acceptance_criteria: Option<String>,
    status_reason: Option<String>,
    summary: Option<String>,
    stop_reason: Option<String>,
    stop_requested_at: Option<String>,
    deleted_at: Option<String>,
    deleted_reason: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, sqlx::FromRow)]
struct LegacyTaskRow {
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

#[derive(Debug, sqlx::FromRow)]
struct LegacySharedStateRow {
    key: String,
    value: Vec<u8>,
    created_at: String,
    updated_at: String,
}

fn parse_task_type(value: &str) -> Result<TaskType> {
    match value {
        "Objective" => Ok(TaskType::Objective),
        "Story" => Ok(TaskType::Story),
        "Task" => Ok(TaskType::Task),
        "Subtask" => Ok(TaskType::Subtask),
        _ => Err(TaskError::InvalidStatus),
    }
}

fn parse_durability(value: &str) -> Result<Durability> {
    match value {
        "BestEffort" => Ok(Durability::BestEffort),
        "AtMostOnce" => Ok(Durability::AtMostOnce),
        _ => Err(TaskError::InvalidStatus),
    }
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
        .map(|dt| DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc))
        .map_err(|e| {
            TaskError::Serialization(format!("Failed to parse timestamp '{}': {}", value, e))
        })
}

fn format_timestamp(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%d %H:%M:%S%.f").to_string()
}

fn now_timestamp() -> String {
    format_timestamp(Utc::now())
}

fn optional_i64_from_text(value: Option<&str>) -> Option<i64> {
    value.and_then(|raw| raw.parse::<i64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{NewTaskSpec, TaskStatus, TaskType},
        storage::Storage,
    };
    use sqlx::SqlitePool;
    use tempfile::TempDir;

    fn test_spec(thread_id: &str, public_id: &str) -> NewTaskSpec {
        NewTaskSpec {
            job: None,
            agent: 1,
            public_id: Some(Arc::from(public_id)),
            thread_id: Some(Arc::from(thread_id)),
            subject: Some(Arc::from("subject")),
            description: Arc::from("test task"),
            owner: Some(Arc::from("owner")),
            metadata: serde_json::json!({ "surface": "test" }),
            source: Some(TaskSourceMetadata {
                surface: Some("chat".to_string()),
                tool_name: Some("submit".to_string()),
                thread_id: Some(thread_id.to_string()),
                turn_id: Some("turn-1".to_string()),
                run_id: Some("run-1".to_string()),
                call_id: Some("call-1".to_string()),
            }),
            acceptance_criteria: Some(Arc::from("be durable")),
            input: Bytes::from("test input"),
            dependencies: Vec::new().into(),
            durability: Durability::BestEffort,
            task_type: TaskType::Task,
            timeout: None,
            max_retries: Some(3),
            parent: None,
        }
    }

    async fn connect_file_pool(path: &std::path::Path) -> Result<SqlitePool> {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .create_if_missing(true)
            .disable_statement_logging();
        SqlitePool::connect_with(options)
            .await
            .map_err(TaskError::Sqlite)
    }

    #[tokio::test]
    async fn test_sqlite_storage_operations() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let db_path = temp_dir.path().join("test.db");
        let storage = SqliteStorage::open(&db_path).await?;

        let task_id = storage.next_task_id().await?;
        let task = Task::from_spec(task_id, test_spec("thread-a", "task-public-1"));

        storage.put(&task).await?;
        let retrieved = storage.get(task_id).await?.expect("task stored");
        assert_eq!(retrieved.id, task_id);
        assert_eq!(retrieved.public_id.as_ref(), "task-public-1");
        assert_eq!(
            retrieved.thread_id.as_deref().map(|s| s.as_ref()),
            Some("thread-a")
        );
        assert_eq!(retrieved.metadata["surface"], "test");
        assert_eq!(
            retrieved.source.as_ref().and_then(|s| s.call_id.as_deref()),
            Some("call-1")
        );

        storage
            .update_status(task_id, TaskStatus::Pending, TaskStatus::Running)
            .await?;
        assert_eq!(storage.get_status(task_id).await?, TaskStatus::Running);

        let output = Bytes::from("test output");
        storage.update_output(task_id, output.clone()).await?;
        let retrieved_output = storage.get_output(task_id).await?;
        assert_eq!(retrieved_output, Some(output.clone()));
        let outputs = storage.list_outputs(task_id).await?;
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].output, output);

        let event = storage
            .append_event(
                task_id,
                NewTaskEvent {
                    event_type: "host_note".to_string(),
                    status: Some(TaskStatus::Running),
                    reason: Some("note".to_string()),
                    payload: Some(serde_json::json!({ "kind": "note" })),
                },
            )
            .await?;
        assert_eq!(event.sequence, 3);
        let events = storage.list_events(task_id).await?;
        assert_eq!(events.len(), 3);

        let stopped = storage.request_stop(task_id, Some("stop now")).await?;
        assert_eq!(stopped.status(), TaskStatus::Cancelling);

        let deleted = storage.soft_delete(task_id, Some("cleanup")).await?;
        assert!(deleted.is_deleted());

        Ok(())
    }

    #[tokio::test]
    async fn test_pool_based_storage_initialization() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let db_path = temp_dir.path().join("embedded.db");
        let pool = connect_file_pool(&db_path).await?;

        sqlx::query("CREATE TABLE IF NOT EXISTS host_records (id INTEGER PRIMARY KEY, value TEXT)")
            .execute(&pool)
            .await
            .map_err(TaskError::Sqlite)?;

        let storage = SqliteStorage::open_with_pool(pool.clone()).await?;
        let task = Task::from_spec(
            storage.next_task_id().await?,
            test_spec("thread-b", "task-public-2"),
        );
        storage.put(&task).await?;

        let host_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'host_records'",
        )
        .fetch_one(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        assert_eq!(host_count, 1);

        let dagger_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'dagger_tasks'",
        )
        .fetch_one(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        assert_eq!(dagger_count, 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_query_surfaces_and_relational_dependencies() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let db_path = temp_dir.path().join("query.db");
        let storage = SqliteStorage::open(&db_path).await?;

        let parent_id = storage.next_task_id().await?;
        let mut parent_spec = test_spec("thread-c", "parent-public");
        parent_spec.subject = Some(Arc::from("parent"));
        let parent = Task::from_spec(parent_id, parent_spec);
        storage.put(&parent).await?;

        let dep_id = storage.next_task_id().await?;
        let dep = Task::from_spec(dep_id, test_spec("thread-c", "dep-public"));
        storage.put(&dep).await?;

        let child_id = storage.next_task_id().await?;
        let mut child_spec = test_spec("thread-c", "child-public");
        child_spec.parent = Some(parent_id);
        child_spec.dependencies = vec![dep_id].into();
        child_spec.job = Some(parent.job);
        let child = Task::from_spec(child_id, child_spec);
        storage.put(&child).await?;

        assert_eq!(
            storage
                .get_by_public_id("child-public")
                .await?
                .expect("child by public id")
                .id,
            child_id
        );
        assert_eq!(storage.list_tasks_by_thread("thread-c").await?.len(), 3);
        assert_eq!(storage.list_child_tasks(parent_id).await?.len(), 1);
        assert_eq!(storage.list_task_dependencies(child_id).await?.len(), 1);
        assert_eq!(storage.list_task_dependents(dep_id).await?.len(), 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_legacy_schema_migration() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let db_path = temp_dir.path().join("legacy.db");
        let pool = connect_file_pool(&db_path).await?;

        sqlx::query(
            r#"
            CREATE TABLE tasks (
                id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL,
                agent_id INTEGER NOT NULL,
                status TEXT NOT NULL,
                durability TEXT NOT NULL,
                retry_count INTEGER NOT NULL,
                max_retries INTEGER NOT NULL,
                task_type TEXT NOT NULL,
                parent_id TEXT,
                dependencies TEXT NOT NULL,
                payload_input BLOB,
                payload_output BLOB,
                payload_description TEXT,
                timeout_secs INTEGER,
                error_data BLOB,
                acceptance_criteria TEXT,
                status_reason TEXT,
                summary TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )
            "#,
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        sqlx::query(
            r#"
            CREATE TABLE shared_state (
                key TEXT PRIMARY KEY,
                value BLOB NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )
            "#,
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;

        sqlx::query(
            r#"
            INSERT INTO tasks (
                id, job_id, agent_id, status, durability, retry_count, max_retries, task_type,
                parent_id, dependencies, payload_input, payload_output, payload_description,
                timeout_secs, error_data, acceptance_criteria, status_reason, summary, created_at, updated_at
            ) VALUES (
                '1', '1', 1, 'running', 'BestEffort', 0, 3, 'Task',
                NULL, '[2]', X'6869', X'6f7574', 'legacy desc',
                NULL, NULL, 'legacy accept', 'legacy reason', 'legacy summary',
                '2026-04-03 10:00:00.000', '2026-04-03 10:00:01.000'
            )
            "#,
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        sqlx::query(
            r#"
            INSERT INTO tasks (
                id, job_id, agent_id, status, durability, retry_count, max_retries, task_type,
                parent_id, dependencies, payload_input, payload_output, payload_description,
                timeout_secs, error_data, acceptance_criteria, status_reason, summary, created_at, updated_at
            ) VALUES (
                '2', '1', 1, 'completed', 'BestEffort', 0, 3, 'Task',
                NULL, '[]', X'626965', NULL, 'dep',
                NULL, NULL, NULL, NULL, NULL,
                '2026-04-03 10:00:00.000', '2026-04-03 10:00:01.000'
            )
            "#,
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        sqlx::query(
            r#"
            INSERT INTO shared_state (key, value, created_at, updated_at)
            VALUES ('scope/key', X'76616c', '2026-04-03 10:00:00.000', '2026-04-03 10:00:01.000')
            "#,
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;

        let storage = SqliteStorage::open_with_pool(pool.clone()).await?;
        let migrated = storage.get(1).await?.expect("migrated task");
        assert_eq!(migrated.public_id.as_ref(), "task-1");
        assert_eq!(migrated.dependencies.as_slice(), &[2]);
        assert_eq!(storage.list_outputs(1).await?.len(), 1);
        assert_eq!(
            storage.get_shared_state("scope/key").await?,
            Some(Bytes::from_static(b"val"))
        );

        Ok(())
    }
}
