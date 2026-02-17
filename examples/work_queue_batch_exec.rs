//! Work Queue batch fanout + exec primitive example.
//!
//! Demonstrates:
//! - Deterministic per-item step IDs
//! - Per-item isolation with continue-on-error
//! - JSON-in/JSON-out exec via `/bin/echo`

use dagger::coord::ActionRegistry;
use dagger::{
    execute_batch, BatchFanoutSpec, BatchInput, Cache, DagExecutor, ExecKind, ExecServices,
    LocalExecHost, PerItemStepTemplate,
};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let registry = ActionRegistry::new();
    let mut executor = DagExecutor::new(None, registry, "sqlite::memory:").await?;

    // Provide an ExecHost via services. In a real host app, this is where you
    // would enforce allowlists / approvals and resolve sidecars.
    let host = LocalExecHost::new().allow_system("/bin/echo");
    executor.set_services(Arc::new(ExecServices {
        host: Arc::new(host),
    }));

    let cache = Cache::new();
    let (_tx, rx) = tokio::sync::oneshot::channel();

    let spec = BatchFanoutSpec {
        dag_name: "work_queue_batch_exec_demo".to_string(),
        input: BatchInput {
            container_id: "outbox_batch_123".to_string(),
            selected_item_ids: vec!["item_a".to_string(), "item_b".to_string()],
            approved_revision_ids: vec!["rev_001".to_string()],
        },
        steps: vec![PerItemStepTemplate {
            name: "send".to_string(),
            action: "exec".to_string(),
            inputs: json!({
                "kind": ExecKind::System,
                "executable": "/bin/echo",
                "args": [ "{\"ok\":true,\"item_id\":\"{{item_id}}\"}" ],
                "parse_stdout_as_json": true,
                "timeout_ms": 5_000,
                "max_stdout_bytes": 4 * 1024,
                "max_stderr_bytes": 4 * 1024
            }),
            timeout_s: Some(10),
            try_count: Some(1),
        }],
        continue_on_error: true,
    };

    let result = execute_batch(&mut executor, &cache, spec, rx).await?;

    println!("Overall success: {}", result.report.overall_success);
    println!(
        "Item → step mapping:\n{}",
        serde_json::to_string_pretty(&result.plan.item_steps)?
    );

    Ok(())
}
