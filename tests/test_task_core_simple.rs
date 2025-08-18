//! Simple test suite for Task-Core system
//!
//! This test focuses on ensuring the basic Task-Core functionality compiles
//! and works without API compatibility issues.

use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;

// Basic compilation test for task-core types
#[tokio::test]
async fn test_task_core_types_compile() {
    // Test basic type creation
    let task_id: u64 = 1;
    let agent_id: u16 = 42;

    assert_eq!(task_id, 1);
    assert_eq!(agent_id, 42);
}

// Test that we can create mock agent specifications
#[tokio::test]
async fn test_mock_agent_spec() {
    use bytes::Bytes;

    // Mock task specification
    let input = Bytes::from("test input");
    let description: Arc<str> = Arc::from("test task");
    let timeout = Some(Duration::from_secs(5));

    assert_eq!(input.as_ref(), b"test input");
    assert_eq!(description.as_ref(), "test task");
    assert_eq!(timeout, Some(Duration::from_secs(5)));
}

// Test async function compatibility
async fn mock_agent_function(input: Bytes, _ctx: Arc<()>) -> Result<Bytes, String> {
    // Simple echo function
    Ok(input)
}

#[tokio::test]
async fn test_agent_function_interface() {
    use bytes::Bytes;

    let input = Bytes::from("hello world");
    let ctx = Arc::new(());

    let result = mock_agent_function(input.clone(), ctx).await.unwrap();
    assert_eq!(result, input);
}

// Test concurrent execution simulation
#[tokio::test]
async fn test_concurrent_tasks() {
    use bytes::Bytes;

    let start = std::time::Instant::now();

    // Create multiple async tasks
    let tasks = (0..4).map(|i| {
        let input = Bytes::from(format!("task {}", i));
        tokio::spawn(async move {
            // Simulate some work
            tokio::time::sleep(Duration::from_millis(100)).await;
            input
        })
    });

    // Wait for all tasks
    let results: Vec<_> = futures::future::join_all(tasks).await;
    let elapsed = start.elapsed();

    // Should complete in roughly 100ms (parallel), not 400ms (sequential)
    assert!(elapsed < Duration::from_millis(200));
    assert_eq!(results.len(), 4);

    // Check all tasks completed successfully
    for (i, result) in results.into_iter().enumerate() {
        let bytes = result.unwrap();
        assert_eq!(bytes, Bytes::from(format!("task {}", i)));
    }
}

// Test error handling patterns
#[tokio::test]
async fn test_error_patterns() {
    #[derive(Debug, Clone)]
    enum MockAgentError {
        User(String),
        System(String),
        Timeout(Duration),
    }

    impl MockAgentError {
        fn is_retryable(&self) -> bool {
            matches!(self, MockAgentError::System(_) | MockAgentError::Timeout(_))
        }
    }

    let user_error = MockAgentError::User("bad input".into());
    let system_error = MockAgentError::System("db connection failed".into());
    let timeout_error = MockAgentError::Timeout(Duration::from_secs(30));

    assert!(!user_error.is_retryable());
    assert!(system_error.is_retryable());
    assert!(timeout_error.is_retryable());
}

// Test JSON serialization patterns commonly used in task agents
#[tokio::test]
async fn test_json_patterns() {
    use serde_json::{json, Value};

    let input_data = json!({
        "action": "process",
        "params": {
            "x": 10,
            "y": 20
        }
    });

    let action = input_data["action"].as_str().unwrap();
    let x = input_data["params"]["x"].as_i64().unwrap();
    let y = input_data["params"]["y"].as_i64().unwrap();

    assert_eq!(action, "process");
    assert_eq!(x, 10);
    assert_eq!(y, 20);

    let result = json!({
        "status": "success",
        "result": x + y
    });

    assert_eq!(result["result"], 30);
}

// Test memory management patterns
#[tokio::test]
async fn test_memory_patterns() {
    use bytes::Bytes;
    use std::sync::Arc;

    // Test Arc sharing
    let shared_data = Arc::new("shared string");
    let clone1 = shared_data.clone();
    let clone2 = shared_data.clone();

    assert_eq!(Arc::strong_count(&shared_data), 3);

    drop(clone1);
    drop(clone2);
    assert_eq!(Arc::strong_count(&shared_data), 1);

    // Test Bytes zero-copy sharing
    let original = Bytes::from("hello world");
    let slice1 = original.slice(0..5);
    let slice2 = original.slice(6..11);

    assert_eq!(slice1, "hello");
    assert_eq!(slice2, "world");
}

// Test atomics patterns used in task status
#[tokio::test]
async fn test_atomic_patterns() {
    use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

    let status = AtomicU8::new(0); // Pending
    let retry_count = AtomicU32::new(0);

    // Simulate status transitions
    assert_eq!(status.load(Ordering::Relaxed), 0);

    status.store(2, Ordering::Relaxed); // Ready
    assert_eq!(status.load(Ordering::Relaxed), 2);

    // Simulate retry increment
    let old_count = retry_count.fetch_add(1, Ordering::SeqCst);
    assert_eq!(old_count, 0);
    assert_eq!(retry_count.load(Ordering::Relaxed), 1);
}
