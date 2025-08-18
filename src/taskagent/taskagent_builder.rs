// use crate::core::builders::DaggerConfig;
use crate::core::errors::{DaggerError, Result};
use crate::core::limits::ResourceTracker;
use crate::core::memory::Cache;
// use crate::core::concurrency::{ConcurrentTaskRegistry, TaskNotificationSystem, AtomicTaskState};
// use crate::core::performance::PerformanceMonitor;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

/// Task agent configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAgentConfig {
    /// Agent name/identifier
    pub name: String,
    /// Agent description
    pub description: Option<String>,
    /// Maximum number of concurrent tasks
    pub max_concurrent_tasks: usize,
    /// Task polling interval
    pub polling_interval: Duration,
    /// Task execution timeout
    pub task_timeout: Duration,
    /// Agent heartbeat interval
    pub heartbeat_interval: Duration,
    /// Graceful shutdown timeout
    pub shutdown_timeout: Duration,
    /// Task types this agent can handle
    pub supported_task_types: Vec<String>,
    /// Agent capabilities/features
    pub capabilities: AgentCapabilities,
    /// Custom agent metadata
    pub metadata: HashMap<String, Value>,
    /// Agent-specific resource limits
    pub resource_limits: Option<AgentResourceLimits>,
}

impl Default for TaskAgentConfig {
    fn default() -> Self {
        Self {
            name: format!("agent_{}", Uuid::new_v4()),
            description: None,
            max_concurrent_tasks: 5,
            polling_interval: Duration::from_secs(1),
            task_timeout: Duration::from_secs(300), // 5 minutes
            heartbeat_interval: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(30),
            supported_task_types: vec!["*".to_string()], // Support all types by default
            capabilities: AgentCapabilities::default(),
            metadata: HashMap::new(),
            resource_limits: None,
        }
    }
}

/// Agent capabilities and features
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilities {
    /// Whether the agent supports task caching
    pub caching: bool,
    /// Whether the agent supports result streaming
    pub streaming: bool,
    /// Whether the agent supports partial results
    pub partial_results: bool,
    /// Whether the agent supports task cancellation
    pub cancellation: bool,
    /// Whether the agent supports task retries
    pub retries: bool,
    /// Whether the agent supports result validation
    pub validation: bool,
    /// Custom capabilities
    pub custom: HashMap<String, Value>,
}

impl Default for AgentCapabilities {
    fn default() -> Self {
        Self {
            caching: true,
            streaming: false,
            partial_results: false,
            cancellation: true,
            retries: true,
            validation: false,
            custom: HashMap::new(),
        }
    }
}

/// Agent-specific resource limits
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResourceLimits {
    /// Maximum memory usage in bytes
    pub max_memory_bytes: u64,
    /// Maximum CPU usage percentage
    pub max_cpu_percent: f64,
    /// Maximum disk usage in bytes
    pub max_disk_bytes: u64,
    /// Maximum network bandwidth in bytes/sec
    pub max_network_bytes_per_sec: u64,
    /// Maximum file handles
    pub max_file_handles: usize,
}

impl Default for AgentResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 512 * 1024 * 1024, // 512MB
            max_cpu_percent: 80.0,
            max_disk_bytes: 1024 * 1024 * 1024,          // 1GB
            max_network_bytes_per_sec: 10 * 1024 * 1024, // 10MB/s
            max_file_handles: 100,
        }
    }
}

/// Task execution context for agents
#[derive(Debug, Clone)]
pub struct TaskExecutionContext {
    /// Task ID
    pub task_id: String,
    /// Job ID
    pub job_id: String,
    /// Task type
    pub task_type: String,
    /// Task configuration/parameters
    pub config: Value,
    /// Task metadata
    pub metadata: HashMap<String, Value>,
    /// Execution timeout
    pub timeout: Duration,
    /// Retry count
    pub retry_count: u32,
    /// Maximum retries allowed
    pub max_retries: u32,
    /// Task dependencies (if any)
    pub dependencies: Vec<String>,
    /// Expected result schema (if any)
    pub result_schema: Option<Value>,
}

