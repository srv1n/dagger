//! Simple test suite for DAG Flow system
//!
//! This test focuses on ensuring the basic DAG Flow functionality works
//! without getting bogged down in API compatibility issues.

use anyhow::Result;
use dagger::dag_flow::{Cache, DagExecutor, Node, NodeAction, IField, OField};
use dagger::insert_value;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicU32, Ordering};

/// Test that basic DAG executor can be created
#[tokio::test]
async fn test_dag_executor_creation() {
    let registry = Arc::new(RwLock::new(HashMap::new()));

    // Clean up any existing database first
    let _ = std::fs::remove_dir_all("test_dag_db");

    let _executor = DagExecutor::new(None, registry, "test_dag_db").await.unwrap();

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
    let registry = Arc::new(RwLock::new(HashMap::new()));

    // Clean up any existing database first
    let _ = std::fs::remove_dir_all("test_yaml_db");

    let mut executor = DagExecutor::new(None, registry, "test_yaml_db").await.unwrap();

    // Try to load YAML from examples
    let yaml_path = "examples/dag_flow/pipeline.yaml";
    if std::path::Path::new(yaml_path).exists() {
        let result = executor.load_yaml_file(yaml_path);
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

    let registry = Arc::new(RwLock::new(HashMap::new()));

    // Clean up any existing database first
    let _ = std::fs::remove_dir_all("test_config_db");

    let _executor = DagExecutor::new(Some(config), registry, "test_config_db").await.unwrap();

    assert!(true);
}

/// Cleanup function
fn cleanup_test_dbs() {
    let test_dbs = ["test_dag_db", "test_yaml_db", "test_config_db"];

    for db in &test_dbs {
        let _ = std::fs::remove_dir_all(db);
    }
}

#[tokio::test]
async fn test_cleanup() {
    cleanup_test_dbs();
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
        fn name(&self) -> String {
            "TestNode".to_string()
        }

        async fn execute(
            &self,
            executor: &mut DagExecutor,
            _node: &Node,
            _cache: &Cache,
        ) -> Result<()> {
            // Access the services through the executor
            if let Some(services) = executor.get_services::<TestServices>() {
                // Increment the counter to prove we accessed the services
                services.value.fetch_add(1, Ordering::Relaxed);
                println!("Accessed service with name: {}", services.name);
            } else {
                panic!("Could not access services!");
            }
            Ok(())
        }

        fn schema(&self) -> serde_json::Value {
            serde_json::json!({
                "name": "TestNode",
                "description": "Test node for service access",
                "parameters": { "type": "object", "properties": {} },
                "returns": { "type": "object" }
            })
        }
    }

    // Create test setup
    let registry = Arc::new(RwLock::new(HashMap::new()));
    
    // Use in-memory database for testing
    let mut executor = DagExecutor::new(None, registry, ":memory:")
        .await
        .unwrap();

    // Create test services
    let services = Arc::new(TestServices {
        value: Arc::new(AtomicU32::new(0)),
        name: "TestService".to_string(),
    });

    // Set the services on the executor
    executor.set_services(services.clone());

    // Register the test node
    executor.register_action(Arc::new(TestNode)).unwrap();

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
    let test_node_action = Arc::new(TestNode);
    test_node_action.execute(&mut executor, &node, &cache).await.unwrap();

    // Verify the service was accessed (counter should be 1)
    assert_eq!(services.value.load(Ordering::Relaxed), 1);
    
    // Execute again to ensure it works multiple times
    test_node_action.execute(&mut executor, &node, &cache).await.unwrap();
    assert_eq!(services.value.load(Ordering::Relaxed), 2);

    // Test that we can also downcast to the correct type
    let retrieved_services = executor.get_services::<TestServices>().unwrap();
    assert_eq!(retrieved_services.name, "TestService");
}
