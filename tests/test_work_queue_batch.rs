use dagger::coord::ActionRegistry;
use dagger::{
    execute_batch, get_input, BatchFanoutSpec, BatchInput, Cache, DagExecutor, ExecKind,
    ExecServices, LocalExecHost, PerItemStepTemplate,
};
use serde_json::json;
use std::sync::Arc;

#[cfg(not(windows))]
#[tokio::test]
async fn test_work_queue_batch_exec_fanout() {
    let registry = ActionRegistry::new();
    let mut executor = DagExecutor::new(None, registry, "sqlite::memory:")
        .await
        .unwrap();

    // This test assumes a Unix-like environment.
    let host = LocalExecHost::new().allow_system("/bin/echo");
    executor.set_services(Arc::new(ExecServices {
        host: Arc::new(host),
    }));

    let cache = Cache::new();
    let (_tx, rx) = tokio::sync::oneshot::channel();

    let spec = BatchFanoutSpec {
        dag_name: "test_work_queue_batch_exec_fanout".to_string(),
        input: BatchInput {
            container_id: "container_1".to_string(),
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

    let result = execute_batch(&mut executor, &cache, spec, rx)
        .await
        .unwrap();
    assert!(result.report.overall_success);

    // Verify mapping and that each item produced JSON output.
    for (item_id, steps) in &result.plan.item_steps {
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].step_name, "send");

        let step_id = &steps[0].step_id;
        let stdout_json: serde_json::Value = get_input(&cache, step_id, "stdout_json").unwrap();
        assert_eq!(stdout_json["ok"], true);
        assert_eq!(stdout_json["item_id"], item_id.as_str());
    }
}
