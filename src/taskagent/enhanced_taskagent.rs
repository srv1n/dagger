use crate::errors::{DaggerError, Result};
use crate::memory::Cache as ImprovedCache;
use crate::limits::{ResourceTracker, ResourceLimits};
use crate::concurrency::{ConcurrentTaskRegistry, TaskNotificationSystem, AtomicTaskState, TaskStatus as ConcurrentTaskStatus};
use crate::performance::{PerformanceMonitor, AsyncSerializationService};
use crate::taskagent_builder::{TaskAgentConfig, TaskExecutionContext, TaskExecutionResult, TaskExecutionStatus, TaskExecutionMetrics, AgentState, AgentStatus, AgentMetrics};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Mutex, mpsc};
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info, warn, instrument};
use uuid::Uuid;

/// Enhanced task agent trait with improved infrastructure
#[async_trait]
pub trait EnhancedTaskAgent: Send + Sync {
    /// Agent name for identification
    fn name(&self) -> String;
    
    /// Input schema for validation
    fn input_schema(&self) -> Value;
    
    /// Output schema for validation
    fn output_schema(&self) -> Value;
    
    /// Execute a task with enhanced context
    async fn execute(
        &self,
        context: &TaskExecutionContext,
        manager: &EnhancedTaskManager,
    ) -> Result<TaskExecutionResult>;
    
    /// Validate task before execution (optional)
    async fn validate_task(&self, context: &TaskExecutionContext) -> Result<()> {
        Ok(())
    }
    
    /// Pre-execution setup (optional)
    async fn setup(&self, context: &TaskExecutionContext) -> Result<()> {
        Ok(())
    }
    
    /// Post-execution cleanup (optional)
    async fn cleanup(&self, context: &TaskExecutionContext) -> Result<()> {
        Ok(())
    }
}

/// Enhanced task manager with improved infrastructure
#[derive(Debug)]
pub struct EnhancedTaskManager {
    /// Agent configuration
    config: Arc<TaskAgentConfig>,
    /// Resource tracker
    resource_tracker: Arc<ResourceTracker>,
    /// Task registry
    task_registry: Arc<ConcurrentTaskRegistry>,
    /// Notification system
    notification_system: Arc<TaskNotificationSystem>,
    /// Performance monitor
    performance_monitor: Arc<PerformanceMonitor>,
    /// Serialization service
    serialization_service: Arc<AsyncSerializationService>,
    /// Improved cache
    cache: Arc<ImprovedCache>,
    /// Agent state
    state: Arc<RwLock<AgentState>>,
    /// Registered agents
    agents: Arc<RwLock<HashMap<String, Arc<dyn EnhancedTaskAgent>>>>,
    /// Task execution channel
    task_channel: Arc<RwLock<Option<mpsc::Sender<TaskExecutionContext>>>>,
    /// Shutdown signal
    shutdown_signal: Arc<tokio::sync::Notify>,
}

impl EnhancedTaskManager {
    /// Create a new enhanced task manager
    pub fn new(
        config: TaskAgentConfig,
        resource_tracker: Arc<ResourceTracker>,
        task_registry: Arc<ConcurrentTaskRegistry>,
        notification_system: Arc<TaskNotificationSystem>,
        cache: Arc<ImprovedCache>,
    ) -> Self {
        let performance_monitor = Arc::new(PerformanceMonitor::new(1000));
        let serialization_service = Arc::new(AsyncSerializationService::new(config.max_concurrent_tasks));
        
        let state = Arc::new(RwLock::new(AgentState {
            status: AgentStatus::Starting,
            active_tasks: HashMap::new(),
            total_tasks_processed: 0,
            successful_tasks: 0,
            failed_tasks: 0,
            start_time: Instant::now(),
            last_heartbeat: Instant::now(),
            metrics: AgentMetrics::default(),
        }));
        
        let (task_sender, _task_receiver) = mpsc::channel(config.max_concurrent_tasks * 2);
        
        Self {
            config: Arc::new(config),
            resource_tracker,
            task_registry,
            notification_system,
            performance_monitor,
            serialization_service,
            cache,
            state,
            agents: Arc::new(RwLock::new(HashMap::new())),
            task_channel: Arc::new(RwLock::new(Some(task_sender))),
            shutdown_signal: Arc::new(tokio::sync::Notify::new()),
        }
    }
    
