use anyhow::Result;
use async_trait::async_trait;
use dagger::coord::{ActionRegistry, NodeAction, NodeCtx, NodeOutput};
use dagger::dag_flow::{DagExecutor, Graph, Node};
use std::sync::Arc;

struct NoopAction;

#[async_trait]
impl NodeAction for NoopAction {
    fn name(&self) -> &str {
        "noop"
    }

    async fn execute(&self, _ctx: &NodeCtx) -> Result<NodeOutput> {
        Ok(NodeOutput::success_empty())
    }
}

fn node(id: &str, deps: &[&str]) -> Node {
    Node {
        id: id.to_string(),
        dependencies: deps.iter().map(|dep| dep.to_string()).collect(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        action: "noop".to_string(),
        failure: String::new(),
        onfailure: true,
        description: String::new(),
        timeout: 30,
        try_count: 1,
        instructions: None,
    }
}

fn graph(name: &str, nodes: Vec<Node>) -> Graph {
    Graph {
        name: name.to_string(),
        description: String::new(),
        instructions: None,
        tags: Vec::new(),
        author: String::new(),
        version: String::new(),
        signature: String::new(),
        config: None,
        nodes,
    }
}

async fn executor() -> DagExecutor {
    let registry = ActionRegistry::new();
    let mut executor = DagExecutor::new(None, registry, ":memory:").await.unwrap();
    executor
        .register_action(Arc::new(NoopAction))
        .await
        .unwrap();
    executor
}

#[tokio::test]
async fn rejects_cycle_after_edges_are_added() {
    let mut executor = executor().await;
    let err = executor
        .load_graph(graph("cyclic", vec![node("a", &["b"]), node("b", &["a"])]))
        .await
        .unwrap_err()
        .to_string();

    assert!(err.to_lowercase().contains("cycle") || err.to_lowercase().contains("dag"));
}

#[tokio::test]
async fn rejects_duplicate_node_ids() {
    let mut executor = executor().await;
    let err = executor
        .load_graph(graph("duplicate", vec![node("a", &[]), node("a", &[])]))
        .await
        .unwrap_err()
        .to_string();

    assert!(err.to_lowercase().contains("duplicate"));
}

#[tokio::test]
async fn rejects_missing_dependency() {
    let mut executor = executor().await;
    let err = executor
        .load_graph(graph("missing_dep", vec![node("a", &["missing"])]))
        .await
        .unwrap_err()
        .to_string();

    assert!(err.to_lowercase().contains("dependency"));
}

#[test]
fn minimal_yaml_uses_defaults() {
    let yaml = r#"
name: minimal
nodes:
  - id: fetch
    action: noop
  - id: process
    action: noop
    dependencies: [fetch]
"#;

    let graph: Graph = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(graph.name, "minimal");
    assert_eq!(graph.description, "");
    assert_eq!(graph.tags.len(), 0);
    assert_eq!(graph.nodes[0].dependencies.len(), 0);
    assert_eq!(graph.nodes[0].timeout, 300);
    assert_eq!(graph.nodes[0].try_count, 3);
}
