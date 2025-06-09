use crate::core::errors::{DaggerError, Result};
use crate::core::memory::{Cache, CacheConfig};
use crate::core::limits::{ResourceLimits, ResourceTracker};
use crate::core::performance::PerformanceConfig;
use crate::core::concurrency::{ConcurrentTaskRegistry, TaskNotificationSystem};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Unified configuration for the entire Dagger system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaggerConfig {
    /// Cache configuration
    pub cache: CacheConfig,
    /// Resource limits
    pub limits: ResourceLimits,
    /// Performance settings
    pub performance: PerformanceConfig,
    /// Database path
    pub database_path: Option<PathBuf>,
    /// Logging configuration
    pub logging: LoggingConfig,
    /// Network configuration
    pub network: NetworkConfig,
    /// Security settings
    pub security: SecurityConfig,
}

impl Default for DaggerConfig {
    fn default() -> Self {
        Self {
            cache: CacheConfig::default(),
            limits: ResourceLimits::default(),
            performance: PerformanceConfig::default(),
            database_path: None,
            logging: LoggingConfig::default(),
            network: NetworkConfig::default(),
            security: SecurityConfig::default(),
        }
    }
}

impl DaggerConfig {
    /// Validate the entire configuration
    pub fn validate(&self) -> Result<()> {
        self.cache.validate()?;
        self.limits.validate()?;
        self.logging.validate()?;
        self.network.validate()?;
        self.security.validate()?;
        Ok(())
    }

    /// Create a configuration optimized for development
    pub fn development() -> Self {
        Self {
            cache: CacheConfig {
                max_memory_bytes: 50 * 1024 * 1024, // 50MB
                max_entries: 1_000,
                cleanup_interval: Duration::from_secs(60),
                ..Default::default()
            },
            limits: ResourceLimits::conservative(),
            performance: PerformanceConfig {
                async_serialization_threshold: 50_000, // 50KB
                database_batch_size: 50,
                connection_pool_size: 5,
                background_task_interval: Duration::from_secs(60),
                ..Default::default()
            },
            logging: LoggingConfig {
                level: LogLevel::Debug,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Create a configuration optimized for production
    pub fn production() -> Self {
        Self {
            cache: CacheConfig {
                max_memory_bytes: 500 * 1024 * 1024, // 500MB
                max_entries: 100_000,
                cleanup_interval: Duration::from_secs(30),
                ..Default::default()
            },
            limits: ResourceLimits::default(),
            performance: PerformanceConfig::default(),
            logging: LoggingConfig {
                level: LogLevel::Info,
                structured: true,
                ..Default::default()
            },
            security: SecurityConfig {
                enable_auth: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Create a configuration optimized for testing
    pub fn testing() -> Self {
        Self {
            cache: CacheConfig {
                max_memory_bytes: 10 * 1024 * 1024, // 10MB
                max_entries: 100,
                ttl: Duration::from_secs(60),
                cleanup_interval: Duration::from_secs(10),
                ..Default::default()
            },
            limits: ResourceLimits::conservative(),
            performance: PerformanceConfig {
                async_serialization_threshold: 10_000, // 10KB
                database_batch_size: 10,
                connection_pool_size: 2,
                background_task_interval: Duration::from_secs(5),
                ..Default::default()
            },
            logging: LoggingConfig {
                level: LogLevel::Trace,
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: LogLevel,
    pub structured: bool,
    pub file_path: Option<PathBuf>,
    pub max_file_size_mb: usize,
    pub max_files: usize,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            structured: false,
            file_path: None,
            max_file_size_mb: 100,
            max_files: 5,
        }
    }
}

impl LoggingConfig {
    pub fn validate(&self) -> Result<()> {
        if self.max_file_size_mb == 0 {
            return Err(DaggerError::configuration("max_file_size_mb must be greater than 0"));
        }
        if self.max_files == 0 {
            return Err(DaggerError::configuration("max_files must be greater than 0"));
        }
        Ok(())
    }
}

/// Log level enumeration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// Network configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub bind_address: String,
    pub port: u16,
    pub max_connections: usize,
    pub connection_timeout: Duration,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
    pub keep_alive: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1".to_string(),
            port: 8080,
            max_connections: 1000,
            connection_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(30),
            keep_alive: true,
        }
    }
}

impl NetworkConfig {
    pub fn validate(&self) -> Result<()> {
        if self.port == 0 {
            return Err(DaggerError::configuration("port must be greater than 0"));
        }
        if self.max_connections == 0 {
            return Err(DaggerError::configuration("max_connections must be greater than 0"));
        }
        Ok(())
    }
}

/// Security configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub enable_auth: bool,
    pub auth_token_ttl: Duration,
    pub enable_tls: bool,
    pub cert_path: Option<PathBuf>,
    pub key_path: Option<PathBuf>,
    pub allowed_origins: Vec<String>,
    pub rate_limit_requests_per_minute: usize,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            enable_auth: false,
            auth_token_ttl: Duration::from_secs(3600), // 1 hour
            enable_tls: false,
            cert_path: None,
            key_path: None,
            allowed_origins: vec!["*".to_string()],
            rate_limit_requests_per_minute: 1000,
        }
    }
}

impl SecurityConfig {
    pub fn validate(&self) -> Result<()> {
        if self.enable_tls {
            if self.cert_path.is_none() {
                return Err(DaggerError::configuration("cert_path required when TLS is enabled"));
            }
            if self.key_path.is_none() {
                return Err(DaggerError::configuration("key_path required when TLS is enabled"));
            }
        }
        if self.rate_limit_requests_per_minute == 0 {
            return Err(DaggerError::configuration("rate_limit_requests_per_minute must be greater than 0"));
        }
        Ok(())
    }
}

/// Fluent builder for Dagger configuration
#[derive(Debug)]
pub struct DaggerBuilder {
    config: DaggerConfig,
}

impl Default for DaggerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DaggerBuilder {
    /// Create a new builder with default configuration
    pub fn new() -> Self {
        Self {
            config: DaggerConfig::default(),
        }
    }

