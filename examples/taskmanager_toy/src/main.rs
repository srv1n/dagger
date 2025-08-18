use dagger::{
    taskagent::{
        taskagent, Cache, HumanTimeoutAction, JobStatus, RetryStrategy, StallAction, Task,
        TaskAgentRegistry, TaskConfiguration, TaskManager, TaskOutput, TaskStatus, TaskType,
    },
    TaskExecutor,
};

use anyhow::{anyhow, Result};
use chrono::Utc;
use dagger_macros::task_agent;
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use dagger::{TaskAgent, TASK_AGENTS};

// Define our agents using the task_agent macro
#[task_agent(
    name = "planning_agent",
    description = "Creates a plan based on the objective",
    input_schema = r#"{"type": "object", "properties": {"objective": {"type": "string"}}, "required": ["objective"]}"#,
    output_schema = r#"{"type": "object", "properties": {"plan": {"type": "string"}}, "required": ["plan"]}"#
)]
async fn planning_agent(
    input: Value,
    task_id: &str,
    job_id: &str,
    task_manager: &TaskManager,
) -> Result<Value, String> {
    // Extract the objective
    let objective = input["objective"]
        .as_str()
        .unwrap_or("No objective provided");
    info!("Planning agent processing objective: {}", objective);

    // Create a plan
    let plan = format!("1. Research schools in the area\n2. Compare ratings\n3. Check admission requirements\n4. Visit top choices\n5. Make a decision based on all factors");

    // Create retrieval task
    let now = Utc::now().naive_utc();
    let retrieval_task_id = task_manager
        .add_task(
            job_id.to_string(),
            format!("retrieval_{}", Utc::now().timestamp_millis()),
            Some(task_id.to_string()),
            Some("Find relevant information".to_string()),
            "Retrieve school information".to_string(),
            "retrieval_agent".to_string(),
            vec![],
            json!({"query": "top rated schools"}),
            None,
            3,
            TaskType::Task,
            None,
            now,
            now,
            0,
        )
        .map_err(|e| format!("Failed to create retrieval task: {}", e))?;

    info!("Created retrieval task with ID: {}", retrieval_task_id);

    // Create review task
    let review_task_id = task_manager
        .add_task(
            job_id.to_string(),
            format!("review_{}", Utc::now().timestamp_millis()),
            Some(task_id.to_string()),
            Some("Analyze the information".to_string()),
            "Review school information".to_string(),
            "review_agent".to_string(),
            vec![retrieval_task_id.clone()],
            json!({"instruction": "Review the retrieved school information"}),
            None,
            3,
            TaskType::Task,
            None,
            now,
            now,
            0,
        )
        .map_err(|e| format!("Failed to create review task: {}", e))?;

    info!("Created review task with ID: {}", review_task_id);

    // Create drafting task
    let drafting_task_id = task_manager
        .add_task(
            job_id.to_string(),
            format!("drafting_{}", Utc::now().timestamp_millis()),
            Some(task_id.to_string()),
            Some("Create final report".to_string()),
            "Draft final report".to_string(),
            "drafting_agent".to_string(),
            vec![review_task_id.clone()],
            json!({"instruction": "Create a final report of school options"}),
            None,
            3,
            TaskType::Task,
            None,
            now,
            now,
            0,
        )
        .map_err(|e| format!("Failed to create drafting task: {}", e))?;

    info!("Created drafting task with ID: {}", drafting_task_id);

    // Return the plan - make sure we return exactly what's in the output_schema
    // The task_agent macro expects the output to match the output_schema exactly
    Ok(json!({
        "plan": plan
    }))
}

#[task_agent(
    name = "retrieval_agent",
    description = "Retrieves information for a story",
    input_schema = r#"{"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}"#,
    output_schema = r#"{"type": "object", "properties": {"information": {"type": "string"}}, "required": ["information"]}"#
)]
async fn retrieval_agent(
    input: Value,
    task_id: &str,
    job_id: &str,
    task_manager: &TaskManager,
) -> Result<Value, String> {
    // Extract the query
    let query = input["query"].as_str().unwrap_or("No query provided");
    info!("Retrieval agent processing query: {}", query);

    // Simulate retrieval work
    tokio::time::sleep(Duration::from_millis(700)).await;

    // Return a successful output with mock information
    let information = format!(
        "Found 5 top-rated schools in the area: 
1. Lincoln High School (Rating: 9.2/10)
2. Washington Academy (Rating: 9.0/10)
3. Jefferson STEM School (Rating: 8.8/10)
4. Roosevelt Preparatory (Rating: 8.7/10)
5. Franklin Arts Academy (Rating: 8.5/10)"
    );

    Ok(json!({
        "information": information
    }))
}

#[allow(unused_must_use)]
#[task_agent(
    name = "review_agent",
    description = "Reviews and summarizes information",
    input_schema = r#"{"type": "object", "properties": {"instruction": {"type": "string"}}, "required": ["instruction"]}"#,
    output_schema = r#"{"type": "object", "properties": {"summary": {"type": "string"}}, "required": ["summary"]}"#
)]
async fn review_agent(
    input: Value,
    task_id: &str,
    job_id: &str,
    task_manager: &TaskManager,
) -> Result<Value, String> {
    // Extract the instruction
    let instruction = input["instruction"]
        .as_str()
        .unwrap_or("No instruction provided");
    info!("Review agent processing instruction: {}", instruction);

    // Get the task to find dependencies
    let task = match task_manager.get_task_by_id(task_id) {
        Some(t) => t,
        None => return Err(format!("Task not found: {}", task_id)),
    };

    // Get dependency information
    let mut dependency_info = String::new();
    for dep_id in &task.dependencies {
        if let Some(dep_task) = task_manager.get_task_by_id(dep_id) {
            if let Some(data) = &dep_task.output.data {
                if let Some(info) = data.get("information") {
                    dependency_info = info.as_str().unwrap_or("").to_string();
                    break;
                }
            }
        }
    }

    if dependency_info.is_empty() {
        dependency_info = "No information found from dependencies".to_string();
    }

    // Simulate review work
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Create a summary
    let summary = format!("After reviewing the school information, I recommend focusing on Lincoln High School and Washington Academy due to their excellent academic programs and high ratings. Jefferson STEM School is also worth considering if your child has interest in science and technology.");

    // Return a summary
    Ok(json!({
        "summary": summary
    }))
}