/// Task execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskExecutionResult {
    /// Task ID
    pub task_id: String,
    /// Execution status
    pub status: TaskExecutionStatus,
    /// Task result data
    pub result: Option<Value>,
    /// Error information if failed
    pub error: Option<String>,
    /// Execution metrics
    pub metrics: TaskExecutionMetrics,
    /// Result metadata
    pub metadata: HashMap<String, Value>,
}

/// Task execution status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskExecutionStatus {
    Success,
    Failed,
    Timeout,
    Cancelled,
    PartialSuccess,
}

/// Task execution metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskExecutionMetrics {
    /// Execution duration
    pub duration: Duration,
    /// Memory usage peak
    pub peak_memory_bytes: u64,
    /// CPU time used
    pub cpu_time: Duration,
    /// Number of cache hits
    pub cache_hits: u32,
    /// Number of cache misses
    pub cache_misses: u32,
    /// Custom metrics
    pub custom: HashMap<String, f64>,
}

impl Default for TaskExecutionMetrics {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(0),
            peak_memory_bytes: 0,
            cpu_time: Duration::from_secs(0),
            cache_hits: 0,
            cache_misses: 0,
            custom: HashMap::new(),
        }
    }
}

/// Agent runtime state
#[derive(Debug, Clone)]
pub struct AgentState {
    /// Current status
    pub status: AgentStatus,
    /// Currently executing tasks
    pub active_tasks: HashMap<String, TaskExecutionContext>,
    /// Total tasks processed
    pub total_tasks_processed: u64,
    /// Total successful tasks
    pub successful_tasks: u64,
    /// Total failed tasks
    pub failed_tasks: u64,
    /// Agent start time
    pub start_time: std::time::Instant,
    /// Last heartbeat time
    pub last_heartbeat: std::time::Instant,
    /// Agent metrics
    pub metrics: AgentMetrics,
}

/// Agent status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentStatus {
    Starting,
    Idle,
    Busy,
    Stopping,
    Stopped,
    Error(String),
}

/// Agent performance metrics
#[derive(Debug, Clone, Default)]
pub struct AgentMetrics {
    /// Average task execution time
    pub avg_execution_time: Duration,
    /// Task success rate
    pub success_rate: f64,
    /// Current memory usage
    pub memory_usage_bytes: u64,
    /// Current CPU usage
    pub cpu_usage_percent: f64,
    /// Tasks per second (throughput)
    pub tasks_per_second: f64,
    /// Custom metrics
    pub custom: HashMap<String, f64>,
}

/// Task agent execution context
#[derive(Debug)]
pub struct TaskAgentExecutionContext {
    /// Agent configuration
    pub config: TaskAgentConfig,
    /// Shared cache
    pub cache: Arc<Cache>,
    /// Resource tracker
    pub resource_tracker: Arc<ResourceTracker>,
    /// Agent state
    pub state: Arc<RwLock<AgentState>>,
    /// Shutdown signal
    pub shutdown_signal: Arc<tokio::sync::Notify>,
}

/// Fluent builder for TaskAgent configuration
#[derive(Debug)]
pub struct TaskAgentBuilder {
    config: TaskAgentConfig,
}

impl Default for TaskAgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskAgentBuilder {
    /// Create a new TaskAgent builder
    pub fn new() -> Self {
        Self {
            config: TaskAgentConfig::default(),
        }
    }

    /// Create a builder with a specific name
    pub fn named<S: Into<String>>(name: S) -> Self {
        Self {
            config: TaskAgentConfig {
                name: name.into(),
                ..Default::default()
            },
        }
    }

    /// Set agent name
    pub fn name<S: Into<String>>(mut self, name: S) -> Self {
        self.config.name = name.into();
        self
    }