    /// Start with development configuration
    pub fn development() -> Self {
        Self {
            config: DaggerConfig::development(),
        }
    }

    /// Start with production configuration
    pub fn production() -> Self {
        Self {
            config: DaggerConfig::production(),
        }
    }

    /// Start with testing configuration
    pub fn testing() -> Self {
        Self {
            config: DaggerConfig::testing(),
        }
    }

    /// Configure cache settings
    pub fn cache<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(CacheConfigBuilder) -> CacheConfigBuilder,
    {
        let builder = CacheConfigBuilder::new(self.config.cache);
        self.config.cache = configure(builder).build();
        self
    }

    /// Configure resource limits
    pub fn limits<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(ResourceLimitsBuilder) -> ResourceLimitsBuilder,
    {
        let builder = ResourceLimitsBuilder::new(self.config.limits);
        self.config.limits = configure(builder).build();
        self
    }

    /// Configure performance settings
    pub fn performance<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(PerformanceConfigBuilder) -> PerformanceConfigBuilder,
    {
        let builder = PerformanceConfigBuilder::new(self.config.performance);
        self.config.performance = configure(builder).build();
        self
    }

    /// Configure logging
    pub fn logging<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(LoggingConfigBuilder) -> LoggingConfigBuilder,
    {
        let builder = LoggingConfigBuilder::new(self.config.logging);
        self.config.logging = configure(builder).build();
        self
    }

    /// Configure network settings
    pub fn network<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(NetworkConfigBuilder) -> NetworkConfigBuilder,
    {
        let builder = NetworkConfigBuilder::new(self.config.network);
        self.config.network = configure(builder).build();
        self
    }

    /// Configure security settings
    pub fn security<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(SecurityConfigBuilder) -> SecurityConfigBuilder,
    {
        let builder = SecurityConfigBuilder::new(self.config.security);
        self.config.security = configure(builder).build();
        self
    }

    /// Set database path
    pub fn database_path<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.config.database_path = Some(path.into());
        self
    }

    /// Build and validate the configuration
    pub fn build(self) -> Result<DaggerConfig> {
        self.config.validate()?;
        Ok(self.config)
    }

    /// Build without validation (for testing)
    pub fn build_unchecked(self) -> DaggerConfig {
        self.config
    }
}

/// Builder for cache configuration
#[derive(Debug)]
pub struct CacheConfigBuilder {
    config: CacheConfig,
}

impl CacheConfigBuilder {
    fn new(config: CacheConfig) -> Self {
        Self { config }
    }