    /// Register an enhanced agent
    pub async fn register_agent(&self, agent: Arc<dyn EnhancedTaskAgent>) -> Result<()> {
        let name = agent.name();
        let mut agents = self.agents.write().await;
        
        if agents.contains_key(&name) {
            return Err(DaggerError::validation(format!("Agent '{}' already registered", name)));
        }
        
        agents.insert(name.clone(), agent);
        info!("Registered agent: {}", name);
        Ok(())
    }
    
    /// Start the task manager
    #[instrument(skip(self))]
    pub async fn start(&self) -> Result<()> {
        info!("Starting enhanced task manager: {}", self.config.name);
        
        // Update state
        {
            let mut state = self.state.write().await;
            state.status = AgentStatus::Idle;
        }
        
        // Start background workers
        self.start_task_processor().await?;
        self.start_heartbeat_worker().await?;
        self.start_metrics_collector().await?;
        
        Ok(())
    }
    
    /// Submit a task for execution
    #[instrument(skip(self, context))]
    pub async fn submit_task(&self, context: TaskExecutionContext) -> Result<String> {
        // Validate task type is supported
        if !self.config.supported_task_types.contains(&"*".to_string()) &&
           !self.config.supported_task_types.contains(&context.task_type) {
            return Err(DaggerError::validation(format!(
                "Task type '{}' not supported by agent '{}'",
                context.task_type, self.config.name
            )));
        }
        
        // Check resource limits
        self.check_resource_limits(&context).await?;
        
        // Create atomic task state
        let task_state = Arc::new(AtomicTaskState::new(
            context.task_id.clone(),
            context.job_id.clone(),
            self.config.name.clone(),
        ));
        
        // Register task with concurrent registry
        self.task_registry.add_task(task_state.clone()).await?;
        
        // Send to processing channel
        if let Some(ref sender) = *self.task_channel.read().await {
            sender.send(context.clone()).await
                .map_err(|_| DaggerError::channel("task_submission", "Task channel closed"))?;
        } else {
            return Err(DaggerError::channel("task_submission", "Task channel not initialized"));
        }
        
        // Notify that a task is ready
        self.notification_system.notify_task_ready();
        
        info!("Submitted task: {}", context.task_id);
        Ok(context.task_id)
    }
    
    /// Start task processor worker
    async fn start_task_processor(&self) -> Result<()> {
        let config = Arc::clone(&self.config);
        let resource_tracker = Arc::clone(&self.resource_tracker);
        let task_registry = Arc::clone(&self.task_registry);
        let notification_system = Arc::clone(&self.notification_system);
        let performance_monitor = Arc::clone(&self.performance_monitor);
        let cache = Arc::clone(&self.cache);
        let state = Arc::clone(&self.state);
        let agents = Arc::clone(&self.agents);
        let shutdown_signal = Arc::clone(&self.shutdown_signal);
        
        let (tx, mut rx) = mpsc::channel::<TaskExecutionContext>(config.max_concurrent_tasks * 2);
        *self.task_channel.write().await = Some(tx);
        
        tokio::spawn(async move {
            info!("Task processor started");
            
            loop {
                tokio::select! {
                    _ = shutdown_signal.notified() => {
                        info!("Task processor shutting down");
                        break;
                    }
                    
                    _ = notification_system.wait_for_ready_tasks(config.polling_interval) => {
                        // Process ready tasks
                        let ready_tasks = task_registry.get_ready_tasks();
                        
                        for task_state in ready_tasks {
                            if task_state.try_claim() {
                                // Execute task
                                let task_id = task_state.id.clone();
                                debug!("Processing task: {}", task_id);
                                
                                // TODO: Create TaskExecutionContext from task_state and execute
                            }
                        }
                    }
                    
                    Some(context) = rx.recv() => {
                        // Process received task
                        let task_id = context.task_id.clone();
                        
                        // Update state
                        {
                            let mut state_guard = state.write().await;
                            state_guard.active_tasks.insert(task_id.clone(), context.clone());
                            state_guard.status = AgentStatus::Busy;
                        }
                        
                        // Process task
                        let process_result = process_single_task(
                            context,
                            &agents,
                            &resource_tracker,
                            &performance_monitor,
                            &cache,
                            &notification_system,
                        ).await;
                        
                        // Update state based on result
                        {
                            let mut state_guard = state.write().await;
                            state_guard.active_tasks.remove(&task_id);
                            state_guard.total_tasks_processed += 1;
                            
                            match process_result {
                                Ok(_) => state_guard.successful_tasks += 1,
                                Err(_) => state_guard.failed_tasks += 1,
                            }
                            
                            if state_guard.active_tasks.is_empty() {
                                state_guard.status = AgentStatus::Idle;
                            }
                        }
                    }
                }
            }
        });
        
        Ok(())
    }
    
