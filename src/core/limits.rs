use crate::core::errors::{DaggerError, Result};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Global resource limits configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Maximum memory usage in bytes
    pub max_memory_bytes: u64,
    /// Maximum number of concurrent tasks
    pub max_concurrent_tasks: usize,
    /// Maximum number of nodes in execution tree
    pub max_execution_tree_nodes: usize,
    /// Maximum cache entries
    pub max_cache_entries: usize,
    /// Maximum file handles
    pub max_file_handles: usize,
    /// Maximum network connections
    pub max_network_connections: usize,
    /// Maximum execution time per task
    pub max_task_execution_time: Duration,
    /// Maximum total execution time
    pub max_total_execution_time: Duration,
    /// Maximum depth of task dependencies
    pub max_dependency_depth: usize,
    /// Maximum number of retries per task
    pub max_task_retries: u32,
    /// Maximum size of individual messages
    pub max_message_size_bytes: usize,
    /// Maximum queue size for channels
    pub max_channel_queue_size: usize,
    /// CPU usage limit (percentage)
    pub max_cpu_usage_percent: f64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 1_000_000_000, // 1GB
            max_concurrent_tasks: 100,
            max_execution_tree_nodes: 10_000,
            max_cache_entries: 50_000,
            max_file_handles: 1_000,
            max_network_connections: 100,
            max_task_execution_time: Duration::from_secs(300), // 5 minutes
            max_total_execution_time: Duration::from_secs(3600), // 1 hour
            max_dependency_depth: 20,
            max_task_retries: 5,
            max_message_size_bytes: 10_000_000, // 10MB
            max_channel_queue_size: 1_000,
            max_cpu_usage_percent: 80.0,
        }
    }
}

impl ResourceLimits {
    pub fn validate(&self) -> Result<()> {
        if self.max_memory_bytes == 0 {
            return Err(DaggerError::configuration(
                "max_memory_bytes must be greater than 0",
            ));
        }
        if self.max_concurrent_tasks == 0 {
            return Err(DaggerError::configuration(
                "max_concurrent_tasks must be greater than 0",
            ));
        }
        if self.max_execution_tree_nodes == 0 {
            return Err(DaggerError::configuration(
                "max_execution_tree_nodes must be greater than 0",
            ));
        }
        if self.max_cache_entries == 0 {
            return Err(DaggerError::configuration(
                "max_cache_entries must be greater than 0",
            ));
        }
        if self.max_dependency_depth == 0 {
            return Err(DaggerError::configuration(
                "max_dependency_depth must be greater than 0",
            ));
        }
        if self.max_cpu_usage_percent <= 0.0 || self.max_cpu_usage_percent > 100.0 {
            return Err(DaggerError::configuration(
                "max_cpu_usage_percent must be between 0 and 100",
            ));
        }
        if self.max_message_size_bytes == 0 {
            return Err(DaggerError::configuration(
                "max_message_size_bytes must be greater than 0",
            ));
        }
        if self.max_channel_queue_size == 0 {
            return Err(DaggerError::configuration(
                "max_channel_queue_size must be greater than 0",
            ));
        }
        Ok(())
    }

    /// Create conservative limits for testing
    pub fn conservative() -> Self {
        Self {
            max_memory_bytes: 100_000_000, // 100MB
            max_concurrent_tasks: 10,
            max_execution_tree_nodes: 1_000,
            max_cache_entries: 5_000,
            max_file_handles: 100,
            max_network_connections: 10,
            max_task_execution_time: Duration::from_secs(30),
            max_total_execution_time: Duration::from_secs(300),
            max_dependency_depth: 10,
            max_task_retries: 3,
            max_message_size_bytes: 1_000_000, // 1MB
            max_channel_queue_size: 100,
            max_cpu_usage_percent: 50.0,
        }
    }

    /// Create aggressive limits for high-performance scenarios
    pub fn aggressive() -> Self {
        Self {
            max_memory_bytes: 8_000_000_000, // 8GB
            max_concurrent_tasks: 1000,
            max_execution_tree_nodes: 100_000,
            max_cache_entries: 500_000,
            max_file_handles: 10_000,
            max_network_connections: 1000,
            max_task_execution_time: Duration::from_secs(1800), // 30 minutes
            max_total_execution_time: Duration::from_secs(14400), // 4 hours
            max_dependency_depth: 50,
            max_task_retries: 10,
            max_message_size_bytes: 100_000_000, // 100MB
            max_channel_queue_size: 10_000,
            max_cpu_usage_percent: 95.0,
        }
    }
}