    /// Set agent description
    pub fn description<S: Into<String>>(mut self, description: S) -> Self {
        self.config.description = Some(description.into());
        self
    }

    /// Set maximum concurrent tasks
    pub fn max_concurrent_tasks(mut self, count: usize) -> Self {
        self.config.max_concurrent_tasks = count;
        self
    }

    /// Set task polling interval
    pub fn polling_interval(mut self, interval: Duration) -> Self {
        self.config.polling_interval = interval;
        self
    }

    /// Set task execution timeout
    pub fn task_timeout(mut self, timeout: Duration) -> Self {
        self.config.task_timeout = timeout;
        self
    }

    /// Set heartbeat interval
    pub fn heartbeat_interval(mut self, interval: Duration) -> Self {
        self.config.heartbeat_interval = interval;
        self
    }

    /// Set shutdown timeout
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.config.shutdown_timeout = timeout;
        self
    }

    /// Set supported task types
    pub fn supported_task_types<I, S>(mut self, types: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.config.supported_task_types = types.into_iter().map(|s| s.into()).collect();
        self
    }

    /// Support all task types
    pub fn support_all_types(mut self) -> Self {
        self.config.supported_task_types = vec!["*".to_string()];
        self
    }

    /// Support specific task types
    pub fn support_types<I, S>(mut self, types: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.config.supported_task_types = types.into_iter().map(|s| s.into()).collect();
        self
    }

    /// Configure agent capabilities
    pub fn capabilities<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(CapabilitiesBuilder) -> CapabilitiesBuilder,
    {
        let builder = CapabilitiesBuilder::new(self.config.capabilities);
        self.config.capabilities = configure(builder).build();
        self
    }

    /// Configure resource limits
    pub fn resource_limits<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(AgentResourceLimitsBuilder) -> AgentResourceLimitsBuilder,
    {
        let limits = self.config.resource_limits.unwrap_or_default();
        let builder = AgentResourceLimitsBuilder::new(limits);
        self.config.resource_limits = Some(configure(builder).build());
        self
    }

    /// Add metadata
    pub fn metadata<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Serialize,
    {
        if let Ok(json_value) = serde_json::to_value(value) {
            self.config.metadata.insert(key.into(), json_value);
        }
        self
    }

    /// Build the configuration
    pub fn build(self) -> Result<TaskAgentConfig> {
        self.validate()?;
        Ok(self.config)
    }

    /// Build without validation (for testing)
    pub fn build_unchecked(self) -> TaskAgentConfig {
        self.config
    }

    /// Validate the configuration
    fn validate(&self) -> Result<()> {
        if self.config.name.is_empty() {
            return Err(DaggerError::configuration("Agent name cannot be empty"));
        }

        if self.config.max_concurrent_tasks == 0 {
            return Err(DaggerError::configuration(
                "max_concurrent_tasks must be greater than 0",
            ));
        }

        if self.config.supported_task_types.is_empty() {
            return Err(DaggerError::configuration(
                "Agent must support at least one task type",
            ));
        }

        Ok(())
    }

    /// Build execution context
    pub fn build_execution_context(
        self,
        cache: Arc<Cache>,
        resource_tracker: Arc<ResourceTracker>,
    ) -> Result<TaskAgentExecutionContext> {
        let config = self.build()?;

        let state = Arc::new(RwLock::new(AgentState {
            status: AgentStatus::Starting,
            active_tasks: HashMap::new(),
            total_tasks_processed: 0,
            successful_tasks: 0,
            failed_tasks: 0,
            start_time: std::time::Instant::now(),
            last_heartbeat: std::time::Instant::now(),
            metrics: AgentMetrics::default(),
        }));

        let shutdown_signal = Arc::new(tokio::sync::Notify::new());

        Ok(TaskAgentExecutionContext {
            config,
            cache,
            resource_tracker,
            state,
            shutdown_signal,
        })
    }
}

