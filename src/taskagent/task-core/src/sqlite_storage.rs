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
        Self::migrate_embedded_schema_to_host_shape(&pool).await?;
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
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                public_id TEXT NOT NULL UNIQUE,
                thread_id TEXT,
                root_task_id INTEGER,
                status TEXT NOT NULL CHECK (
                    status IN ('pending', 'accepted', 'running', 'completed', 'failed', 'cancelled')
                ),
                summary TEXT,
                metadata_json TEXT NOT NULL DEFAULT '{}',
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                finished_at_ms INTEGER
            )
            "#,
            "CREATE INDEX IF NOT EXISTS idx_dagger_jobs_thread_status ON dagger_jobs(thread_id, status, updated_at_ms DESC)",
            r#"
            CREATE TABLE IF NOT EXISTS dagger_tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                public_id TEXT NOT NULL UNIQUE,
                job_id INTEGER,
                thread_id TEXT,
                agent_id INTEGER NOT NULL,
                task_type TEXT NOT NULL CHECK (task_type IN ('Objective', 'Story', 'Task', 'Subtask')),
                status TEXT NOT NULL CHECK (
                    status IN (
                        'pending', 'blocked', 'ready', 'accepted', 'running', 'paused',
                        'completed', 'failed', 'rejected', 'cancelled', 'deleted'
                    )
                ),
                durability TEXT NOT NULL CHECK (durability IN ('BestEffort', 'AtMostOnce')),
                retry_count INTEGER NOT NULL DEFAULT 0,
                max_retries INTEGER NOT NULL DEFAULT 3,
                parent_id INTEGER,
                subject TEXT NOT NULL,
                description TEXT NOT NULL,
                acceptance_criteria TEXT,
                owner TEXT,
                metadata_json TEXT NOT NULL DEFAULT '{}',
                source_surface TEXT NOT NULL DEFAULT 'rzn',
                source_kind TEXT NOT NULL DEFAULT 'manual',
                source_public_tool_name TEXT,
                source_thread_id TEXT,
                source_turn_id TEXT,
                source_run_id TEXT,
                source_call_id TEXT,
                timeout_secs INTEGER,
                input_blob BLOB,
                output_blob BLOB,
                output_summary TEXT,
                status_reason TEXT,
                error_blob BLOB,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                started_at_ms INTEGER,
                finished_at_ms INTEGER,
                deleted_at_ms INTEGER,
                FOREIGN KEY (job_id) REFERENCES dagger_jobs(id) ON DELETE SET NULL,
                FOREIGN KEY (parent_id) REFERENCES dagger_tasks(id) ON DELETE SET NULL
            )
            "#,
            "CREATE INDEX IF NOT EXISTS idx_dagger_tasks_thread_status ON dagger_tasks(thread_id, status, updated_at_ms DESC)",
            "CREATE INDEX IF NOT EXISTS idx_dagger_tasks_parent ON dagger_tasks(parent_id)",
            "CREATE INDEX IF NOT EXISTS idx_dagger_tasks_job ON dagger_tasks(job_id)",
            "CREATE INDEX IF NOT EXISTS idx_dagger_tasks_type_status ON dagger_tasks(task_type, status, updated_at_ms DESC)",
            r#"
            CREATE TABLE IF NOT EXISTS dagger_task_dependencies (
                task_id INTEGER NOT NULL,
                depends_on_task_id INTEGER NOT NULL,
                edge_kind TEXT NOT NULL DEFAULT 'blocks',
                created_at_ms INTEGER NOT NULL,
                PRIMARY KEY (task_id, depends_on_task_id, edge_kind),
                FOREIGN KEY (task_id) REFERENCES dagger_tasks(id) ON DELETE CASCADE,
                FOREIGN KEY (depends_on_task_id) REFERENCES dagger_tasks(id) ON DELETE CASCADE
            )
            "#,
            "CREATE INDEX IF NOT EXISTS idx_dagger_task_dependencies_reverse ON dagger_task_dependencies(depends_on_task_id, edge_kind)",
            r#"
            CREATE TABLE IF NOT EXISTS dagger_task_outputs (
                id TEXT PRIMARY KEY,
                task_id INTEGER NOT NULL,
                seq INTEGER NOT NULL,
                channel TEXT NOT NULL CHECK (
                    channel IN ('stdout', 'stderr', 'assistant', 'artifact', 'final')
                ),
                mime_type TEXT,
                content_text TEXT,
                content_blob BLOB,
                metadata_json TEXT NOT NULL DEFAULT '{}',
                created_at_ms INTEGER NOT NULL,
                UNIQUE (task_id, seq),
                FOREIGN KEY (task_id) REFERENCES dagger_tasks(id) ON DELETE CASCADE
            )
            "#,
            "CREATE INDEX IF NOT EXISTS idx_dagger_task_outputs_task_seq ON dagger_task_outputs(task_id, seq)",
            r#"
            CREATE TABLE IF NOT EXISTS dagger_task_events (
                id TEXT PRIMARY KEY,
                task_id INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                from_status TEXT,
                to_status TEXT,
                actor_type TEXT NOT NULL DEFAULT 'agent',
                actor_id TEXT,
                origin_thread_id TEXT,
                origin_turn_id TEXT,
                origin_run_id TEXT,
                payload_json TEXT NOT NULL DEFAULT '{}',
                created_at_ms INTEGER NOT NULL,
                FOREIGN KEY (task_id) REFERENCES dagger_tasks(id) ON DELETE CASCADE
            )
            "#,
            "CREATE INDEX IF NOT EXISTS idx_dagger_task_events_task_created ON dagger_task_events(task_id, created_at_ms DESC)",
            r#"
            CREATE TABLE IF NOT EXISTS dagger_shared_state (
                key TEXT PRIMARY KEY,
                value BLOB NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            )
            "#,
        ];

        for statement in statements {
            sqlx::query(statement).execute(pool).await.map_err(|e| {
                TaskError::Storage(format!("Failed to initialize Dagger schema: {}", e))
            })?;
        }

        Ok(())
    }

    async fn migrate_embedded_schema_to_host_shape(pool: &Pool<Sqlite>) -> Result<()> {
        if !Self::table_exists(pool, "dagger_tasks").await? {
            return Ok(());
        }

        if Self::table_has_column(pool, "dagger_tasks", "created_at_ms").await? {
            return Ok(());
        }

        let old_jobs = if Self::table_exists(pool, "dagger_jobs").await? {
            sqlx::query_as::<_, OldEmbeddedJobRow>(
                "SELECT id, public_id, root_task_id, status, created_at, updated_at FROM dagger_jobs ORDER BY id ASC",
            )
            .fetch_all(pool)
            .await
            .map_err(|e| TaskError::Storage(format!("Failed to read old embedded jobs: {}", e)))?
        } else {
            Vec::new()
        };
        let old_tasks = sqlx::query_as::<_, OldEmbeddedTaskRow>(
            r#"
            SELECT
                id, public_id, job_id, agent_id, status, durability, retry_count, max_retries,
                task_type, parent_id, payload_input, payload_output, timeout_secs, error_data,
                thread_id, subject, description, owner, metadata_json, source_metadata_json,
                acceptance_criteria, status_reason, summary, stop_reason, stop_requested_at,
                deleted_at, deleted_reason, created_at, updated_at
            FROM dagger_tasks
            ORDER BY id ASC
            "#,
        )
        .fetch_all(pool)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to read old embedded tasks: {}", e)))?;
        let old_deps = if Self::table_exists(pool, "dagger_task_dependencies").await? {
            sqlx::query_as::<_, OldEmbeddedDependencyRow>(
                "SELECT task_id, depends_on_task_id, position FROM dagger_task_dependencies ORDER BY task_id ASC, position ASC",
            )
            .fetch_all(pool)
            .await
            .map_err(|e| {
                TaskError::Storage(format!("Failed to read old embedded task dependencies: {}", e))
            })?
        } else {
            Vec::new()
        };
        let old_outputs = if Self::table_exists(pool, "dagger_task_outputs").await? {
            sqlx::query_as::<_, OldEmbeddedOutputRow>(
                "SELECT task_id, sequence, output, created_at FROM dagger_task_outputs ORDER BY task_id ASC, sequence ASC",
            )
            .fetch_all(pool)
            .await
            .map_err(|e| TaskError::Storage(format!("Failed to read old embedded outputs: {}", e)))?
        } else {
            Vec::new()
        };
        let old_events = if Self::table_exists(pool, "dagger_task_events").await? {
            sqlx::query_as::<_, OldEmbeddedEventRow>(
                "SELECT task_id, sequence, event_type, status, reason, payload_json, created_at FROM dagger_task_events ORDER BY task_id ASC, sequence ASC",
            )
            .fetch_all(pool)
            .await
            .map_err(|e| TaskError::Storage(format!("Failed to read old embedded events: {}", e)))?
        } else {
            Vec::new()
        };
        let old_shared_state = if Self::table_exists(pool, "dagger_shared_state").await? {
            sqlx::query_as::<_, OldEmbeddedSharedStateRow>(
                "SELECT key, value, created_at, updated_at FROM dagger_shared_state",
            )
            .fetch_all(pool)
            .await
            .map_err(|e| {
                TaskError::Storage(format!("Failed to read old embedded shared state: {}", e))
            })?
        } else {
            Vec::new()
        };

        let mut tx = pool.begin().await?;
        for table in [
            "dagger_task_events",
            "dagger_task_outputs",
            "dagger_task_dependencies",
            "dagger_tasks",
            "dagger_jobs",
            "dagger_shared_state",
        ] {
            sqlx::query(&format!("DROP TABLE IF EXISTS {}", table))
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    TaskError::Storage(format!("Failed to drop {} during migration: {}", table, e))
                })?;
        }
        tx.commit().await?;

        Self::initialize_schema(pool).await?;

        let mut tx = pool.begin().await?;
        for row in old_jobs {
            let created_at_ms = parse_timestamp(&row.created_at)?.timestamp_millis();
            let updated_at_ms = parse_timestamp(&row.updated_at)?.timestamp_millis();
            sqlx::query(
                r#"
                INSERT INTO dagger_jobs (
                    id, public_id, thread_id, root_task_id, status, summary, metadata_json,
                    created_at_ms, updated_at_ms, finished_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(row.id)
            .bind(row.public_id)
            .bind(Option::<String>::None)
            .bind(row.root_task_id)
            .bind(normalize_status_for_host(&row.status))
            .bind(Option::<String>::None)
            .bind("{}")
            .bind(created_at_ms)
            .bind(updated_at_ms)
            .bind(
                is_terminal_status(normalize_status_for_host(&row.status)).then_some(updated_at_ms),
            )
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                TaskError::Storage(format!("Failed to migrate embedded job {}: {}", row.id, e))
            })?;
        }

        for row in old_tasks {
            let source = row
                .source_metadata_json
                .as_deref()
                .map(|json| serde_json::from_str::<TaskSourceMetadata>(json))
                .transpose()
                .map_err(|e| {
                    TaskError::Serialization(format!(
                        "Failed to deserialize embedded source metadata for task {}: {}",
                        row.id, e
                    ))
                })?;
            let created_at_ms = parse_timestamp(&row.created_at)?.timestamp_millis();
            let updated_at_ms = parse_timestamp(&row.updated_at)?.timestamp_millis();
            let deleted_at_ms = row
                .deleted_at
                .as_deref()
                .map(parse_timestamp)
                .transpose()?
                .map(|ts| ts.timestamp_millis());
            let stop_requested_ms = row
                .stop_requested_at
                .as_deref()
                .map(parse_timestamp)
                .transpose()?
                .map(|ts| ts.timestamp_millis());
            let status = normalize_status_for_host(&row.status);

            sqlx::query(
                r#"
                INSERT INTO dagger_tasks (
                    id, public_id, job_id, thread_id, agent_id, task_type, status, durability,
                    retry_count, max_retries, parent_id, subject, description, acceptance_criteria,
                    owner, metadata_json, source_surface, source_kind, source_public_tool_name,
                    source_thread_id, source_turn_id, source_run_id, source_call_id, timeout_secs,
                    input_blob, output_blob, output_summary, status_reason, error_blob,
                    created_at_ms, updated_at_ms, started_at_ms, finished_at_ms, deleted_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(row.id)
            .bind(row.public_id)
            .bind(Some(row.job_id))
            .bind(row.thread_id.or_else(|| source.as_ref().and_then(|s| s.thread_id.clone())))
            .bind(row.agent_id)
            .bind(row.task_type)
            .bind(if deleted_at_ms.is_some() { "deleted" } else { status })
            .bind(row.durability)
            .bind(row.retry_count)
            .bind(row.max_retries)
            .bind(row.parent_id)
            .bind(row.subject.unwrap_or_else(|| row.description.clone()))
            .bind(row.description)
            .bind(row.acceptance_criteria)
            .bind(row.owner)
            .bind(row.metadata_json)
            .bind(source.as_ref().and_then(|s| s.surface.clone()).unwrap_or_else(|| "rzn".to_string()))
            .bind(if source.as_ref().and_then(|s| s.tool_name.clone()).is_some() { "tool" } else { "manual" })
            .bind(source.as_ref().and_then(|s| s.tool_name.clone()))
            .bind(source.as_ref().and_then(|s| s.thread_id.clone()))
            .bind(source.as_ref().and_then(|s| s.turn_id.clone()))
            .bind(source.as_ref().and_then(|s| s.run_id.clone()))
            .bind(source.as_ref().and_then(|s| s.call_id.clone()))
            .bind(row.timeout_secs)
            .bind(row.payload_input)
            .bind(row.payload_output)
            .bind(row.summary)
            .bind(row.status_reason.or(row.stop_reason).or(row.deleted_reason))
            .bind(row.error_data)
            .bind(created_at_ms)
            .bind(updated_at_ms)
            .bind(matches!(status, "running" | "completed" | "failed" | "cancelled").then_some(stop_requested_ms.unwrap_or(updated_at_ms)))
            .bind(is_terminal_status(status).then_some(updated_at_ms))
            .bind(deleted_at_ms)
            .execute(&mut *tx)
            .await
            .map_err(|e| TaskError::Storage(format!("Failed to migrate embedded task {}: {}", row.id, e)))?;
        }

        for dep in old_deps {
            sqlx::query(
                r#"
                INSERT INTO dagger_task_dependencies (task_id, depends_on_task_id, edge_kind, created_at_ms)
                VALUES (?, ?, 'blocks', ?)
                "#,
            )
            .bind(dep.task_id)
            .bind(dep.depends_on_task_id)
            .bind(dep.position)
            .execute(&mut *tx)
            .await
            .map_err(|e| TaskError::Storage(format!("Failed to migrate embedded dependency edge: {}", e)))?;
        }

        for output in old_outputs {
            let created_at_ms = parse_timestamp(&output.created_at)?.timestamp_millis();
            sqlx::query(
                r#"
                INSERT INTO dagger_task_outputs (
                    id, task_id, seq, channel, mime_type, content_text, content_blob, metadata_json, created_at_ms
                ) VALUES (?, ?, ?, 'final', ?, ?, ?, '{}', ?)
                "#,
            )
            .bind(format!("migrated-output-{}-{}", output.task_id, output.sequence))
            .bind(output.task_id)
            .bind(output.sequence)
            .bind(infer_mime_type(&output.output))
            .bind(bytes_to_optional_text_slice(&output.output))
            .bind(output.output)
            .bind(created_at_ms)
            .execute(&mut *tx)
            .await
            .map_err(|e| TaskError::Storage(format!("Failed to migrate embedded output: {}", e)))?;
        }

        for event in old_events {
            let created_at_ms = parse_timestamp(&event.created_at)?.timestamp_millis();
            let payload_json =
                merge_event_payload(event.payload_json.clone(), event.reason.clone())?;
            sqlx::query(
                r#"
                INSERT INTO dagger_task_events (
                    id, task_id, event_type, from_status, to_status, actor_type, actor_id,
                    origin_thread_id, origin_turn_id, origin_run_id, payload_json, created_at_ms
                ) VALUES (?, ?, ?, ?, ?, 'agent', ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(format!(
                "migrated-event-{}-{}",
                event.task_id, event.sequence
            ))
            .bind(event.task_id)
            .bind(event.event_type)
            .bind(event.payload_json.as_deref().and_then(extract_from_status))
            .bind(event.status.as_deref().map(normalize_status_for_host))
            .bind(Option::<String>::None)
            .bind(Option::<String>::None)
            .bind(Option::<String>::None)
            .bind(Option::<String>::None)
            .bind(payload_json)
            .bind(created_at_ms)
            .execute(&mut *tx)
            .await
            .map_err(|e| TaskError::Storage(format!("Failed to migrate embedded event: {}", e)))?;
        }

        for state in old_shared_state {
            sqlx::query(
                r#"
                INSERT INTO dagger_shared_state (key, value, created_at_ms, updated_at_ms)
                VALUES (?, ?, ?, ?)
                "#,
            )
            .bind(state.key)
            .bind(state.value)
            .bind(parse_timestamp(&state.created_at)?.timestamp_millis())
            .bind(parse_timestamp(&state.updated_at)?.timestamp_millis())
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                TaskError::Storage(format!("Failed to migrate embedded shared state: {}", e))
            })?;
        }

        tx.commit().await?;
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
            let mut dependency_edges: Vec<(TaskId, TaskId, i64)> = Vec::new();
            for row in legacy_rows {
                let task_id = row
                    .id
                    .parse::<TaskId>()
                    .map_err(|_| TaskError::InvalidId(row.id.clone()))?;
                let job_id = row.job_id.parse::<JobId>().unwrap_or(task_id);
                let status = TaskStatus::from_str(&row.status).ok_or(TaskError::InvalidStatus)?;
                let created_at_ms = parse_timestamp(&row.created_at)?.timestamp_millis();
                let updated_at_ms = parse_timestamp(&row.updated_at)?.timestamp_millis();

                Self::upsert_job_tx(
                    &mut tx,
                    job_id,
                    Some(task_id),
                    status,
                    updated_at_ms,
                    None,
                    row.summary.as_deref(),
                )
                .await?;

                sqlx::query(
                    r#"
                    INSERT INTO dagger_tasks (
                        id, public_id, job_id, thread_id, agent_id, task_type, status, durability,
                        retry_count, max_retries, parent_id, subject, description, acceptance_criteria,
                        owner, metadata_json, source_surface, source_kind, source_public_tool_name,
                        source_thread_id, source_turn_id, source_run_id, source_call_id, timeout_secs,
                        input_blob, output_blob, output_summary, status_reason, error_blob,
                        created_at_ms, updated_at_ms, started_at_ms, finished_at_ms, deleted_at_ms
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    "#,
                )
                .bind(task_id as i64)
                .bind(format!("task-{}", task_id))
                .bind(Some(job_id as i64))
                .bind(Option::<String>::None)
                .bind(row.agent_id)
                .bind(row.task_type)
                .bind(normalize_status_for_host(&row.status))
                .bind(row.durability)
                .bind(row.retry_count)
                .bind(row.max_retries)
                .bind(optional_i64_from_text(row.parent_id.as_deref()))
                .bind(Some(row.payload_description.clone()))
                .bind(row.payload_description)
                .bind(row.acceptance_criteria)
                .bind(Option::<String>::None)
                .bind("{}")
                .bind("rzn")
                .bind("manual")
                .bind(Option::<String>::None)
                .bind(Option::<String>::None)
                .bind(Option::<String>::None)
                .bind(Option::<String>::None)
                .bind(Option::<String>::None)
                .bind(row.timeout_secs)
                .bind(row.payload_input)
                .bind(row.payload_output.clone())
                .bind(row.summary)
                .bind(row.status_reason)
                .bind(row.error_data)
                .bind(created_at_ms)
                .bind(updated_at_ms)
                .bind(matches!(normalize_status_for_host(&row.status), "running" | "completed" | "failed" | "cancelled").then_some(updated_at_ms))
                .bind(is_terminal_status(normalize_status_for_host(&row.status)).then_some(updated_at_ms))
                .bind(Option::<i64>::None)
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
                    dependency_edges.push((task_id, *dep_id, position as i64));
                }

                if let Some(output) = row.payload_output {
                    sqlx::query(
                        r#"
                        INSERT OR IGNORE INTO dagger_task_outputs (
                            id, task_id, seq, channel, mime_type, content_text, content_blob, metadata_json, created_at_ms
                        ) VALUES (?, ?, 1, 'final', ?, ?, ?, '{}', ?)
                        "#,
                    )
                    .bind(format!("legacy-output-{}-1", task_id))
                    .bind(task_id as i64)
                    .bind(infer_mime_type(&output))
                    .bind(bytes_to_optional_text_slice(&output))
                    .bind(output)
                    .bind(updated_at_ms)
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

            for (task_id, dep_id, position) in dependency_edges {
                sqlx::query(
                    r#"
                    INSERT OR IGNORE INTO dagger_task_dependencies (task_id, depends_on_task_id, edge_kind, created_at_ms)
                    VALUES (?, ?, 'blocks', ?)
                    "#,
                )
                .bind(task_id as i64)
                .bind(dep_id as i64)
                .bind(position)
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    TaskError::Storage(format!(
                        "Failed to migrate dependency {} -> {}: {}",
                        task_id, dep_id, e
                    ))
                })?;
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
                    INSERT OR IGNORE INTO dagger_shared_state (key, value, created_at_ms, updated_at_ms)
                    VALUES (?, ?, ?, ?)
                    "#,
                )
                .bind(row.key)
                .bind(row.value)
                .bind(parse_timestamp(&row.created_at)?.timestamp_millis())
                .bind(parse_timestamp(&row.updated_at)?.timestamp_millis())
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

    async fn table_has_column(
        pool: &Pool<Sqlite>,
        table_name: &str,
        column_name: &str,
    ) -> Result<bool> {
        let pragma = format!("PRAGMA table_info({})", table_name);
        let rows = sqlx::query(&pragma).fetch_all(pool).await.map_err(|e| {
            TaskError::Storage(format!(
                "Failed to inspect table_info for {}: {}",
                table_name, e
            ))
        })?;
        Ok(rows.into_iter().any(|row| {
            row.try_get::<String, _>("name")
                .map(|name| name == column_name)
                .unwrap_or(false)
        }))
    }

    async fn upsert_job_tx(
        tx: &mut Transaction<'_, Sqlite>,
        job_id: JobId,
        root_task_id: Option<TaskId>,
        status: TaskStatus,
        now_ms: i64,
        thread_id: Option<&str>,
        summary: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO dagger_jobs (
                id, public_id, thread_id, root_task_id, status, summary, metadata_json,
                created_at_ms, updated_at_ms, finished_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, '{}', ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                public_id = excluded.public_id,
                thread_id = COALESCE(excluded.thread_id, dagger_jobs.thread_id),
                root_task_id = COALESCE(dagger_jobs.root_task_id, excluded.root_task_id),
                status = excluded.status,
                summary = COALESCE(excluded.summary, dagger_jobs.summary),
                updated_at_ms = excluded.updated_at_ms,
                finished_at_ms = COALESCE(excluded.finished_at_ms, dagger_jobs.finished_at_ms)
            "#,
        )
        .bind(job_id as i64)
        .bind(format!("job-{}", job_id))
        .bind(thread_id)
        .bind(root_task_id.map(|id| id as i64))
        .bind(normalize_job_status(status.as_str()))
        .bind(summary)
        .bind(now_ms)
        .bind(now_ms)
        .bind(is_terminal_status(normalize_job_status(status.as_str())).then_some(now_ms))
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
            WHERE task_id = ? AND edge_kind = 'blocks'
            ORDER BY created_at_ms ASC, depends_on_task_id ASC
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
        let created_at = timestamp_from_ms(row.created_at_ms);
        let updated_at = timestamp_from_ms(row.updated_at_ms);
        let stop_requested_at = row.stop_requested_at_ms.map(timestamp_from_ms);
        let deleted_at = row.deleted_at_ms.map(timestamp_from_ms);
        let metadata = serde_json::from_str::<Value>(&row.metadata_json).map_err(|e| {
            TaskError::Serialization(format!(
                "Failed to deserialize metadata for task {}: {}",
                row.id, e
            ))
        })?;
        let source = serde_json::from_str::<TaskSourceMetadata>(&row.source_metadata_json)
            .map_err(|e| {
                TaskError::Serialization(format!(
                    "Failed to deserialize source metadata for task {}: {}",
                    row.id, e
                ))
            })?;

        Ok(Task {
            id: row.id as TaskId,
            public_id: Arc::from(row.public_id),
            job: row.job_id.unwrap_or(row.id) as JobId,
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
            subject: Some(Arc::from(row.subject)),
            description: Arc::from(row.description),
            owner: row.owner.map(Arc::from),
            metadata,
            source: normalize_source_metadata(source),
            acceptance_criteria: row.acceptance_criteria.map(Arc::from),
            status_reason: row.status_reason.map(Arc::from),
            summary: row.summary.map(Arc::from),
            stop_reason: row
                .stop_requested_at_ms
                .map(|_| Arc::from("Stop requested")),
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
                task_type, parent_id, COALESCE(input_blob, X'') AS payload_input,
                output_blob AS payload_output, timeout_secs, error_blob AS error_data,
                thread_id, subject, description, owner, metadata_json,
                json_object(
                    'surface', NULLIF(source_surface, ''),
                    'tool_name', source_public_tool_name,
                    'thread_id', source_thread_id,
                    'turn_id', source_turn_id,
                    'run_id', source_run_id,
                    'call_id', source_call_id
                ) AS source_metadata_json,
                acceptance_criteria, status_reason, output_summary AS summary,
                (
                    SELECT MAX(e.created_at_ms)
                    FROM dagger_task_events e
                    WHERE e.task_id = dagger_tasks.id AND e.event_type = 'stop_requested'
                ) AS stop_requested_at_ms,
                deleted_at_ms,
                CASE WHEN status = 'deleted' THEN status_reason END AS deleted_reason,
                created_at_ms, updated_at_ms
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
                task_type, parent_id, COALESCE(input_blob, X'') AS payload_input,
                output_blob AS payload_output, timeout_secs, error_blob AS error_data,
                thread_id, subject, description, owner, metadata_json,
                json_object(
                    'surface', NULLIF(source_surface, ''),
                    'tool_name', source_public_tool_name,
                    'thread_id', source_thread_id,
                    'turn_id', source_turn_id,
                    'run_id', source_run_id,
                    'call_id', source_call_id
                ) AS source_metadata_json,
                acceptance_criteria, status_reason, output_summary AS summary,
                (
                    SELECT MAX(e.created_at_ms)
                    FROM dagger_task_events e
                    WHERE e.task_id = dagger_tasks.id AND e.event_type = 'stop_requested'
                ) AS stop_requested_at_ms,
                deleted_at_ms,
                CASE WHEN status = 'deleted' THEN status_reason END AS deleted_reason,
                created_at_ms, updated_at_ms
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
                INSERT INTO dagger_task_dependencies (task_id, depends_on_task_id, edge_kind, created_at_ms)
                VALUES (?, ?, 'blocks', ?)
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
            "SELECT COALESCE(COUNT(*), 0) + 1 FROM dagger_task_events WHERE task_id = ?",
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

        let created_at_ms = now_ms();
        let payload_json = merge_event_payload(
            event
                .payload
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| {
                    TaskError::Serialization(format!(
                        "Failed to serialize task event payload for task {}: {}",
                        task_id, e
                    ))
                })?,
            event.reason.clone(),
        )?;
        let from_status = event
            .payload
            .as_ref()
            .and_then(|payload| payload.get("from"))
            .and_then(|value| value.as_str())
            .map(normalize_status_for_host);
        let to_status = event
            .status
            .map(|status| normalize_status_for_host(status.as_str()));

        sqlx::query(
            r#"
            INSERT INTO dagger_task_events (
                id, task_id, event_type, from_status, to_status, actor_type, actor_id,
                origin_thread_id, origin_turn_id, origin_run_id, payload_json, created_at_ms
            ) VALUES (?, ?, ?, ?, ?, 'agent', ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(format!("task-event-{}-{}", task_id, next_sequence))
        .bind(task_id as i64)
        .bind(&event.event_type)
        .bind(from_status)
        .bind(to_status)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(payload_json)
        .bind(created_at_ms)
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            TaskError::Storage(format!(
                "Failed to append task event for task {}: {}",
                task_id, e
            ))
        })?;

        Ok(TaskEventRecord {
            sequence: next_sequence as u64,
            event_type: event.event_type.clone(),
            status: event.status,
            reason: event.reason.clone(),
            payload: event.payload.clone(),
            created_at: timestamp_from_ms(created_at_ms),
        })
    }

    async fn append_output_tx(
        tx: &mut Transaction<'_, Sqlite>,
        task_id: TaskId,
        output: Bytes,
    ) -> Result<TaskOutputRecord> {
        let next_sequence: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM dagger_task_outputs WHERE task_id = ?",
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

        let created_at_ms = now_ms();
        let output_vec = output.to_vec();
        let task_updated =
            sqlx::query("UPDATE dagger_tasks SET output_blob = ?, output_summary = ?, updated_at_ms = ? WHERE id = ?")
                .bind(output_vec.clone())
                .bind(bytes_to_optional_text(&output))
                .bind(created_at_ms)
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
            INSERT INTO dagger_task_outputs (
                id, task_id, seq, channel, mime_type, content_text, content_blob, metadata_json, created_at_ms
            ) VALUES (?, ?, ?, 'final', ?, ?, ?, '{}', ?)
            "#,
        )
        .bind(format!("task-output-{}-{}", task_id, next_sequence))
        .bind(task_id as i64)
        .bind(next_sequence)
        .bind(infer_mime_type(&output))
        .bind(bytes_to_optional_text(&output))
        .bind(output_vec)
        .bind(created_at_ms)
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
            created_at: timestamp_from_ms(created_at_ms),
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
        let now_ms = now_ms();
        let output_lock = task
            .payload
            .output
            .try_read()
            .map_err(|_| TaskError::Concurrency("Failed to read task output".into()))?;
        let metadata_json = serde_json::to_string(&task.metadata).map_err(|e| {
            TaskError::Serialization(format!("Failed to serialize task metadata: {}", e))
        })?;
        let source = task.source.clone().unwrap_or_default();
        let status = normalize_status_for_host(task.status().as_str());
        let deleted_at_ms = task.deleted_at.map(|ts| ts.timestamp_millis());
        let subject = task.subject.as_deref().unwrap_or(task.description.as_ref());
        let output_summary = task.summary.as_ref().map(|v| v.as_ref());

        Self::upsert_job_tx(
            &mut tx,
            task.job,
            task.parent.is_none().then_some(task.id),
            task.status(),
            now_ms,
            task.thread_id.as_deref().map(|v| v.as_ref()),
            output_summary,
        )
        .await?;

        sqlx::query(
            r#"
            INSERT INTO dagger_tasks (
                id, public_id, job_id, thread_id, agent_id, task_type, status, durability,
                retry_count, max_retries, parent_id, subject, description, acceptance_criteria,
                owner, metadata_json, source_surface, source_kind, source_public_tool_name,
                source_thread_id, source_turn_id, source_run_id, source_call_id, timeout_secs,
                input_blob, output_blob, output_summary, status_reason, error_blob,
                created_at_ms, updated_at_ms, started_at_ms, finished_at_ms, deleted_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                public_id = excluded.public_id,
                job_id = excluded.job_id,
                thread_id = excluded.thread_id,
                agent_id = excluded.agent_id,
                task_type = excluded.task_type,
                status = excluded.status,
                durability = excluded.durability,
                retry_count = excluded.retry_count,
                max_retries = excluded.max_retries,
                parent_id = excluded.parent_id,
                subject = excluded.subject,
                description = excluded.description,
                acceptance_criteria = excluded.acceptance_criteria,
                owner = excluded.owner,
                metadata_json = excluded.metadata_json,
                source_surface = excluded.source_surface,
                source_kind = excluded.source_kind,
                source_public_tool_name = excluded.source_public_tool_name,
                source_thread_id = excluded.source_thread_id,
                source_turn_id = excluded.source_turn_id,
                source_run_id = excluded.source_run_id,
                source_call_id = excluded.source_call_id,
                timeout_secs = excluded.timeout_secs,
                input_blob = excluded.input_blob,
                output_blob = excluded.output_blob,
                output_summary = excluded.output_summary,
                status_reason = excluded.status_reason,
                error_blob = excluded.error_blob,
                updated_at_ms = excluded.updated_at_ms,
                started_at_ms = COALESCE(dagger_tasks.started_at_ms, excluded.started_at_ms),
                finished_at_ms = COALESCE(excluded.finished_at_ms, dagger_tasks.finished_at_ms),
                deleted_at_ms = COALESCE(excluded.deleted_at_ms, dagger_tasks.deleted_at_ms)
            "#,
        )
        .bind(task.id as i64)
        .bind(task.public_id.as_ref())
        .bind(Some(task.job as i64))
        .bind(task.thread_id.as_ref().map(|v| v.as_ref()))
        .bind(task.agent as i64)
        .bind(match task.task_type {
            TaskType::Objective => "Objective",
            TaskType::Story => "Story",
            TaskType::Task => "Task",
            TaskType::Subtask => "Subtask",
        })
        .bind(if deleted_at_ms.is_some() {
            "deleted"
        } else {
            status
        })
        .bind(match task.durability {
            Durability::BestEffort => "BestEffort",
            Durability::AtMostOnce => "AtMostOnce",
        })
        .bind(task.retry_count.load(Ordering::Relaxed) as i64)
        .bind(task.max_retries as i64)
        .bind(task.parent.map(|id| id as i64))
        .bind(subject)
        .bind(task.description.as_ref())
        .bind(task.acceptance_criteria.as_ref().map(|v| v.as_ref()))
        .bind(task.owner.as_ref().map(|v| v.as_ref()))
        .bind(metadata_json)
        .bind(source.surface.as_deref().unwrap_or("rzn"))
        .bind(if source.tool_name.is_some() { "tool" } else { "manual" })
        .bind(source.tool_name.as_deref())
        .bind(source.thread_id.as_deref())
        .bind(source.turn_id.as_deref())
        .bind(source.run_id.as_deref())
        .bind(source.call_id.as_deref())
        .bind(task.timeout.map(|timeout| timeout.as_secs() as i64))
        .bind(task.payload.input.to_vec())
        .bind(output_lock.as_ref().map(|bytes| bytes.to_vec()))
        .bind(output_summary)
        .bind(task.status_reason.as_ref().map(|v| v.as_ref()))
        .bind(task.error.as_ref().map(|bytes| bytes.to_vec()))
        .bind(task.created_at.timestamp_millis())
        .bind(now_ms)
        .bind(matches!(status, "running" | "completed" | "failed" | "cancelled").then_some(now_ms))
        .bind(is_terminal_status(status).then_some(now_ms))
        .bind(deleted_at_ms)
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
        let now_ms = now_ms();
        let result = sqlx::query(
            r#"
            UPDATE dagger_tasks
            SET
                status = ?,
                updated_at_ms = ?,
                started_at_ms = CASE
                    WHEN ? = 'running' THEN COALESCE(started_at_ms, ?)
                    ELSE started_at_ms
                END,
                finished_at_ms = CASE
                    WHEN ? IN ('completed', 'failed', 'cancelled', 'deleted') THEN COALESCE(finished_at_ms, ?)
                    ELSE finished_at_ms
                END,
                deleted_at_ms = CASE
                    WHEN ? = 'deleted' THEN COALESCE(deleted_at_ms, ?)
                    ELSE deleted_at_ms
                END
            WHERE id = ? AND status = ?
            "#,
        )
        .bind(normalize_status_for_host(new.as_str()))
        .bind(now_ms)
        .bind(normalize_status_for_host(new.as_str()))
        .bind(now_ms)
        .bind(normalize_status_for_host(new.as_str()))
        .bind(now_ms)
        .bind(normalize_status_for_host(new.as_str()))
        .bind(now_ms)
        .bind(id as i64)
        .bind(normalize_status_for_host(old.as_str()))
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
            "SELECT id FROM dagger_tasks WHERE status = 'running' AND deleted_at_ms IS NULL",
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
                task_type, parent_id, COALESCE(input_blob, X'') AS payload_input,
                output_blob AS payload_output, timeout_secs, error_blob AS error_data,
                thread_id, subject, description, owner, metadata_json,
                json_object(
                    'surface', NULLIF(source_surface, ''),
                    'tool_name', source_public_tool_name,
                    'thread_id', source_thread_id,
                    'turn_id', source_turn_id,
                    'run_id', source_run_id,
                    'call_id', source_call_id
                ) AS source_metadata_json,
                acceptance_criteria, status_reason, output_summary AS summary,
                (
                    SELECT MAX(e.created_at_ms)
                    FROM dagger_task_events e
                    WHERE e.task_id = dagger_tasks.id AND e.event_type = 'stop_requested'
                ) AS stop_requested_at_ms,
                deleted_at_ms,
                CASE WHEN status = 'deleted' THEN status_reason END AS deleted_reason,
                created_at_ms, updated_at_ms
            FROM dagger_tasks
            ORDER BY created_at_ms ASC, id ASC
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
                    task_type, parent_id, COALESCE(input_blob, X'') AS payload_input,
                    output_blob AS payload_output, timeout_secs, error_blob AS error_data,
                    thread_id, subject, description, owner, metadata_json,
                    json_object(
                        'surface', NULLIF(source_surface, ''),
                        'tool_name', source_public_tool_name,
                        'thread_id', source_thread_id,
                        'turn_id', source_turn_id,
                        'run_id', source_run_id,
                        'call_id', source_call_id
                    ) AS source_metadata_json,
                    acceptance_criteria, status_reason, output_summary AS summary,
                    (
                        SELECT MAX(e.created_at_ms)
                        FROM dagger_task_events e
                        WHERE e.task_id = dagger_tasks.id AND e.event_type = 'stop_requested'
                    ) AS stop_requested_at_ms,
                    deleted_at_ms,
                    CASE WHEN status = 'deleted' THEN status_reason END AS deleted_reason,
                    created_at_ms, updated_at_ms
                FROM dagger_tasks
                WHERE job_id = ?
                ORDER BY created_at_ms ASC, id ASC
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
                    task_type, parent_id, COALESCE(input_blob, X'') AS payload_input,
                    output_blob AS payload_output, timeout_secs, error_blob AS error_data,
                    thread_id, subject, description, owner, metadata_json,
                    json_object(
                        'surface', NULLIF(source_surface, ''),
                        'tool_name', source_public_tool_name,
                        'thread_id', source_thread_id,
                        'turn_id', source_turn_id,
                        'run_id', source_run_id,
                        'call_id', source_call_id
                    ) AS source_metadata_json,
                    acceptance_criteria, status_reason, output_summary AS summary,
                    (
                        SELECT MAX(e.created_at_ms)
                        FROM dagger_task_events e
                        WHERE e.task_id = dagger_tasks.id AND e.event_type = 'stop_requested'
                    ) AS stop_requested_at_ms,
                    deleted_at_ms,
                    CASE WHEN status = 'deleted' THEN status_reason END AS deleted_reason,
                    created_at_ms, updated_at_ms
                FROM dagger_tasks
                WHERE thread_id = ?
                ORDER BY created_at_ms ASC, id ASC
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
                    task_type, parent_id, COALESCE(input_blob, X'') AS payload_input,
                    output_blob AS payload_output, timeout_secs, error_blob AS error_data,
                    thread_id, subject, description, owner, metadata_json,
                    json_object(
                        'surface', NULLIF(source_surface, ''),
                        'tool_name', source_public_tool_name,
                        'thread_id', source_thread_id,
                        'turn_id', source_turn_id,
                        'run_id', source_run_id,
                        'call_id', source_call_id
                    ) AS source_metadata_json,
                    acceptance_criteria, status_reason, output_summary AS summary,
                    (
                        SELECT MAX(e.created_at_ms)
                        FROM dagger_task_events e
                        WHERE e.task_id = dagger_tasks.id AND e.event_type = 'stop_requested'
                    ) AS stop_requested_at_ms,
                    deleted_at_ms,
                    CASE WHEN status = 'deleted' THEN status_reason END AS deleted_reason,
                    created_at_ms, updated_at_ms
                FROM dagger_tasks
                WHERE parent_id = ?
                ORDER BY created_at_ms ASC, id ASC
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
                    t.retry_count, t.max_retries, t.task_type, t.parent_id,
                    COALESCE(t.input_blob, X'') AS payload_input,
                    t.output_blob AS payload_output, t.timeout_secs, t.error_blob AS error_data,
                    t.thread_id, t.subject, t.description, t.owner, t.metadata_json,
                    json_object(
                        'surface', NULLIF(t.source_surface, ''),
                        'tool_name', t.source_public_tool_name,
                        'thread_id', t.source_thread_id,
                        'turn_id', t.source_turn_id,
                        'run_id', t.source_run_id,
                        'call_id', t.source_call_id
                    ) AS source_metadata_json,
                    t.acceptance_criteria, t.status_reason, t.output_summary AS summary,
                    (
                        SELECT MAX(e.created_at_ms)
                        FROM dagger_task_events e
                        WHERE e.task_id = t.id AND e.event_type = 'stop_requested'
                    ) AS stop_requested_at_ms,
                    t.deleted_at_ms,
                    CASE WHEN t.status = 'deleted' THEN t.status_reason END AS deleted_reason,
                    t.created_at_ms, t.updated_at_ms
                FROM dagger_tasks t
                JOIN dagger_task_dependencies d
                  ON d.depends_on_task_id = t.id
                WHERE d.task_id = ? AND d.edge_kind = 'blocks'
                ORDER BY d.created_at_ms ASC, t.id ASC
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
                    t.retry_count, t.max_retries, t.task_type, t.parent_id,
                    COALESCE(t.input_blob, X'') AS payload_input,
                    t.output_blob AS payload_output, t.timeout_secs, t.error_blob AS error_data,
                    t.thread_id, t.subject, t.description, t.owner, t.metadata_json,
                    json_object(
                        'surface', NULLIF(t.source_surface, ''),
                        'tool_name', t.source_public_tool_name,
                        'thread_id', t.source_thread_id,
                        'turn_id', t.source_turn_id,
                        'run_id', t.source_run_id,
                        'call_id', t.source_call_id
                    ) AS source_metadata_json,
                    t.acceptance_criteria, t.status_reason, t.output_summary AS summary,
                    (
                        SELECT MAX(e.created_at_ms)
                        FROM dagger_task_events e
                        WHERE e.task_id = t.id AND e.event_type = 'stop_requested'
                    ) AS stop_requested_at_ms,
                    t.deleted_at_ms,
                    CASE WHEN t.status = 'deleted' THEN t.status_reason END AS deleted_reason,
                    t.created_at_ms, t.updated_at_ms
                FROM dagger_tasks t
                JOIN dagger_task_dependencies d
                  ON d.task_id = t.id
                WHERE d.depends_on_task_id = ? AND d.edge_kind = 'blocks'
                ORDER BY t.created_at_ms ASC, t.id ASC
                "#,
            )
            .bind(task_id as i64),
        )
        .await
    }

    async fn get_output(&self, id: TaskId) -> Result<Option<Bytes>> {
        let row = sqlx::query("SELECT output_blob FROM dagger_tasks WHERE id = ?")
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| {
                TaskError::Storage(format!("Failed to get output for task {}: {}", id, e))
            })?;

        Ok(row
            .and_then(|r| r.try_get::<Option<Vec<u8>>, _>("output_blob").ok())
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
            SELECT seq, content_text, content_blob, created_at_ms
            FROM dagger_task_outputs
            WHERE task_id = ?
            ORDER BY seq ASC
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
            let content_blob = row
                .try_get::<Option<Vec<u8>>, _>("content_blob")
                .map_err(TaskError::Sqlite)?;
            let content_text = row
                .try_get::<Option<String>, _>("content_text")
                .map_err(TaskError::Sqlite)?;
            records.push(TaskOutputRecord {
                sequence: row.try_get::<i64, _>("seq").map_err(TaskError::Sqlite)? as u64,
                output: match (content_blob, content_text) {
                    (Some(blob), _) => Bytes::from(blob),
                    (None, Some(text)) => Bytes::from(text),
                    (None, None) => Bytes::new(),
                },
                created_at: timestamp_from_ms(
                    row.try_get::<i64, _>("created_at_ms")
                        .map_err(TaskError::Sqlite)?,
                ),
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
            SELECT event_type, from_status, to_status, payload_json, created_at_ms
            FROM dagger_task_events
            WHERE task_id = ?
            ORDER BY created_at_ms ASC, id ASC
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
            let payload_json = row
                .try_get::<String, _>("payload_json")
                .map_err(TaskError::Sqlite)?;
            let payload = serde_json::from_str::<Value>(&payload_json).map_err(|e| {
                TaskError::Serialization(format!(
                    "Failed to deserialize event payload for task {}: {}",
                    task_id, e
                ))
            })?;
            records.push(TaskEventRecord {
                sequence: records.len() as u64 + 1,
                event_type: row
                    .try_get::<String, _>("event_type")
                    .map_err(TaskError::Sqlite)?,
                status: row
                    .try_get::<Option<String>, _>("to_status")
                    .map_err(TaskError::Sqlite)?
                    .as_deref()
                    .and_then(TaskStatus::from_str),
                reason: payload
                    .get("reason")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
                payload: Some(payload),
                created_at: timestamp_from_ms(
                    row.try_get::<i64, _>("created_at_ms")
                        .map_err(TaskError::Sqlite)?,
                ),
            });
        }

        Ok(records)
    }

    async fn request_stop(&self, id: TaskId, reason: Option<&str>) -> Result<Task> {
        let current_task = self.get(id).await?.ok_or(TaskError::TaskNotFound(id))?;
        let current_status = current_task.status();
        let new_status = match current_status {
            TaskStatus::Pending
            | TaskStatus::Blocked
            | TaskStatus::Ready
            | TaskStatus::Accepted
            | TaskStatus::Paused => TaskStatus::Cancelled,
            TaskStatus::Running => TaskStatus::Running,
            _ => current_status,
        };

        let reason_text = reason
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "Task stop requested".to_string());
        let now_ms = now_ms();

        let mut tx = self.pool.begin().await?;
        if new_status != current_status {
            sqlx::query(
                r#"
                UPDATE dagger_tasks
                SET
                    status = ?,
                    status_reason = ?,
                    updated_at_ms = ?,
                    finished_at_ms = CASE
                        WHEN ? IN ('cancelled', 'deleted') THEN COALESCE(finished_at_ms, ?)
                        ELSE finished_at_ms
                    END
                WHERE id = ?
                "#,
            )
            .bind(normalize_status_for_host(new_status.as_str()))
            .bind(&reason_text)
            .bind(now_ms)
            .bind(normalize_status_for_host(new_status.as_str()))
            .bind(now_ms)
            .bind(id as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                TaskError::Storage(format!("Failed to request stop for task {}: {}", id, e))
            })?;
        } else {
            sqlx::query(
                "UPDATE dagger_tasks SET status_reason = ?, updated_at_ms = ? WHERE id = ?",
            )
            .bind(&reason_text)
            .bind(now_ms)
            .bind(id as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                TaskError::Storage(format!(
                    "Failed to record stop request for task {}: {}",
                    id, e
                ))
            })?;
        }

        let event = NewTaskEvent {
            event_type: "stop_requested".to_string(),
            status: Some(if new_status == current_status {
                current_status
            } else {
                new_status
            }),
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
        let reason_text = reason
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "Task soft deleted".to_string());
        let now_ms = now_ms();

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            UPDATE dagger_tasks
            SET
                status = 'deleted',
                status_reason = ?,
                deleted_at_ms = COALESCE(deleted_at_ms, ?),
                finished_at_ms = COALESCE(finished_at_ms, ?),
                updated_at_ms = ?
            WHERE id = ?
            "#,
        )
        .bind(&reason_text)
        .bind(now_ms)
        .bind(now_ms)
        .bind(now_ms)
        .bind(id as i64)
        .execute(&mut *tx)
        .await
        .map_err(|e| TaskError::Storage(format!("Failed to soft delete task {}: {}", id, e)))?;

        let event = NewTaskEvent {
            event_type: "task_deleted".to_string(),
            status: Some(TaskStatus::Deleted),
            reason: Some(reason_text),
            payload: Some(serde_json::json!({ "deleted_at_ms": now_ms })),
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
        let now_ms = now_ms();
        sqlx::query(
            r#"
            INSERT INTO dagger_shared_state (key, value, created_at_ms, updated_at_ms)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at_ms = excluded.updated_at_ms
            "#,
        )
        .bind(key)
        .bind(value.to_vec())
        .bind(now_ms)
        .bind(now_ms)
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
    job_id: Option<i64>,
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
    subject: String,
    description: String,
    owner: Option<String>,
    metadata_json: String,
    source_metadata_json: String,
    acceptance_criteria: Option<String>,
    status_reason: Option<String>,
    summary: Option<String>,
    stop_requested_at_ms: Option<i64>,
    deleted_at_ms: Option<i64>,
    deleted_reason: Option<String>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct OldEmbeddedJobRow {
    id: i64,
    public_id: String,
    root_task_id: Option<i64>,
    status: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, sqlx::FromRow)]
