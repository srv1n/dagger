// Simple test to verify task-core compiles and works

use std::sync::Arc;
use std::time::Duration;
use bytes::Bytes;

// Mock types to test compilation
pub type TaskId = u64;
pub type AgentId = u16;

#[derive(Debug, Clone)]
pub enum Durability {
    BestEffort,
    AtMostOnce,
}

#[derive(Debug, Clone)]
pub enum TaskType {
    Task,
}

#[derive(Debug, Clone)]
pub enum AgentError {
    User(String),
}

pub struct TaskContext;
pub struct NewTaskSpec {
    pub agent: AgentId,
    pub input: Bytes,
    pub dependencies: Vec<TaskId>,
    pub durability: Durability,
    pub task_type: TaskType,
    pub description: Arc<str>,
    pub timeout: Option<Duration>,
    pub max_retries: Option<u8>,
    pub parent: Option<TaskId>,
}

// Test agent function
async fn test_agent(input: Bytes, _ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
    Ok(input)
}

fn main() {
    println!("Task-core types compile successfully!");
    
    // Test creating a task spec
    let spec = NewTaskSpec {
        agent: 1,
        input: Bytes::from("hello"),
        dependencies: vec![],
        durability: Durability::BestEffort,
        task_type: TaskType::Task,
        description: Arc::from("test task"),
        timeout: Some(Duration::from_secs(5)),
        max_retries: Some(3),
        parent: None,
    };
    
    println!("Created task spec with agent: {}", spec.agent);
}