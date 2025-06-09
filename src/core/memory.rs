use crate::core::errors::{DaggerError, Result};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep};
use tracing::{debug, info, warn};

/// Configuration for cache behavior
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Maximum memory usage in bytes
    pub max_memory_bytes: usize,
    /// Maximum number of entries
    pub max_entries: usize,
    /// Time-to-live for entries
    pub ttl: Duration,
    /// Cleanup interval
    pub cleanup_interval: Duration,
    /// Low water mark for cleanup (percentage)
    pub low_water_mark: f64,
    /// High water mark for cleanup (percentage)
    pub high_water_mark: f64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_memory_bytes: 100 * 1024 * 1024, // 100MB
            max_entries: 10_000,
            ttl: Duration::from_secs(3600), // 1 hour
            cleanup_interval: Duration::from_secs(30),
            low_water_mark: 0.7,  // Start cleanup at 70%
            high_water_mark: 0.9, // Aggressive cleanup at 90%
        }
    }
}

impl CacheConfig {
    pub fn validate(&self) -> Result<()> {
        if self.max_memory_bytes == 0 {
            return Err(DaggerError::configuration("max_memory_bytes cannot be zero"));
        }
        if self.max_entries == 0 {
            return Err(DaggerError::configuration("max_entries cannot be zero"));
        }
        if self.low_water_mark >= self.high_water_mark {
            return Err(DaggerError::configuration("low_water_mark must be less than high_water_mark"));
        }
        if self.high_water_mark > 1.0 || self.low_water_mark < 0.0 {
            return Err(DaggerError::configuration("water marks must be between 0.0 and 1.0"));
        }
        Ok(())
    }
}

/// Cache entry with metadata
#[derive(Debug)]
struct CacheEntry {
    data: DashMap<String, SerializableData>,
    created_at: Instant,
    last_accessed: Arc<RwLock<Instant>>,
    access_count: AtomicU64,
    size_bytes: AtomicUsize,
}

impl Clone for CacheEntry {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            created_at: self.created_at,
            last_accessed: self.last_accessed.clone(),
            access_count: AtomicU64::new(self.access_count.load(Ordering::Relaxed)),
            size_bytes: AtomicUsize::new(self.size_bytes.load(Ordering::Relaxed)),
        }
    }
}

impl CacheEntry {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            data: DashMap::new(),
            created_at: now,
            last_accessed: Arc::new(RwLock::new(now)),
            access_count: AtomicU64::new(0),
            size_bytes: AtomicUsize::new(0),
        }
    }

    async fn access(&self) {
        *self.last_accessed.write().await = Instant::now();
        self.access_count.fetch_add(1, Ordering::Relaxed);
    }

    async fn is_expired(&self, ttl: Duration) -> bool {
        let last_access = *self.last_accessed.read().await;
        last_access.elapsed() > ttl
    }

    fn size(&self) -> usize {
        self.size_bytes.load(Ordering::Relaxed)
    }

    fn update_size(&self, delta: isize) {
        if delta > 0 {
            self.size_bytes.fetch_add(delta as usize, Ordering::Relaxed);
        } else {
            self.size_bytes.fetch_sub((-delta) as usize, Ordering::Relaxed);
        }
    }
}

/// Serializable data wrapper with size tracking
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SerializableData {
    pub value: Value,
}

impl SerializableData {
    pub fn new(value: Value) -> Self {
        Self { value }
    }

    /// Estimate memory size of the data
    pub fn estimate_size(&self) -> usize {
        // Rough estimation of JSON memory usage
        match &self.value {
            Value::Null => 8,
            Value::Bool(_) => 8,
            Value::Number(_) => 16,
            Value::String(s) => s.len() + 24,
            Value::Array(arr) => {
                arr.iter().map(|v| Self::new(v.clone()).estimate_size()).sum::<usize>() + 24
            }
            Value::Object(obj) => {
                obj.iter()
                    .map(|(k, v)| k.len() + Self::new(v.clone()).estimate_size())
                    .sum::<usize>() + 24
            }
        }
    }
}

/// Cache statistics
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub total_entries: usize,
    pub total_size_bytes: usize,
    pub hit_count: u64,
    pub miss_count: u64,
    pub eviction_count: u64,
    pub cleanup_count: u64,
}