    pub fn max_memory_bytes(mut self, bytes: usize) -> Self {
        self.config.max_memory_bytes = bytes;
        self
    }

    pub fn max_memory_mb(mut self, mb: usize) -> Self {
        self.config.max_memory_bytes = mb * 1024 * 1024;
        self
    }

    pub fn max_entries(mut self, entries: usize) -> Self {
        self.config.max_entries = entries;
        self
    }

    pub fn ttl(mut self, duration: Duration) -> Self {
        self.config.ttl = duration;
        self
    }

    pub fn cleanup_interval(mut self, duration: Duration) -> Self {
        self.config.cleanup_interval = duration;
        self
    }

    pub fn water_marks(mut self, low: f64, high: f64) -> Self {
        self.config.low_water_mark = low;
        self.config.high_water_mark = high;
        self
    }

    fn build(self) -> CacheConfig {
        self.config
    }
}

/// Builder for resource limits
#[derive(Debug)]
pub struct ResourceLimitsBuilder {
    limits: ResourceLimits,
}

impl ResourceLimitsBuilder {
    fn new(limits: ResourceLimits) -> Self {
        Self { limits }
    }

    pub fn max_memory_bytes(mut self, bytes: u64) -> Self {
        self.limits.max_memory_bytes = bytes;
        self
    }

    pub fn max_memory_gb(mut self, gb: u64) -> Self {
        self.limits.max_memory_bytes = gb * 1024 * 1024 * 1024;
        self
    }

    pub fn max_concurrent_tasks(mut self, tasks: usize) -> Self {
        self.limits.max_concurrent_tasks = tasks;
        self
    }

    pub fn max_execution_tree_nodes(mut self, nodes: usize) -> Self {
        self.limits.max_execution_tree_nodes = nodes;
        self
    }

    pub fn max_cache_entries(mut self, entries: usize) -> Self {
        self.limits.max_cache_entries = entries;
        self
    }

    pub fn max_file_handles(mut self, handles: usize) -> Self {
        self.limits.max_file_handles = handles;
        self
    }

    pub fn max_network_connections(mut self, connections: usize) -> Self {
        self.limits.max_network_connections = connections;
        self
    }

    pub fn task_execution_timeout(mut self, duration: Duration) -> Self {
        self.limits.max_task_execution_time = duration;
        self
    }

    pub fn total_execution_timeout(mut self, duration: Duration) -> Self {
        self.limits.max_total_execution_time = duration;
        self
    }

    pub fn max_dependency_depth(mut self, depth: usize) -> Self {
        self.limits.max_dependency_depth = depth;
        self
    }

    pub fn max_task_retries(mut self, retries: u32) -> Self {
        self.limits.max_task_retries = retries;
        self
    }

    pub fn max_message_size_bytes(mut self, bytes: usize) -> Self {
        self.limits.max_message_size_bytes = bytes;
        self
    }

    pub fn max_message_size_mb(mut self, mb: usize) -> Self {
        self.limits.max_message_size_bytes = mb * 1024 * 1024;
        self
    }

    pub fn max_channel_queue_size(mut self, size: usize) -> Self {
        self.limits.max_channel_queue_size = size;
        self
    }

    pub fn max_cpu_usage_percent(mut self, percent: f64) -> Self {
        self.limits.max_cpu_usage_percent = percent;
        self
    }

    fn build(self) -> ResourceLimits {
        self.limits
    }
}

/// Builder for performance configuration
#[derive(Debug)]
pub struct PerformanceConfigBuilder {
    config: PerformanceConfig,
}

impl PerformanceConfigBuilder {
    fn new(config: PerformanceConfig) -> Self {
        Self { config }
    }

    pub fn async_serialization_threshold(mut self, bytes: usize) -> Self {
        self.config.async_serialization_threshold = bytes;
        self
    }

    pub fn async_serialization_threshold_kb(mut self, kb: usize) -> Self {
        self.config.async_serialization_threshold = kb * 1024;
        self
    }

    pub fn database_batch_size(mut self, size: usize) -> Self {
        self.config.database_batch_size = size;
        self
    }

    pub fn connection_pool_size(mut self, size: usize) -> Self {
        self.config.connection_pool_size = size;
        self
    }

    pub fn background_task_interval(mut self, duration: Duration) -> Self {
        self.config.background_task_interval = duration;
        self
    }

