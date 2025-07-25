//! Test suite for DAG Flow system

use dagger::dag_flow::{
    DagExecutor, DagConfig, Node, IField, OField, WorkflowSpec, NodeAction,
    Cache, insert_value, parse_input_from_name, OnFailure,
};
use dagger::register_action;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use anyhow::Result;

/// Simple test action that adds two numbers
async fn add_numbers(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache,
) -> Result<()> {
    let a: f64 = parse_input_from_name(cache, "a", &node.inputs)?;
    let b: f64 = parse_input_from_name(cache, "b", &node.inputs)?;
    let result = a + b;
    insert_value(cache, &node.id, "sum", result)?;
    Ok(())
}

/// Test action that multiplies a number by 2
async fn multiply_by_two(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache,
) -> Result<()> {
    let input: f64 = parse_input_from_name(cache, "value", &node.inputs)?;
    let result = input * 2.0;
    insert_value(cache, &node.id, "result", result)?;
    Ok(())
}

/// Test action that concatenates strings
async fn concat_strings(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache,
) -> Result<()> {
    let s1: String = parse_input_from_name(cache, "s1", &node.inputs)?;
    let s2: String = parse_input_from_name(cache, "s2", &node.inputs)?;
    let result = format!("{}{}", s1, s2);
    insert_value(cache, &node.id, "concatenated", result)?;
    Ok(())
}

/// Test action that simulates a failure
async fn failing_action(
    _executor: &mut DagExecutor,
    _node: &Node,
    _cache: &Cache,
) -> Result<()> {
    Err(anyhow::anyhow!("This action always fails"))
}

/// Test action with retry that succeeds on second attempt
static RETRY_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

async fn retry_action(
    _executor: &mut DagExecutor,
    node: &Node,
    cache: &Cache,
) -> Result<()> {
    let count = RETRY_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if count == 0 {
        Err(anyhow::anyhow!("First attempt fails"))
    } else {
        insert_value(cache, &node.id, "success", true)?;
        Ok(())
    }
}

fn setup_executor() -> DagExecutor {
    let registry = Arc::new(RwLock::new(HashMap::new()));
    let config = DagConfig {
        max_attempts: Some(3),
        on_failure: OnFailure::Pause,
        timeout_seconds: Some(60),
        enable_parallel_execution: true,
        max_parallel_nodes: 3,
        ..Default::default()
    };
    DagExecutor::new(Some(config), registry, "test_dag_db").unwrap()
}

#[tokio::test]
async fn test_simple_dag_execution() {
    let mut executor = setup_executor();
    
    // Register actions
    register_action!(executor, "add_numbers", add_numbers);
    register_action!(executor, "multiply_by_two", multiply_by_two);
    
    // Create a simple DAG: add two numbers, then multiply result by 2
    let nodes = vec![
        Node {
            id: "add".to_string(),
            dependencies: vec![],
            inputs: vec![
                IField {
                    name: "a".to_string(),
                    field_type: "f64".to_string(),
                    reference: Some("inputs.num1".to_string()),
                    value: None,
                },
                IField {
                    name: "b".to_string(),
                    field_type: "f64".to_string(),
                    reference: Some("inputs.num2".to_string()),
                    value: None,
                },
            ],
            outputs: vec![OField {
                name: "sum".to_string(),
                field_type: "f64".to_string(),
            }],
            action: "add_numbers".to_string(),
            timeout: 5,
            try_count: 1,
        },
        Node {
            id: "multiply".to_string(),
            dependencies: vec!["add".to_string()],
            inputs: vec![IField {
                name: "value".to_string(),
                field_type: "f64".to_string(),
                reference: Some("add.sum".to_string()),
                value: None,
            }],
            outputs: vec![OField {
                name: "result".to_string(),
                field_type: "f64".to_string(),
            }],
            action: "multiply_by_two".to_string(),
            timeout: 5,
            try_count: 1,
        },
    ];
    
    executor.insert_dag("test_dag", nodes).unwrap();
    
    // Set up inputs
    let cache = Cache::new();
    insert_value(&cache, "inputs", "num1", 10.0).unwrap();
    insert_value(&cache, "inputs", "num2", 5.0).unwrap();
    
    // Execute
    let (_, cancel_rx) = tokio::sync::oneshot::channel();
    let spec = WorkflowSpec::Static {
        name: "test_dag".to_string(),
    };
    let report = executor.execute_dag(spec, &cache, cancel_rx).await.unwrap();
    
    // Verify results
    assert!(report.overall_success);
    let sum: f64 = cache.get("add").unwrap().get("sum").unwrap()
        .as_f64().unwrap();
    assert_eq!(sum, 15.0);
    
    let result: f64 = cache.get("multiply").unwrap().get("result").unwrap()
        .as_f64().unwrap();
    assert_eq!(result, 30.0);
}