impl CacheStats {
    fn new() -> Self {
        Self {
            total_entries: 0,
            total_size_bytes: 0,
            hit_count: 0,
            miss_count: 0,
            eviction_count: 0,
            cleanup_count: 0,
        }
    }

    pub fn hit_ratio(&self) -> f64 {
        let total = self.hit_count + self.miss_count;
        if total == 0 {
            0.0
        } else {
            self.hit_count as f64 / total as f64
        }
    }

    pub fn memory_utilization(&self, max_memory: usize) -> f64 {
        if max_memory == 0 {
            0.0
        } else {
            self.total_size_bytes as f64 / max_memory as f64
        }
    }
}

/// Improved cache with automatic cleanup and memory management
#[derive(Debug)]
pub struct Cache {
    data: Arc<DashMap<String, CacheEntry>>,
    config: CacheConfig,
    stats: Arc<RwLock<CacheStats>>,
    cleanup_handle: Option<JoinHandle<()>>,
    is_shutdown: Arc<AtomicUsize>, // 0 = running, 1 = shutdown
}

impl Cache {
    pub fn new(config: CacheConfig) -> Result<Self> {
        config.validate()?;
        
        let cache = Self {
            data: Arc::new(DashMap::new()),
            config,
            stats: Arc::new(RwLock::new(CacheStats::new())),
            cleanup_handle: None,
            is_shutdown: Arc::new(AtomicUsize::new(0)),
        };

        Ok(cache)
    }

    pub fn with_cleanup(mut self) -> Self {
        self.start_cleanup_task();
        self
    }

    fn start_cleanup_task(&mut self) {
        let data = Arc::clone(&self.data);
        let config = self.config.clone();
        let stats = Arc::clone(&self.stats);
        let is_shutdown = Arc::clone(&self.is_shutdown);

        let handle = tokio::spawn(async move {
            let mut interval = interval(config.cleanup_interval);
            
            while is_shutdown.load(Ordering::Relaxed) == 0 {
                interval.tick().await;
                
                if let Err(e) = Self::cleanup_expired(&data, &config, &stats).await {
                    warn!("Cache cleanup failed: {}", e);
                }
            }
            
            info!("Cache cleanup task terminated");
        });

        self.cleanup_handle = Some(handle);
    }

    async fn cleanup_expired(
        data: &DashMap<String, CacheEntry>,
        config: &CacheConfig,
        stats: &RwLock<CacheStats>,
    ) -> Result<()> {
        let start = Instant::now();
        let mut expired_keys = Vec::new();
        let mut total_size = 0usize;
        let mut entries_by_age = Vec::new();

        // Collect expired entries and size information
        for entry in data.iter() {
            let key = entry.key().clone();
            let cache_entry = entry.value();
            
            total_size += cache_entry.size();
            
            if cache_entry.is_expired(config.ttl).await {
                expired_keys.push(key.clone());
            } else {
                let last_accessed = *cache_entry.last_accessed.read().await;
                entries_by_age.push((key, last_accessed, cache_entry.size()));
            }
        }

        // Remove expired entries
        let mut evicted_count = 0;
        for key in &expired_keys {
            if data.remove(key).is_some() {
                evicted_count += 1;
            }
        }

        // Check if we need to evict more due to memory pressure
        let memory_usage = total_size as f64 / config.max_memory_bytes as f64;
        let entry_usage = data.len() as f64 / config.max_entries as f64;

        if memory_usage > config.high_water_mark || entry_usage > config.high_water_mark {
            // Aggressive cleanup - remove LRU entries
            entries_by_age.sort_by_key(|(_, last_accessed, _)| *last_accessed);
            
            let target_reduction = if memory_usage > config.high_water_mark {
                (memory_usage - config.low_water_mark) * config.max_memory_bytes as f64
            } else {
                (entry_usage - config.low_water_mark) * config.max_entries as f64
            };

            let mut removed_size = 0.0;
            for (key, _, size) in entries_by_age {
                if removed_size >= target_reduction {
                    break;
                }
                
                if data.remove(&key).is_some() {
                    removed_size += size as f64;
                    evicted_count += 1;
                }
            }
        }

        // Update statistics
        {
            let mut stats = stats.write().await;
            stats.eviction_count += evicted_count;
            stats.cleanup_count += 1;
            stats.total_entries = data.len();
            
            // Recalculate total size
            stats.total_size_bytes = data.iter()
                .map(|entry| entry.value().size())
                .sum();
        }

        let elapsed = start.elapsed();
        debug!(
            "Cache cleanup completed: {} expired, {} evicted, took {:?}",
            expired_keys.len(),
            evicted_count,
            elapsed
        );

        Ok(())
    }