/// Builder for agent capabilities
#[derive(Debug)]
pub struct CapabilitiesBuilder {
    capabilities: AgentCapabilities,
}

impl CapabilitiesBuilder {
    fn new(capabilities: AgentCapabilities) -> Self {
        Self { capabilities }
    }

    /// Enable/disable caching
    pub fn caching(mut self, enabled: bool) -> Self {
        self.capabilities.caching = enabled;
        self
    }

    /// Enable/disable streaming
    pub fn streaming(mut self, enabled: bool) -> Self {
        self.capabilities.streaming = enabled;
        self
    }

    /// Enable/disable partial results
    pub fn partial_results(mut self, enabled: bool) -> Self {
        self.capabilities.partial_results = enabled;
        self
    }

    /// Enable/disable cancellation
    pub fn cancellation(mut self, enabled: bool) -> Self {
        self.capabilities.cancellation = enabled;
        self
    }

    /// Enable/disable retries
    pub fn retries(mut self, enabled: bool) -> Self {
        self.capabilities.retries = enabled;
        self
    }

    /// Enable/disable validation
    pub fn validation(mut self, enabled: bool) -> Self {
        self.capabilities.validation = enabled;
        self
    }

    /// Add custom capability
    pub fn custom<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Serialize,
    {
        if let Ok(json_value) = serde_json::to_value(value) {
            self.capabilities.custom.insert(key.into(), json_value);
        }
        self
    }

    /// Enable all basic capabilities
    pub fn enable_all_basic(mut self) -> Self {
        self.capabilities.caching = true;
        self.capabilities.streaming = true;
        self.capabilities.partial_results = true;
        self.capabilities.cancellation = true;
        self.capabilities.retries = true;
        self.capabilities.validation = true;
        self
    }

    fn build(self) -> AgentCapabilities {
        self.capabilities
    }
}

/// Builder for agent resource limits
#[derive(Debug)]
pub struct AgentResourceLimitsBuilder {
    limits: AgentResourceLimits,
}

impl AgentResourceLimitsBuilder {
    fn new(limits: AgentResourceLimits) -> Self {
        Self { limits }
    }

    /// Set maximum memory in bytes
    pub fn max_memory_bytes(mut self, bytes: u64) -> Self {
        self.limits.max_memory_bytes = bytes;
        self
    }

    /// Set maximum memory in MB
    pub fn max_memory_mb(mut self, mb: u64) -> Self {
        self.limits.max_memory_bytes = mb * 1024 * 1024;
        self
    }

    /// Set maximum memory in GB
    pub fn max_memory_gb(mut self, gb: u64) -> Self {
        self.limits.max_memory_bytes = gb * 1024 * 1024 * 1024;
        self
    }

    /// Set maximum CPU usage percentage
    pub fn max_cpu_percent(mut self, percent: f64) -> Self {
        self.limits.max_cpu_percent = percent;
        self
    }

    /// Set maximum disk usage in bytes
    pub fn max_disk_bytes(mut self, bytes: u64) -> Self {
        self.limits.max_disk_bytes = bytes;
        self
    }

    /// Set maximum disk usage in GB
    pub fn max_disk_gb(mut self, gb: u64) -> Self {
        self.limits.max_disk_bytes = gb * 1024 * 1024 * 1024;
        self
    }

    /// Set maximum network bandwidth in bytes/sec
    pub fn max_network_bytes_per_sec(mut self, bytes_per_sec: u64) -> Self {
        self.limits.max_network_bytes_per_sec = bytes_per_sec;
        self
    }

    /// Set maximum network bandwidth in MB/sec
    pub fn max_network_mb_per_sec(mut self, mb_per_sec: u64) -> Self {
        self.limits.max_network_bytes_per_sec = mb_per_sec * 1024 * 1024;
        self
    }

    /// Set maximum file handles
    pub fn max_file_handles(mut self, handles: usize) -> Self {
        self.limits.max_file_handles = handles;
        self
    }

