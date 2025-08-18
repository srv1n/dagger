use async_trait::async_trait;
use dagger::dag_builder::{BackoffStrategy, ErrorHandling};
use dagger::pubsub_builder::{DeliveryGuarantees, Message, MessageOrdering, SubscriptionType};
use dagger::taskagent_builder::{TaskExecutionContext, TaskExecutionResult, TaskExecutionStatus};
use dagger::{
    DagFlowBuilder, Dagger, DaggerBuilder, DaggerConfig, EnhancedDagExecutor, EnhancedNodeAction,
    EnhancedPubSub, EnhancedTaskAgent, EnhancedTaskManager, HealthStatus, MonitoringDashboard,
    PubSubBuilder, SystemHealth, TaskAgentBuilder,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio;
use tracing::{debug, info};
use tracing_subscriber;

/// Example integrated system demonstrating all enhanced features working together
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    println!("🚀 Starting Integrated Dagger System Demo\n");

    // 1. Create unified system configuration
    let config = DaggerBuilder::production()
        .cache(|cache| {
            cache
                .max_memory_mb(200)
                .max_entries(10_000)
                .ttl(Duration::from_secs(1800))
                .cleanup_interval(Duration::from_secs(30))
        })
        .limits(|limits| {
            limits
                .max_concurrent_tasks(50)
                .max_memory_gb(2)
                .task_execution_timeout(Duration::from_secs(300))
                .max_dependency_depth(10)
        })
        .performance(|perf| {
            perf.async_serialization_threshold_kb(50)
                .database_batch_size(50)
                .enable_object_pooling(true)
        })
        .database_path("/tmp/dagger_integrated.db")
        .build()?;

    // 2. Initialize the Dagger system
    let dagger = Dagger::new(config)?.initialize().await?;
    println!("✅ Dagger system initialized");

    // 3. Create monitoring dashboard
    let dashboard = MonitoringDashboard::new(
        dagger.resource_tracker().unwrap().clone(),
        dagger.resource_tracker().unwrap().clone(),
        dagger.cache().unwrap().clone(),
        dagger.task_registry().unwrap().clone(),
        Duration::from_secs(5),
    );
    dashboard.start_monitoring().await;
    println!("📊 Monitoring dashboard started\n");

    // 4. Set up PubSub for inter-component communication
    let (pubsub_config, channels, subscriptions) = PubSubBuilder::new()
        .name("system_events")
        .delivery_guarantees(DeliveryGuarantees::AtLeastOnce)
        .channel(|ch| {
            ch.name("dag_events")
                .description("DAG execution events")
                .persistent(true)
        })
        .channel(|ch| {
            ch.name("task_events")
                .description("Task execution events")
                .persistent(true)
        })
        .channel(|ch| {
            ch.name("alerts")
                .description("System alerts")
                .persistent(true)
        })
        .build()?;

    let pubsub = Arc::new(EnhancedPubSub::new(
        pubsub_config,
        dagger.resource_tracker().unwrap().clone(),
        dagger.cache().unwrap().clone(),
    )?);

    // Create channels
    for (_, channel_config) in channels {
        pubsub.create_channel(channel_config).await?;
    }
    println!("📢 PubSub system configured");

    // 5. Create and register task agents
    let data_processor_config = TaskAgentBuilder::high_performance()
        .name("data_processor")
        .description("Processes data with ML models")
        .support_types(vec!["process", "transform", "analyze"])
        .capabilities(|caps| caps.enable_all_basic().custom("ml_support", true))
        .build()?;

    let io_agent_config = TaskAgentBuilder::specialized(vec!["ingest", "export"])
        .name("io_agent")
        .description("Handles data ingestion and export")
        .max_concurrent_tasks(20)
        .build()?;

    // Create task managers
    let data_processor_manager = Arc::new(EnhancedTaskManager::new(
        data_processor_config,
        dagger.resource_tracker().unwrap().clone(),
        dagger.task_registry().unwrap().clone(),
        Arc::new(dagger::concurrency::TaskNotificationSystem::new(50)),
        dagger.cache().unwrap().clone(),
    ));

    let io_agent_manager = Arc::new(EnhancedTaskManager::new(
        io_agent_config,
        dagger.resource_tracker().unwrap().clone(),
        dagger.task_registry().unwrap().clone(),
        Arc::new(dagger::concurrency::TaskNotificationSystem::new(50)),
        dagger.cache().unwrap().clone(),
    ));

    // Register agents
    data_processor_manager
        .register_agent(Arc::new(DataProcessorAgent {
            pubsub: pubsub.clone(),
        }))
        .await?;
    io_agent_manager
        .register_agent(Arc::new(IOAgent {
            pubsub: pubsub.clone(),
        }))
        .await?;

    // Start agents
    data_processor_manager.start().await?;
    io_agent_manager.start().await?;
    println!("🤖 Task agents started\n");

    // 6. Create a complex DAG workflow
    let (dag_config, nodes) = DagFlowBuilder::new("data_pipeline")
        .description("Comprehensive data processing pipeline")
        .max_execution_time(Duration::from_secs(600))
        .max_parallel_nodes(5)
        .error_handling(ErrorHandling::ContinueOnError)
        .retry_policy(|retry| {
            retry
                .max_retries(3)
                .exponential_backoff_with_jitter()
                .retry_on(vec!["timeout", "resource"])
        })
        // Data ingestion node
        .node(|node| {
            node.id("ingest_data")
                .name("Data Ingestion")
                .node_type("ingest_action")
                .config(json!({
                    "source": "api",
                    "endpoint": "https://api.example.com/data",
                    "batch_size": 1000
                }))
                .timeout(Duration::from_secs(60))
        })
        // Data validation node
        .node(|node| {
            node.id("validate_data")
                .name("Data Validation")
                .node_type("process_action")
                .config(json!({
                    "validation_rules": ["not_null", "type_check", "range_check"],
                    "fail_threshold": 0.05
                }))
                .timeout(Duration::from_secs(30))
        })
        // Data processing node
        .node(|node| {
            node.id("process_data")
                .name("ML Processing")
                .node_type("process_action")
                .config(json!({
                    "model": "transformer_v2",
                    "batch_size": 100,
                    "features": ["normalize", "encode", "transform"]
                }))
                .timeout(Duration::from_secs(300))
                .cacheable(true)
        })
        // Results aggregation node
        .node(|node| {
            node.id("aggregate_results")
                .name("Results Aggregation")
                .node_type("process_action")
                .config(json!({
                    "aggregations": ["sum", "mean", "percentiles"],
                    "group_by": ["category", "timestamp"]
                }))
        })
        // Export results node
        .node(|node| {
            node.id("export_results")
                .name("Export Results")
                .node_type("export_action")
                .config(json!({
                    "format": "parquet",
                    "destination": "s3://results/processed/",
                    "compression": "snappy"
                }))
        })
        // Define dependencies
        .depends_on("validate_data", "ingest_data")
        .depends_on("process_data", "validate_data")
        .depends_on("aggregate_results", "process_data")
        .depends_on("export_results", "aggregate_results")
        .build()?;

    println!("🔧 DAG workflow configured with {} nodes", nodes.len());

    // 7. Create DAG execution context
    let dag_context = dag_config.build_execution_context(
        dagger.config().clone(),
        dagger.cache().unwrap().clone(),
        dagger.resource_tracker().unwrap().clone(),
        dagger.task_registry().unwrap().clone(),
    )?;

    let executor = Arc::new(EnhancedDagExecutor::new(Arc::new(dag_context)));

    // Register DAG actions
    executor.register_action(Arc::new(IngestAction {
        agent_manager: io_agent_manager.clone(),
        pubsub: pubsub.clone(),
    }));
    executor.register_action(Arc::new(ProcessAction {
        agent_manager: data_processor_manager.clone(),
        pubsub: pubsub.clone(),
    }));
    executor.register_action(Arc::new(ExportAction {
        agent_manager: io_agent_manager.clone(),
        pubsub: pubsub.clone(),
    }));

    println!("\n🎯 Starting DAG execution...\n");

    // 8. Execute the DAG
    let start_time = Instant::now();
    let execution_result = executor.execute_dag(nodes).await;
    let execution_duration = start_time.elapsed();

    // 9. Process results and publish events
    match execution_result {
        Ok(results) => {
            println!(
                "✅ DAG execution completed successfully in {:?}",
                execution_duration
            );

            // Publish success event
            pubsub
                .publish(
                    "dag_events",
                    Message {
                        id: String::new(),
                        key: Some("dag_completed".to_string()),
                        payload: json!({
                            "dag_name": "data_pipeline",
                            "duration_ms": execution_duration.as_millis(),
                            "nodes_executed": results.len(),
                            "status": "success"
                        }),
                        headers: HashMap::new(),
                        timestamp: 0,
                        priority: Some(5),
                        ttl: None,
                        publisher_id: "dag_executor".to_string(),
                        metadata: HashMap::new(),
                    },
                )
                .await?;

            // Display node results
            for (node_id, result) in results {
                println!(
                    "  📍 Node '{}': {:?} (took {:?})",
                    node_id,
                    result.status,
                    result.metrics.execution_duration.unwrap_or_default()
                );
            }
        }
        Err(e) => {
            println!("❌ DAG execution failed: {}", e);

            // Publish failure event
            pubsub
                .publish(
                    "alerts",
                    Message {
                        id: String::new(),
                        key: Some("dag_failed".to_string()),
                        payload: json!({
                            "dag_name": "data_pipeline",
                            "error": e.to_string(),
                            "category": e.category()
                        }),
                        headers: HashMap::new(),
                        timestamp: 0,
                        priority: Some(10),
                        ttl: None,
                        publisher_id: "dag_executor".to_string(),
                        metadata: HashMap::new(),
                    },
                )
                .await?;
        }
    }

    // 10. Check system health and generate report
    println!("\n📊 System Health Check:\n");

    let system_status = dashboard.get_current_status().await?;
    let health = &system_status.health_status;

    println!("Overall System Health: {:?}", health.overall_status);
    println!(
        "├── Memory Health: {:?} ({:.1}% used)",
        health.memory_health, system_status.resource_utilization.memory_utilization
    );
    println!(
        "├── CPU Health: {:?} ({:.1}% utilized)",
        health.cpu_health, system_status.resource_utilization.task_utilization
    );
    println!(
        "├── Cache Health: {:?} ({:.1}% hit ratio)",
        health.cache_health,
        system_status.cache_stats.hit_ratio() * 100.0
    );
    println!("└── Performance Health: {:?}", health.performance_health);

    // Display alerts
    if !system_status.alerts.is_empty() {
        println!("\n⚠️  Active Alerts:");
        for alert in &system_status.alerts {
            println!(
                "  - [{:?}] {}: {}",
                alert.level, alert.component, alert.message
            );
        }
    }

    // 11. Generate comprehensive report
    let report = dashboard.generate_report().await;

    println!("\n📈 System Summary:");
    println!(
        "├── Total Tasks Executed: {}",
        report.summary.total_tasks_executed
    );
    println!(
        "├── Average Memory Usage: {} MB",
        report.summary.average_memory_usage / (1024 * 1024)
    );
    println!(
        "├── Peak Memory Usage: {} MB",
        report.summary.peak_memory_usage / (1024 * 1024)
    );
    println!(
        "├── Cache Hit Ratio: {:.1}%",
        report.summary.average_cache_hit_ratio * 100.0
    );
    println!("└── Total Errors: {}", report.summary.total_errors);

    if !report.recommendations.is_empty() {
        println!("\n💡 Recommendations:");
        for (i, recommendation) in report.recommendations.iter().enumerate() {
            println!("  {}. {}", i + 1, recommendation);
        }
    }

    // 12. Cleanup
    println!("\n🧹 Shutting down systems...");

    data_processor_manager.shutdown().await?;
    io_agent_manager.shutdown().await?;

    println!("\n✨ Integrated system demo completed successfully!");

    Ok(())
}

