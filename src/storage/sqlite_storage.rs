//! SQLite storage implementation for Dagger
//! 
//! This module provides a SQLite-based storage backend for the Dagger workflow engine,
//! including database initialization, connection management, and helper methods for
//! artifact storage.

use sqlx::{SqlitePool, Row, Error as SqlxError, sqlite::SqliteRow};
use serde_json::{Value, to_string as json_to_string, from_str as json_from_str};
use chrono::{DateTime, Utc, ParseError};
use std::path::{Path, PathBuf};
use std::fs;
use thiserror::Error;
use uuid::Uuid;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{
    FlowRun, NodeRun, Artifact, OutboxEvent, Task, TaskDependency, SharedState,
    FlowRunStatus, NodeRunStatus
};

/// Storage errors
#[derive(Error, Debug)]
pub enum StorageError {
    #[error("Database error: {0}")]
    Database(#[from] SqlxError),
    
    #[error("JSON serialization error: {0}")]
    JsonSerialization(#[from] serde_json::Error),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Date parse error: {0}")]
    ParseError(#[from] ParseError),
    
    #[error("Invalid status: {0}")]
    InvalidStatus(String),
    
    #[error("Artifact not found: {0}")]
    ArtifactNotFound(String),
    
    #[error("File operation error: {0}")]
    FileOperation(String),
    
    #[error("Invalid configuration: {0}")]
    Configuration(String),
}

/// Configuration for artifact storage
#[derive(Debug, Clone)]
pub struct ArtifactStorage {
    /// Directory to store large artifact files
    pub artifact_dir: PathBuf,
    /// Maximum size for inline storage (in bytes)
    pub inline_threshold: usize,
}

impl Default for ArtifactStorage {
    fn default() -> Self {
        Self {
            artifact_dir: PathBuf::from("artifacts"),
            inline_threshold: 1024 * 1024, // 1MB
        }
    }
}

/// SQLite storage implementation
pub struct SqliteStorage {
    pool: SqlitePool,
    artifact_config: ArtifactStorage,
}

impl SqliteStorage {
    /// Create a new SQLite storage instance
    /// 
    /// # Arguments
    /// * `database_url` - SQLite database URL (e.g., "sqlite:dagger.db")
    /// * `artifact_config` - Configuration for artifact storage
    pub async fn new(database_url: &str, artifact_config: Option<ArtifactStorage>) -> Result<Self, StorageError> {
        let pool = SqlitePool::connect(database_url).await?;
        let artifact_config = artifact_config.unwrap_or_default();
        
        // Create artifact directory if it doesn't exist
        if !artifact_config.artifact_dir.exists() {
            fs::create_dir_all(&artifact_config.artifact_dir)?;
        }
        
        let storage = Self {
            pool,
            artifact_config,
        };
        
        // Initialize the database schema
        storage.initialize_schema().await?;
        
        Ok(storage)
    }
    
    /// Initialize the database schema from the embedded SQL file
    pub async fn initialize_schema(&self) -> Result<(), StorageError> {
        let schema_sql = include_str!("schema.sql");
        
        // Execute the schema in a transaction
        let mut tx = self.pool.begin().await?;
        
        // Split and execute each statement
        for statement in schema_sql.split(';') {
            let statement = statement.trim();
            if !statement.is_empty() {
                sqlx::query(statement).execute(&mut *tx).await?;
            }
        }
        
        tx.commit().await?;
        Ok(())
    }
    
    /// Get the database pool
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
    
    /// Close the database connection
    pub async fn close(self) {
        self.pool.close().await;
    }
    
    // Flow Run methods
    
    /// Create a new flow run
    pub async fn create_flow_run(&self, flow_run: &FlowRun) -> Result<(), StorageError> {
        let parameters_json = match &flow_run.parameters {
            Some(params) => Some(json_to_string(params)?),
            None => None,
        };
        
        sqlx::query(
            r#"
            INSERT INTO flow_runs (id, name, status, created_at, updated_at, started_at, finished_at, parameters, error_message)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#
        )
        .bind(&flow_run.id)
        .bind(&flow_run.name)
        .bind(flow_run.status.as_str())
        .bind(flow_run.created_at.to_rfc3339())
        .bind(flow_run.updated_at.to_rfc3339())
        .bind(flow_run.started_at.map(|dt| dt.to_rfc3339()))
        .bind(flow_run.finished_at.map(|dt| dt.to_rfc3339()))
        .bind(parameters_json)
        .bind(&flow_run.error_message)
        .execute(&self.pool).await?;
        
        Ok(())
    }
    
    /// Update an existing flow run
    pub async fn update_flow_run(&self, flow_run: &FlowRun) -> Result<(), StorageError> {
        let parameters_json = match &flow_run.parameters {
            Some(params) => Some(json_to_string(params)?),
            None => None,
        };
        
        sqlx::query(
            r#"
            UPDATE flow_runs 
            SET name = ?, status = ?, started_at = ?, finished_at = ?, parameters = ?, error_message = ?
            WHERE id = ?
            "#
        )
        .bind(&flow_run.name)
        .bind(flow_run.status.as_str())
        .bind(flow_run.started_at.map(|dt| dt.to_rfc3339()))
        .bind(flow_run.finished_at.map(|dt| dt.to_rfc3339()))
        .bind(parameters_json)
        .bind(&flow_run.error_message)
        .bind(&flow_run.id)
        .execute(&self.pool).await?;
        
        Ok(())
    }
    
    /// Get a flow run by ID
    pub async fn get_flow_run(&self, id: &str) -> Result<Option<FlowRun>, StorageError> {
        let row = sqlx::query("SELECT id, name, status, created_at, updated_at, started_at, finished_at, parameters, error_message FROM flow_runs WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool).await?;
        
        match row {
            Some(row) => {
                let parameters_str: Option<String> = row.try_get("parameters")?;
                let parameters = match parameters_str {
                    Some(json) => Some(json_from_str(&json)?),
                    None => None,
                };
                
                let flow_run = FlowRun {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    status: FlowRunStatus::from_str(&row.try_get::<String, _>("status")?)
                        .map_err(|e| StorageError::InvalidStatus(e))?,
                    created_at: DateTime::parse_from_rfc3339(&row.try_get::<String, _>("created_at")?)?.with_timezone(&Utc),
                    updated_at: DateTime::parse_from_rfc3339(&row.try_get::<String, _>("updated_at")?)?.with_timezone(&Utc),
                    started_at: row.try_get::<Option<String>, _>("started_at")?
                        .map(|s| DateTime::parse_from_rfc3339(&s))
                        .transpose()?
                        .map(|dt| dt.with_timezone(&Utc)),
                    finished_at: row.try_get::<Option<String>, _>("finished_at")?
                        .map(|s| DateTime::parse_from_rfc3339(&s))
                        .transpose()?
                        .map(|dt| dt.with_timezone(&Utc)),
                    parameters,
                    error_message: row.try_get("error_message")?,
                };
                
                Ok(Some(flow_run))
            }
            None => Ok(None),
        }
    }
    
    // Node Run methods
    
    /// Create a new node run
    pub async fn create_node_run(&self, node_run: &NodeRun) -> Result<(), StorageError> {
        let inputs_json = match &node_run.inputs {
            Some(inputs) => Some(json_to_string(inputs)?),
            None => None,
        };
        
        let outputs_json = match &node_run.outputs {
            Some(outputs) => Some(json_to_string(outputs)?),
            None => None,
        };
        
        sqlx::query(
            r#"
            INSERT INTO node_runs (id, flow_run_id, node_name, status, created_at, updated_at, 
                                 started_at, finished_at, inputs, outputs, error_message, retry_count)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#
        )
        .bind(&node_run.id)
        .bind(&node_run.flow_run_id)
        .bind(&node_run.node_name)
        .bind(node_run.status.as_str())
        .bind(node_run.created_at.to_rfc3339())
        .bind(node_run.updated_at.to_rfc3339())
        .bind(node_run.started_at.map(|dt| dt.to_rfc3339()))
        .bind(node_run.finished_at.map(|dt| dt.to_rfc3339()))
        .bind(inputs_json)
        .bind(outputs_json)
        .bind(&node_run.error_message)
        .bind(node_run.retry_count)
        .execute(&self.pool).await?;
        
        Ok(())
    }
    
    // Artifact methods with smart storage
    
    /// Store an artifact with automatic inline vs file storage decision
    pub async fn store_artifact(&self, artifact: &Artifact, data: &[u8]) -> Result<String, StorageError> {
        let artifact_id = artifact.id.clone();
        
        if data.len() <= self.artifact_config.inline_threshold {
            // Store inline as JSON
            let data_value = serde_json::to_value(data)?;
            self.store_artifact_inline(&artifact_id, artifact, &data_value).await?;
        } else {
            // Store as file
            let file_path = self.generate_artifact_file_path(&artifact_id, &artifact.artifact_type);
            self.store_artifact_file(&artifact_id, artifact, &file_path, data).await?;
        }
        
        Ok(artifact_id)
    }
    
    /// Store artifact data inline in the database
    async fn store_artifact_inline(&self, artifact_id: &str, artifact: &Artifact, data: &Value) -> Result<(), StorageError> {
        let data_json = json_to_string(data)?;
        let metadata_json = match &artifact.metadata {
            Some(meta) => Some(json_to_string(meta)?),
            None => None,
        };
        
        sqlx::query(
            r#"
            INSERT INTO artifacts (id, flow_run_id, node_run_id, key, artifact_type, created_at, data, metadata)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#
        )
        .bind(artifact_id)
        .bind(&artifact.flow_run_id)
        .bind(&artifact.node_run_id)
        .bind(&artifact.key)
        .bind(&artifact.artifact_type)
        .bind(artifact.created_at.to_rfc3339())
        .bind(data_json)
        .bind(metadata_json)
        .execute(&self.pool).await?;
        
        Ok(())
    }
    
    /// Store artifact as a file reference
    async fn store_artifact_file(&self, artifact_id: &str, artifact: &Artifact, file_path: &Path, data: &[u8]) -> Result<(), StorageError> {
        // Ensure parent directory exists
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        
        // Write file
        let mut file = File::create(file_path).await?;
        file.write_all(data).await?;
        file.flush().await?;
        
        let metadata_json = match &artifact.metadata {
            Some(meta) => Some(json_to_string(meta)?),
            None => None,
        };
        
        sqlx::query(
            r#"
            INSERT INTO artifacts (id, flow_run_id, node_run_id, key, artifact_type, created_at, file_path, metadata)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#
        )
        .bind(artifact_id)
        .bind(&artifact.flow_run_id)
        .bind(&artifact.node_run_id)
        .bind(&artifact.key)
        .bind(&artifact.artifact_type)
        .bind(artifact.created_at.to_rfc3339())
        .bind(file_path.to_string_lossy().to_string())
        .bind(metadata_json)
        .execute(&self.pool).await?;
        
        Ok(())
    }
    
    /// Retrieve artifact data
    pub async fn get_artifact_data(&self, artifact_id: &str) -> Result<Vec<u8>, StorageError> {
        let row = sqlx::query("SELECT data, file_path FROM artifacts WHERE id = ?")
            .bind(artifact_id)
            .fetch_optional(&self.pool).await?;
        
        match row {
            Some(row) => {
                let data_opt: Option<String> = row.try_get("data")?;
                let file_path_opt: Option<String> = row.try_get("file_path")?;
                
                if let Some(data_json) = data_opt {
                    // Inline data
                    let data_value: Value = json_from_str(&data_json)?;
                    let bytes: Vec<u8> = serde_json::from_value(data_value)?;
                    Ok(bytes)
                } else if let Some(file_path) = file_path_opt {
                    // File data
                    let mut file = File::open(&file_path).await
                        .map_err(|e| StorageError::FileOperation(format!("Failed to open artifact file {}: {}", file_path, e)))?;
                    let mut data = Vec::new();
                    file.read_to_end(&mut data).await?;
                    Ok(data)
                } else {
                    Err(StorageError::ArtifactNotFound(format!("No data found for artifact {}", artifact_id)))
                }
            }
            None => Err(StorageError::ArtifactNotFound(artifact_id.to_string())),
        }
    }
    
    /// Generate a file path for storing artifacts
    fn generate_artifact_file_path(&self, artifact_id: &str, artifact_type: &str) -> PathBuf {
        // Create subdirectory based on artifact type
        let mut path = self.artifact_config.artifact_dir.clone();
        path.push(artifact_type);
        
        // Use first 2 characters of ID for sharding
        if artifact_id.len() >= 2 {
            path.push(&artifact_id[0..2]);
        }
        
        // Use the full artifact ID as filename
        path.push(format!("{}.dat", artifact_id));
        path
    }
    
    // Shared State methods
    
    /// Set shared state value
    pub async fn set_shared_state(&self, key: &str, value: &Value, ttl: Option<DateTime<Utc>>) -> Result<(), StorageError> {
        let value_json = json_to_string(value)?;
        let ttl_str = ttl.map(|dt| dt.to_rfc3339());
        
        sqlx::query(
            r#"
            INSERT OR REPLACE INTO shared_state (key, value, ttl)
            VALUES (?, ?, ?)
            "#
        )
        .bind(key)
        .bind(value_json)
        .bind(ttl_str)
        .execute(&self.pool).await?;
        
        Ok(())
    }
    
    /// Get shared state value
    pub async fn get_shared_state(&self, key: &str) -> Result<Option<Value>, StorageError> {
        // Clean up expired entries first
        sqlx::query("DELETE FROM shared_state WHERE ttl IS NOT NULL AND ttl < datetime('now', 'utc')")
            .execute(&self.pool).await?;
        
        let row = sqlx::query("SELECT value FROM shared_state WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool).await?;
        
        match row {
            Some(row) => {
                let value_str: String = row.try_get("value")?;
            let value: Value = json_from_str(&value_str)?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }
    
    /// Delete shared state
    pub async fn delete_shared_state(&self, key: &str) -> Result<bool, StorageError> {
        let result = sqlx::query("DELETE FROM shared_state WHERE key = ?")
            .bind(key)
            .execute(&self.pool).await?;
        
        Ok(result.rows_affected() > 0)
    }
    
    // Utility methods
    
    /// Clean up expired shared state entries
    pub async fn cleanup_expired_state(&self) -> Result<u64, StorageError> {
        let result = sqlx::query("DELETE FROM shared_state WHERE ttl IS NOT NULL AND ttl < datetime('now', 'utc')")
            .execute(&self.pool).await?;
        
        Ok(result.rows_affected())
    }
    
    /// Get database statistics
    pub async fn get_stats(&self) -> Result<DatabaseStats, StorageError> {
        let flow_runs_row = sqlx::query("SELECT COUNT(*) as count FROM flow_runs")
            .fetch_one(&self.pool).await?;
        let flow_runs: i64 = flow_runs_row.try_get("count")?;
        
        let node_runs_row = sqlx::query("SELECT COUNT(*) as count FROM node_runs")
            .fetch_one(&self.pool).await?;
        let node_runs: i64 = node_runs_row.try_get("count")?;
        
        let artifacts_row = sqlx::query("SELECT COUNT(*) as count FROM artifacts")
            .fetch_one(&self.pool).await?;
        let artifacts: i64 = artifacts_row.try_get("count")?;
        
        let tasks_row = sqlx::query("SELECT COUNT(*) as count FROM tasks")
            .fetch_one(&self.pool).await?;
        let tasks: i64 = tasks_row.try_get("count")?;
        
        let shared_state_row = sqlx::query("SELECT COUNT(*) as count FROM shared_state")
            .fetch_one(&self.pool).await?;
        let shared_state: i64 = shared_state_row.try_get("count")?;
        
        Ok(DatabaseStats {
            flow_runs: flow_runs as u64,
            node_runs: node_runs as u64,
            artifacts: artifacts as u64,
            tasks: tasks as u64,
            shared_state_entries: shared_state as u64,
        })
    }
}

/// Database statistics
#[derive(Debug, Clone)]
pub struct DatabaseStats {
    pub flow_runs: u64,
    pub node_runs: u64,
    pub artifacts: u64,
    pub tasks: u64,
    pub shared_state_entries: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio;
    
    #[tokio::test]
    async fn test_storage_initialization() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let db_url = format!("sqlite:{}", db_path.to_string_lossy());
        
        let storage = SqliteStorage::new(&db_url, None).await.unwrap();
        
        // Verify tables exist by getting stats
        let stats = storage.get_stats().await.unwrap();
        assert_eq!(stats.flow_runs, 0);
        
        storage.close().await;
    }
    
    #[tokio::test]
    async fn test_shared_state() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let db_url = format!("sqlite:{}", db_path.to_string_lossy());
        
        let storage = SqliteStorage::new(&db_url, None).await.unwrap();
        
        let test_value = serde_json::json!({"test": "value", "number": 42});
        
        // Set value
        storage.set_shared_state("test_key", &test_value, None).await.unwrap();
        
        // Get value
        let retrieved = storage.get_shared_state("test_key").await.unwrap();
        assert_eq!(retrieved, Some(test_value));
        
        // Delete value
        let deleted = storage.delete_shared_state("test_key").await.unwrap();
        assert!(deleted);
        
        // Verify deletion
        let retrieved = storage.get_shared_state("test_key").await.unwrap();
        assert_eq!(retrieved, None);
        
        storage.close().await;
    }
}