    /// Start heartbeat worker
    async fn start_heartbeat_worker(&self) -> Result<()> {
        let config = Arc::clone(&self.config);
        let state = Arc::clone(&self.state);
        let shutdown_signal = Arc::clone(&self.shutdown_signal);
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.heartbeat_interval);
            
            loop {
                tokio::select! {
                    _ = shutdown_signal.notified() => {
                        info!("Heartbeat worker shutting down");
                        break;
                    }
                    
                    _ = interval.tick() => {
                        let mut state_guard = state.write().await;
                        state_guard.last_heartbeat = Instant::now();
                        debug!("Agent heartbeat: {}", config.name);
                    }
                }
            }
        });
        
        Ok(())
    }
    
    /// Start metrics collector worker
    async fn start_metrics_collector(&self) -> Result<()> {
        let config = Arc::clone(&self.config);
        let state = Arc::clone(&self.state);
        let performance_monitor = Arc::clone(&self.performance_monitor);
        let shutdown_signal = Arc::clone(&self.shutdown_signal);
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60)); // Collect every minute
            
            loop {
                tokio::select! {
                    _ = shutdown_signal.notified() => {
                        info!("Metrics collector shutting down");
                        break;
                    }
                    
                    _ = interval.tick() => {
                        // Collect performance metrics
                        let all_metrics = performance_monitor.get_all_metrics();
                        
                        // Calculate aggregated metrics
                        let mut state_guard = state.write().await;
                        let elapsed = state_guard.start_time.elapsed();
                        
                        if state_guard.total_tasks_processed > 0 {
                            state_guard.metrics.avg_execution_time = elapsed / state_guard.total_tasks_processed as u32;
                            state_guard.metrics.success_rate = state_guard.successful_tasks as f64 / 
                                                               state_guard.total_tasks_processed as f64;
                            state_guard.metrics.tasks_per_second = state_guard.total_tasks_processed as f64 / 
                                                                   elapsed.as_secs_f64();
                        }
                        
                        // TODO: Collect system metrics (memory, CPU)
                        
                        debug!("Metrics collected for agent: {}", config.name);
                    }
                }
            }
        });
        
        Ok(())
    }
    
    /// Check resource limits before accepting task
    async fn check_resource_limits(&self, context: &TaskExecutionContext) -> Result<()> {
        // Check concurrent task limit
        let current_tasks = self.state.read().await.active_tasks.len();
        if current_tasks >= self.config.max_concurrent_tasks {
            return Err(DaggerError::resource_exhausted(
                "concurrent_tasks",
                current_tasks as u64,
                self.config.max_concurrent_tasks as u64,
            ));
        }
        
        // Check agent-specific resource limits if configured
        if let Some(ref limits) = self.config.resource_limits {
            // Memory check
            let current_memory = self.resource_tracker.get_usage_stats().memory_usage;
            if current_memory > limits.max_memory_bytes {
                return Err(DaggerError::resource_exhausted(
                    "agent_memory",
                    current_memory,
                    limits.max_memory_bytes,
                ));
            }
            
            // TODO: Add CPU and other resource checks
        }
        
        Ok(())
    }
    
    /// Shutdown the task manager gracefully
    pub async fn shutdown(&self) -> Result<()> {
        info!("Shutting down task manager: {}", self.config.name);
        
        // Update state
        {
            let mut state = self.state.write().await;
            state.status = AgentStatus::Stopping;
        }
        
        // Send shutdown signal
        self.shutdown_signal.notify_waiters();
        
        // Wait for graceful shutdown
        let shutdown_result = timeout(
            self.config.shutdown_timeout,
            self.wait_for_tasks_completion()
        ).await;
        
        match shutdown_result {
            Ok(_) => {
                info!("Task manager shutdown completed");
                let mut state = self.state.write().await;
                state.status = AgentStatus::Stopped;
                Ok(())
            }
            Err(_) => {
                warn!("Task manager shutdown timed out");
                let mut state = self.state.write().await;
                state.status = AgentStatus::Error("Shutdown timeout".to_string());
                Err(DaggerError::timeout("shutdown", self.config.shutdown_timeout.as_millis() as u64))
            }
        }
    }
    
    /// Wait for all active tasks to complete
    async fn wait_for_tasks_completion(&self) {
        loop {
            let active_count = self.state.read().await.active_tasks.len();
            if active_count == 0 {
                break;
            }
            
            debug!("Waiting for {} active tasks to complete", active_count);
            sleep(Duration::from_millis(100)).await;
        }
    }
    
    /// Get current agent state
    pub async fn get_state(&self) -> AgentState {
        self.state.read().await.clone()
    }
}

