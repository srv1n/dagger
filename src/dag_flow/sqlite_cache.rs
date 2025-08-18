//! SQLite-based cache implementation for DAG flow execution
//!
//! This module provides SQLite-based storage for cache data, execution trees,
//! and other persistent state that was previously stored in Sled.

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use serde_json::Value;
use sqlx::{Pool, Sqlite, SqlitePool};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

use super::dag_flow::{Cache, ExecutionTree, SerializableData, SerializableExecutionTree};

/// SQLite-based cache manager for DAG execution
pub struct SqliteCache {
    pool: Pool<Sqlite>,
}

impl SqliteCache {
    /// Create a new SQLite cache manager
    pub async fn new(database_url: &str) -> Result<Self> {
        // Ensure the database URL has the correct format
        let url = if !database_url.starts_with("sqlite:") {
            format!("sqlite:{}", database_url)
        } else {
            database_url.to_string()
        };
        
        let pool = SqlitePool::connect(&url).await?;
        
        // Initialize database schema
        Self::initialize_schema(&pool).await?;
        
        Ok(Self { pool })
    }

    /// Initialize the SQLite database schema
    async fn initialize_schema(pool: &SqlitePool) -> Result<()> {
        // Create artifacts table for cache data
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS artifacts (
                dag_id TEXT NOT NULL,
                node_id TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (dag_id, node_id, key)
            )
            "#
        )
        .execute(pool)
        .await?;

