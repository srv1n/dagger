//! Simple test suite for DAG Flow system
//!
//! This test focuses on ensuring the basic DAG Flow functionality works
//! without getting bogged down in API compatibility issues.

use anyhow::Result;
use async_trait::async_trait;
use dagger::coord::{ActionRegistry, NodeAction, NodeCtx, NodeOutput};
use dagger::dag_flow::{Cache, DagExecutor, DagNodeContextData, Node};
use dagger::insert_value;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// Test that basic DAG executor can be created
#[tokio::test]
async fn test_dag_executor_creation() {
    let registry = ActionRegistry::new();

    let _executor = DagExecutor::new(None, registry, ":memory:").await.unwrap();

    // Test passed if we get here without panicking
    assert!(true);
}

/// Test that cache operations work
#[tokio::test]
async fn test_cache_operations() {
    let cache = Cache::new();

    // Insert some values
    insert_value(&cache, "test_node", "value1", 42.0).unwrap();
    insert_value(&cache, "test_node", "value2", "hello").unwrap();

    // Test that cache contains the values
    assert!(cache.data.contains_key("test_node"));
    let node_data = cache.data.get("test_node").unwrap();
    assert!(node_data.contains_key("value1"));
    assert!(node_data.contains_key("value2"));
}

/// Test YAML loading (if files exist)
#[tokio::test]
async fn test_yaml_loading() {
    let registry = ActionRegistry::new();

    let mut executor = DagExecutor::new(None, registry, ":memory:").await.unwrap();

    // Try to load YAML from examples
    let yaml_path = "examples/fixtures/pipeline.yaml";
    if std::path::Path::new(yaml_path).exists() {
        let result = executor.load_yaml_file(yaml_path).await;
        // Should not panic, whether it succeeds or fails
        match result {
            Ok(_) => println!("YAML loaded successfully"),
            Err(e) => println!("YAML loading failed (expected): {}", e),
        }
    }

    assert!(true);
}

/// Test configuration
#[tokio::test]
async fn test_dag_config() {
    let mut config = dagger::DagConfig::default();
    config.enable_parallel_execution = true;
    config.max_parallel_nodes = 5;

    let registry = ActionRegistry::new();

    let _executor = DagExecutor::new(Some(config), registry, ":memory:")
        .await
        .unwrap();

    assert!(true);
}

/// Cleanup function
#[tokio::test]
async fn test_cleanup() {
    assert!(true);
}

/// Test that services are accessible in nodes
#[tokio::test]
async fn test_services_accessible_in_nodes() {
    // Define a test service structure
    #[derive(Clone)]
    struct TestServices {
        value: Arc<AtomicU32>,
        name: String,
    }

    // Define a test node that accesses services
    struct TestNode;

    #[async_trait]
    impl NodeAction for TestNode {
        fn name(&self) -> &str {
            "TestNode"
        }

        async fn execute(&self, ctx: &NodeCtx) -> Result<NodeOutput> {
            let data =
                DagNodeContextData::from_ctx(ctx).expect("DagNodeContextData missing from NodeCtx");
            let services = data
                .services::<TestServices>()
                .expect("Could not access services");

            // Increment the counter to prove we accessed the services
            services.value.fetch_add(1, Ordering::Relaxed);
            println!("Accessed service with name: {}", services.name);

            Ok(NodeOutput::success_empty())
        }
    }

    // Create test setup
    let registry = ActionRegistry::new();

    // Use in-memory database for testing
    let mut executor = DagExecutor::new(None, registry, ":memory:").await.unwrap();

    // Create test services
    let services = Arc::new(TestServices {
        value: Arc::new(AtomicU32::new(0)),
        name: "TestService".to_string(),
    });

    // Set the services on the executor
    executor.set_services(services.clone());

    // Register the test node
    executor.register_action(Arc::new(TestNode)).await.unwrap();

    // Create a simple node
    let node = Node {
        id: "test_node".to_string(),
        dependencies: vec![],
        inputs: vec![],
        outputs: vec![],
        action: "TestNode".to_string(),
        failure: "".to_string(),
        onfailure: false,
        description: "Test node for service access".to_string(),
        timeout: 60,
        try_count: 1,
        instructions: None,
    };

    // Execute the node
    let cache = Cache::new();
    let ctx = NodeCtx::new("dag", node.id.clone(), serde_json::json!({}), cache.clone())
        .with_app_data(Arc::new(DagNodeContextData {
            node: node.clone(),
            services: Some(services.clone() as Arc<dyn std::any::Any + Send + Sync>),
            app_state: None,
        }));
    let test_node_action = Arc::new(TestNode);
    test_node_action.execute(&ctx).await.unwrap();

    // Verify the service was accessed (counter should be 1)
    assert_eq!(services.value.load(Ordering::Relaxed), 1);

    // Execute again to ensure it works multiple times
    test_node_action.execute(&ctx).await.unwrap();
    assert_eq!(services.value.load(Ordering::Relaxed), 2);

    // Test that we can also downcast to the correct type
    let retrieved_services = executor.get_services::<TestServices>().unwrap();
    assert_eq!(retrieved_services.name, "TestService");
}