// Example DAG action implementations

struct IngestAction {
    agent_manager: Arc<EnhancedTaskManager>,
    pubsub: Arc<EnhancedPubSub>,
}

#[async_trait]
impl EnhancedNodeAction for IngestAction {
    fn name(&self) -> String {
        "ingest_action".to_string()
    }

    fn schema(&self) -> Value {
        json!({
            "name": "ingest_action",
            "parameters": {
                "source": {"type": "string"},
                "endpoint": {"type": "string"},
                "batch_size": {"type": "number"}
            }
        })
    }

    async fn execute(
        &self,
        _executor: &EnhancedDagExecutor,
        node: &dagger::dag_builder::NodeDefinition,
        _context: &dagger::dag_builder::DagExecutionContext,
    ) -> dagger::Result<Value> {
        info!("Executing ingest action for node: {}", node.id);

        // Simulate data ingestion
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Publish event
        self.pubsub
            .publish(
                "task_events",
                Message {
                    id: String::new(),
                    key: Some("ingest_completed".to_string()),
                    payload: json!({
                        "node_id": node.id,
                        "records_ingested": 1000,
                        "source": node.config["source"]
                    }),
                    headers: HashMap::new(),
                    timestamp: 0,
                    priority: None,
                    ttl: None,
                    publisher_id: "ingest_action".to_string(),
                    metadata: HashMap::new(),
                },
            )
            .await?;

        Ok(json!({
            "status": "success",
            "records": 1000,
            "source": node.config["source"]
        }))
    }
}