struct OldEmbeddedTaskRow {
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
struct OldEmbeddedDependencyRow {
    task_id: i64,
    depends_on_task_id: i64,
    position: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct OldEmbeddedOutputRow {
    task_id: i64,
    sequence: i64,
    output: Vec<u8>,
    created_at: String,
}

#[derive(Debug, sqlx::FromRow)]
struct OldEmbeddedEventRow {
    task_id: i64,
    sequence: i64,
    event_type: String,
    status: Option<String>,
    reason: Option<String>,
    payload_json: Option<String>,
    created_at: String,
}

#[derive(Debug, sqlx::FromRow)]
struct OldEmbeddedSharedStateRow {
    key: String,
    value: Vec<u8>,
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

fn timestamp_from_ms(value: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(value).unwrap_or_else(Utc::now)
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn optional_i64_from_text(value: Option<&str>) -> Option<i64> {
    value.and_then(|raw| raw.parse::<i64>().ok())
}

fn normalize_status_for_host(value: &str) -> &str {
    match value {
        "cancelling" => "running",
        other => other,
    }
}

fn normalize_job_status(value: &str) -> &str {
    match value {
        "ready" | "blocked" | "paused" | "rejected" | "accepted" => "accepted",
        "deleted" => "cancelled",
        "cancelling" => "running",
        other => other,
    }
}

fn is_terminal_status(value: &str) -> bool {
    matches!(value, "completed" | "failed" | "cancelled" | "deleted")
}

fn infer_mime_type(output: &[u8]) -> Option<&'static str> {
    if std::str::from_utf8(output).is_ok() {
        Some("text/plain")
    } else {
        Some("application/octet-stream")
    }
}

fn bytes_to_optional_text(output: &Bytes) -> Option<String> {
    std::str::from_utf8(output).ok().map(ToOwned::to_owned)
}

fn bytes_to_optional_text_slice(output: &[u8]) -> Option<String> {
    std::str::from_utf8(output).ok().map(ToOwned::to_owned)
}

fn merge_event_payload(payload_json: Option<String>, reason: Option<String>) -> Result<String> {
    let mut payload = match payload_json {
        Some(json) => {
            serde_json::from_str::<Value>(&json).unwrap_or_else(|_| serde_json::json!({}))
        }
        None => serde_json::json!({}),
    };

    if let Some(reason) = reason {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("reason".to_string(), Value::String(reason));
        } else {
            payload = serde_json::json!({ "reason": reason });
        }
    }

