use std::time::Duration;
use std::path::PathBuf;
use serde::{Serialize, Deserialize};

/// Task system configuration with all tuning parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskConfig {
    // Worker pool configuration
    /// Maximum number of concurrent workers
    pub max_workers: usize,
    /// Minimum number of workers to maintain
    pub min_workers: usize,
    /// Worker idle timeout before shutdown
    pub worker_idle_timeout: Duration,
    
    // Queue configuration
    /// Maximum number of tasks in ready queue
    pub queue_capacity: usize,
    /// Maximum number of priority levels
    pub max_priority_levels: usize,
    
    // Task execution configuration
    /// Default task execution timeout
    pub task_timeout: Option<Duration>,
    /// Maximum task execution time (hard limit)
    pub max_task_duration: Duration,
    /// Task heartbeat interval
    pub heartbeat_interval: Duration,
    /// Task stall detection timeout
    pub stall_timeout: Duration,
    
    // Retry configuration
    /// Default number of retries for failed tasks
    pub default_retries: u32,
    /// Maximum allowed retries
    pub max_retries: u32,
    /// Initial retry delay
    pub retry_delay: Duration,
    /// Maximum retry delay
    pub max_retry_delay: Duration,
    /// Retry backoff multiplier
    pub retry_backoff_multiplier: f64,
    
    // Storage configuration
    /// Database path for persistence
    pub db_path: Option<PathBuf>,
    /// Database cache size in bytes
    pub db_cache_size: usize,
    /// Database flush interval
    pub db_flush_interval: Duration,
    /// Enable write-ahead logging
    pub db_wal_enabled: bool,
    
    // Scheduler configuration
    /// Scheduler tick interval
    pub scheduler_interval: Duration,
    /// Maximum tasks to schedule per tick
    pub max_schedule_batch: usize,
    /// Enable task preemption
    pub enable_preemption: bool,
    /// Task fair scheduling quantum
    pub scheduling_quantum: Duration,
    
    // Recovery configuration
    /// Enable automatic recovery
    pub enable_recovery: bool,
    /// Recovery scan interval
    pub recovery_interval: Duration,
    /// Maximum recovery attempts
    pub max_recovery_attempts: u32,
    
    // Performance tuning
    /// Enable metrics collection
    pub enable_metrics: bool,
    /// Metrics collection interval
    pub metrics_interval: Duration,
    /// Maximum cached results
    pub max_cache_entries: usize,
    /// Cache entry TTL
    pub cache_ttl: Duration,
    
    // Resource limits
    /// Maximum memory usage in bytes (0 = unlimited)
    pub max_memory_usage: usize,
    /// Maximum CPU usage percentage (0-100, 0 = unlimited)
    pub max_cpu_percentage: u8,
    /// Enable resource monitoring
    pub enable_resource_monitoring: bool,
}

impl Default for TaskConfig {
    fn default() -> Self {
        let cpu_count = num_cpus::get();
        
        Self {
            // Worker pool - scale with CPU count
            max_workers: cpu_count * 2,
            min_workers: cpu_count.max(2),
            worker_idle_timeout: Duration::from_secs(60),
            
            // Queue configuration
            queue_capacity: 10_000,
            max_priority_levels: 10,
            
            // Task execution
            task_timeout: Some(Duration::from_secs(300)), // 5 minutes default
            max_task_duration: Duration::from_secs(3600), // 1 hour hard limit
            heartbeat_interval: Duration::from_secs(30),
            stall_timeout: Duration::from_secs(120),
            
            // Retry configuration
            default_retries: 3,
            max_retries: 10,
            retry_delay: Duration::from_secs(1),
            max_retry_delay: Duration::from_secs(300),
            retry_backoff_multiplier: 2.0,
            
            // Storage
            db_path: None,
            db_cache_size: 64 * 1024 * 1024, // 64MB
            db_flush_interval: Duration::from_secs(5),
            db_wal_enabled: true,
            
            // Scheduler
            scheduler_interval: Duration::from_millis(100),
            max_schedule_batch: 100,
            enable_preemption: false,
            scheduling_quantum: Duration::from_secs(10),
            
            // Recovery
            enable_recovery: true,
            recovery_interval: Duration::from_secs(30),
            max_recovery_attempts: 5,
            
            // Performance
            enable_metrics: true,
            metrics_interval: Duration::from_secs(60),
            max_cache_entries: 10_000,
            cache_ttl: Duration::from_secs(3600),
            
            // Resource limits
            max_memory_usage: 0, // unlimited
            max_cpu_percentage: 0, // unlimited
            enable_resource_monitoring: false,
        }
    }
}