struct ProcessAction {
    agent_manager: Arc<EnhancedTaskManager>,
    pubsub: Arc<EnhancedPubSub>,
}

#[async_trait]
impl EnhancedNodeAction for ProcessAction {
    fn name(&self) -> String {
        "process_action".to_string()
    }

    fn schema(&self) -> Value {
        json!({
            "name": "process_action",
            "parameters": {
                "type": "object"
            }
        })
    }

    async fn execute(
        &self,
        _executor: &EnhancedDagExecutor,
        node: &dagger::dag_builder::NodeDefinition,
        context: &dagger::dag_builder::DagExecutionContext,
    ) -> dagger::Result<Value> {
        info!("Executing process action for node: {}", node.id);

        // Create task for agent
        let task_context = TaskExecutionContext {
            task_id: format!("task_{}", uuid::Uuid::new_v4()),
            job_id: context.config.name.clone(),
            task_type: "process".to_string(),
            config: node.config.clone(),
            metadata: HashMap::new(),
            timeout: node.timeout,
            retry_count: 0,
            max_retries: 3,
            dependencies: vec![],
            result_schema: None,
        };

        // Submit to agent
        let task_id = self.agent_manager.submit_task(task_context).await?;

        // Simulate processing
        tokio::time::sleep(Duration::from_millis(1000)).await;

        Ok(json!({
            "status": "processed",
            "task_id": task_id,
            "node_id": node.id
        }))
    }
}