/// Process a single task
async fn process_single_task(
    context: TaskExecutionContext,
    agents: &Arc<RwLock<HashMap<String, Arc<dyn EnhancedTaskAgent>>>>,
    resource_tracker: &Arc<ResourceTracker>,
    performance_monitor: &Arc<PerformanceMonitor>,
    cache: &Arc<ImprovedCache>,
    notification_system: &Arc<TaskNotificationSystem>,
) -> Result<TaskExecutionResult> {
    let start_time = Instant::now();
    let task_type = &context.task_type;
    
    // Find appropriate agent
    let agents_guard = agents.read().await;
    let agent = agents_guard.values()
        .find(|a| a.name() == *task_type)
        .ok_or_else(|| DaggerError::agent(task_type, "No agent found for task type"))?;
    
    // Allocate resources
    let estimated_memory = 1024 * 1024; // 1MB default
    let _memory_allocation = resource_tracker.allocate_memory(estimated_memory)?;
    let _task_execution = resource_tracker.start_task()?;
    
    // Create enhanced task manager for agent to use
    let manager = EnhancedTaskManager {
        config: Arc::new(TaskAgentConfig::default()), // TODO: Use actual config
        resource_tracker: Arc::clone(resource_tracker),
        task_registry: Arc::new(ConcurrentTaskRegistry::new(10)), // TODO: Use actual registry
        notification_system: Arc::clone(notification_system),
        performance_monitor: Arc::clone(performance_monitor),
        serialization_service: Arc::new(AsyncSerializationService::new(10)),
        cache: Arc::clone(cache),
        state: Arc::new(RwLock::new(AgentState {
            status: AgentStatus::Busy,
            active_tasks: HashMap::new(),
            total_tasks_processed: 0,
            successful_tasks: 0,
            failed_tasks: 0,
            start_time: Instant::now(),
            last_heartbeat: Instant::now(),
            metrics: AgentMetrics::default(),
        })),
        agents: Arc::clone(agents),
        task_channel: Arc::new(RwLock::new(Some(mpsc::channel(10).0))),
        shutdown_signal: Arc::new(tokio::sync::Notify::new()),
    };
    
    // Validate task
    agent.validate_task(&context).await?;
    
    // Setup
    agent.setup(&context).await?;
    
    // Execute with timeout
    let execution_future = agent.execute(&context, &manager);
    let timeout_duration = context.timeout;
    
    let result = match timeout(timeout_duration, execution_future).await {
        Ok(Ok(execution_result)) => {
            // Record success
            performance_monitor.record_operation(
                &format!("task_execution_{}", task_type),
                start_time.elapsed()
            );
            execution_result
        }
        Ok(Err(e)) => {
            // Record failure
            performance_monitor.record_error(&format!("task_execution_{}", task_type));
            
            TaskExecutionResult {
                task_id: context.task_id.clone(),
                status: TaskExecutionStatus::Failed,
                result: None,
                error: Some(e.to_string()),
                metrics: TaskExecutionMetrics {
                    duration: start_time.elapsed(),
                    ..Default::default()
                },
                metadata: HashMap::new(),
            }
        }
        Err(_) => {
            // Timeout
            performance_monitor.record_error(&format!("task_execution_{}", task_type));
            
            TaskExecutionResult {
                task_id: context.task_id.clone(),
                status: TaskExecutionStatus::Timeout,
                result: None,
                error: Some(format!("Task timed out after {:?}", timeout_duration)),
                metrics: TaskExecutionMetrics {
                    duration: start_time.elapsed(),
                    ..Default::default()
                },
                metadata: HashMap::new(),
            }
        }
    };
    
    // Cleanup
    let _ = agent.cleanup(&context).await;
    
    // Notify completion
    match result.status {
        TaskExecutionStatus::Success => notification_system.notify_task_completed(),
        _ => notification_system.notify_task_failed(),
    }
    
    Ok(result)
}

/// Example enhanced agent implementation
pub struct ExampleEnhancedAgent {
    name: String,
}

impl ExampleEnhancedAgent {
    pub fn new(name: String) -> Self {
        Self { name }
    }
}

