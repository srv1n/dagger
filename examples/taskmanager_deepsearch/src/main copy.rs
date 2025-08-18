use dagger::{
    taskagent::{
        taskagent, Cache, HumanTimeoutAction, JobStatus, RetryStrategy, StallAction, Task,
        TaskAgentRegistry, TaskConfiguration, TaskManager, TaskOutput, TaskStatus, TaskType,
    },
    TaskExecutor,
};
use dagger_macros::task_agent;
use anyhow::{anyhow, Result};
use chrono::Utc;
use async_openai::{
    Client,
   
};
use serde_json::{json, Value};
use std::{env, path::PathBuf, sync::Arc, time::Duration};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

// OpenAI Client Setup
async fn get_openai_client() -> Result<Client> {
    let api_key = env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    Ok(Client::new(&api_key))
}

// Planning Agent: Breaks down the question into smaller tasks
#[task_agent(
    name = "planning_agent",
    description = "Breaks down a question into smaller tasks",
    input_schema = r#"{"type": "object", "properties": {"question": {"type": "string"}}, "required": ["question"]}"#,
    output_schema = r#"{"type": "object", "properties": {"tasks": {"type": "array", "items": {"type": "string"}}}, "required": ["tasks"]}"#
)]
async fn planning_agent(
    input: Value,
    task_id: &str,
    job_id: &str,
    task_manager: &TaskManager,
) -> Result<Value, String> {
    let question = input["question"]
        .as_str()
        .ok_or("No question provided")?;
    info!("Planning agent processing question: {}", question);

    let client = get_openai_client()
        .await
        .map_err(|e| format!("Failed to initialize OpenAI client: {}", e))?;

    let prompt = format!(
        "Given the question '{}', break it down into 3-10 smaller, actionable tasks for research and analysis. Provide the tasks as a JSON array of strings.",
        question
    );

    let messages = vec![ChatCompletionMessage {
        role: ChatCompletionMessageRole::User,
        content: prompt,
        name: None,
    }];

    let chat_completion = ChatCompletion::builder("gpt-4o", messages)
        .create(&client)
        .await
        .map_err(|e| format!("OpenAI API error: {}", e))?;

    let response = chat_completion
        .choices
        .first()
        .ok_or("No response from OpenAI")?
        .message
        .content
        .as_ref()
        .ok_or("No content in OpenAI response")?;

    let tasks: Vec<String> = serde_json::from_str(response)
        .map_err(|e| format!("Failed to parse OpenAI response: {}", e))?;

    // Create search tasks for each identified task
    let now = Utc::now().naive_utc();
    let mut search_task_ids = Vec::new();

    for (i, task_desc) in tasks.iter().enumerate() {
        let search_task_id = task_manager
            .add_task(
                job_id.to_string(),
                format!("search_{}_{}", task_id, i),
                Some(task_id.to_string()),
                Some("Gather relevant information".to_string()),
                task_desc.to_string(),
                "search_agent".to_string(),
                vec![],
                json!({"query": task_desc}),
                None,
                3,
                TaskType::Task,
                None,
                now,
                now,
                0,
            )
            .map_err(|e| format!("Failed to create search task: {}", e))?;
        search_task_ids.push(search_task_id);
    }

    // Block this task until all search tasks are complete
    for search_task_id in &search_task_ids {
        task_manager
            .add_dependency(task_id, search_task_id)
            .map_err(|e| format!("Failed to add dependency: {}", e))?;
    }
    task_manager
        .update_task_status(task_id, TaskStatus::Blocked)
        .map_err(|e| format!("Failed to block planning task: {}", e))?;

    Ok(json!({"tasks": tasks}))
}

