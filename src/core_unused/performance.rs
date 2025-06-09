use crate::core::errors::{DaggerError, Result};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio::task::JoinHandle;
use tracing::{debug, info, instrument, warn};

/// Performance configuration
#[derive(Debug, Clone)]
pub struct PerformanceConfig {
    /// Enable async serialization for large objects
    pub async_serialization_threshold: usize,
    /// Batch size for database operations
    pub database_batch_size: usize,
    /// Connection pool size
    pub connection_pool_size: usize,
    /// Background task interval
    pub background_task_interval: Duration,
    /// Cache pre-warming settings
    pub cache_prewarming_enabled: bool,
    /// Object pooling settings
    pub object_pooling_enabled: bool,
    /// Compression threshold for large data
    pub compression_threshold: usize,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            async_serialization_threshold: 100_000, // 100KB
            database_batch_size: 100,
            connection_pool_size: 10,
            background_task_interval: Duration::from_secs(30),
            cache_prewarming_enabled: true,
            object_pooling_enabled: true,
            compression_threshold: 1_000_000, // 1MB
        }
    }
}

/// Async serialization service for large objects
#[derive(Debug)]
pub struct AsyncSerializationService {
    pool: Arc<Semaphore>,
    stats: Arc<RwLock<SerializationStats>>,
}

#[derive(Debug, Default, Clone)]
struct SerializationStats {
    total_operations: u64,
    async_operations: u64,
    sync_operations: u64,
    total_bytes_processed: u64,
    avg_async_time_ms: f64,
    avg_sync_time_ms: f64,
}

impl AsyncSerializationService {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            pool: Arc::new(Semaphore::new(max_concurrent)),
            stats: Arc::new(RwLock::new(SerializationStats::default())),
        }
    }

    /// Serialize data, using async for large objects
    #[instrument(skip(self, data))]
    pub async fn serialize<T: Serialize + Send + 'static>(
        &self,
        data: T,
        threshold: usize,
    ) -> Result<Vec<u8>> {
        let start = Instant::now();
        
        // Quick size estimation for simple types
        let estimated_size = std::mem::size_of::<T>();
        
        if estimated_size > threshold {
            // Use async serialization for large objects
            let _permit = self.pool.acquire().await.map_err(|e| {
                DaggerError::concurrency(format!("Failed to acquire serialization permit: {}", e))
            })?;
            
            let result = tokio::task::spawn_blocking(move || {
                serde_json::to_vec(&data)
            })
            .await
            .map_err(|e| DaggerError::internal(format!("Serialization task failed: {}", e)))?
            .map_err(|e| DaggerError::serialization("json", e))?;
            
            let elapsed = start.elapsed();
            self.update_stats(result.len(), elapsed, true).await;
            
            debug!(
                "Async serialized {} bytes in {:?}",
                result.len(),
                elapsed
            );
            
            Ok(result)
        } else {
            // Use sync serialization for small objects
            let result = serde_json::to_vec(&data)
                .map_err(|e| DaggerError::serialization("json", e))?;
            
            let elapsed = start.elapsed();
            self.update_stats(result.len(), elapsed, false).await;
            
            Ok(result)
        }
    }

    /// Deserialize data, using async for large objects
    #[instrument(skip(self, data))]
    pub async fn deserialize<T: for<'de> Deserialize<'de> + Send + 'static>(
        &self,
        data: Vec<u8>,
        threshold: usize,
    ) -> Result<T> {
        let start = Instant::now();
        
        if data.len() > threshold {
            // Use async deserialization for large objects
            let _permit = self.pool.acquire().await.map_err(|e| {
                DaggerError::concurrency(format!("Failed to acquire deserialization permit: {}", e))
            })?;
            
            let data_len = data.len();
            let result = tokio::task::spawn_blocking(move || {
                serde_json::from_slice(&data)
            })
            .await
            .map_err(|e| DaggerError::internal(format!("Deserialization task failed: {}", e)))?
            .map_err(|e| DaggerError::serialization("json", e))?;
            
            let elapsed = start.elapsed();
            self.update_stats(data_len, elapsed, true).await;
            
            debug!(
                "Async deserialized {} bytes in {:?}",
                data_len,
                elapsed
            );
            
            Ok(result)
        } else {
            // Use sync deserialization for small objects
            let result = serde_json::from_slice(&data)
                .map_err(|e| DaggerError::serialization("json", e))?;
            
            let elapsed = start.elapsed();
            self.update_stats(data.len(), elapsed, false).await;
            
            Ok(result)
        }
    }

    async fn update_stats(&self, bytes: usize, elapsed: Duration, is_async: bool) {
        let mut stats = self.stats.write().await;
        stats.total_operations += 1;
        stats.total_bytes_processed += bytes as u64;
        
        let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
        
        if is_async {
            stats.async_operations += 1;
            stats.avg_async_time_ms = 
                (stats.avg_async_time_ms * (stats.async_operations - 1) as f64 + elapsed_ms) / 
                stats.async_operations as f64;
        } else {
            stats.sync_operations += 1;
            stats.avg_sync_time_ms = 
                (stats.avg_sync_time_ms * (stats.sync_operations - 1) as f64 + elapsed_ms) / 
                stats.sync_operations as f64;
        }
    }

    pub async fn get_stats(&self) -> SerializationStats {
        self.stats.read().await.clone()
    }
}

