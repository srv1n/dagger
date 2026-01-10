//! Minimal DAG Flow example using the action macro

use dagger::{action, coord::ActionRegistry, Cache, DagExecutor};
use serde_json::json;

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

    Ok(())
}