impl TaskConfig {
    /// Create a new builder for TaskConfig
    pub fn builder() -> TaskConfigBuilder {
        TaskConfigBuilder::new()
    }
    
    /// Validate the configuration
    pub fn validate(&self) -> Result<(), String> {
        // Worker validation
        if self.max_workers == 0 {
            return Err("max_workers must be greater than 0".to_string());
        }
        if self.min_workers > self.max_workers {
            return Err("min_workers cannot be greater than max_workers".to_string());
        }
        
        // Queue validation
        if self.queue_capacity == 0 {
            return Err("queue_capacity must be greater than 0".to_string());
        }
        if self.max_priority_levels == 0 {
            return Err("max_priority_levels must be greater than 0".to_string());
        }
        
        // Timeout validation
        if let Some(timeout) = self.task_timeout {
            if timeout > self.max_task_duration {
                return Err("task_timeout cannot exceed max_task_duration".to_string());
            }
        }
        if self.stall_timeout < self.heartbeat_interval {
            return Err("stall_timeout should be greater than heartbeat_interval".to_string());
        }
        
        // Retry validation
        if self.default_retries > self.max_retries {
            return Err("default_retries cannot exceed max_retries".to_string());
        }
        if self.retry_backoff_multiplier < 1.0 {
            return Err("retry_backoff_multiplier must be >= 1.0".to_string());
        }
        if self.retry_delay > self.max_retry_delay {
            return Err("retry_delay cannot exceed max_retry_delay".to_string());
        }
        
        // Resource validation
        if self.max_cpu_percentage > 100 {
            return Err("max_cpu_percentage cannot exceed 100".to_string());
        }
        
        // Performance validation
        if self.max_cache_entries == 0 && self.enable_metrics {
            return Err("max_cache_entries must be > 0 when metrics are enabled".to_string());
        }
        
        Ok(())
    }
    
    /// Create a configuration optimized for development/testing
    pub fn development() -> Self {
        Self {
            max_workers: 4,
            min_workers: 1,
            queue_capacity: 100,
            task_timeout: Some(Duration::from_secs(60)),
            heartbeat_interval: Duration::from_secs(5),
            db_flush_interval: Duration::from_secs(1),
            scheduler_interval: Duration::from_millis(50),
            recovery_interval: Duration::from_secs(10),
            metrics_interval: Duration::from_secs(10),
            ..Default::default()
        }
    }
    
    /// Create a configuration optimized for production
    pub fn production() -> Self {
        let cpu_count = num_cpus::get();
        
        Self {
            max_workers: cpu_count * 4,
            min_workers: cpu_count,
            queue_capacity: 100_000,
            db_cache_size: 256 * 1024 * 1024, // 256MB
            enable_resource_monitoring: true,
            max_schedule_batch: 1000,
            ..Default::default()
        }
    }
    
    /// Create a configuration for high-throughput scenarios
    pub fn high_throughput() -> Self {
        let cpu_count = num_cpus::get();
        
        Self {
            max_workers: cpu_count * 8,
            min_workers: cpu_count * 2,
            queue_capacity: 1_000_000,
            scheduler_interval: Duration::from_millis(10),
            max_schedule_batch: 10_000,
            db_cache_size: 512 * 1024 * 1024, // 512MB
            enable_preemption: true,
            ..Default::default()
        }
    }
}