    pub async fn insert_value<T: Serialize>(
        &self,
        node_id: &str,
        key: &str,
        value: &T,
    ) -> Result<()> {
        let json_value = serde_json::to_value(value)
            .map_err(|e| DaggerError::serialization("json", e))?;
        
        let serializable_data = SerializableData::new(json_value);
        let data_size = serializable_data.estimate_size();

        // Check memory limits before insertion
        {
            let stats = self.stats.read().await;
            if stats.total_size_bytes + data_size > self.config.max_memory_bytes {
                return Err(DaggerError::resource_exhausted(
                    "cache_memory",
                    (stats.total_size_bytes + data_size) as u64,
                    self.config.max_memory_bytes as u64,
                ));
            }
            
            if stats.total_entries >= self.config.max_entries {
                return Err(DaggerError::resource_exhausted(
                    "cache_entries",
                    stats.total_entries as u64,
                    self.config.max_entries as u64,
                ));
            }
        }

        let cache_entry = self.data
            .entry(node_id.to_string())
            .or_insert_with(CacheEntry::new);
        
        cache_entry.access().await;
        
        let old_size = cache_entry.data.get(key).map(|v| v.estimate_size()).unwrap_or(0);
        cache_entry.data.insert(key.to_string(), serializable_data);
        
        let size_delta = data_size as isize - old_size as isize;
        cache_entry.update_size(size_delta);

        // Update global stats
        {
            let mut stats = self.stats.write().await;
            stats.total_size_bytes = (stats.total_size_bytes as isize + size_delta) as usize;
            if old_size == 0 {
                // New entry
                stats.total_entries = self.data.len();
            }
        }

        Ok(())
    }

    pub async fn get_value(&self, node_id: &str, key: &str) -> Option<Value> {
        let result = self.data.get(node_id).and_then(|cache_entry| {
            cache_entry.data.get(key).map(|v| v.value.clone())
        });

        // Update statistics
        {
            let mut stats = self.stats.write().await;
            if result.is_some() {
                stats.hit_count += 1;
            } else {
                stats.miss_count += 1;
            }
        }

        // Update access time if entry exists
        if result.is_some() {
            if let Some(cache_entry) = self.data.get(node_id) {
                cache_entry.access().await;
            }
        }

        result
    }

    pub async fn remove_node(&self, node_id: &str) -> Option<usize> {
        if let Some((_, cache_entry)) = self.data.remove(node_id) {
            let size = cache_entry.size();
            
            // Update stats
            {
                let mut stats = self.stats.write().await;
                stats.total_size_bytes = stats.total_size_bytes.saturating_sub(size);
                stats.total_entries = self.data.len();
            }
            
            Some(size)
        } else {
            None
        }
    }

    pub async fn clear(&self) {
        self.data.clear();
        
        let mut stats = self.stats.write().await;
        *stats = CacheStats::new();
    }