/// Batch processing service for database operations
pub struct BatchProcessor<T> {
    batch_size: usize,
    pending_operations: Arc<Mutex<VecDeque<T>>>,
    flush_handle: Option<JoinHandle<()>>,
    processor: Arc<dyn Fn(Vec<T>) -> Result<()> + Send + Sync>,
}

impl<T: Send + 'static> BatchProcessor<T> {
    pub fn new<F>(batch_size: usize, flush_interval: Duration, processor: F) -> Self
    where
        F: Fn(Vec<T>) -> Result<()> + Send + Sync + 'static,
    {
        let pending = Arc::new(Mutex::new(VecDeque::new()));
        let processor = Arc::new(processor);
        
        let mut batch_processor = Self {
            batch_size,
            pending_operations: pending.clone(),
            flush_handle: None,
            processor: processor.clone(),
        };
        
        // Start background flush task
        let pending_clone = pending;
        let processor_clone = processor;
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(flush_interval);
            
            loop {
                interval.tick().await;
                
                let mut pending = pending_clone.lock().await;
                if !pending.is_empty() {
                    let batch: Vec<T> = pending.drain(..).collect();
                    drop(pending); // Release lock before processing
                    
                    if let Err(e) = processor_clone(batch) {
                        warn!("Batch processing failed: {}", e);
                    }
                }
            }
        });
        
        batch_processor.flush_handle = Some(handle);
        batch_processor
    }

    /// Add operation to batch
    pub async fn add(&self, operation: T) -> Result<()> {
        let mut pending = self.pending_operations.lock().await;
        pending.push_back(operation);
        
        if pending.len() >= self.batch_size {
            let batch: Vec<T> = pending.drain(..).collect();
            drop(pending); // Release lock before processing
            
            (self.processor)(batch)?;
        }
        
        Ok(())
    }

    /// Flush all pending operations
    pub async fn flush(&self) -> Result<()> {
        let mut pending = self.pending_operations.lock().await;
        if !pending.is_empty() {
            let batch: Vec<T> = pending.drain(..).collect();
            drop(pending); // Release lock before processing
            
            (self.processor)(batch)?;
        }
        
        Ok(())
    }
}

impl<T> Drop for BatchProcessor<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.flush_handle.take() {
            handle.abort();
        }
    }
}

/// Object pool for reusing expensive objects
pub struct ObjectPool<T> {
    objects: Arc<Mutex<Vec<T>>>,
    factory: Arc<dyn Fn() -> T + Send + Sync>,
    max_size: usize,
    created_count: AtomicU64,
    reused_count: AtomicU64,
}

impl<T: Send + 'static> ObjectPool<T> {
    pub fn new<F>(max_size: usize, factory: F) -> Self
    where
        F: Fn() -> T + Send + Sync + 'static,
    {
        Self {
            objects: Arc::new(Mutex::new(Vec::with_capacity(max_size))),
            factory: Arc::new(factory),
            max_size,
            created_count: AtomicU64::new(0),
            reused_count: AtomicU64::new(0),
        }
    }

    /// Get object from pool or create new one
    pub async fn get(&self) -> PooledObject<T> {
        let mut objects = self.objects.lock().await;
        
        if let Some(object) = objects.pop() {
            self.reused_count.fetch_add(1, Ordering::Relaxed);
            PooledObject {
                object: Some(object),
                pool: self.objects.clone(),
            }
        } else {
            drop(objects); // Release lock
            self.created_count.fetch_add(1, Ordering::Relaxed);
            let object = (self.factory)();
            PooledObject {
                object: Some(object),
                pool: self.objects.clone(),
            }
        }
    }

    /// Get pool statistics
    pub fn get_stats(&self) -> PoolStats {
        PoolStats {
            created_count: self.created_count.load(Ordering::Relaxed),
            reused_count: self.reused_count.load(Ordering::Relaxed),
            current_size: self.objects.try_lock().map(|objects| objects.len()).unwrap_or(0),
            max_size: self.max_size,
        }
    }
}

/// RAII wrapper for pooled objects
pub struct PooledObject<T> {
    object: Option<T>,
    pool: Arc<Mutex<Vec<T>>>,
}

impl<T> PooledObject<T> {
    /// Get reference to the pooled object
    pub fn get(&self) -> &T {
        self.object.as_ref().expect("Object should be available")
    }

