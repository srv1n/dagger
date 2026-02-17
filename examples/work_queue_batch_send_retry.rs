use dagger::coord::ActionRegistry;
use dagger::{execute_batch, BatchFanoutSpec, Cache, DagExecutor, ExecServices, LocalExecHost};
use std::sync::Arc;

fn failed_item_ids(result: &dagger::BatchExecution) -> Vec<String> {
    let mut failed = Vec::new();

    for (item_id, step_refs) in &result.plan.item_steps {
        let item_failed = step_refs.iter().any(|step_ref| {
            result
                .report
                .node_outcomes
                .iter()
                .find(|o| o.node_id == step_ref.step_id)
                .map(|o| !o.success)
                .unwrap_or(true)
        });

        if item_failed {
            failed.push(item_id.clone());
        }
    }

    failed
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let spec_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "examples/work_queue_batch_send_retry_spec.yaml".to_string());
    let raw = std::fs::read_to_string(&spec_path)?;
    let spec: BatchFanoutSpec = serde_yaml::from_str(&raw)?;

    let registry = ActionRegistry::new();
    let mut executor = DagExecutor::new(None, registry, "sqlite::memory:").await?;

    // This example assumes a Unix-like environment.
    let host = LocalExecHost::new().allow_system("/bin/echo");
    executor.set_services(Arc::new(ExecServices {
        host: Arc::new(host),
    }));

    let cache = Cache::new();
    let (_tx, rx) = tokio::sync::oneshot::channel();

    println!("== First run ==");
    println!("Spec: {}", spec_path);
    let result = execute_batch(&mut executor, &cache, spec.clone(), rx).await?;
    println!("overall_success: {}", result.report.overall_success);

    let failed = failed_item_ids(&result);
    println!("failed_item_ids: {:?}", failed);

    if failed.is_empty() {
        println!("No failures; nothing to retry.");
        return Ok(());
    }

    // Retry only failed items (same container_id + step names → stable step_ids).
    let mut retry_spec = spec;
    retry_spec.dag_name = format!("{}_retry", retry_spec.dag_name);
    retry_spec.input.selected_item_ids = failed.clone();

    // Increase stdout cap so long item_ids don't truncate JSON on retry.
    for step in &mut retry_spec.steps {
        if let Some(obj) = step.inputs.as_object_mut() {
            obj.insert(
                "max_stdout_bytes".to_string(),
                serde_json::Value::Number(serde_json::Number::from(4 * 1024)),
            );
        }
    }

    let (_tx2, rx2) = tokio::sync::oneshot::channel();
    println!("\n== Retry run (failed-only) ==");
    let retry_result = execute_batch(&mut executor, &cache, retry_spec, rx2).await?;
    println!("overall_success: {}", retry_result.report.overall_success);

    Ok(())
}