        // Create execution_trees table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS execution_trees (
                dag_id TEXT PRIMARY KEY,
                tree_data BLOB NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#
        )
        .execute(pool)
        .await?;

        // Create snapshots table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS snapshots (
                dag_id TEXT NOT NULL,
                snapshot_data BLOB NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (dag_id, created_at)
            )
            "#
        )
        .execute(pool)
        .await?;

        // Create active/pending execution state tables
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS execution_state (
                dag_id TEXT NOT NULL,
                state_type TEXT NOT NULL, -- 'active' or 'pending'
                state_data BLOB NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (dag_id, state_type)
            )
            "#
        )
        .execute(pool)
        .await?;

        // Create indexes for better performance
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_artifacts_dag_id ON artifacts(dag_id)")
            .execute(pool)
            .await?;
        
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_snapshots_dag_id ON snapshots(dag_id)")
            .execute(pool)
            .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_execution_state_dag_id ON execution_state(dag_id)")
            .execute(pool)
            .await?;

        info!("SQLite schema initialized successfully");
        Ok(())
    }

    /// Save cache data to the artifacts table
    pub async fn save_cache(&self, dag_id: &str, cache: &Cache) -> Result<()> {
        let start = std::time::Instant::now();
        let cache_read = &cache.data;

        // Begin transaction for atomic updates
        let mut tx = self.pool.begin().await?;

        // Clear existing cache data for this DAG
        sqlx::query("DELETE FROM artifacts WHERE dag_id = ?")
            .bind(dag_id)
            .execute(&mut *tx)
            .await?;

        let mut total_entries = 0;
        // Insert all cache entries
        for entry in cache_read.iter() {
            let node_id = entry.key();
            let node_cache = entry.value();
            for entry in node_cache.iter() {
                let key = entry.key();
                let value = entry.value();
                
                let serialized_value = serde_json::to_string(value)
                    .map_err(|e| anyhow!("Failed to serialize cache value: {}", e))?;

                sqlx::query(
                    "INSERT INTO artifacts (dag_id, node_id, key, value) VALUES (?, ?, ?, ?)"
                )
                .bind(dag_id)
                .bind(node_id)
                .bind(key)
                .bind(&serialized_value)
                .execute(&mut *tx)
                .await?;

                total_entries += 1;
            }
        }

        tx.commit().await?;

        let elapsed = start.elapsed();
        info!(
            "Saved cache for '{}': {} entries in {:?}",
            dag_id, total_entries, elapsed
        );

        Ok(())
    }

    /// Save cache delta (incremental update)
    pub async fn save_cache_delta(
        &self,
        dag_id: &str,
        delta: HashMap<String, HashMap<String, SerializableData>>,
    ) -> Result<()> {
        let start = std::time::Instant::now();
        
        let mut tx = self.pool.begin().await?;
        let mut total_updates = 0;

        for (node_id, node_delta) in delta {
            for (key, value) in node_delta {
                let serialized_value = serde_json::to_string(&value)
                    .map_err(|e| anyhow!("Failed to serialize delta value: {}", e))?;

                // Use INSERT OR REPLACE for upsert behavior
                sqlx::query(
                    r#"
                    INSERT OR REPLACE INTO artifacts (dag_id, node_id, key, value, updated_at) 
                    VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
                    "#
                )
                .bind(dag_id)
                .bind(&node_id)
                .bind(&key)
                .bind(&serialized_value)
                .execute(&mut *tx)
                .await?;

                total_updates += 1;
            }
        }

        tx.commit().await?;

        let elapsed = start.elapsed();
        debug!(
            "Applied cache delta for '{}': {} updates in {:?}",
            dag_id, total_updates, elapsed
        );

        Ok(())
    }

    /// Load cache from the artifacts table
    pub async fn load_cache(&self, dag_id: &str) -> Result<Cache> {
        info!("Loading cache for DAG: {}", dag_id);

        let rows = sqlx::query_as::<_, (String, String, String)>(
            "SELECT node_id, key, value FROM artifacts WHERE dag_id = ? ORDER BY node_id, key"
        )
        .bind(dag_id)
        .fetch_all(&self.pool)
        .await?;

        let cache_data = DashMap::new();
        let mut total_entries = 0;

        for (node_id, key, value_str) in rows {
            let value: SerializableData = serde_json::from_str(&value_str)
                .map_err(|e| anyhow!("Failed to deserialize cache value: {}", e))?;

            cache_data
                .entry(node_id)
                .or_insert_with(DashMap::new)
                .insert(key, value);
            
            total_entries += 1;
        }

        info!("Loaded {} entries from cache", total_entries);
        Ok(Cache { data: Arc::new(cache_data) })
    }

    /// Save execution tree to SQLite
    pub async fn save_execution_tree(&self, dag_id: &str, tree: &ExecutionTree) -> Result<()> {
        let serializable_tree = SerializableExecutionTree::from_execution_tree(tree);
        let serialized = serde_json::to_vec(&serializable_tree)?;
        let compressed = zstd::encode_all(&*serialized, 3)
            .map_err(|e| anyhow!("Failed to compress execution tree: {}", e))?;

        sqlx::query(
            r#"
            INSERT OR REPLACE INTO execution_trees (dag_id, tree_data, updated_at) 
            VALUES (?, ?, CURRENT_TIMESTAMP)
            "#
        )
        .bind(dag_id)
        .bind(&compressed)
        .execute(&self.pool)
        .await?;

        info!("Saved execution tree for '{}'", dag_id);
        Ok(())
    }

    /// Load execution tree from SQLite
    pub async fn load_execution_tree(&self, dag_id: &str) -> Result<Option<ExecutionTree>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>(
            "SELECT tree_data FROM execution_trees WHERE dag_id = ?"
        )
        .bind(dag_id)
        .fetch_optional(&self.pool)
        .await?;

        if let Some((compressed,)) = row {
            let bytes = zstd::decode_all(&compressed[..])?;
            let serializable_tree: SerializableExecutionTree = serde_json::from_slice(&bytes)?;
            let tree = serializable_tree.to_execution_tree();
            Ok(Some(tree))
        } else {
            Ok(None)
        }
    }

    /// Save snapshot data
    pub async fn save_snapshot(&self, dag_id: &str, snapshot_data: &[u8]) -> Result<()> {
        let compressed = zstd::encode_all(snapshot_data, 3)
            .map_err(|e| anyhow!("Failed to compress snapshot: {}", e))?;

        sqlx::query(
            "INSERT INTO snapshots (dag_id, snapshot_data) VALUES (?, ?)"
        )
        .bind(dag_id)
        .bind(&compressed)
        .execute(&self.pool)
        .await?;

        debug!("Saved snapshot for DAG: {}", dag_id);
        Ok(())
    }

    /// Load latest snapshot
    pub async fn load_latest_snapshot(&self, dag_id: &str) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>(
            "SELECT snapshot_data FROM snapshots WHERE dag_id = ? ORDER BY created_at DESC LIMIT 1"
        )
        .bind(dag_id)
        .fetch_optional(&self.pool)
        .await?;

        if let Some((compressed,)) = row {
            let bytes = zstd::decode_all(&compressed[..])?;
            Ok(Some(bytes))
        } else {
            Ok(None)
        }
    }

    /// Save execution state (active/pending)
    pub async fn save_execution_state(
        &self,
        dag_id: &str,
        state_type: &str,
        state_data: &[u8]
    ) -> Result<()> {
        let compressed = zstd::encode_all(state_data, 3)
            .map_err(|e| anyhow!("Failed to compress execution state: {}", e))?;

        sqlx::query(
            r#"
            INSERT OR REPLACE INTO execution_state (dag_id, state_type, state_data, updated_at) 
            VALUES (?, ?, ?, CURRENT_TIMESTAMP)
            "#
        )
        .bind(dag_id)
        .bind(state_type)
        .bind(&compressed)
        .execute(&self.pool)
        .await?;

        debug!("Saved {} state for DAG: {}", state_type, dag_id);
        Ok(())
    }

    /// Load execution state
    pub async fn load_execution_state(
        &self,
        dag_id: &str,
        state_type: &str
    ) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>(
            "SELECT state_data FROM execution_state WHERE dag_id = ? AND state_type = ?"
        )
        .bind(dag_id)
        .bind(state_type)
        .fetch_optional(&self.pool)
        .await?;

        if let Some((compressed,)) = row {
            let bytes = zstd::decode_all(&compressed[..])?;
            Ok(Some(bytes))
        } else {
            Ok(None)
        }
    }

    /// Debug utility to print database contents
    pub async fn debug_print_db(&self) -> Result<()> {
        info!("=== SQLite DB Contents ===");

        // Print artifacts count by DAG
        let artifact_rows = sqlx::query_as::<_, (String, i64)>(
            "SELECT dag_id, COUNT(*) FROM artifacts GROUP BY dag_id"
        )
        .fetch_all(&self.pool)
        .await?;

        info!("Artifacts by DAG:");
        for (dag_id, count) in artifact_rows {
            info!("  DAG '{}': {} entries", dag_id, count);
        }

        // Print execution trees
        let tree_rows = sqlx::query_as::<_, (String, i64)>(
            "SELECT dag_id, LENGTH(tree_data) FROM execution_trees"
        )
        .fetch_all(&self.pool)
        .await?;

        info!("Execution trees:");
        for (dag_id, size) in tree_rows {
            info!("  DAG '{}': {} bytes", dag_id, size);
        }

        // Print snapshots count
        let snapshot_rows = sqlx::query_as::<_, (String, i64)>(
            "SELECT dag_id, COUNT(*) FROM snapshots GROUP BY dag_id"
        )
        .fetch_all(&self.pool)
        .await?;

        info!("Snapshots by DAG:");
        for (dag_id, count) in snapshot_rows {
            info!("  DAG '{}': {} snapshots", dag_id, count);
        }

        // Print execution states
        let state_rows = sqlx::query_as::<_, (String, String, i64)>(
            "SELECT dag_id, state_type, LENGTH(state_data) FROM execution_state"
        )
        .fetch_all(&self.pool)
        .await?;

        info!("Execution states:");
        for (dag_id, state_type, size) in state_rows {
            info!("  DAG '{}' {}: {} bytes", dag_id, state_type, size);
        }

        Ok(())
    }
}