/// Resource usage tracker
#[derive(Debug)]
pub struct ResourceTracker {
    limits: ResourceLimits,

    // Current usage counters
    memory_usage: AtomicU64,
    concurrent_tasks: AtomicUsize,
    execution_tree_nodes: AtomicUsize,
    cache_entries: AtomicUsize,
    file_handles: AtomicUsize,
    network_connections: AtomicUsize,

    // Execution time tracking
    start_time: RwLock<Option<Instant>>,

    // Statistics
    peak_memory_usage: AtomicU64,
    peak_concurrent_tasks: AtomicUsize,
    total_tasks_executed: AtomicU64,
    total_limit_violations: AtomicU64,
}

impl ResourceTracker {
    pub fn new(limits: ResourceLimits) -> Result<Self> {
        limits.validate()?;

        Ok(Self {
            limits,
            memory_usage: AtomicU64::new(0),
            concurrent_tasks: AtomicUsize::new(0),
            execution_tree_nodes: AtomicUsize::new(0),
            cache_entries: AtomicUsize::new(0),
            file_handles: AtomicUsize::new(0),
            network_connections: AtomicUsize::new(0),
            start_time: RwLock::new(None),
            peak_memory_usage: AtomicU64::new(0),
            peak_concurrent_tasks: AtomicUsize::new(0),
            total_tasks_executed: AtomicU64::new(0),
            total_limit_violations: AtomicU64::new(0),
        })
    }

    /// Start execution time tracking
    pub async fn start_execution(&self) {
        let mut start_time = self.start_time.write().await;
        *start_time = Some(Instant::now());
        info!("Execution time tracking started");
    }

    /// Check if total execution time limit is exceeded
    pub async fn check_execution_time(&self) -> Result<()> {
        if let Some(start) = *self.start_time.read().await {
            let elapsed = start.elapsed();
            if elapsed > self.limits.max_total_execution_time {
                return Err(DaggerError::timeout(
                    "total_execution",
                    elapsed.as_millis() as u64,
                ));
            }
        }
        Ok(())
    }

    /// Allocate memory and check limits
    pub fn allocate_memory(&self, bytes: u64) -> Result<MemoryAllocation> {
        let current = self.memory_usage.load(Ordering::Relaxed);
        let new_total = current + bytes;

        if new_total > self.limits.max_memory_bytes {
            self.total_limit_violations.fetch_add(1, Ordering::Relaxed);
            return Err(DaggerError::resource_exhausted(
                "memory",
                new_total,
                self.limits.max_memory_bytes,
            ));
        }

        self.memory_usage.store(new_total, Ordering::Relaxed);

        // Update peak
        let current_peak = self.peak_memory_usage.load(Ordering::Relaxed);
        if new_total > current_peak {
            self.peak_memory_usage.store(new_total, Ordering::Relaxed);
        }

        debug!("Allocated {} bytes, total: {} bytes", bytes, new_total);

        Ok(MemoryAllocation {
            tracker: self,
            bytes,
        })
    }

    fn deallocate_memory(&self, bytes: u64) {
        let current = self.memory_usage.load(Ordering::Relaxed);
        let new_total = current.saturating_sub(bytes);
        self.memory_usage.store(new_total, Ordering::Relaxed);
        debug!("Deallocated {} bytes, total: {} bytes", bytes, new_total);
    }

    /// Start a task and check concurrent task limits
    pub fn start_task(&self) -> Result<TaskExecution> {
        let current = self.concurrent_tasks.load(Ordering::Relaxed);
        let new_total = current + 1;

        if new_total > self.limits.max_concurrent_tasks {
            self.total_limit_violations.fetch_add(1, Ordering::Relaxed);
            return Err(DaggerError::resource_exhausted(
                "concurrent_tasks",
                new_total as u64,
                self.limits.max_concurrent_tasks as u64,
            ));
        }

        self.concurrent_tasks.store(new_total, Ordering::Relaxed);

        // Update peak
        let current_peak = self.peak_concurrent_tasks.load(Ordering::Relaxed);
        if new_total > current_peak {
            self.peak_concurrent_tasks
                .store(new_total, Ordering::Relaxed);
        }

        self.total_tasks_executed.fetch_add(1, Ordering::Relaxed);
        debug!("Started task, concurrent: {}", new_total);

        Ok(TaskExecution {
            tracker: self,
            start_time: Instant::now(),
        })
    }

