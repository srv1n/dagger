use dagger::coord::ActionRegistry;
use dagger::dag_flow::events::{BufferingEventSink, RuntimeEvent};
use dagger::{
    get_input, Cache, DagExecutor, Graph, HitlDecision, HitlResumeToken, HitlRuntime, Node,
    ServiceRegistry,
};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn test_work_queue_hitl_checkpoint_resume() {
    let registry = ActionRegistry::new();
    let mut executor = DagExecutor::new(None, registry, "sqlite::memory:")
        .await
        .unwrap();

    let sink = Arc::new(BufferingEventSink::new());
    executor.set_event_sink(sink.clone());

    let runtime = Arc::new(HitlRuntime::new(
        executor.branches.clone(),
        Some(sink.clone()),
    ));
    let services = ServiceRegistry::new().with(runtime.clone());
    executor.set_services(Arc::new(services));

    let dag_name = "test_work_queue_hitl_checkpoint_resume";
    let graph = Graph {
        name: dag_name.to_string(),
        description: "hitl test".to_string(),
        instructions: None,
        tags: vec!["test".to_string()],
        author: "test".to_string(),
        version: "1.0".to_string(),
        signature: "".to_string(),
        config: None,
        nodes: vec![Node {
            id: "checkpoint".to_string(),
            action: "hitl_checkpoint".to_string(),
            dependencies: vec![],
            inputs: vec![],
            outputs: vec![],
            failure: String::new(),
            onfailure: true,
            description: "checkpoint".to_string(),
            timeout: 300,
            try_count: 1,
            instructions: None,
        }],
    };
    executor.load_graph(graph).await.unwrap();

    let cache = Cache::new();
    executor
        .set_node_inputs_json(
            dag_name,
            "checkpoint",
            json!({
                "run_id": "run_001",
                "branch_id": "branch_a",
                "prompt": "approve?",
                "payload": { "k": "v" }
            }),
            &cache,
        )
        .await
        .unwrap();

    let (_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let cache_for_exec = cache.clone();
    let handle = tokio::spawn(async move {
        executor
            .execute_static_dag(dag_name, &cache_for_exec, cancel_rx)
            .await
    });

    // Wait for the "needs approval" domain event, then resume.
    let token: HitlResumeToken = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(token) = sink
                .get_events()
                .into_iter()
                .filter(|env| env.run_id == "run_001")
                .find_map(|env| match env.event {
                    RuntimeEvent::DomainEvent { name, payload }
                        if name == "hitl.needs_approval" =>
                    {
                        payload
                            .get("token")
                            .cloned()
                            .map(|v| serde_json::from_value(v).unwrap())
                    }
                    _ => None,
                })
            {
                break token;
            }
            if std::time::Instant::now() > deadline {
                panic!("timed out waiting for hitl.needs_approval event");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };

    runtime
        .resume(
            &token,
            HitlDecision {
                approved: true,
                payload: json!({ "approved_by": "test" }),
            },
        )
        .unwrap();

    let report = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(report.overall_success);

    let approved: bool = get_input(&cache, "checkpoint", "approved").unwrap();
    assert!(approved);
    let payload: serde_json::Value = get_input(&cache, "checkpoint", "payload").unwrap();
    assert_eq!(payload["approved_by"], "test");
}
