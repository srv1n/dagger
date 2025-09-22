use dagger::{Cache, DagExecutor, DagConfig, insert_value};
use dagger::dag_flow::{Graph, Node, IField, OField};
use dagger::coord::{Coordinator, ActionRegistry, NodeAction, NodeCtx, NodeOutput};
use async_trait::async_trait;
use std::sync::Arc;
use serde_json::json;
use tokio::sync::oneshot;

// Simple action that just passes data through
struct PassThroughAction {
    name: String,
}

#[async_trait]
impl NodeAction for PassThroughAction {
    fn name(&self) -> &str {
        &self.name
    }
    
    async fn execute(&self, ctx: &NodeCtx) -> anyhow::Result<NodeOutput> {
        println!("Executing node: {} with inputs: {:?}", ctx.node_id, ctx.inputs);
        
        // Store output in cache for dependent nodes
        dagger::insert_value(&ctx.cache, &ctx.node_id, "output", json!({
            "message": format!("Output from {}", ctx.node_id),
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }))?;
        
        Ok(NodeOutput::success(json!({
            "status": "completed",
            "node": ctx.node_id.clone(),
        })))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Set up logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();
    
    println!("Testing multi-node DAG execution");
    
    // Create registry and register actions
    let registry = ActionRegistry::new();
    registry.register(Arc::new(PassThroughAction { name: "action1".to_string() }));
    registry.register(Arc::new(PassThroughAction { name: "action2".to_string() }));
    registry.register(Arc::new(PassThroughAction { name: "action3".to_string() }));
    registry.register(Arc::new(PassThroughAction { name: "action4".to_string() }));
    
    // Create executor
    let config = DagConfig {
        enable_parallel_execution: true,
        max_parallel_nodes: 4,
        ..Default::default()
    };
    
    let mut executor = DagExecutor::new(
        Some(config),
        registry.clone(),
        "sqlite::memory:"
    ).await?;
    
    // Create a simple DAG with dependencies
    // node1 -> node2 -> node4
    //       -> node3 -> node4
    let graph = Graph {
        name: "test_dag".to_string(),
        description: "Test DAG with multiple nodes".to_string(),
        author: "Test".to_string(),
        version: "1.0.0".to_string(),
        signature: "test_sig".to_string(),
        tags: vec![],
        instructions: None,
        nodes: vec![
            Node {
                id: "node1".to_string(),
                dependencies: vec![],
                inputs: vec![],
                outputs: vec![OField { name: "output".to_string(), description: None }],
                action: "action1".to_string(),
                failure: "".to_string(),
                onfailure: false,
                description: "First node".to_string(),
                timeout: 10,
                try_count: 1,
                instructions: None,
            },
            Node {
                id: "node2".to_string(),
                dependencies: vec!["node1".to_string()],
                inputs: vec![IField {
                    name: "input".to_string(),
                    description: None,
                    reference: "node1.output".to_string(),
                }],
                outputs: vec![OField { name: "output".to_string(), description: None }],
                action: "action2".to_string(),
                failure: "".to_string(),
                onfailure: false,
                description: "Second node".to_string(),
                timeout: 10,
                try_count: 1,
                instructions: None,
            },
            Node {
                id: "node3".to_string(),
                dependencies: vec!["node1".to_string()],
                inputs: vec![IField {
                    name: "input".to_string(),
                    description: None,
                    reference: "node1.output".to_string(),
                }],
                outputs: vec![OField { name: "output".to_string(), description: None }],
                action: "action3".to_string(),
                failure: "".to_string(),
                onfailure: false,
                description: "Third node (parallel with node2)".to_string(),
                timeout: 10,
                try_count: 1,
                instructions: None,
            },
            Node {
                id: "node4".to_string(),
                dependencies: vec!["node2".to_string(), "node3".to_string()],
                inputs: vec![
                    IField {
                        name: "input2".to_string(),
                        description: None,
                        reference: "node2.output".to_string(),
                    },
                    IField {
                        name: "input3".to_string(),
                        description: None,
                        reference: "node3.output".to_string(),
                    },
                ],
                outputs: vec![OField { name: "output".to_string(), description: None }],
                action: "action4".to_string(),
                failure: "".to_string(),
                onfailure: false,
                description: "Final node".to_string(),
                timeout: 10,
                try_count: 1,
                instructions: None,
            },
        ],
        config: None,
    };
    
    // Load the DAG from graph structure
    // We need to convert the graph to YAML and load it
    let yaml = serde_yaml::to_string(&graph)?;
    executor.load_yaml_string(&yaml).await?;
    
    // Create coordinator
    let coordinator = Coordinator::new(vec![], 100, 100);
    
    // Create cache
    let cache = Cache::new();
    
    // Add initial input
    insert_value(&cache, "inputs", "test_input", "Hello DAG")?;
    
    // Execute with coordinator
    let (_tx, rx) = oneshot::channel();
    
    println!("\n=== Starting DAG execution ===\n");
    
    coordinator.run_parallel(
        &mut executor,
        &cache,
        "test_dag",
        "run_001",
        rx
    ).await?;
    
    println!("\n=== DAG execution complete ===\n");
    
    // Check that all nodes executed by looking at cache
    let cache_json = dagger::serialize_cache_to_prettyjson(&cache)?;
    println!("Final cache state:\n{}", cache_json);
    
    // Verify all nodes completed
    for node_id in ["node1", "node2", "node3", "node4"] {
        match dagger::get_input::<serde_json::Value>(&cache, node_id, "output") {
            Ok(value) => println!("✅ {} executed successfully: {:?}", node_id, value),
            Err(e) => println!("❌ {} did not execute: {}", node_id, e),
        }
    }
    
    Ok(())
}