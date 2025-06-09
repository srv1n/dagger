// Core infrastructure modules
pub mod core {
    pub mod errors;
    pub mod memory;
    pub mod limits;
}

// Three main execution paradigms
pub mod dag_flow;    // DAG-based workflow execution
pub mod taskagent;   // Task-based agent execution
pub mod pubsub;      // Pub/Sub messaging execution

// Re-exports for convenience
pub use core::errors::{DaggerError, Result};
// Export the original Cache for backward compatibility
pub use dag_flow::{Cache, insert_value};
// Export the enhanced cache with a different name  
pub use core::memory::Cache as EnhancedCache;
pub use core::limits::{ResourceLimits, ResourceTracker};

// Re-export existing modules for backward compatibility
pub use dag_flow::*;
pub use taskagent::*;

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio;
    use async_trait::async_trait;

    struct ExampleAgent;

    #[async_trait]
    impl taskagent::TaskAgent for ExampleAgent {
        fn name(&self) -> String {
            "example".to_string()
        }

        fn input_schema(&self) -> serde_json::Value {
            json!({
                "type": "object",
                "properties": {
                    "message": {"type": "string"}
                },
                "required": ["message"]
            })
        }

        fn output_schema(&self) -> serde_json::Value {
            json!({
                "type": "object",
                "properties": {
                    "response": {"type": "string"}
                },
                "required": ["response"]
            })
        }

        async fn execute(
            &self,
            task_id: &str,
            input: serde_json::Value,
            task_manager: &taskagent::TaskManager,
        ) -> Result<taskagent::TaskOutput> {
            let message = input["message"].as_str().unwrap_or("No message");
            let response = format!("Processed: {}", message);
            
            // Store in cache for testing
            task_manager.cache.insert_value(task_id, "test_key", &response)?;
            
            Ok(taskagent::TaskOutput {
                success: true,
                data: Some(json!({"response": response})),
                error: None,
            })
        }
    }

    #[tokio::test]
    async fn test_task_manager() {
        // Create components
        let cache = taskagent::Cache::new();
        let agent_registry = taskagent::TaskAgentRegistry::new();
        
        // Register an agent
        agent_registry.register("example", Box::new(ExampleAgent)).unwrap();
        
        // Create task manager
        let task_manager = Arc::new(taskagent::TaskManager::new(
            Duration::from_secs(30),
            taskagent::StallAction::NotifyPlanningAgent,
            cache,
            agent_registry,
            None,
        ));
        
        // Create a job
        let job_id = "test_job".to_string();
        let now = chrono::Utc::now().naive_utc();
        
        let task = taskagent::Task {
            job_id: job_id.clone(),
            task_id: "test_task".to_string(),
            parent_task_id: None,
            acceptance_criteria: None,
            description: "Test task".to_string(),
            status: taskagent::TaskStatus::Pending,
            status_reason: None,
            agent: "example".to_string(),
            dependencies: vec![],
            input: json!({"message": "Hello, world!"}),
            output: taskagent::TaskOutput {
                success: false,
                data: None,
                error: None,
            },
            created_at: now,
            updated_at: now,
            timeout: None,
            max_retries: 0,
            retry_count: 0,
            task_type: taskagent::TaskType::Task,
            summary: None,
        };
        
        // Start the job
        let job_handle = task_manager.start_job(
            job_id.clone(),
            vec![task],
            None,
            None,
        ).unwrap();
        
        // Wait for completion
        let mut completed = false;
        for _ in 0..10 {
            let status = job_handle.get_status().await;
            if let taskagent::JobStatus::Completed(_) = status {
                completed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        
        assert!(completed, "Job did not complete in time");
        
        // Check the task result
        let task_ids: Vec<String> = {
            let tasks = task_manager.tasks_by_job.get(&job_id).expect("Job not found");
            tasks.iter().map(|entry| entry.key().clone()).collect()
        };
        
        for task_id in task_ids {
            let task = task_manager.tasks_by_id.get(&task_id).expect("Task not found");
            assert_eq!(task.status, taskagent::TaskStatus::Completed);
            assert!(task.output.success);
            
            // Check cache
            let cache_value = task_manager.cache.get_value(&task.task_id, "test_key");
            assert!(cache_value.is_some());
            assert_eq!(cache_value.unwrap().as_str().unwrap(), "Processed: Hello, world!");
        }
    }
}