// Search Agent: Simulates searching for information using OpenAI
#[task_agent(
    name = "search_agent",
    description = "Searches for information based on a query",
    input_schema = r#"{"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}"#,
    output_schema = r#"{"type": "object", "properties": {"information": {"type": "string"}}, "required": ["information"]}"#
)]
async fn search_agent(
    input: Value,
    task_id: &str,
    _job_id: &str,
    task_manager: &TaskManager,
) -> Result<Value, String> {
    let query = input["query"].as_str().ok_or("No query provided")?;
    info!("Search agent processing query: {}", query);

    let client = get_openai_client()
        .await
        .map_err(|e| format!("Failed to initialize OpenAI client: {}", e))?;

    let prompt = format!(
        "Provide a concise summary (100-200 words) of information relevant to the query '{}'.",
        query
    );

    let messages = vec![ChatCompletionMessage {
        role: ChatCompletionMessageRole::User,
        content: prompt,
        name: None,
    }];

    let chat_completion = ChatCompletion::builder("gpt-4o", messages)
        .create(&client)
        .await
        .map_err(|e| format!("OpenAI API error: {}", e))?;

    let information = chat_completion
        .choices
        .first()
        .ok_or("No response from OpenAI")?
        .message
        .content
        .as_ref()
        .ok_or("No content in OpenAI response")?
        .to_string();

    Ok(json!({"information": information}))
}

// Review Agent: Reviews search results and prepares a summary
#[task_agent(
    name = "review_agent",
    description = "Reviews and summarizes search results",
    input_schema = r#"{"type": "object", "properties": {"question": {"type": "string"}}, "required": ["question"]}"#,
    output_schema = r#"{"type": "object", "properties": {"summary": {"type": "string"}}, "required": ["summary"]}"#
)]
async fn review_agent(
    input: Value,
    task_id: &str,
    job_id: &str,
    task_manager: &TaskManager,
) -> Result<Value, String> {
    let question = input["question"]
        .as_str()
        .ok_or("No question provided")?;
    info!("Review agent processing question: {}", question);

    // Get the planning task (parent)
    let task = task_manager
        .get_task_by_id(task_id)
        .ok_or("Task not found")?;

    // Collect all search results from dependencies
    let search_results = task_manager
        .get_dependency_outputs(task_id)
        .map_err(|e| format!("Failed to get dependency outputs: {}", e))?;

    let mut information = Vec::new();
    for result in search_results {
        if let Some(info) = result.get("information") {
            information.push(info.as_str().unwrap_or("").to_string());
        }
    }

    let combined_info = information.join("\n\n");

    let client = get_openai_client()
        .await
        .map_err(|e| format!("Failed to initialize OpenAI client: {}", e))?;

    let prompt = format!(
        "Given the question '{}', review the following information and provide a concise summary (150-250 words):\n\n{}",
        question, combined_info
    );

    let messages = vec![ChatCompletionMessage {
        role: ChatCompletionMessageRole::User,
        content: prompt,
        name: None,
    }];

    let chat_completion = ChatCompletion::builder("gpt-4o", messages)
        .create(&client)
        .await
        .map_err(|e| format!("OpenAI API error: {}", e))?;

    let summary = chat_completion
        .choices
        .first()
        .ok_or("No response from OpenAI")?
        .message
        .content
        .as_ref()
        .ok_or("No content in OpenAI response")?
        .to_string();

    // Once reviewed, create a drafting task
    let drafting_task_id = task_manager
        .add_task(
            job_id.to_string(),
            format!("drafting_{}", Utc::now().timestamp_millis()),
            Some(task_id.to_string()),
            Some("Draft final answer".to_string()),
            "Draft final answer".to_string(),
            "drafting_agent".to_string(),
            vec![task_id.to_string()],
            json!({"summary": &summary, "question": question}),
            None,
            3,
            TaskType::Task,
            None,
            Utc::now().naive_utc(),
            Utc::now().naive_utc(),
            0,
        )
        .map_err(|e| format!("Failed to create drafting task: {}", e))?;

    Ok(json!({"summary": summary}))
}