struct ExportAction {
    agent_manager: Arc<EnhancedTaskManager>,
    pubsub: Arc<EnhancedPubSub>,
}

#[async_trait]
impl EnhancedNodeAction for ExportAction {
    fn name(&self) -> String {
        "export_action".to_string()
    }

    fn schema(&self) -> Value {
        json!({
            "name": "export_action",
            "parameters": {
                "format": {"type": "string"},
                "destination": {"type": "string"}
            }
        })
    }

    async fn execute(
        &self,
        _executor: &EnhancedDagExecutor,
        node: &dagger::dag_builder::NodeDefinition,
        _context: &dagger::dag_builder::DagExecutionContext,
    ) -> dagger::Result<Value> {
        info!("Executing export action for node: {}", node.id);

        // Simulate export
        tokio::time::sleep(Duration::from_millis(300)).await;

        Ok(json!({
            "status": "exported",
            "format": node.config["format"],
            "destination": node.config["destination"],
            "records_exported": 950
        }))
    }
}

// Example agent implementations

struct DataProcessorAgent {
    pubsub: Arc<EnhancedPubSub>,
}

#[async_trait]
impl EnhancedTaskAgent for DataProcessorAgent {
    fn name(&self) -> String {
        "data_processor".to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "data": {"type": "array"},
                "model": {"type": "string"}
            }
        })
    }

    fn output_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "processed_data": {"type": "array"},
                "metrics": {"type": "object"}
            }
        })
    }

    async fn execute(
        &self,
        context: &TaskExecutionContext,
        manager: &EnhancedTaskManager,
    ) -> dagger::Result<TaskExecutionResult> {
        debug!("Processing task: {}", context.task_id);

        // Simulate ML processing
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Cache intermediate results
        manager
            .cache
            .insert_value(&context.task_id, "processing_stage", &"completed")
            .await?;

        Ok(TaskExecutionResult {
            task_id: context.task_id.clone(),
            status: TaskExecutionStatus::Success,
            result: Some(json!({
                "processed_data": [1, 2, 3, 4, 5],
                "metrics": {
                    "accuracy": 0.95,
                    "processing_time_ms": 500
                }
            })),
            error: None,
            metrics: dagger::taskagent_builder::TaskExecutionMetrics {
                duration: Duration::from_millis(500),
                memory_used_bytes: 1024 * 1024,
                cpu_time: Duration::from_millis(400),
                cache_hits: 0,
                cache_misses: 1,
                custom: HashMap::new(),
            },
            metadata: HashMap::new(),
        })
    }
}

struct IOAgent {
    pubsub: Arc<EnhancedPubSub>,
}

#[async_trait]
impl EnhancedTaskAgent for IOAgent {
    fn name(&self) -> String {
        "io_agent".to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object"
        })
    }

    fn output_schema(&self) -> Value {
        json!({
            "type": "object"
        })
    }

    async fn execute(
        &self,
        context: &TaskExecutionContext,
        _manager: &EnhancedTaskManager,
    ) -> dagger::Result<TaskExecutionResult> {
        debug!("IO operation for task: {}", context.task_id);

        // Simulate IO operation
        tokio::time::sleep(Duration::from_millis(200)).await;

        Ok(TaskExecutionResult {
            task_id: context.task_id.clone(),
            status: TaskExecutionStatus::Success,
            result: Some(json!({
                "operation": "completed",
                "bytes_processed": 1024 * 100
            })),
            error: None,
            metrics: Default::default(),
            metadata: HashMap::new(),
        })
    }
}