    fn end_task(&self, start_time: Instant) -> Result<()> {
        let current = self.concurrent_tasks.load(Ordering::Relaxed);
        let new_total = current.saturating_sub(1);
        self.concurrent_tasks.store(new_total, Ordering::Relaxed);

        let execution_time = start_time.elapsed();
        if execution_time > self.limits.max_task_execution_time {
            warn!(
                "Task execution time {} exceeded limit {:?}",
                execution_time.as_secs(),
                self.limits.max_task_execution_time
            );
            return Err(DaggerError::timeout(
                "task_execution",
                execution_time.as_millis() as u64,
            ));
        }

        debug!(
            "Ended task, concurrent: {}, duration: {:?}",
            new_total, execution_time
        );
        Ok(())
    }

    /// Add execution tree node
    pub fn add_execution_tree_node(&self) -> Result<()> {
        let current = self.execution_tree_nodes.load(Ordering::Relaxed);
        let new_total = current + 1;

        if new_total > self.limits.max_execution_tree_nodes {
            self.total_limit_violations.fetch_add(1, Ordering::Relaxed);
            return Err(DaggerError::resource_exhausted(
                "execution_tree_nodes",
                new_total as u64,
                self.limits.max_execution_tree_nodes as u64,
            ));
        }

        self.execution_tree_nodes
            .store(new_total, Ordering::Relaxed);
        Ok(())
    }

    /// Remove execution tree node
    pub fn remove_execution_tree_node(&self) {
        let current = self.execution_tree_nodes.load(Ordering::Relaxed);
        let new_total = current.saturating_sub(1);
        self.execution_tree_nodes
            .store(new_total, Ordering::Relaxed);
    }

    /// Add cache entry
    pub fn add_cache_entry(&self) -> Result<()> {
        let current = self.cache_entries.load(Ordering::Relaxed);
        let new_total = current + 1;

        if new_total > self.limits.max_cache_entries {
            self.total_limit_violations.fetch_add(1, Ordering::Relaxed);
            return Err(DaggerError::resource_exhausted(
                "cache_entries",
                new_total as u64,
                self.limits.max_cache_entries as u64,
            ));
        }

        self.cache_entries.store(new_total, Ordering::Relaxed);
        Ok(())
    }

    /// Remove cache entry
    pub fn remove_cache_entry(&self) {
        let current = self.cache_entries.load(Ordering::Relaxed);
        let new_total = current.saturating_sub(1);
        self.cache_entries.store(new_total, Ordering::Relaxed);
    }

    /// Open file handle
    pub fn open_file_handle(&self) -> Result<FileHandle> {
        let current = self.file_handles.load(Ordering::Relaxed);
        let new_total = current + 1;

        if new_total > self.limits.max_file_handles {
            self.total_limit_violations.fetch_add(1, Ordering::Relaxed);
            return Err(DaggerError::resource_exhausted(
                "file_handles",
                new_total as u64,
                self.limits.max_file_handles as u64,
            ));
        }

        self.file_handles.store(new_total, Ordering::Relaxed);
        debug!("Opened file handle, total: {}", new_total);

        Ok(FileHandle { tracker: self })
    }

    fn close_file_handle(&self) {
        let current = self.file_handles.load(Ordering::Relaxed);
        let new_total = current.saturating_sub(1);
        self.file_handles.store(new_total, Ordering::Relaxed);
        debug!("Closed file handle, total: {}", new_total);
    }

    /// Open network connection
    pub fn open_network_connection(&self) -> Result<NetworkConnection> {
        let current = self.network_connections.load(Ordering::Relaxed);
        let new_total = current + 1;

        if new_total > self.limits.max_network_connections {
            self.total_limit_violations.fetch_add(1, Ordering::Relaxed);
            return Err(DaggerError::resource_exhausted(
                "network_connections",
                new_total as u64,
                self.limits.max_network_connections as u64,
            ));
        }

        self.network_connections.store(new_total, Ordering::Relaxed);
        debug!("Opened network connection, total: {}", new_total);

        Ok(NetworkConnection { tracker: self })
    }

    fn close_network_connection(&self) {
        let current = self.network_connections.load(Ordering::Relaxed);
        let new_total = current.saturating_sub(1);
        self.network_connections.store(new_total, Ordering::Relaxed);
        debug!("Closed network connection, total: {}", new_total);
    }

    /// Validate message size
    pub fn validate_message_size(&self, size: usize) -> Result<()> {
        if size > self.limits.max_message_size_bytes {
            self.total_limit_violations.fetch_add(1, Ordering::Relaxed);
            return Err(DaggerError::resource_exhausted(
                "message_size",
                size as u64,
                self.limits.max_message_size_bytes as u64,
            ));
        }
        Ok(())
    }