    fn build(self) -> AgentResourceLimits {
        self.limits
    }
}

/// Convenience functions for creating common agent configurations
impl TaskAgentBuilder {
    /// Create a lightweight agent for simple tasks
    pub fn lightweight() -> Self {
        Self::new()
            .max_concurrent_tasks(2)
            .polling_interval(Duration::from_secs(2))
            .task_timeout(Duration::from_secs(60))
            .resource_limits(|limits| {
                limits
                    .max_memory_mb(256)
                    .max_cpu_percent(50.0)
                    .max_file_handles(50)
            })
            .capabilities(|caps| {
                caps.caching(true)
                    .streaming(false)
                    .partial_results(false)
                    .cancellation(true)
                    .retries(true)
            })
    }

    /// Create a high-performance agent for heavy tasks
    pub fn high_performance() -> Self {
        Self::new()
            .max_concurrent_tasks(20)
            .polling_interval(Duration::from_millis(500))
            .task_timeout(Duration::from_secs(1800)) // 30 minutes
            .resource_limits(|limits| {
                limits
                    .max_memory_gb(4)
                    .max_cpu_percent(90.0)
                    .max_file_handles(500)
                    .max_network_mb_per_sec(100)
            })
            .capabilities(|caps| caps.enable_all_basic())
    }

    /// Create a specialized agent for specific task types
    pub fn specialized<I, S>(task_types: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::new()
            .max_concurrent_tasks(10)
            .support_types(task_types)
            .capabilities(|caps| caps.caching(true).validation(true).retries(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_agent_builder() {
        let config = TaskAgentBuilder::new()
            .name("test_agent")
            .description("A test agent")
            .max_concurrent_tasks(10)
            .polling_interval(Duration::from_secs(2))
            .task_timeout(Duration::from_secs(300))
            .support_types(vec!["processor", "transformer"])
            .capabilities(|caps| {
                caps.caching(true)
                    .streaming(true)
                    .cancellation(true)
                    .custom("feature_x", true)
            })
            .resource_limits(|limits| {
                limits
                    .max_memory_gb(2)
                    .max_cpu_percent(80.0)
                    .max_file_handles(200)
            })
            .metadata("version", "1.0.0")
            .build()
            .unwrap();

        assert_eq!(config.name, "test_agent");
        assert_eq!(config.max_concurrent_tasks, 10);
        assert_eq!(
            config.supported_task_types,
            vec!["processor", "transformer"]
        );
        assert!(config.capabilities.caching);
        assert!(config.capabilities.streaming);
        assert!(config.resource_limits.is_some());
    }

    #[test]
    fn test_preset_configurations() {
        let lightweight = TaskAgentBuilder::lightweight().build().unwrap();
        assert_eq!(lightweight.max_concurrent_tasks, 2);
        assert_eq!(
            lightweight
                .resource_limits
                .as_ref()
                .unwrap()
                .max_memory_bytes,
            256 * 1024 * 1024
        );

        let high_perf = TaskAgentBuilder::high_performance().build().unwrap();
        assert_eq!(high_perf.max_concurrent_tasks, 20);
        assert!(high_perf.capabilities.streaming);

        let specialized = TaskAgentBuilder::specialized(vec!["ml_training", "data_processing"])
            .build()
            .unwrap();
        assert_eq!(
            specialized.supported_task_types,
            vec!["ml_training", "data_processing"]
        );
    }

    #[test]
    fn test_validation() {
        // Test empty name
        let result = TaskAgentBuilder::new().name("").build();
        assert!(result.is_err());

        // Test zero concurrent tasks
        let result = TaskAgentBuilder::new().max_concurrent_tasks(0).build();
        assert!(result.is_err());

        // Test empty task types
        let result = TaskAgentBuilder::new()
            .supported_task_types(Vec::<String>::new())
            .build();
        assert!(result.is_err());
    }
}
