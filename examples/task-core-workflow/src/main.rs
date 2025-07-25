use task_core::{
    TaskSystem, TaskSystemBuilder, TaskConfig,
    model::{NewTaskSpec, TaskId, JobId, AgentId, Durability, TaskType, AgentError},
    executor::{Agent, TaskContext, AgentRegistry},
};
use dagger_macros::task_agent;
use task_core::AGENTS;
use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug)]
struct ResearchRequest {
    topic: String,
    depth: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct ResearchResult {
    topic: String,
    findings: Vec<String>,
    sources: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct AnalysisResult {
    summary: String,
    key_points: Vec<String>,
    confidence: f64,
}

#[derive(Serialize, Deserialize, Debug)]
struct Report {
    title: String,
    executive_summary: String,
    sections: Vec<String>,
    conclusion: String,
}

/// Planning agent - creates subtasks for research workflow
#[task_agent(
    name = "planner",
    description = "Plans research workflow by creating subtasks"
)]
async fn planner(input: Bytes, ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    let request: ResearchRequest = rmp_serde::from_slice(&input)
        .map_err(|e| AgentError::User(format!("Failed to parse request: {}", e)))?;
    
    info!("Planning research for topic: {}", request.topic);
    
    // Create research task
    let research_spec = NewTaskSpec {
        agent: researcherAgent::AGENT_ID,
        input: input.clone(),
        dependencies: vec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from(format!("Research: {}", request.topic)),
        timeout: Some(Duration::from_secs(30)),
        max_retries: Some(3),
        parent: ctx.parent,
    };
    
    let research_id = ctx.handle.spawn_task(research_spec).await
        .map_err(|e| AgentError::System(e.into()))?;
    
    info!("Created research task: {}", research_id);
    
    // Create analysis task that depends on research
    let analysis_spec = NewTaskSpec {
        agent: analyzerAgent::AGENT_ID,
        input: Bytes::from(format!("{}", research_id)), // Pass task ID to fetch results
        dependencies: vec![research_id],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Analyze research findings"),
        timeout: Some(Duration::from_secs(20)),
        max_retries: Some(2),
        parent: ctx.parent,
    };
    
    let analysis_id = ctx.handle.spawn_task(analysis_spec).await
        .map_err(|e| AgentError::System(e.into()))?;
    
    info!("Created analysis task: {}", analysis_id);
    
    // Create report task that depends on both
    let report_spec = NewTaskSpec {
        agent: reporterAgent::AGENT_ID,
        input: Bytes::from(format!("{},{}", research_id, analysis_id)),
        dependencies: vec![research_id, analysis_id],
        durability: Durability::AtMostOnce, // Don't duplicate reports
        task_type: TaskType::Task,
        description: Arc::from("Generate final report"),
        timeout: Some(Duration::from_secs(15)),
        max_retries: Some(1),
        parent: ctx.parent,
    };
    
    let report_id = ctx.handle.spawn_task(report_spec).await
        .map_err(|e| AgentError::System(e.into()))?;
    
    info!("Created report task: {}", report_id);
    
    // Store task IDs in shared state for monitoring
    ctx.shared.put("workflow", &request.topic, &format!("{},{},{}", research_id, analysis_id, report_id).as_bytes())
        .map_err(|e| AgentError::System(e.into()))?;
    
    // Return plan summary
    let plan = serde_json::json!({
        "status": "planned",
        "tasks": {
            "research": research_id,
            "analysis": analysis_id,
            "report": report_id
        }
    });
    
    Ok(Bytes::from(serde_json::to_vec(&plan).unwrap()))
}

/// Researcher agent - gathers information
#[task_agent(
    name = "researcher",
    description = "Performs research on given topics"
)]
async fn researcher(input: Bytes, ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    let request: ResearchRequest = rmp_serde::from_slice(&input)
        .map_err(|e| AgentError::User(format!("Failed to parse request: {}", e)))?;
    
    info!("Researching topic: {} (depth: {})", request.topic, request.depth);
    
    // Simulate research work
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    // Generate mock research results
    let result = ResearchResult {
        topic: request.topic.clone(),
        findings: vec![
            format!("{} is a complex topic with many facets", request.topic),
            format!("Recent developments in {} show promising trends", request.topic),
            format!("Experts predict {} will be significant in coming years", request.topic),
        ],
        sources: vec![
            "Academic Journal 2024".to_string(),
            "Industry Report Q3".to_string(),
            "Expert Interview Series".to_string(),
        ],
    };
    
    info!("Research complete: found {} findings", result.findings.len());
    
    Ok(Bytes::from(rmp_serde::to_vec(&result).unwrap()))
}