    /// Validate dependency depth
    pub fn validate_dependency_depth(&self, depth: usize) -> Result<()> {
        if depth > self.limits.max_dependency_depth {
            self.total_limit_violations.fetch_add(1, Ordering::Relaxed);
            return Err(DaggerError::resource_exhausted(
                "dependency_depth",
                depth as u64,
                self.limits.max_dependency_depth as u64,
            ));
        }
        Ok(())
    }

    /// Validate retry count
    pub fn validate_retry_count(&self, count: u32) -> Result<()> {
        if count > self.limits.max_task_retries {
            self.total_limit_violations.fetch_add(1, Ordering::Relaxed);
            return Err(DaggerError::resource_exhausted(
                "task_retries",
                count as u64,
                self.limits.max_task_retries as u64,
            ));
        }
        Ok(())
    }

    /// Get current resource usage statistics
    pub fn get_usage_stats(&self) -> ResourceUsageStats {
        ResourceUsageStats {
            memory_usage: self.memory_usage.load(Ordering::Relaxed),
            concurrent_tasks: self.concurrent_tasks.load(Ordering::Relaxed),
            execution_tree_nodes: self.execution_tree_nodes.load(Ordering::Relaxed),
            cache_entries: self.cache_entries.load(Ordering::Relaxed),
            file_handles: self.file_handles.load(Ordering::Relaxed),
            network_connections: self.network_connections.load(Ordering::Relaxed),
            peak_memory_usage: self.peak_memory_usage.load(Ordering::Relaxed),
            peak_concurrent_tasks: self.peak_concurrent_tasks.load(Ordering::Relaxed),
            total_tasks_executed: self.total_tasks_executed.load(Ordering::Relaxed),
            total_limit_violations: self.total_limit_violations.load(Ordering::Relaxed),
        }
    }

    /// Get resource utilization percentages
    pub fn get_utilization(&self) -> ResourceUtilization {
        let stats = self.get_usage_stats();

        ResourceUtilization {
            memory_utilization: (stats.memory_usage as f64 / self.limits.max_memory_bytes as f64)
                * 100.0,
            task_utilization: (stats.concurrent_tasks as f64
                / self.limits.max_concurrent_tasks as f64)
                * 100.0,
            tree_utilization: (stats.execution_tree_nodes as f64
                / self.limits.max_execution_tree_nodes as f64)
                * 100.0,
            cache_utilization: (stats.cache_entries as f64 / self.limits.max_cache_entries as f64)
                * 100.0,
            file_utilization: (stats.file_handles as f64 / self.limits.max_file_handles as f64)
                * 100.0,
            network_utilization: (stats.network_connections as f64
                / self.limits.max_network_connections as f64)
                * 100.0,
        }
    }

    /// Check if any resource is approaching limits (>80%)
    pub fn is_approaching_limits(&self) -> bool {
        let utilization = self.get_utilization();
        utilization.memory_utilization > 80.0
            || utilization.task_utilization > 80.0
            || utilization.tree_utilization > 80.0
            || utilization.cache_utilization > 80.0
            || utilization.file_utilization > 80.0
            || utilization.network_utilization > 80.0
    }

    /// Get resource limits
    pub fn get_limits(&self) -> &ResourceLimits {
        &self.limits
    }
}

/// RAII memory allocation tracker
pub struct MemoryAllocation<'a> {
    tracker: &'a ResourceTracker,
    bytes: u64,
}

impl Drop for MemoryAllocation<'_> {
    fn drop(&mut self) {
        self.tracker.deallocate_memory(self.bytes);
    }
}

/// RAII task execution tracker
pub struct TaskExecution<'a> {
    tracker: &'a ResourceTracker,
    start_time: Instant,
}

impl Drop for TaskExecution<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.tracker.end_task(self.start_time) {
            warn!("Task execution limit violation: {}", e);
        }
    }
}

/// RAII file handle tracker
pub struct FileHandle<'a> {
    tracker: &'a ResourceTracker,
}

impl Drop for FileHandle<'_> {
    fn drop(&mut self) {
        self.tracker.close_file_handle();
    }
}

/// RAII network connection tracker
pub struct NetworkConnection<'a> {
    tracker: &'a ResourceTracker,
}

impl Drop for NetworkConnection<'_> {
    fn drop(&mut self) {
        self.tracker.close_network_connection();
    }
}