#[tokio::test]
async fn test_parallel_execution() {
    let mut executor = setup_executor();
    
    // Register actions
    register_action!(executor, "concat_strings", concat_strings);
    
    // Create a DAG with parallel branches
    let nodes = vec![
        // Two parallel nodes
        Node {
            id: "branch1".to_string(),
            dependencies: vec![],
            inputs: vec![
                IField {
                    name: "s1".to_string(),
                    field_type: "String".to_string(),
                    value: Some("Hello".into()),
                },
                IField {
                    name: "s2".to_string(),
                    field_type: "String".to_string(),
                    value: Some(" World".into()),
                },
            ],
            outputs: vec![OField {
                name: "concatenated".to_string(),
                field_type: "String".to_string(),
            }],
            action: "concat_strings".to_string(),
            timeout: 5,
            try_count: 1,
        },
        Node {
            id: "branch2".to_string(),
            dependencies: vec![],
            inputs: vec![
                IField {
                    name: "s1".to_string(),
                    field_type: "String".to_string(),
                    value: Some("Parallel".into()),
                },
                IField {
                    name: "s2".to_string(),
                    field_type: "String".to_string(),
                    value: Some(" Execution".into()),
                },
            ],
            outputs: vec![OField {
                name: "concatenated".to_string(),
                field_type: "String".to_string(),
            }],
            action: "concat_strings".to_string(),
            timeout: 5,
            try_count: 1,
        },
        // Merge node that depends on both branches
        Node {
            id: "merge".to_string(),
            dependencies: vec!["branch1".to_string(), "branch2".to_string()],
            inputs: vec![
                IField {
                    name: "s1".to_string(),
                    field_type: "String".to_string(),
                    reference: Some("branch1.concatenated".to_string()),
                },
                IField {
                    name: "s2".to_string(),
                    field_type: "String".to_string(),
                    reference: Some("branch2.concatenated".to_string()),
                },
            ],
            outputs: vec![OField {
                name: "concatenated".to_string(),
                field_type: "String".to_string(),
            }],
            action: "concat_strings".to_string(),
            timeout: 5,
            try_count: 1,
        },
    ];
    
    executor.insert_dag("parallel_dag", nodes).unwrap();
    
    // Execute
    let cache = Cache::new();
    let (_, cancel_rx) = tokio::sync::oneshot::channel();
    let spec = WorkflowSpec::Static {
        name: "parallel_dag".to_string(),
    };
    
    let start = std::time::Instant::now();
    let report = executor.execute_dag(spec, &cache, cancel_rx).await.unwrap();
    let duration = start.elapsed();
    
    // Verify results
    assert!(report.overall_success);
    
    let branch1_result: String = cache.get("branch1").unwrap()
        .get("concatenated").unwrap().as_str().unwrap().to_string();
    assert_eq!(branch1_result, "Hello World");
    
    let branch2_result: String = cache.get("branch2").unwrap()
        .get("concatenated").unwrap().as_str().unwrap().to_string();
    assert_eq!(branch2_result, "Parallel Execution");
    
    let merge_result: String = cache.get("merge").unwrap()
        .get("concatenated").unwrap().as_str().unwrap().to_string();
    assert_eq!(merge_result, "Hello WorldParallel Execution");
    
    // Verify parallel execution happened (should be faster than sequential)
    println!("Parallel execution took: {:?}", duration);
}

#[tokio::test]
async fn test_failure_handling() {
    let mut executor = setup_executor();
    
    // Register actions
    register_action!(executor, "add_numbers", add_numbers);
    register_action!(executor, "failing_action", failing_action);
    
    // Create a DAG with a failing node
    let nodes = vec![
        Node {
            id: "succeed".to_string(),
            dependencies: vec![],
            inputs: vec![
                IField {
                    name: "a".to_string(),
                    field_type: "f64".to_string(),
                    value: Some(1.0.into()),
                },
                IField {
                    name: "b".to_string(),
                    field_type: "f64".to_string(),
                    value: Some(2.0.into()),
                },
            ],
            outputs: vec![OField {
                name: "sum".to_string(),
                field_type: "f64".to_string(),
            }],
            action: "add_numbers".to_string(),
            timeout: 5,
            try_count: 1,
        },
        Node {
            id: "fail".to_string(),
            dependencies: vec!["succeed".to_string()],
            inputs: vec![],
            outputs: vec![],
            action: "failing_action".to_string(),
            timeout: 5,
            try_count: 1,
        },
    ];
    
    executor.insert_dag("failing_dag", nodes).unwrap();
    
    // Execute
    let cache = Cache::new();
    let (_, cancel_rx) = tokio::sync::oneshot::channel();
    let spec = WorkflowSpec::Static {
        name: "failing_dag".to_string(),
    };
    let report = executor.execute_dag(spec, &cache, cancel_rx).await.unwrap();
    
    // Verify failure
    assert!(!report.overall_success);
    assert!(report.error.is_some());
    
    // First node should have succeeded
    let sum: f64 = cache.get("succeed").unwrap().get("sum").unwrap()
        .as_f64().unwrap();
    assert_eq!(sum, 3.0);
}