    pub fn enable_cache_prewarming(mut self, enabled: bool) -> Self {
        self.config.cache_prewarming_enabled = enabled;
        self
    }

    pub fn enable_object_pooling(mut self, enabled: bool) -> Self {
        self.config.object_pooling_enabled = enabled;
        self
    }

    pub fn compression_threshold(mut self, bytes: usize) -> Self {
        self.config.compression_threshold = bytes;
        self
    }

    pub fn compression_threshold_mb(mut self, mb: usize) -> Self {
        self.config.compression_threshold = mb * 1024 * 1024;
        self
    }

    fn build(self) -> PerformanceConfig {
        self.config
    }
}

/// Builder for logging configuration
#[derive(Debug)]
pub struct LoggingConfigBuilder {
    config: LoggingConfig,
}

impl LoggingConfigBuilder {
    fn new(config: LoggingConfig) -> Self {
        Self { config }
    }

    pub fn level(mut self, level: LogLevel) -> Self {
        self.config.level = level;
        self
    }

    pub fn trace(mut self) -> Self {
        self.config.level = LogLevel::Trace;
        self
    }

    pub fn debug(mut self) -> Self {
        self.config.level = LogLevel::Debug;
        self
    }

    pub fn info(mut self) -> Self {
        self.config.level = LogLevel::Info;
        self
    }

    pub fn warn(mut self) -> Self {
        self.config.level = LogLevel::Warn;
        self
    }

    pub fn error(mut self) -> Self {
        self.config.level = LogLevel::Error;
        self
    }

    pub fn structured(mut self, enabled: bool) -> Self {
        self.config.structured = enabled;
        self
    }

    pub fn file_path<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.config.file_path = Some(path.into());
        self
    }

    pub fn max_file_size_mb(mut self, mb: usize) -> Self {
        self.config.max_file_size_mb = mb;
        self
    }

    pub fn max_files(mut self, count: usize) -> Self {
        self.config.max_files = count;
        self
    }

    fn build(self) -> LoggingConfig {
        self.config
    }
}

/// Builder for network configuration
#[derive(Debug)]
pub struct NetworkConfigBuilder {
    config: NetworkConfig,
}

impl NetworkConfigBuilder {
    fn new(config: NetworkConfig) -> Self {
        Self { config }
    }

    pub fn bind_address<S: Into<String>>(mut self, address: S) -> Self {
        self.config.bind_address = address.into();
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.config.port = port;
        self
    }

    pub fn max_connections(mut self, connections: usize) -> Self {
        self.config.max_connections = connections;
        self
    }

    pub fn connection_timeout(mut self, duration: Duration) -> Self {
        self.config.connection_timeout = duration;
        self
    }

    pub fn read_timeout(mut self, duration: Duration) -> Self {
        self.config.read_timeout = duration;
        self
    }

    pub fn write_timeout(mut self, duration: Duration) -> Self {
        self.config.write_timeout = duration;
        self
    }

    pub fn keep_alive(mut self, enabled: bool) -> Self {
        self.config.keep_alive = enabled;
        self
    }

    fn build(self) -> NetworkConfig {
        self.config
    }
}

/// Builder for security configuration
#[derive(Debug)]
pub struct SecurityConfigBuilder {
    config: SecurityConfig,
}

impl SecurityConfigBuilder {
    fn new(config: SecurityConfig) -> Self {
        Self { config }
    }

    pub fn enable_auth(mut self, enabled: bool) -> Self {
        self.config.enable_auth = enabled;
        self
    }

    pub fn auth_token_ttl(mut self, duration: Duration) -> Self {
        self.config.auth_token_ttl = duration;
        self
    }

    pub fn enable_tls(mut self, enabled: bool) -> Self {
        self.config.enable_tls = enabled;
        self
    }

    pub fn cert_path<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.config.cert_path = Some(path.into());
        self
    }

    pub fn key_path<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.config.key_path = Some(path.into());
        self
    }

    pub fn allowed_origins<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.config.allowed_origins = origins.into_iter().map(|s| s.into()).collect();
        self
    }

    pub fn rate_limit_requests_per_minute(mut self, requests: usize) -> Self {
        self.config.rate_limit_requests_per_minute = requests;
        self
    }

    fn build(self) -> SecurityConfig {
        self.config
    }
}