/// Resource usage statistics
#[derive(Debug, Clone)]
pub struct ResourceUsageStats {
    pub memory_usage: u64,
    pub concurrent_tasks: usize,
    pub execution_tree_nodes: usize,
    pub cache_entries: usize,
    pub file_handles: usize,
    pub network_connections: usize,
    pub peak_memory_usage: u64,
    pub peak_concurrent_tasks: usize,
    pub total_tasks_executed: u64,
    pub total_limit_violations: u64,
}

/// Resource utilization percentages
#[derive(Debug, Clone)]
pub struct ResourceUtilization {
    pub memory_utilization: f64,
    pub task_utilization: f64,
    pub tree_utilization: f64,
    pub cache_utilization: f64,
    pub file_utilization: f64,
    pub network_utilization: f64,
}

impl ResourceUtilization {
    /// Get the highest utilization percentage
    pub fn max_utilization(&self) -> f64 {
        [
            self.memory_utilization,
            self.task_utilization,
            self.tree_utilization,
            self.cache_utilization,
            self.file_utilization,
            self.network_utilization,
        ]
        .iter()
        .copied()
        .fold(0.0, f64::max)
    }

    /// Check if any resource is over the threshold
    pub fn is_over_threshold(&self, threshold: f64) -> bool {
        self.max_utilization() > threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_resource_limits_validation() {
        let mut limits = ResourceLimits::default();
        assert!(limits.validate().is_ok());

        limits.max_memory_bytes = 0;
        assert!(limits.validate().is_err());
    }

    #[tokio::test]
    async fn test_memory_allocation() {
        let limits = ResourceLimits::conservative();
        let tracker = ResourceTracker::new(limits).unwrap();

        // Allocate within limits
        let allocation = tracker.allocate_memory(1000).unwrap();
        assert_eq!(tracker.get_usage_stats().memory_usage, 1000);

        // Try to exceed limits
        let result = tracker.allocate_memory(tracker.limits.max_memory_bytes);
        assert!(result.is_err());

        // Drop allocation
        drop(allocation);
        assert_eq!(tracker.get_usage_stats().memory_usage, 0);
    }

    #[tokio::test]
    async fn test_task_execution() {
        let limits = ResourceLimits::conservative();
        let tracker = ResourceTracker::new(limits).unwrap();

        // Start task within limits
        let task = tracker.start_task().unwrap();
        assert_eq!(tracker.get_usage_stats().concurrent_tasks, 1);

        // Drop task
        drop(task);
        assert_eq!(tracker.get_usage_stats().concurrent_tasks, 0);
    }

    #[tokio::test]
    async fn test_resource_utilization() {
        let limits = ResourceLimits::conservative();
        let tracker = ResourceTracker::new(limits.clone()).unwrap();

        let _allocation = tracker
            .allocate_memory(limits.max_memory_bytes / 2)
            .unwrap();
        let utilization = tracker.get_utilization();

        assert!((utilization.memory_utilization - 50.0).abs() < 1.0);
        
        // Allocate more to trigger approaching limits (>80%)
        let _allocation2 = tracker
            .allocate_memory((limits.max_memory_bytes * 35) / 100)
            .unwrap();
        assert!(tracker.is_approaching_limits());
    }

    #[tokio::test]
    async fn test_execution_time_limits() {
        let mut limits = ResourceLimits::default();
        limits.max_total_execution_time = Duration::from_millis(100);

        let tracker = ResourceTracker::new(limits.clone()).unwrap();
        tracker.start_execution().await;

        // Wait for limit to be exceeded
        sleep(Duration::from_millis(150)).await;

        let result = tracker.check_execution_time().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_file_handles() {
        let limits = ResourceLimits::conservative();
        let tracker = ResourceTracker::new(limits).unwrap();

        let handle = tracker.open_file_handle().unwrap();
        assert_eq!(tracker.get_usage_stats().file_handles, 1);

        drop(handle);
        assert_eq!(tracker.get_usage_stats().file_handles, 0);
    }

    #[tokio::test]
    async fn test_validation_functions() {
        let limits = ResourceLimits::conservative();
        let tracker = ResourceTracker::new(limits.clone()).unwrap();

        // Message size validation
        assert!(tracker.validate_message_size(1000).is_ok());
        assert!(tracker
            .validate_message_size(limits.max_message_size_bytes + 1)
            .is_err());

        // Dependency depth validation
        assert!(tracker.validate_dependency_depth(5).is_ok());
        assert!(tracker
            .validate_dependency_depth(limits.max_dependency_depth + 1)
            .is_err());

        // Retry count validation
        assert!(tracker.validate_retry_count(2).is_ok());
        assert!(tracker
            .validate_retry_count(limits.max_task_retries + 1)
            .is_err());
    }
}