#[tokio::test]
async fn test_retry_mechanism() {
    let mut executor = setup_executor();
    
    // Reset retry counter
    RETRY_COUNTER.store(0, std::sync::atomic::Ordering::SeqCst);
    
    // Register action
    register_action!(executor, "retry_action", retry_action);
    
    // Create a node that will retry
    let nodes = vec![Node {
        id: "retry_node".to_string(),
        dependencies: vec![],
        inputs: vec![],
        outputs: vec![OField {
            name: "success".to_string(),
            field_type: "bool".to_string(),
        }],
        action: "retry_action".to_string(),
        timeout: 5,
        try_count: 3, // Allow 3 attempts
    }];
    
    executor.insert_dag("retry_dag", nodes).unwrap();
    
    // Execute
    let cache = Cache::new();
    let (_, cancel_rx) = tokio::sync::oneshot::channel();
    let spec = WorkflowSpec::Static {
        name: "retry_dag".to_string(),
    };
    let report = executor.execute_dag(spec, &cache, cancel_rx).await.unwrap();
    
    // Should succeed on second attempt
    assert!(report.overall_success);
    let success: bool = cache.get("retry_node").unwrap()
        .get("success").unwrap().as_bool().unwrap();
    assert!(success);
    
    // Verify it took 2 attempts
    assert_eq!(RETRY_COUNTER.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_yaml_workflow_loading() {
    let mut executor = setup_executor();
    
    // Register actions
    register_action!(executor, "add_numbers", add_numbers);
    register_action!(executor, "multiply_by_two", multiply_by_two);
    
    // Create a YAML workflow
    let yaml_content = r#"
name: yaml_workflow
description: Test workflow loaded from YAML
author: Test
version: 1.0.0
nodes:
  - id: step1
    dependencies: []
    inputs:
      - name: a
        type: f64
        value: 5.0
      - name: b
        type: f64
        value: 3.0
    outputs:
      - name: sum
        type: f64
    action: add_numbers
    timeout: 10
    try_count: 1

  - id: step2
    dependencies: [step1]
    inputs:
      - name: value
        type: f64
        reference: step1.sum
    outputs:
      - name: result
        type: f64
    action: multiply_by_two
    timeout: 10
    try_count: 1
"#;
    
    // Save YAML to temp file
    let temp_dir = tempfile::tempdir().unwrap();
    let yaml_path = temp_dir.path().join("workflow.yaml");
    std::fs::write(&yaml_path, yaml_content).unwrap();
    
    // Load workflow
    executor.load_yaml_file(&yaml_path).unwrap();
    
    // Execute
    let cache = Cache::new();
    let (_, cancel_rx) = tokio::sync::oneshot::channel();
    let spec = WorkflowSpec::Static {
        name: "yaml_workflow".to_string(),
    };
    let report = executor.execute_dag(spec, &cache, cancel_rx).await.unwrap();
    
    // Verify results
    assert!(report.overall_success);
    let result: f64 = cache.get("step2").unwrap()
        .get("result").unwrap().as_f64().unwrap();
    assert_eq!(result, 16.0); // (5 + 3) * 2
}

#[tokio::test]
async fn test_cache_persistence() {
    let mut executor = setup_executor();
    
    // Register action
    register_action!(executor, "add_numbers", add_numbers);
    
    // Create simple node
    let nodes = vec![Node {
        id: "cache_test".to_string(),
        dependencies: vec![],
        inputs: vec![
            IField {
                name: "a".to_string(),
                field_type: "f64".to_string(),
                value: Some(7.0.into()),
            },
            IField {
                name: "b".to_string(),
                field_type: "f64".to_string(),
                value: Some(3.0.into()),
            },
        ],
        outputs: vec![OField {
            name: "sum".to_string(),
            field_type: "f64".to_string(),
        }],
        action: "add_numbers".to_string(),
        timeout: 5,
        try_count: 1,
    }];
    
    executor.insert_dag("cache_dag", nodes).unwrap();
    
    // Execute
    let cache = Cache::new();
    let (_, cancel_rx) = tokio::sync::oneshot::channel();
    let spec = WorkflowSpec::Static {
        name: "cache_dag".to_string(),
    };
    executor.execute_dag(spec, &cache, cancel_rx).await.unwrap();
    
    // Verify cache contains results
    assert!(cache.contains_key("cache_test"));
    let sum: f64 = cache.get("cache_test").unwrap()
        .get("sum").unwrap().as_f64().unwrap();
    assert_eq!(sum, 10.0);
}

/// Cleanup function to remove test databases
fn cleanup_test_dbs() {
    let _ = std::fs::remove_dir_all("test_dag_db");
}