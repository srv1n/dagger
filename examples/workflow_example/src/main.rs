use anyhow::Result;
use dagger::taskagent::{TaskSystemBuilder, TaskSystem};
use dagger::task_agent;
use serde_json::{json, Value};
use tokio::time::Duration;

/// Agent that generates a plan
#[task_agent(
    name = "planner_agent", 
    description = "Creates a plan based on an objective",
    input_schema = r#"{"type": "object", "properties": {"objective": {"type": "string"}}, "required": ["objective"]}"#,
    output_schema = r#"{"type": "object", "properties": {"plan": {"type": "string"}}, "required": ["plan"]}"#
)]
async fn planner_agent(input: Value, task_id: &str, job_id: &str) -> Result<Value, String> {
    // Extract the objective
    let objective = input["objective"].as_str().unwrap_or("No objective provided");
    println!("Planning for objective: {}", objective);
    
    // Simulate planning work
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    // Return a simple plan
    Ok(json!({
        "plan": format!("1. Research {}\n2. Analyze findings\n3. Prepare report", objective)
    }))
}

/// Agent that performs research
#[task_agent(
    name = "research_agent", 
    description = "Researches a topic from a plan",
    input_schema = r#"{"type": "object", "properties": {"plan": {"type": "string"}, "topic": {"type": "string"}}, "required": ["plan", "topic"]}"#,
    output_schema = r#"{"type": "object", "properties": {"findings": {"type": "string"}}, "required": ["findings"]}"#
)]
async fn research_agent(input: Value, task_id: &str, job_id: &str) -> Result<Value, String> {
    // Extract inputs
    let plan = input["plan"].as_str().unwrap_or("No plan provided");
    let topic = input["topic"].as_str().unwrap_or("No topic provided");
    
    println!("Researching topic: {} according to plan: {}", topic, plan);
    
    // Simulate research work
    tokio::time::sleep(Duration::from_millis(700)).await;
    
    // Return findings
    Ok(json!({
        "findings": format!("Found important information about {}: Lorem ipsum dolor sit amet", topic)
    }))
}

/// Agent that prepares a final report
#[task_agent(
    name = "report_agent", 
    description = "Prepares a final report based on research findings",
    input_schema = r#"{"type": "object", "properties": {"findings": {"type": "string"}}, "required": ["findings"]}"#,
    output_schema = r#"{"type": "object", "properties": {"report": {"type": "string"}}, "required": ["report"]}"#
)]
async fn report_agent(input: Value, task_id: &str, job_id: &str) -> Result<Value, String> {
    // Extract findings
    let findings = input["findings"].as_str().unwrap_or("No findings provided");
    
    println!("Preparing report based on: {}", findings);
    
    // Simulate report preparation
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    // Return the final report
    Ok(json!({
        "report": format!("FINAL REPORT\n\nExecutive Summary:\n{}\n\nConclusion: Task completed successfully.", findings)
    }))
}

/// Set up a workflow with multiple dependent tasks
async fn setup_workflow(task_system: &TaskSystem) -> Result<()> {
    // The objective is the starting point
    let objective_id = task_system.task_manager.add_objective(
        task_system.job_id.clone(),
        "Research renewable energy sources".to_string(),
        "planner_agent".to_string(),
        json!({"objective": "renewable energy sources"}),
    )?;
    
    // Mark the objective as ready to start the workflow
    task_system.task_manager.ready_tasks.insert(objective_id.clone());
    
    // Add research task that depends on the planning task
    let research_id = task_system.add_task(
        "Research renewable energy sources".to_string(),
        "research_agent".to_string(),
        vec![objective_id.clone()], // Depends on the objective/planning task
        json!({
            "topic": "renewable energy sources",
            "plan": "Will be populated from dependency output"
        }),
    )?;
    
    // Add report task that depends on the research task
    let report_id = task_system.add_task(
        "Create final report".to_string(),
        "report_agent".to_string(),
        vec![research_id.clone()], // Depends on the research task
        json!({
            "findings": "Will be populated from dependency output"
        }),
    )?;
    
    println!("Workflow set up with tasks: \nObjective: {}\nResearch: {}\nReport: {}", 
        objective_id, research_id, report_id);
    
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Set up logging
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set default tracing subscriber");
    
    println!("Creating task system with multiple agents...");
    
    // Create a task system with multiple agents
    let mut task_system = TaskSystemBuilder::new()
        .register_agents(&["planner_agent", "research_agent", "report_agent"])?
        .build()?;
    
    // Set up the workflow with dependencies
    setup_workflow(&task_system).await?;
    
    // Get the objective task to start execution
    let objective_id = task_system.task_manager
        .get_tasks_by_type(&task_system.job_id, dagger::taskagent::TaskType::Objective)
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No objective task found"))?;
    
    // Execute the workflow starting with the objective
    let initial_tasks = vec![objective_id];
    
    // Create a cancellation channel
    let (_, cancel_rx) = tokio::sync::oneshot::channel();
    
    println!("Starting workflow execution...");
    
    // Execute the workflow
    let report = task_system.executor.execute(
        task_system.job_id.clone(), 
        initial_tasks, 
        cancel_rx
    ).await?;
    
    // Print execution results
    println!("\nWorkflow completed with success: {}", report.overall_success);
    println!("Total tasks completed: {}", report.outcomes.len());
    
    // Print the DOT graph for visualization
    let dot_graph = task_system.executor.generate_detailed_dot_graph();
    println!("\nWorkflow Graph:\n{}", dot_graph);
    
    Ok(())
} 