/// Analyzer agent - analyzes research findings
#[task_agent(
    name = "analyzer",
    description = "Analyzes research results"
)]
async fn analyzer(input: Bytes, ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    let research_id = String::from_utf8(input.to_vec())
        .map_err(|e| AgentError::User(format!("Invalid task ID: {}", e)))?
        .parse::<u64>()
        .map_err(|e| AgentError::User(format!("Invalid task ID format: {}", e)))?;
    
    info!("Analyzing results from research task: {}", research_id);
    
    // Get research output
    let research_output = ctx.dependency_output(research_id).await
        .map_err(|e| AgentError::System(e.into()))?
        .ok_or_else(|| AgentError::User("Research output not found".to_string()))?;
    
    let research: ResearchResult = rmp_serde::from_slice(&research_output)
        .map_err(|e| AgentError::User(format!("Failed to parse research: {}", e)))?;
    
    // Simulate analysis
    tokio::time::sleep(Duration::from_millis(300)).await;
    
    let analysis = AnalysisResult {
        summary: format!("Analysis of '{}' reveals significant insights", research.topic),
        key_points: vec![
            "Strong correlation between factors".to_string(),
            "Emerging patterns identified".to_string(),
            "Future implications are substantial".to_string(),
        ],
        confidence: 0.85,
    };
    
    info!("Analysis complete with {:.0}% confidence", analysis.confidence * 100.0);
    
    Ok(Bytes::from(rmp_serde::to_vec(&analysis).unwrap()))
}