/// Main Dagger system builder
#[derive(Debug)]
pub struct Dagger {
    config: DaggerConfig,
    cache: Option<Arc<Cache>>,
    resource_tracker: Option<Arc<ResourceTracker>>,
    task_registry: Option<Arc<ConcurrentTaskRegistry>>,
}

impl Dagger {
    /// Create a new Dagger instance with configuration
    pub fn new(config: DaggerConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            cache: None,
            resource_tracker: None,
            task_registry: None,
        })
    }

    /// Create from builder
    pub fn from_builder(builder: DaggerBuilder) -> Result<Self> {
        let config = builder.build()?;
        Self::new(config)
    }

    /// Initialize all components
    pub async fn initialize(mut self) -> Result<Self> {
        // Initialize cache
        let cache = Cache::new(self.config.cache.clone())?;
        self.cache = Some(Arc::new(cache.with_cleanup()));

        // Initialize resource tracker
        let resource_tracker = ResourceTracker::new(self.config.limits.clone())?;
        self.resource_tracker = Some(Arc::new(resource_tracker));

        // Initialize task registry
        let task_registry = ConcurrentTaskRegistry::new(self.config.limits.max_concurrent_tasks);
        self.task_registry = Some(Arc::new(task_registry));

        Ok(self)
    }

    /// Get cache instance
    pub fn cache(&self) -> Option<&Arc<Cache>> {
        self.cache.as_ref()
    }

    /// Get resource tracker
    pub fn resource_tracker(&self) -> Option<&Arc<ResourceTracker>> {
        self.resource_tracker.as_ref()
    }

    /// Get task registry
    pub fn task_registry(&self) -> Option<&Arc<ConcurrentTaskRegistry>> {
        self.task_registry.as_ref()
    }

    /// Get configuration
    pub fn config(&self) -> &DaggerConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_pattern() {
        let config = DaggerBuilder::new()
            .cache(|cache| cache.max_memory_mb(100).max_entries(1000))
            .limits(|limits| {
                limits
                    .max_concurrent_tasks(50)
                    .max_memory_gb(2)
                    .task_execution_timeout(Duration::from_secs(300))
            })
            .performance(|perf| {
                perf.async_serialization_threshold_kb(50)
                    .database_batch_size(100)
                    .enable_object_pooling(true)
            })
            .logging(|log| log.debug().structured(true))
            .network(|net| net.port(8080).max_connections(1000))
            .security(|sec| sec.enable_auth(true).rate_limit_requests_per_minute(1000))
            .database_path("/tmp/dagger.db")
            .build()
            .unwrap();

        assert_eq!(config.cache.max_memory_bytes, 100 * 1024 * 1024);
        assert_eq!(config.limits.max_concurrent_tasks, 50);
        assert_eq!(config.performance.async_serialization_threshold, 50 * 1024);
        assert_eq!(config.network.port, 8080);
        assert!(config.security.enable_auth);
    }

    #[test]
    fn test_preset_configurations() {
        let dev_config = DaggerConfig::development();
        assert_eq!(dev_config.cache.max_memory_bytes, 50 * 1024 * 1024);
        assert!(matches!(dev_config.logging.level, LogLevel::Debug));

        let prod_config = DaggerConfig::production();
        assert_eq!(prod_config.cache.max_memory_bytes, 500 * 1024 * 1024);
        assert!(matches!(prod_config.logging.level, LogLevel::Info));
        assert!(prod_config.security.enable_auth);

        let test_config = DaggerConfig::testing();
        assert_eq!(test_config.cache.max_memory_bytes, 10 * 1024 * 1024);
        assert!(matches!(test_config.logging.level, LogLevel::Trace));
    }

    #[tokio::test]
    async fn test_dagger_initialization() {
        let config = DaggerBuilder::testing().build().unwrap();
        let dagger = Dagger::new(config).unwrap().initialize().await.unwrap();

        assert!(dagger.cache().is_some());
        assert!(dagger.resource_tracker().is_some());
        assert!(dagger.task_registry().is_some());
    }

    #[test]
    fn test_configuration_validation() {
        // Test invalid configuration
        let invalid_config = DaggerConfig {
            limits: ResourceLimits {
                max_memory_bytes: 0, // Invalid
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(invalid_config.validate().is_err());
    }
}