    pub async fn get_stats(&self) -> CacheStats {
        self.stats.read().await.clone()
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub async fn get_memory_usage(&self) -> usize {
        self.stats.read().await.total_size_bytes
    }

    pub fn get_config(&self) -> &CacheConfig {
        &self.config
    }

    /// Append a value to an array in the cache
    pub async fn append_global_value<T: Serialize + for<'de> Deserialize<'de>>(
        &self,
        dag_name: &str,
        key: &str,
        value: T,
    ) -> Result<()> {
        let global_key = format!("global:{}", dag_name);
        let json_value = serde_json::to_value(value)
            .map_err(|e| DaggerError::serialization("json", e))?;

        let cache_entry = self.data
            .entry(global_key)
            .or_insert_with(CacheEntry::new);
        
        cache_entry.access().await;

        let old_size = cache_entry.data.get(key).map(|v| v.estimate_size()).unwrap_or(0);

        cache_entry.data
            .entry(key.to_string())
            .and_modify(|existing_data| {
                if let Some(existing_vec) = existing_data.value.as_array_mut() {
                    existing_vec.push(json_value.clone());
                } else {
                    existing_data.value = Value::Array(vec![existing_data.value.clone(), json_value.clone()]);
                }
            })
            .or_insert_with(|| SerializableData::new(Value::Array(vec![json_value])));

        // Update size tracking
        let new_size = cache_entry.data.get(key).map(|v| v.estimate_size()).unwrap_or(0);
        let size_delta = new_size as isize - old_size as isize;
        cache_entry.update_size(size_delta);

        Ok(())
    }

    /// Get a global value from the cache
    pub async fn get_global_value<T: for<'de> Deserialize<'de>>(
        &self,
        dag_name: &str,
        key: &str,
    ) -> Result<Option<T>> {
        let global_key = format!("global:{}", dag_name);
        
        if let Some(cache_entry) = self.data.get(&global_key) {
            cache_entry.access().await;
            
            if let Some(serializable_data) = cache_entry.data.get(key) {
                let value: T = serde_json::from_value(serializable_data.value.clone())
                    .map_err(|e| DaggerError::serialization("json", e))?;
                Ok(Some(value))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        // Signal shutdown
        self.is_shutdown.store(1, Ordering::Relaxed);
        
        // Cancel cleanup task
        if let Some(handle) = self.cleanup_handle.take() {
            handle.abort();
        }
    }
}

// Implement Clone for Cache (shallow clone sharing the same data)
impl Clone for Cache {
    fn clone(&self) -> Self {
        Self {
            data: Arc::clone(&self.data),
            config: self.config.clone(),
            stats: Arc::clone(&self.stats),
            cleanup_handle: None, // Don't clone the cleanup handle
            is_shutdown: Arc::clone(&self.is_shutdown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_cache_basic_operations() {
        let config = CacheConfig::default();
        let cache = Cache::new(config).unwrap();

        // Test insertion and retrieval
        cache.insert_value("node1", "key1", &"value1").await.unwrap();
        let value = cache.get_value("node1", "key1").await;
        assert_eq!(value, Some(Value::String("value1".to_string())));

        // Test statistics
        let stats = cache.get_stats().await;
        assert_eq!(stats.hit_count, 1);
        assert_eq!(stats.miss_count, 0);
    }

    #[tokio::test]
    async fn test_cache_memory_limits() {
        let config = CacheConfig {
            max_memory_bytes: 100, // Very small limit
            ..Default::default()
        };
        let cache = Cache::new(config).unwrap();

        // Insert small value
        cache.insert_value("node1", "key1", &"small").await.unwrap();

        // Try to insert large value that exceeds limit
        let large_value = "x".repeat(200);
        let result = cache.insert_value("node1", "key2", &large_value).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_cache_cleanup() {
        let config = CacheConfig {
            ttl: Duration::from_millis(100),
            cleanup_interval: Duration::from_millis(50),
            ..Default::default()
        };
        let cache = Cache::new(config).unwrap().with_cleanup();

        // Insert value
        cache.insert_value("node1", "key1", &"value1").await.unwrap();
        
        // Wait for expiry
        sleep(Duration::from_millis(150)).await;
        
        // Wait for cleanup
        sleep(Duration::from_millis(100)).await;

        // Value should be removed
        let value = cache.get_value("node1", "key1").await;
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn test_global_operations() {
        let config = CacheConfig::default();
        let cache = Cache::new(config).unwrap();

        // Test append global value
              // Test append global value
        cache.append_global_value("dag1", "results", "result1".to_string()).await.unwrap();
        cache.append_global_value("dag1", "results", "result2".to_string()).await.unwrap();

        // Test get global value
        let results: Option<Vec<String>> = cache.get_global_value("dag1", "results").await.unwrap();
        assert_eq!(results, Some(vec!["result1".to_string(), "result2".to_string()]));
    }
}