    serde_json::to_string(&payload).map_err(|e| {
        TaskError::Serialization(format!("Failed to serialize merged event payload: {}", e))
    })
}

fn extract_from_status(payload_json: &str) -> Option<String> {
    serde_json::from_str::<Value>(payload_json)
        .ok()
        .and_then(|payload| {
            payload
                .get("from")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned)
        })
}

fn normalize_source_metadata(source: TaskSourceMetadata) -> Option<TaskSourceMetadata> {
    let has_value = source.surface.is_some()
        || source.tool_name.is_some()
        || source.thread_id.is_some()
        || source.turn_id.is_some()
        || source.run_id.is_some()
        || source.call_id.is_some();
    has_value.then_some(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{NewTaskSpec, TaskStatus, TaskType},
        storage::Storage,
    };
    use sqlx::Row;
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

    async fn column_names(pool: &SqlitePool, table: &str) -> Result<Vec<String>> {
        let pragma = format!("PRAGMA table_info({})", table);
        sqlx::query(&pragma)
            .fetch_all(pool)
            .await
            .map_err(TaskError::Sqlite)?
            .into_iter()
            .map(|row| row.try_get::<String, _>("name").map_err(TaskError::Sqlite))
            .collect()
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
        assert_eq!(stopped.status(), TaskStatus::Running);
        assert!(stopped.stop_requested_at.is_some());

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
    async fn test_host_schema_matches_embedded_contract() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let db_path = temp_dir.path().join("host-shape.db");
        let pool = connect_file_pool(&db_path).await?;

        SqliteStorage::open_with_pool(pool.clone()).await?;

        let task_columns = column_names(&pool, "dagger_tasks").await?;
        assert!(task_columns.contains(&"created_at_ms".to_string()));
        assert!(task_columns.contains(&"updated_at_ms".to_string()));
        assert!(task_columns.contains(&"input_blob".to_string()));
        assert!(task_columns.contains(&"output_blob".to_string()));
        assert!(task_columns.contains(&"output_summary".to_string()));
        assert!(task_columns.contains(&"source_surface".to_string()));
        assert!(task_columns.contains(&"source_public_tool_name".to_string()));
        assert!(!task_columns.contains(&"created_at".to_string()));
        assert!(!task_columns.contains(&"updated_at".to_string()));
        assert!(!task_columns.contains(&"payload_input".to_string()));
        assert!(!task_columns.contains(&"payload_output".to_string()));

        let output_columns = column_names(&pool, "dagger_task_outputs").await?;
        assert!(output_columns.contains(&"seq".to_string()));
        assert!(output_columns.contains(&"content_text".to_string()));
        assert!(output_columns.contains(&"content_blob".to_string()));
        assert!(output_columns.contains(&"created_at_ms".to_string()));
        assert!(!output_columns.contains(&"sequence".to_string()));
        assert!(!output_columns.contains(&"output".to_string()));

        let event_columns = column_names(&pool, "dagger_task_events").await?;
        assert!(event_columns.contains(&"from_status".to_string()));
        assert!(event_columns.contains(&"to_status".to_string()));
        assert!(event_columns.contains(&"payload_json".to_string()));
        assert!(event_columns.contains(&"created_at_ms".to_string()));
        assert!(!event_columns.contains(&"status".to_string()));
        assert!(!event_columns.contains(&"reason".to_string()));

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

    #[tokio::test]
    async fn test_embedded_schema_is_migrated_to_host_shape() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let db_path = temp_dir.path().join("embedded-legacy.db");
        let pool = connect_file_pool(&db_path).await?;

        sqlx::query(
            r#"
            CREATE TABLE dagger_jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                public_id TEXT NOT NULL UNIQUE,
                root_task_id INTEGER,
                status TEXT NOT NULL,
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
            CREATE TABLE dagger_tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                public_id TEXT NOT NULL UNIQUE,
                job_id INTEGER NOT NULL,
                agent_id INTEGER NOT NULL,
                status TEXT NOT NULL,
                durability TEXT NOT NULL,
                retry_count INTEGER NOT NULL DEFAULT 0,
                max_retries INTEGER NOT NULL DEFAULT 3,
                task_type TEXT NOT NULL,
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
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        sqlx::query(
            r#"
            CREATE TABLE dagger_task_dependencies (
                task_id INTEGER NOT NULL,
                depends_on_task_id INTEGER NOT NULL,
                position INTEGER NOT NULL,
                PRIMARY KEY (task_id, depends_on_task_id)
            )
            "#,
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        sqlx::query(
            r#"
            CREATE TABLE dagger_task_outputs (
                task_id INTEGER NOT NULL,
                sequence INTEGER NOT NULL,
                output BLOB NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (task_id, sequence)
            )
            "#,
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        sqlx::query(
            r#"
            CREATE TABLE dagger_task_events (
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
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        sqlx::query(
            r#"
            CREATE TABLE dagger_shared_state (
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
            INSERT INTO dagger_jobs (id, public_id, root_task_id, status, created_at, updated_at)
            VALUES (1, 'job-1', 1, 'running', '2026-04-03 10:00:00.000', '2026-04-03 10:00:03.000')
            "#,
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        sqlx::query(
            r#"
            INSERT INTO dagger_tasks (
                id, public_id, job_id, agent_id, status, durability, retry_count, max_retries,
                task_type, parent_id, payload_input, payload_output, timeout_secs, error_data,
                thread_id, subject, description, owner, metadata_json, source_metadata_json,
                acceptance_criteria, status_reason, summary, stop_reason, stop_requested_at,
                deleted_at, deleted_reason, created_at, updated_at
            ) VALUES (
                1, 'task-old-1', 1, 1, 'cancelling', 'BestEffort', 0, 3,
                'Task', NULL, X'6869', X'6f7574', NULL, NULL,
                'thread-old', 'old subject', 'old description', 'owner-1', '{"scope":"test"}',
                '{"surface":"codex","tool_name":"update_plan","thread_id":"thread-old","turn_id":"turn-1","run_id":"run-1","call_id":"call-1"}',
                'ship it', 'waiting', 'old summary', 'please stop', '2026-04-03 10:00:02.000',
                NULL, NULL, '2026-04-03 10:00:00.000', '2026-04-03 10:00:03.000'
            )
            "#,
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        sqlx::query(
            r#"
            INSERT INTO dagger_tasks (
                id, public_id, job_id, agent_id, status, durability, retry_count, max_retries,
                task_type, parent_id, payload_input, payload_output, timeout_secs, error_data,
                thread_id, subject, description, owner, metadata_json, source_metadata_json,
                acceptance_criteria, status_reason, summary, stop_reason, stop_requested_at,
                deleted_at, deleted_reason, created_at, updated_at
            ) VALUES (
                2, 'task-old-2', 1, 1, 'completed', 'BestEffort', 0, 3,
                'Task', NULL, X'626965', NULL, NULL, NULL,
                'thread-old', 'dependency', 'dependency description', NULL, '{}',
                NULL, NULL, NULL, NULL, NULL, NULL,
                NULL, NULL, '2026-04-03 10:00:00.000', '2026-04-03 10:00:03.000'
            )
            "#,
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        sqlx::query(
            "INSERT INTO dagger_task_dependencies (task_id, depends_on_task_id, position) VALUES (1, 2, 7)",
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        sqlx::query(
            r#"
            INSERT INTO dagger_task_outputs (task_id, sequence, output, created_at)
            VALUES (1, 1, X'6f7574', '2026-04-03 10:00:03.000')
            "#,
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        sqlx::query(
            r#"
            INSERT INTO dagger_task_events (task_id, sequence, event_type, status, reason, payload_json, created_at)
            VALUES (1, 1, 'status_change', 'running', 'please stop', '{"from":"accepted"}', '2026-04-03 10:00:02.000')
            "#,
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;
        sqlx::query(
            r#"
            INSERT INTO dagger_shared_state (key, value, created_at, updated_at)
            VALUES ('scope/key', X'76616c', '2026-04-03 10:00:00.000', '2026-04-03 10:00:03.000')
            "#,
        )
        .execute(&pool)
        .await
        .map_err(TaskError::Sqlite)?;

        let storage = SqliteStorage::open_with_pool(pool.clone()).await?;
        let migrated = storage.get(1).await?.expect("migrated embedded task");
        assert_eq!(migrated.public_id.as_ref(), "task-old-1");
        assert_eq!(
            migrated.thread_id.as_deref().map(|value| value.as_ref()),
            Some("thread-old")
        );
        assert_eq!(
            migrated
                .source
                .as_ref()
                .and_then(|source| source.surface.as_deref()),
            Some("codex")
        );
        assert_eq!(
            migrated
                .source
                .as_ref()
                .and_then(|source| source.tool_name.as_deref()),
            Some("update_plan")
        );
        assert_eq!(migrated.dependencies.as_slice(), &[2]);
        assert!(migrated.stop_requested_at.is_some());
        assert_eq!(
            storage.get_output(1).await?,
            Some(Bytes::from_static(b"out"))
        );
        assert_eq!(storage.list_outputs(1).await?.len(), 1);
        assert_eq!(storage.list_events(1).await?.len(), 1);
        assert_eq!(
            storage.get_shared_state("scope/key").await?,
            Some(Bytes::from_static(b"val"))
        );

        let task_columns = column_names(&pool, "dagger_tasks").await?;
        assert!(task_columns.contains(&"created_at_ms".to_string()));
        assert!(task_columns.contains(&"input_blob".to_string()));
        assert!(!task_columns.contains(&"created_at".to_string()));
        assert!(!task_columns.contains(&"payload_input".to_string()));

        Ok(())
    }
}
