use dagger::{
    DaggerBuilder, DagFlowBuilder, TaskAgentBuilder, PubSubBuilder,
    DaggerConfig, Dagger
};
use dagger::pubsub_builder::{
    DeliveryGuarantees, MessageOrdering, SubscriptionType, StartPosition
};
use dagger::dag_builder::{ErrorHandling, BackoffStrategy};
use dagger::builders::LogLevel;
use serde_json::json;
use std::time::Duration;
use tokio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Configure the main Dagger system with fluent builder
    let dagger_config = DaggerBuilder::production()
        .cache(|cache| {
            cache
                .max_memory_mb(500)
                .max_entries(50_000)
                .ttl(Duration::from_secs(3600))
                .cleanup_interval(Duration::from_secs(30))
                .water_marks(0.7, 0.9)
        })
        .limits(|limits| {
            limits
                .max_memory_gb(4)
                .max_concurrent_tasks(100)
                .max_execution_tree_nodes(10_000)
                .task_execution_timeout(Duration::from_secs(300))
                .total_execution_timeout(Duration::from_secs(3600))
                .max_dependency_depth(20)
                .max_task_retries(3)
                .max_message_size_mb(10)
                .max_cpu_usage_percent(85.0)
        })
        .performance(|perf| {
            perf
                .async_serialization_threshold_kb(100)
                .database_batch_size(100)
                .connection_pool_size(10)
                .background_task_interval(Duration::from_secs(30))
                .enable_cache_prewarming(true)
                .enable_object_pooling(true)
                .compression_threshold_mb(1)
        })
        .logging(|log| {
            log
                .info()
                .structured(true)
                .max_file_size_mb(100)
                .max_files(5)
        })
        .network(|net| {
            net
                .bind_address("0.0.0.0")
                .port(8080)
                .max_connections(1000)
                .connection_timeout(Duration::from_secs(30))
                .keep_alive(true)
        })
        .security(|sec| {
            sec
                .enable_auth(true)
                .auth_token_ttl(Duration::from_secs(3600))
                .enable_tls(false)
                .rate_limit_requests_per_minute(10000)
        })
        .database_path("/tmp/dagger_production.db")
        .build()?;

    println!("✅ Dagger system configured");

    // 2. Initialize the Dagger system
    let dagger = dagger::Dagger::new(dagger_config)?.initialize().await?;
    println!("✅ Dagger system initialized");

    // 3. Create a complex DAG with the fluent builder
    let (dag_config, dag_nodes) = DagFlowBuilder::new("data_processing_pipeline")
        .description("A comprehensive data processing pipeline")
        .max_execution_time(Duration::from_secs(1800)) // 30 minutes
        .max_parallel_nodes(10)
        .error_handling(ErrorHandling::ContinueOnError)
        .retry_policy(|retry| {
            retry
                .max_retries(3)
                .exponential_backoff_with_jitter()
                .base_delay(Duration::from_secs(2))
                .max_delay(Duration::from_secs(60))
                .retry_on(vec!["timeout", "resource", "network"])
        })
        .enable_caching(true)
        .enable_metrics(true)
        .metadata("version", "1.0.0")
        .metadata("owner", "data-team")

        // Add nodes to the DAG
        .node(|node| {
            node
                .id("data_ingestion")
                .name("Data Ingestion")
                .description("Ingest data from external sources")
                .node_type("ingestion")
                .config(json!({
                    "sources": ["api", "database", "files"],
                    "batch_size": 1000,
                    "parallel_workers": 4
                }))
                .timeout(Duration::from_secs(300))
                .cacheable(true)
                .metadata("estimated_duration", "5min")
        })
        .node(|node| {
            node
                .id("data_validation")
                .name("Data Validation")
                .description("Validate ingested data quality")
                .node_type("validation")
                .config(json!({
                    "rules": ["not_null", "format_check", "range_validation"],
                    "error_threshold": 0.05
                }))
                .timeout(Duration::from_secs(180))
                .retry_policy(|retry| {
                    retry
                        .max_retries(2)
                        .linear_backoff()
                        .base_delay(Duration::from_secs(1))
                })
        })
        .node(|node| {
            node
                .id("data_transformation")
                .name("Data Transformation")
                .description("Transform and enrich data")
                .node_type("transformation")
                .config(json!({
                    "transformations": ["normalize", "enrich", "aggregate"],
                    "output_format": "parquet"
                }))
                .timeout(Duration::from_secs(600))
                .cacheable(true)
        })
        .node(|node| {
            node
                .id("data_analysis")
                .name("Data Analysis")
                .description("Perform analytical computations")
                .node_type("analysis")
                .config(json!({
                    "algorithms": ["clustering", "regression", "classification"],
                    "confidence_threshold": 0.8
                }))
                .timeout(Duration::from_secs(900))
                .metadata("resource_intensive", true)
        })
        .node(|node| {
            node
                .id("report_generation")
                .name("Report Generation")
                .description("Generate final reports")
                .node_type("reporting")
                .config(json!({
                    "formats": ["pdf", "html", "csv"],
                    "include_charts": true
                }))
                .timeout(Duration::from_secs(120))
        })

        // Define dependencies
        .depends_on("data_validation", "data_ingestion")
        .depends_on("data_transformation", "data_validation")
        .depends_on_all("data_analysis", vec!["data_transformation"])
        .depends_on("report_generation", "data_analysis")

        .build()?;

    println!("✅ DAG pipeline configured with {} nodes", dag_nodes.len());

    // 4. Configure task agents with different capabilities
    let lightweight_agent = TaskAgentBuilder::lightweight()
        .name("lightweight_processor")
        .description("Handles simple data processing tasks")
        .support_types(vec!["validation", "simple_transform"])
        .polling_interval(Duration::from_secs(1))
        .heartbeat_interval(Duration::from_secs(15))
        .metadata("tier", "basic")
        .build()?;

    println!("✅ Lightweight agent configured");

    let high_performance_agent = TaskAgentBuilder::high_performance()
        .name("ml_processor")
        .description("Handles ML and heavy computational tasks")
        .support_types(vec!["analysis", "ml_training", "transformation"])
        .capabilities(|caps| {
            caps
                .enable_all_basic()
                .custom("gpu_enabled", true)
                .custom("distributed_processing", true)
        })
        .resource_limits(|limits| {
            limits
                .max_memory_gb(8)
                .max_cpu_percent(95.0)
                .max_disk_gb(50)
                .max_network_mb_per_sec(100)
                .max_file_handles(1000)
        })
        .metadata("tier", "premium")
        .metadata("gpu_type", "nvidia-v100")
        .build()?;

    println!("✅ High-performance agent configured");

    let specialized_agent = TaskAgentBuilder::specialized(vec!["ingestion", "reporting"])
        .name("io_specialist")
        .description("Specialized for input/output operations")
        .max_concurrent_tasks(15)
        .task_timeout(Duration::from_secs(600))
        .capabilities(|caps| {
            caps
                .caching(true)
                .streaming(true)
                .validation(true)
                .custom("file_formats", json!(["csv", "json", "parquet", "avro"]))
        })
        .metadata("specialty", "data_io")
        .build()?;

    println!("✅ Specialized I/O agent configured");

    // 5. Configure PubSub system for inter-component communication
    let (_pubsub_config, _channels, _subscriptions) = PubSubBuilder::named("dagger_messaging")
        .description("Internal messaging system for Dagger components")
        .max_channels(100)
        .max_subscribers_per_channel(50)
        .max_message_size_mb(5)
        .delivery_guarantees(DeliveryGuarantees::AtLeastOnce)
        .ordering(MessageOrdering::PerPublisher)
        
        .retention_policy(|policy| {
            policy
                .max_messages(10000)
                .max_age(Duration::from_secs(3600 * 24)) // 24 hours
                .max_storage_mb(100)
        })
        
        .performance(|perf| {
            perf
                .channel_buffer_size(2000)
                .batch_size(100)
                .flush_interval(Duration::from_millis(50))
                .worker_threads(8)
                .enable_compression(true)
                .compression_threshold(1024)
                .async_persistence(true)
        })
        
        .security(|sec| {
            sec
                .enable_publisher_auth(true)
                .enable_subscriber_auth(true)
                .enable_encryption(false)
                .enable_acl(true)
                .rate_limiting(|rate| {
                    rate
                        .max_messages_per_sec(1000)
                        .max_bytes_per_sec(10 * 1024 * 1024) // 10MB/s
                        .window_size(Duration::from_secs(1))
                })
        })

        // Define channels
        .channel(|ch| {
            ch
                .name("task_events")
                .description("Task lifecycle events")
                .persistent(true)
                .delivery_guarantees(DeliveryGuarantees::ExactlyOnce)
                .retention_policy(|policy| {
                    policy
                        .max_messages(50000)
                        .max_age(Duration::from_secs(3600 * 48)) // 48 hours
                })
                .message_schema(json!({
                    "type": "object",
                    "properties": {
                        "task_id": {"type": "string"},
                        "event_type": {"type": "string"},
                        "timestamp": {"type": "number"},
                        "metadata": {"type": "object"}
                    },
                    "required": ["task_id", "event_type", "timestamp"]
                }))
        })
        
        .channel(|ch| {
            ch
                .name("metrics_stream")
                .description("Real-time metrics and monitoring data")
                .persistent(false)
                .metadata("high_frequency", true)
        })
        
        .channel(|ch| {
            ch
                .name("error_notifications")
                .description("Error and alert notifications")
                .persistent(true)
                .delivery_guarantees(DeliveryGuarantees::ExactlyOnce)
        })

        // Define subscriptions
        .subscription(|sub| {
            sub
                .name("task_monitor")
                .channel("task_events")
                .subscription_type(SubscriptionType::Shared)
                .start_position(StartPosition::Latest)
                .max_unacked_messages(1000)
                .ack_timeout(Duration::from_secs(30))
                .metadata("consumer_group", "monitoring")
        })
        
        .subscription(|sub| {
            sub
                .name("metrics_collector")
                .channel("metrics_stream")
                .subscription_type(SubscriptionType::Exclusive)
                .start_position(StartPosition::Earliest)
                .filter("event_type = 'performance'")
                .metadata("purpose", "analytics")
        })
        
        .subscription(|sub| {
            sub
                .name("alert_handler")
                .channel("error_notifications")
                .subscription_type(SubscriptionType::Failover)
                .start_position(StartPosition::Latest)
                .max_unacked_messages(100)
                .metadata("priority", "high")
        })

        .metadata("environment", "production")
        .metadata("version", "2.0.0")
        .build()?;

    println!("✅ PubSub messaging system configured");

    // 6. Demonstrate configuration introspection
    println!("\n📊 System Configuration Summary:");
    println!("├── Cache: {}MB max, {} entries max", 
        dagger.config().cache.max_memory_bytes / (1024 * 1024),
        dagger.config().cache.max_entries);
    println!("├── Concurrency: {} max tasks", 
        dagger.config().limits.max_concurrent_tasks);
    println!("├── Performance: {}KB async threshold", 
        dagger.config().performance.async_serialization_threshold / 1024);
    println!("├── Network: {}:{}", 
        dagger.config().network.bind_address,
        dagger.config().network.port);
    println!("└── Security: auth={}, tls={}", 
        dagger.config().security.enable_auth,
        dagger.config().security.enable_tls);

    println!("\n🔧 Agent Configurations:");
    println!("├── Lightweight: {} max tasks, supports {:?}", 
        lightweight_agent.max_concurrent_tasks,
        lightweight_agent.supported_task_types);
    println!("├── High-performance: {} max tasks, {}GB memory limit", 
        high_performance_agent.max_concurrent_tasks,
        high_performance_agent.resource_limits.as_ref().unwrap().max_memory_bytes / (1024 * 1024 * 1024));
    println!("└── Specialized: {} max tasks, supports {:?}", 
        specialized_agent.max_concurrent_tasks,
        specialized_agent.supported_task_types);

    println!("\n🌐 DAG Pipeline:");
    println!("├── Name: {}", dag_config.name);
    println!("├── Max parallel: {}", dag_config.max_parallel_nodes);
    println!("├── Timeout: {:?}", dag_config.max_execution_time);
    println!("├── Error handling: {:?}", dag_config.error_handling);
    println!("└── Nodes: {}", dag_nodes.len());
    
    for (node_id, node) in &dag_nodes {
        println!("    ├── {}: {} (type: {})", 
            node_id, 
            node.name, 
            node.node_type);
        if !node.dependencies.is_empty() {
            println!("    │   └── depends on: {:?}", node.dependencies);
        }
    }

    // 7. Show how to create different configuration presets
    println!("\n🎯 Configuration Presets Demo:");
    
    // Development configuration
    let dev_config = DaggerBuilder::development()
        .database_path("/tmp/dagger_dev.db")
        .build()?;
    println!("├── Development: {}MB cache, debug logging", 
        dev_config.cache.max_memory_bytes / (1024 * 1024));

    // Testing configuration  
    let test_config = DaggerBuilder::testing()
        .database_path(":memory:")
        .build()?;
    println!("├── Testing: {}MB cache, trace logging", 
        test_config.cache.max_memory_bytes / (1024 * 1024));

    // Custom configuration
    let custom_config = DaggerBuilder::new()
        .cache(|cache| cache.max_memory_gb(1).ttl(Duration::from_secs(7200)))
        .limits(|limits| limits.max_concurrent_tasks(50))
        .logging(|log| log.warn().file_path("/var/log/dagger.log"))
        .build()?;
    println!("└── Custom: {}GB cache, warn logging", 
        custom_config.cache.max_memory_bytes / (1024 * 1024 * 1024));

    println!("\n✨ All configurations created successfully!");
    println!("The fluent builder pattern provides:");
    println!("├── Type-safe configuration");
    println!("├── Discoverable API with autocomplete");
    println!("├── Validation at build time");
    println!("├── Sensible defaults");
    println!("├── Preset configurations for common scenarios");
    println!("└── Comprehensive error handling");

    Ok(())
}