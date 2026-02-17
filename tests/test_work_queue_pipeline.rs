use dagger::coord::ActionRegistry;
use dagger::{
    execute_pipeline, get_input, Cache, DagExecutor, ExecKind, ExecServices, ExecSpec,
    LocalExecHost, PipeMode, PipeSpec, PipelineSpec, PipelineStep,
};
use serde_json::json;
use std::sync::Arc;

#[cfg(not(windows))]
#[tokio::test]
async fn test_work_queue_pipeline_text_pipe() {
    let registry = ActionRegistry::new();
    let mut executor = DagExecutor::new(None, registry, "sqlite::memory:")
        .await
        .unwrap();

    let host = LocalExecHost::new()
        .allow_system("/bin/echo")
        .allow_system("/bin/cat");
    executor.set_services(Arc::new(ExecServices {
        host: Arc::new(host),
    }));

    let cache = Cache::new();
    let (_tx, rx) = tokio::sync::oneshot::channel();

    let spec = PipelineSpec {
        dag_name: "test_work_queue_pipeline_text_pipe".to_string(),
        steps: vec![
            PipelineStep {
                name: "produce".to_string(),
                exec: ExecSpec {
                    kind: ExecKind::System,
                    executable: "/bin/echo".to_string(),
                    args: vec!["hello".to_string()],
                    cwd: None,
                    env: Default::default(),
                    stdin: None,
                    stdin_json: None,
                    timeout_ms: 5_000,
                    max_stdout_bytes: 4 * 1024,
                    max_stderr_bytes: 4 * 1024,
                    parse_stdout_as_json: false,
                },
                timeout_s: Some(10),
                try_count: Some(1),
                deps: None,
                pipe: None,
            },
            PipelineStep {
                name: "consume".to_string(),
                exec: ExecSpec {
                    kind: ExecKind::System,
                    executable: "/bin/cat".to_string(),
                    args: vec![],
                    cwd: None,
                    env: Default::default(),
                    stdin: None,
                    stdin_json: None,
                    timeout_ms: 5_000,
                    max_stdout_bytes: 4 * 1024,
                    max_stderr_bytes: 4 * 1024,
                    parse_stdout_as_json: false,
                },
                timeout_s: Some(10),
                try_count: Some(1),
                deps: None,
                pipe: Some(PipeSpec {
                    from_step: None,
                    mode: PipeMode::Text,
                }),
            },
        ],
    };

    let result = execute_pipeline(&mut executor, &cache, spec, rx)
        .await
        .unwrap();
    assert!(result.report.overall_success);

    let consume_id = &result.plan.steps[1].step_id;
    let stdout: String = get_input(&cache, consume_id, "stdout").unwrap();
    assert_eq!(stdout, "hello\n");
}

#[cfg(not(windows))]
#[tokio::test]
async fn test_work_queue_pipeline_json_pipe() {
    let registry = ActionRegistry::new();
    let mut executor = DagExecutor::new(None, registry, "sqlite::memory:")
        .await
        .unwrap();

    let host = LocalExecHost::new()
        .allow_system("/bin/echo")
        .allow_system("/bin/cat");
    executor.set_services(Arc::new(ExecServices {
        host: Arc::new(host),
    }));

    let cache = Cache::new();
    let (_tx, rx) = tokio::sync::oneshot::channel();

    let spec = PipelineSpec {
        dag_name: "test_work_queue_pipeline_json_pipe".to_string(),
        steps: vec![
            PipelineStep {
                name: "produce_json".to_string(),
                exec: ExecSpec {
                    kind: ExecKind::System,
                    executable: "/bin/echo".to_string(),
                    args: vec!["{\"ok\":true,\"n\":1}".to_string()],
                    cwd: None,
                    env: Default::default(),
                    stdin: None,
                    stdin_json: None,
                    timeout_ms: 5_000,
                    max_stdout_bytes: 4 * 1024,
                    max_stderr_bytes: 4 * 1024,
                    parse_stdout_as_json: true,
                },
                timeout_s: Some(10),
                try_count: Some(1),
                deps: None,
                pipe: None,
            },
            PipelineStep {
                name: "consume_json".to_string(),
                exec: ExecSpec {
                    kind: ExecKind::System,
                    executable: "/bin/cat".to_string(),
                    args: vec![],
                    cwd: None,
                    env: Default::default(),
                    stdin: None,
                    stdin_json: None,
                    timeout_ms: 5_000,
                    max_stdout_bytes: 4 * 1024,
                    max_stderr_bytes: 4 * 1024,
                    parse_stdout_as_json: true,
                },
                timeout_s: Some(10),
                try_count: Some(1),
                deps: None,
                pipe: Some(PipeSpec {
                    from_step: None,
                    mode: PipeMode::Json,
                }),
            },
        ],
    };

    let result = execute_pipeline(&mut executor, &cache, spec, rx)
        .await
        .unwrap();
    assert!(result.report.overall_success);

    let consume_id = &result.plan.steps[1].step_id;
    let stdout_json: serde_json::Value = get_input(&cache, consume_id, "stdout_json").unwrap();
    assert_eq!(stdout_json, json!({ "ok": true, "n": 1 }));
}