#[async_trait]
impl EnhancedTaskAgent for ExampleEnhancedAgent {
    fn name(&self) -> String {
        self.name.clone()
    }
    
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {"type": "string"},
                "count": {"type": "number"}
            },
            "required": ["message"]
        })
    }
    
    fn output_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "result": {"type": "string"},
                "processed_count": {"type": "number"}
            }
        })
    }
    
    #[instrument(skip(self, manager))]
    async fn execute(
        &self,
        context: &TaskExecutionContext,
        manager: &EnhancedTaskManager,
    ) -> Result<TaskExecutionResult> {
        let message = context.config["message"].as_str().unwrap_or("No message");
        let count = context.config["count"].as_u64().unwrap_or(1);
        
        // Simulate processing
        let mut processed = 0;
        for i in 0..count {
            // Check if we should cancel
            if i > 0 && i % 10 == 0 {
                sleep(Duration::from_millis(10)).await;
            }
            
            // Store intermediate result in cache
            let cache_key = format!("task_{}_progress", context.task_id);
            manager.cache.insert_value(&context.task_id, &cache_key, &i).await?;
            
            processed += 1;
        }
        
        let result = serde_json::json!({
            "result": format!("Processed: {} (x{})", message, processed),
            "processed_count": processed,
            "agent": self.name
        });
        
        // Cache final result
        let result_key = format!("task_{}_result", context.task_id);
        manager.cache.insert_value(&context.task_id, &result_key, &result).await?;
        
        Ok(TaskExecutionResult {
            task_id: context.task_id.clone(),
            status: TaskExecutionStatus::Success,
            result: Some(result),
            error: None,
            metrics: TaskExecutionMetrics {
                duration: Duration::from_millis(100),
                memory_used_bytes: 1024,
                cpu_time: Duration::from_millis(50),
                cache_hits: 0,
                cache_misses: 1,
                custom: HashMap::new(),
                peak_memory_bytes: 1024,
            },
            metadata: HashMap::new(),
        })
    }
    
    async fn validate_task(&self, context: &TaskExecutionContext) -> Result<()> {
        if context.config.get("message").is_none() {
            return Err(DaggerError::validation("Message is required"));
        }
        
        if let Some(count) = context.config.get("count") {
            if let Some(count_val) = count.as_u64() {
                if count_val > 1000 {
                    return Err(DaggerError::validation("Count must be <= 1000"));
                }
            }
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builders::DaggerBuilder;
    use tokio::test;
    
    #[test]
    async fn test_enhanced_task_agent() {
        // Create configuration
        let dagger_config = DaggerBuilder::testing().build().unwrap();
        
        // Create shared resources
        let cache = Arc::new(ImprovedCache::new(dagger_config.cache.clone()).unwrap());
        let resource_tracker = Arc::new(ResourceTracker::new(dagger_config.limits.clone()).unwrap());
        let task_registry = Arc::new(ConcurrentTaskRegistry::new(10));
        let notification_system = Arc::new(TaskNotificationSystem::new(10));
        
        // Create agent configuration
        let agent_config = crate::taskagent_builder::TaskAgentBuilder::new()
            .name("test_agent")
            .max_concurrent_tasks(5)
            .build()
            .unwrap();
        
        // Create task manager
        let manager = EnhancedTaskManager::new(
            agent_config,
            resource_tracker,
            task_registry,
            notification_system,
            cache,
        );
        
        // Register agent
        let agent = Arc::new(ExampleEnhancedAgent::new("example".to_string()));
        manager.register_agent(agent).await.unwrap();
        
        // Start manager
        manager.start().await.unwrap();
        
        // Create and submit task
        let task_context = TaskExecutionContext {
            task_id: "test_task_1".to_string(),
            job_id: "test_job".to_string(),
            task_type: "example".to_string(),
            config: serde_json::json!({
                "message": "Hello, Enhanced Agent!",
                "count": 5
            }),
            metadata: HashMap::new(),
            timeout: Some(Duration::from_secs(10)),
            retry_count: 0,
            max_retries: 3,
            dependencies: vec![],
            result_schema: None,
        };
        
        let task_id = manager.submit_task(task_context).await.unwrap();
        assert_eq!(task_id, "test_task_1");
        
        // Wait a bit for processing
        sleep(Duration::from_millis(200)).await;
        
        // Check state
        let state = manager.get_state().await;
        assert!(state.total_tasks_processed > 0);
        
        // Shutdown
        manager.shutdown().await.unwrap();
    }
}