//! DAG Flow example that emits a DOT graph for visualization

use dagger::coord::ActionRegistry;
use dagger::{action, Cache, DagExecutor};
use serde_json::json;
use std::fs;

#[action(name = "fetch")]
async fn fetch(_input: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    Ok(json!({"data": 42}))
}

#[action(name = "process")]
async fn process(input: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let data = input["data"].as_i64().unwrap_or(0);
    Ok(json!({"result": data * 2}))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let registry = ActionRegistry::new();
    let mut executor = DagExecutor::new(None, registry, "sqlite::memory:").await?;

    executor
        .load_yaml_file("examples/fixtures/basic_pipeline.yaml")
        .await?;

    let cache = Cache::new();
    let (_tx, rx) = tokio::sync::oneshot::channel();
    let report = executor.execute_static_dag("pipeline", &cache, rx).await?;
    println!("DAG success: {}", report.overall_success);

    let dot = executor.serialize_tree_to_dot("pipeline").await?;
    let dot_path = std::env::temp_dir().join("dagger_pipeline.dot");
    fs::write(&dot_path, &dot)?;

    println!("\nDOT graph written to: {}", dot_path.display());
    println!("\n--- DOT ---\n{}\n", dot);
    println!(
        "Render with: dot -Tpng {} -o pipeline.png",
        dot_path.display()
    );

    Ok(())
}