// Drafting Agent: Writes the final answer
#[task_agent(
    name = "drafting_agent",
    description = "Drafts the final answer based on reviewed information",
    input_schema = r#"{"type": "object", "properties": {"summary": {"type": "string"}, "question": {"type": "string"}}, "required": ["summary", "question"]}"#,
    output_schema = r#"{"type": "object", "properties": {"answer": {"type": "string"}}, "required": ["answer"]}"#
)]
async fn drafting_agent(
    input: Value,
    task_id: &str,
    _job_id: &str,
    task_manager: &TaskManager,
) -> Result<Value, String> {
    let summary = input["summary"]
        .as_str()
        .ok_or("No summary provided")?;
    let question = input["question"]
        .as_str()
        .ok_or("No question provided")?;
    info!("Drafting agent processing question: {}", question);

    let client = get_openai_client()
        .await
        .map_err(|e| format!("Failed to initialize OpenAI client: {}", e))?;

    let prompt = format!(
        "Given the question '{}' and the following summary, draft a final answer (200-300 words):\n\n{}",
        question, summary
    );

    let messages = vec![ChatCompletionMessage {
        role: ChatCompletionMessageRole::User,
        content: prompt,
        name: None,
    }];

    let chat_completion = ChatCompletion::builder("gpt-4o", messages)
        .create(&client)
        .await
        .map_err(|e| format!("OpenAI API error: {}", e))?;

    let answer = chat_completion
        .choices
        .first()
        .ok_or("No response from OpenAI")?
        .message
        .content
        .as_ref()
        .ok_or("No content in OpenAI response")?
        .to_string();

    Ok(json!({"answer": answer}))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    // Set up tracing for debugging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    info!("Starting TaskAgent Workflow with OpenAI Integration");

    // Initialize core components
    let cache = Cache::new();
    let agent_registry = TaskAgentRegistry::new();

    // Register all agents
    for agent_factory in dagger::taskagent::TASK_AGENTS {
        let agent = agent_factory();
        let name = agent.name();
        agent_registry.register(&name, agent)?;
    }

    let task_manager = TaskManager::new(
        Duration::from_secs(30),
        StallAction::TerminateJob,
        cache,
        agent_registry.clone(),
        Some(PathBuf::from("question_workflow.db")),
    );

    let config = TaskConfiguration {
        max_execution_time: Some(Duration::from_secs(300)),
        retry_strategy: RetryStrategy::FixedRetry(3),
        human_timeout_action: HumanTimeoutAction::TimeoutAfter(Duration::from_secs(30)),
        sled_db_path: Some(PathBuf::from("question_workflow.db")), // Deprecated - use newer task-core system
    };

    let job_id = "question_workflow".to_string();

    let mut executor = TaskExecutor::new(
        Arc::new(task_manager.clone()),
        Arc::new(agent_registry),
        Arc::new(Cache::new()),
        config,
        job_id.clone(),
        None,
    );

    // Example question
    let question = "What are the best strategies for improving productivity in a remote work environment?";
    let objective_id = task_manager.add_objective(
        job_id.clone(),
        "Answer user question".to_string(),
        "planning_agent".to_string(),
        json!({"question": question}),
    )?;

    info!("Added objective task with ID: {}", objective_id);

    let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

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

            // Retrieve and print the final answer
            if let Some(task) = task_manager.get_task_by_id(&objective_id) {
                if let Some(drafting_task) = task_manager.get_child_tasks(&objective_id).iter().find(|t| t.agent == "drafting_agent") {
                    if let Some(data) = &drafting_task.output.data {
                        if let Some(answer) = data.get("answer") {
                            println!("Final Answer:\n{}", answer.as_str().unwrap_or("No answer generated"));
                        }
                    }
                }
            }
        }
        Err(e) => {
            info!("Job execution failed: {}", e);
        }
    }

    // Persist state and generate DOT graph
    executor.persist_state()?;
    let dot_graph = executor.generate_detailed_dot_graph();
    println!("=== DOT Graph Visualization ===\n{}", dot_graph);

    let job_status = task_manager.get_job_status(&job_id).await?;
    info!("Final job status: {:?}", job_status);

    Ok(())
}