/// Builder for TaskConfig
pub struct TaskConfigBuilder {
    config: TaskConfig,
}

impl TaskConfigBuilder {
    /// Create a new builder with default values
    pub fn new() -> Self {
        Self {
            config: TaskConfig::default(),
        }
    }
    
    /// Set maximum workers
    pub fn max_workers(mut self, max_workers: usize) -> Self {
        self.config.max_workers = max_workers;
        self
    }
    
    /// Set minimum workers
    pub fn min_workers(mut self, min_workers: usize) -> Self {
        self.config.min_workers = min_workers;
        self
    }
    
    /// Set queue capacity
    pub fn queue_capacity(mut self, capacity: usize) -> Self {
        self.config.queue_capacity = capacity;
        self
    }
    
    /// Set task timeout
    pub fn task_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.config.task_timeout = timeout;
        self
    }
    
    /// Set database path
    pub fn db_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.db_path = Some(path.into());
        self
    }
    
    /// Set retry configuration
    pub fn retries(mut self, default: u32, max: u32) -> Self {
        self.config.default_retries = default;
        self.config.max_retries = max;
        self
    }
    
    /// Set retry delays
    pub fn retry_delays(mut self, initial: Duration, max: Duration) -> Self {
        self.config.retry_delay = initial;
        self.config.max_retry_delay = max;
        self
    }
    
    /// Enable/disable recovery
    pub fn recovery(mut self, enabled: bool) -> Self {
        self.config.enable_recovery = enabled;
        self
    }
    
    /// Enable/disable metrics
    pub fn metrics(mut self, enabled: bool) -> Self {
        self.config.enable_metrics = enabled;
        self
    }
    
    /// Set resource limits
    pub fn resource_limits(mut self, max_memory: usize, max_cpu: u8) -> Self {
        self.config.max_memory_usage = max_memory;
        self.config.max_cpu_percentage = max_cpu;
        self.config.enable_resource_monitoring = max_memory > 0 || max_cpu > 0;
        self
    }
    
    /// Build and validate the configuration
    pub fn build(self) -> Result<TaskConfig, String> {
        self.config.validate()?;
        Ok(self.config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_default_config() {
        let config = TaskConfig::default();
        assert!(config.validate().is_ok());
    }
    
    #[test]
    fn test_development_config() {
        let config = TaskConfig::development();
        assert!(config.validate().is_ok());
        assert_eq!(config.max_workers, 4);
        assert_eq!(config.min_workers, 1);
    }
    
    #[test]
    fn test_production_config() {
        let config = TaskConfig::production();
        assert!(config.validate().is_ok());
        assert!(config.enable_resource_monitoring);
    }
    
    #[test]
    fn test_validation_errors() {
        let mut config = TaskConfig::default();
        
        // Test max_workers validation
        config.max_workers = 0;
        assert!(config.validate().is_err());
        config.max_workers = 10;
        
        // Test min > max workers
        config.min_workers = 20;
        assert!(config.validate().is_err());
        config.min_workers = 5;
        
        // Test invalid CPU percentage
        config.max_cpu_percentage = 150;
        assert!(config.validate().is_err());
    }
    
    #[test]
    fn test_builder() {
        let config = TaskConfig::builder()
            .max_workers(20)
            .min_workers(5)
            .queue_capacity(5000)
            .task_timeout(Some(Duration::from_secs(120)))
            .db_path("/tmp/test_db")
            .retries(5, 15)
            .metrics(true)
            .build()
            .unwrap();
        
        assert_eq!(config.max_workers, 20);
        assert_eq!(config.min_workers, 5);
        assert_eq!(config.queue_capacity, 5000);
        assert_eq!(config.default_retries, 5);
        assert_eq!(config.max_retries, 15);
        assert!(config.enable_metrics);
    }
}