    /// Get mutable reference to the pooled object
    pub fn get_mut(&mut self) -> &mut T {
        self.object.as_mut().expect("Object should be available")
    }
}

impl<T> Drop for PooledObject<T> {
    fn drop(&mut self) {
        if let Some(object) = self.object.take() {
            if let Ok(mut pool) = self.pool.try_lock() {
                if pool.len() < pool.capacity() {
                    pool.push(object);
                }
                // If pool is full, object is dropped
            }
            // If lock fails, object is dropped
        }
    }
}

#[derive(Debug, Clone)]
pub struct PoolStats {
    pub created_count: u64,
    pub reused_count: u64,
    pub current_size: usize,
    pub max_size: usize,
}

impl PoolStats {
    pub fn reuse_ratio(&self) -> f64 {
        let total = self.created_count + self.reused_count;
        if total == 0 {
            0.0
        } else {
            self.reused_count as f64 / total as f64
        }
    }
}

/// Smart caching with Copy-on-Write semantics  
pub struct CowCache<K, V>
where
    K: std::hash::Hash + Eq + Clone,
    V: Clone,
{
    cache: Arc<DashMap<K, Arc<V>>>,
    hit_count: AtomicU64,
    miss_count: AtomicU64,
}

impl<K: Hash + Eq + Clone, V: Clone> CowCache<K, V> {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(DashMap::new()),
            hit_count: AtomicU64::new(0),
            miss_count: AtomicU64::new(0),
        }
    }

    /// Get value with copy-on-write semantics
    pub fn get_cow(&self, key: &K) -> Option<Cow<V>> {
        if let Some(value) = self.cache.get(key) {
            self.hit_count.fetch_add(1, Ordering::Relaxed);
            Some(Cow::Borrowed(value.as_ref()))
        } else {
            self.miss_count.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    /// Get owned value (clones if necessary)
    pub fn get_owned(&self, key: &K) -> Option<V> {
        if let Some(value) = self.cache.get(key) {
            self.hit_count.fetch_add(1, Ordering::Relaxed);
            Some((**value).clone())
        } else {
            self.miss_count.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    /// Insert value
    pub fn insert(&self, key: K, value: V) {
        self.cache.insert(key, Arc::new(value));
    }

    /// Remove value
    pub fn remove(&self, key: &K) -> Option<V> {
        self.cache.remove(key).map(|(_, v)| {
            Arc::try_unwrap(v).unwrap_or_else(|arc| (*arc).clone())
        })
    }

    /// Get cache statistics
    pub fn get_cache_stats(&self) -> CacheStats {
        CacheStats {
            hit_count: self.hit_count.load(Ordering::Relaxed),
            miss_count: self.miss_count.load(Ordering::Relaxed),
            entry_count: self.cache.len(),
        }
    }

    /// Clear cache
    pub fn clear(&self) {
        self.cache.clear();
    }
}

impl<K: Hash + Eq + Clone, V: Clone> Default for CowCache<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub hit_count: u64,
    pub miss_count: u64,
    pub entry_count: usize,
}

impl CacheStats {
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hit_count + self.miss_count;
        if total == 0 {
            0.0
        } else {
            self.hit_count as f64 / total as f64
        }
    }
}

/// Performance monitoring and metrics
#[derive(Debug)]
pub struct PerformanceMonitor {
    operation_times: Arc<DashMap<String, VecDeque<Duration>>>,
    operation_counts: Arc<DashMap<String, AtomicU64>>,
    error_counts: Arc<DashMap<String, AtomicU64>>,
    max_history: usize,
}

impl PerformanceMonitor {
    pub fn new(max_history: usize) -> Self {
        Self {
            operation_times: Arc::new(DashMap::new()),
            operation_counts: Arc::new(DashMap::new()),
            error_counts: Arc::new(DashMap::new()),
            max_history,
        }
    }

    /// Record operation timing
    pub fn record_operation(&self, operation: &str, duration: Duration) {
        // Record timing
        let mut times = self.operation_times
            .entry(operation.to_string())
            .or_insert_with(VecDeque::new);
        
        times.push_back(duration);
        if times.len() > self.max_history {
            times.pop_front();
        }

        // Record count
        self.operation_counts
            .entry(operation.to_string())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record error
    pub fn record_error(&self, operation: &str) {
        self.error_counts
            .entry(operation.to_string())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Get operation metrics
    pub fn get_operation_metrics(&self, operation: &str) -> Option<OperationMetrics> {
        let times = self.operation_times.get(operation)?;
        let count = self.operation_counts.get(operation)?;
        let errors = self.error_counts.get(operation)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0);

        if times.is_empty() {
            return None;
        }

        let total_time: Duration = times.iter().sum();
        let avg_time = total_time / times.len() as u32;
        
        let mut sorted_times: Vec<Duration> = times.iter().copied().collect();
        sorted_times.sort();
        
        let p50 = sorted_times[sorted_times.len() / 2];
        let p95 = sorted_times[(sorted_times.len() * 95) / 100];
        let p99 = sorted_times[(sorted_times.len() * 99) / 100];

        Some(OperationMetrics {
            operation: operation.to_string(),
            count: count.load(Ordering::Relaxed),
            error_count: errors,
            avg_duration: avg_time,
            p50_duration: p50,
            p95_duration: p95,
            p99_duration: p99,
            min_duration: *sorted_times.first().unwrap(),
            max_duration: *sorted_times.last().unwrap(),
        })
    }

    /// Get all metrics
    pub fn get_all_metrics(&self) -> Vec<OperationMetrics> {
        self.operation_counts
            .iter()
            .filter_map(|entry| self.get_operation_metrics(entry.key()))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct OperationMetrics {
    pub operation: String,
    pub count: u64,
    pub error_count: u64,
    pub avg_duration: Duration,
    pub p50_duration: Duration,
    pub p95_duration: Duration,
    pub p99_duration: Duration,
    pub min_duration: Duration,
    pub max_duration: Duration,
}

impl OperationMetrics {
    pub fn error_rate(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.error_count as f64 / self.count as f64
        }
    }
    
    pub fn success_rate(&self) -> f64 {
        1.0 - self.error_rate()
    }
}

/// Macro for timing operations
#[macro_export]
macro_rules! time_operation {
    ($monitor:expr, $operation:expr, $code:block) => {{
        let start = std::time::Instant::now();
        let result = $code;
        let duration = start.elapsed();
        
        match &result {
            Ok(_) => $monitor.record_operation($operation, duration),
            Err(_) => {
                $monitor.record_operation($operation, duration);
                $monitor.record_error($operation);
            }
        }
        
        result
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_async_serialization() {
        let service = AsyncSerializationService::new(2);
        
        let data = "small data".to_string();
        let result = service.serialize(data, 1000).await.unwrap();
        assert!(!result.is_empty());
        
        let stats = service.get_stats().await;
        assert_eq!(stats.total_operations, 1);
    }

    #[tokio::test]
    async fn test_batch_processor() {
        let processed = Arc::new(Mutex::new(Vec::new()));
        let processed_clone = processed.clone();
        
        let processor = move |batch: Vec<i32>| -> Result<()> {
            tokio::runtime::Handle::current().block_on(async {
                let mut p = processed_clone.lock().await;
                p.extend(batch);
            });
            Ok(())
        };
        
        let batch_processor = BatchProcessor::new(3, Duration::from_millis(100), processor);
        
        // Add items
        batch_processor.add(1).await.unwrap();
        batch_processor.add(2).await.unwrap();
        batch_processor.add(3).await.unwrap(); // Should trigger batch
        
        sleep(Duration::from_millis(50)).await;
        
        let processed_items = processed.lock().await;
        assert_eq!(*processed_items, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn test_object_pool() {
        let pool = ObjectPool::new(2, || Vec::<i32>::with_capacity(100));
        
        let obj1 = pool.get().await;
        let obj2 = pool.get().await;
        
        assert_eq!(pool.get_stats().created_count, 2);
        assert_eq!(pool.get_stats().reused_count, 0);
        
        drop(obj1);
        drop(obj2);
        
        let obj3 = pool.get().await;
        let stats = pool.get_stats();
        assert_eq!(stats.created_count, 2);
        assert_eq!(stats.reused_count, 1);
    }

    #[tokio::test]
    async fn test_cow_cache() {
        let cache = CowCache::new();
        
        cache.insert("key1".to_string(), "value1".to_string());
        
        let value = cache.get_cow(&"key1".to_string()).unwrap();
        assert_eq!(*value, "value1");
        
        let stats = cache.get_cache_stats();
        assert_eq!(stats.hit_count, 1);
        assert_eq!(stats.miss_count, 0);
        
        let none_value = cache.get_cow(&"nonexistent".to_string());
        assert!(none_value.is_none());
        
        let stats = cache.get_cache_stats();
        assert_eq!(stats.hit_count, 1);
        assert_eq!(stats.miss_count, 1);
    }

    #[tokio::test]
    async fn test_performance_monitor() {
        let monitor = PerformanceMonitor::new(100);
        
        monitor.record_operation("test_op", Duration::from_millis(100));
        monitor.record_operation("test_op", Duration::from_millis(200));
        monitor.record_error("test_op");
        
        let metrics = monitor.get_operation_metrics("test_op").unwrap();
        assert_eq!(metrics.count, 2);
        assert_eq!(metrics.error_count, 1);
        assert_eq!(metrics.error_rate(), 0.5);
    }
}