/// Reporter agent - generates final report
#[task_agent(
    name = "reporter",
    description = "Generates comprehensive reports"
)]
async fn reporter(input: Bytes, ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    let task_ids = String::from_utf8(input.to_vec())
        .map_err(|e| AgentError::User(format!("Invalid input: {}", e)))?;
    
    let ids: Vec<u64> = task_ids.split(',')
        .map(|s| s.parse::<u64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AgentError::User(format!("Invalid task IDs: {}", e)))?;
    
    if ids.len() != 2 {
        return Err(AgentError::User("Expected research_id,analysis_id".to_string()));
    }
    
    info!("Generating report from tasks: {:?}", ids);
    
    // Get outputs from dependencies
    let research_output = ctx.dependency_output(ids[0]).await
        .map_err(|e| AgentError::System(e.into()))?
        .ok_or_else(|| AgentError::User("Research output not found".to_string()))?;
    
    let analysis_output = ctx.dependency_output(ids[1]).await
        .map_err(|e| AgentError::System(e.into()))?
        .ok_or_else(|| AgentError::User("Analysis output not found".to_string()))?;
    
    let research: ResearchResult = rmp_serde::from_slice(&research_output)
        .map_err(|e| AgentError::User(format!("Failed to parse research: {}", e)))?;
    
    let analysis: AnalysisResult = rmp_serde::from_slice(&analysis_output)
        .map_err(|e| AgentError::User(format!("Failed to parse analysis: {}", e)))?;
    
    // Generate report
    let report = Report {
        title: format!("Comprehensive Report: {}", research.topic),
        executive_summary: analysis.summary.clone(),
        sections: vec![
            format!("## Research Findings\n\n{}", research.findings.join("\n- ")),
            format!("## Analysis\n\n{}", analysis.key_points.join("\n- ")),
            format!("## Sources\n\n{}", research.sources.join("\n- ")),
        ],
        conclusion: format!(
            "This report on {} provides comprehensive insights with {:.0}% confidence.",
            research.topic, analysis.confidence * 100.0
        ),
    };
    
    info!("Report generated: '{}'", report.title);
    
    // Store report in shared state
    let report_json = serde_json::to_string_pretty(&report).unwrap();
    ctx.shared.put("reports", &research.topic, report_json.as_bytes())
        .map_err(|e| AgentError::System(e.into()))?;
    
    Ok(Bytes::from(rmp_serde::to_vec(&report).unwrap()))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Setup logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_env_filter("task_core=info,task_core_workflow=info")
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    
    info!("Starting multi-agent workflow example");
    
    // Create configuration - use more workers for parallel execution
    let mut config = TaskConfig::default();
    config.max_parallel = 6;
    config.queue_capacity = 100;
    
    // Build task system
    let mut system_builder = TaskSystemBuilder::new()
        .with_storage_path("workflow_tasks.db")
        .with_config(config);
    
    // Register all agents
    let mut registry = AgentRegistry::new();
    for register_fn in task_core::AGENTS {
        register_fn(&mut registry);
    }
    
    // Build and start system
    let system = Arc::new(system_builder.build(Arc::new(registry)).await?);
    let system_clone = system.clone();
    
    // Start the system
    let handle = tokio::spawn(async move {
        system_clone.run().await
    });
    
    // Wait for system to start
    tokio::time::sleep(Duration::from_millis(200)).await;
    
    // Create research workflow
    info!("\n=== Starting Research Workflow ===");
    
    let research_request = ResearchRequest {
        topic: "Quantum Computing Applications".to_string(),
        depth: "comprehensive".to_string(),
    };
    
    let planning_task = NewTaskSpec {
        agent: plannerAgent::AGENT_ID,
        input: Bytes::from(rmp_serde::to_vec(&research_request)?),
        dependencies: vec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Objective,
        description: Arc::from("Plan quantum computing research"),
        timeout: Some(Duration::from_secs(10)),
        max_retries: Some(2),
        parent: None,
    };
    
    let job_id = 1; // In real system, this would be generated
    let planning_id = system.submit_task(planning_task).await?;
    info!("Submitted planning task: {} for job: {}", planning_id, job_id);
    
    // Monitor progress
    let mut last_status = String::new();
    for _ in 0..30 { // Check for up to 30 seconds
        tokio::time::sleep(Duration::from_secs(1)).await;
        
        // Check workflow status
        if let Ok(Some(task_ids_bytes)) = system.shared_state.get("workflow", &research_request.topic).await {
            let task_ids_str = String::from_utf8(task_ids_bytes.to_vec())?;
            let status_parts: Vec<&str> = task_ids_str.split(',').collect();
            
            if status_parts.len() == 3 {
                let mut statuses = vec![];
                for (i, id_str) in status_parts.iter().enumerate() {
                    if let Ok(task_id) = id_str.parse::<u64>() {
                        if let Ok(Some(task)) = system.get_task(task_id).await {
                            let task_type = match i {
                                0 => "Research",
                                1 => "Analysis",
                                2 => "Report",
                                _ => "Unknown"
                            };
                            statuses.push(format!("{}: {:?}", task_type, task.status));
                        }
                    }
                }
                
                let current_status = statuses.join(", ");
                if current_status != last_status {
                    info!("Workflow progress: {}", current_status);
                    last_status = current_status;
                }
                
                // Check if all completed
                if statuses.iter().all(|s| s.contains("Completed")) {
                    info!("Workflow completed!");
                    break;
                }
            }
        }
    }
    
    // Retrieve final report
    if let Ok(Some(report_bytes)) = system.shared_state.get("reports", &research_request.topic).await {
        let report_json = String::from_utf8(report_bytes.to_vec())?;
        info!("\n=== Final Report ===\n{}", report_json);
    }
    
    // Show statistics
    info!("\n=== System Statistics ===");
    let scheduler_stats = system.scheduler_stats().await?;
    info!("Total tasks: {}", scheduler_stats.total_tasks);
    info!("Ready tasks: {}", scheduler_stats.ready_tasks);
    info!("Blocked tasks: {}", scheduler_stats.blocked_tasks);
    
    // Example of crash recovery
    info!("\n=== Testing Crash Recovery ===");
    
    // Submit a long-running task
    let long_task = NewTaskSpec {
        agent: researcherAgent::AGENT_ID,
        input: Bytes::from(rmp_serde::to_vec(&ResearchRequest {
            topic: "Test Recovery".to_string(),
            depth: "quick".to_string(),
        })?),
        dependencies: vec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("Test recovery task"),
        timeout: Some(Duration::from_secs(60)),
        max_retries: Some(1),
        parent: None,
    };
    
    let recovery_task_id = system.submit_task(long_task).await?;
    info!("Submitted recovery test task: {}", recovery_task_id);
    
    // Wait a bit then "crash"
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    // Shutdown abruptly
    info!("Simulating crash...");
    handle.abort();
    drop(system);
    
    // Restart system
    info!("Restarting system...");
    let system2 = Arc::new(system_builder.build(Arc::new(registry)).await?);
    
    // Check recovery stats
    let recovery_stats = system2.recovery_stats().await?;
    info!("Recovery stats: {:?}", recovery_stats);
    
    // Clean shutdown
    info!("\nClean shutdown...");
    system2.shutdown().await?;
    
    Ok(())
}