#[task_agent(
    name = "drafting_agent",
    description = "Creates a final report",
    input_schema = r#"{"type": "object", "properties": {"instruction": {"type": "string"}}, "required": ["instruction"]}"#,
    output_schema = r#"{"type": "object", "properties": {"report": {"type": "string"}}, "required": ["report"]}"#
)]
async fn drafting_agent(
    input: Value,
    task_id: &str,
    job_id: &str,
    task_manager: &TaskManager,
) -> Result<Value, String> {
    // Extract the instruction
    let instruction = input["instruction"]
        .as_str()
        .unwrap_or("No instruction provided");
    info!("Drafting agent processing instruction: {}", instruction);

    // Get the task to find dependencies
    let task = match task_manager.get_task_by_id(task_id) {
        Some(t) => t,
        None => return Err(format!("Task not found: {}", task_id)),
    };

    // Get dependency information
    let mut summary = String::new();
    for dep_id in &task.dependencies {
        if let Some(dep_task) = task_manager.get_task_by_id(dep_id) {
            if let Some(data) = &dep_task.output.data {
                if let Some(sum) = data.get("summary") {
                    summary = sum.as_str().unwrap_or("").to_string();
                    break;
                }
            }
        }
    }

    if summary.is_empty() {
        summary = "No summary found from dependencies".to_string();
    }

    // Simulate drafting work
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Create the final report
    let report = format!("# School Selection Report\n\n## Executive Summary\n{}\n\n## Next Steps\n1. Schedule visits to Lincoln High School and Washington Academy\n2. Prepare questions for school administrators\n3. Review admission requirements and deadlines\n4. Consider your child's preferences and needs\n\n## Conclusion\nBased on our research, these schools offer excellent educational opportunities. The final decision should balance academic quality with your child's specific interests and needs.", summary);

    // Return the final report
    Ok(json!({
        "report": report
    }))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    // Set up tracing for debugging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    info!("Starting TaskAgent Workflow Example");

    // Initialize core components
    let cache = Cache::new();

    let agent_registry = TaskAgentRegistry::new();

    // Load all agents from the TASK_AGENTS slice
    for agent_factory in taskagent::TASK_AGENTS {
        let agent = agent_factory();
        let name = agent.name();
        agent_registry.register(&name, agent)?;
    }
    let task_manager = TaskManager::new(
        Duration::from_secs(30),   // Heartbeat interval
        StallAction::TerminateJob, // Terminate job if stalled for 30 seconds
        cache,
        agent_registry.clone(),
        Some(PathBuf::from("example_state.db")), // Deprecated Sled persistence - use task-core system
    );

    // Define TaskConfiguration with a 30-second human timeout and max execution time
    let config = TaskConfiguration {
        max_execution_time: Some(Duration::from_secs(60)), // Overall job timeout
        retry_strategy: RetryStrategy::FixedRetry(3),
        human_timeout_action: HumanTimeoutAction::TimeoutAfter(Duration::from_secs(30)),
        sled_db_path: Some(PathBuf::from("example_state.db")), // Deprecated - use task-core system
    };

    // Create a job ID
    let job_id = "school_selection_job".to_string();

    // Create TaskExecutor
    let mut executor = TaskExecutor::new(
        Arc::new(task_manager.clone()),
        Arc::new(agent_registry),
        Arc::new(Cache::new()),
        config,
        job_id.clone(),
        None, // No agent restrictions
    );

    // Add the initial objective task
    let objective_id = task_manager.add_objective(
        job_id.clone(),
        "Plan school selection process".to_string(),
        "planning_agent".to_string(),
        json!({"objective": "Select the best school for my child based on ratings, programs, and admission requirements"})
    )?;

    info!("Added objective task with ID: {}", objective_id);

    // Set up cancellation channel
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

    // Execute the job
    let execution_result = executor.execute(job_id.clone(), vec![], cancel_rx).await;

    match execution_result {
        Ok(report) => {
            info!("Job completed successfully: {:?}", report.overall_success);
            for outcome in report.outcomes {
                info!(
                    "Task {}: Success={}, Error={:?}",
                    outcome.task_id, outcome.success, outcome.error
                );
            }
        }
        Err(e) => {
            info!("Job execution failed: {}", e);
        }
    }

    // Persist the final state
    executor.persist_state()?;
    info!("Persisted job state to Sled");

    // Generate and print the DOT graph
    let dot_graph = executor.generate_detailed_dot_graph();
    println!("=== DOT Graph Visualization ===\n{}", dot_graph);
    info!("DOT graph generated and printed");

    // Check final job status
    let job_status = task_manager.get_job_status(&job_id).await?;
    info!("Final job status: {:?}", job_status);

    Ok(())
}
