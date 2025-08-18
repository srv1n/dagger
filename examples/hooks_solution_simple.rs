//! Simplified Working Solution for Supervisor Hooks
//! 
//! This demonstrates a practical, working solution for the supervisor hooks problem.

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use std::collections::HashMap;

// ============= Core Types =============

#[derive(Clone, Debug)]
pub struct Node {
    pub id: String,
    pub action: String,
}

/// Events that occur during execution
#[derive(Debug)]
pub enum ExecutionEvent {
    NodeStarted { node: Node },
    NodeCompleted { node: Node, success: bool },
    NodeFailed { node: Node, error: String },
}

/// Commands that can mutate the executor state
#[derive(Debug)]
pub enum ExecutorCommand {
    AddNode { dag_name: String, node: Node },
    PauseExecution { reason: String },
    ResumeExecution,
}

/// Simplified hook that works with events
#[async_trait]
pub trait EventBasedHook: Send + Sync {
    async fn handle_event(&self, event: &ExecutionEvent) -> Vec<ExecutorCommand>;
}

// ============= Example Hooks =============

pub struct LoggingHook;

#[async_trait]
impl EventBasedHook for LoggingHook {
    async fn handle_event(&self, event: &ExecutionEvent) -> Vec<ExecutorCommand> {
        match event {
            ExecutionEvent::NodeStarted { node } => {
                println!("[LoggingHook] Node '{}' started", node.id);
            }
            ExecutionEvent::NodeCompleted { node, success } => {
                println!("[LoggingHook] Node '{}' completed (success: {})", node.id, success);
            }
            ExecutionEvent::NodeFailed { node, error } => {
                println!("[LoggingHook] Node '{}' failed: {}", node.id, error);
            }
        }
        Vec::new() // No commands from logging hook
    }
}

pub struct DynamicNodeHook;

#[async_trait]
impl EventBasedHook for DynamicNodeHook {
    async fn handle_event(&self, event: &ExecutionEvent) -> Vec<ExecutorCommand> {
        let mut commands = Vec::new();
        
        if let ExecutionEvent::NodeCompleted { node, success: true } = event {
            if node.action == "process" {
                println!("[DynamicNodeHook] Adding cleanup node for '{}'", node.id);
                commands.push(ExecutorCommand::AddNode {
                    dag_name: "current".to_string(),
                    node: Node {
                        id: format!("cleanup_{}", node.id),
                        action: "cleanup".to_string(),
                    },
                });
            }
        }
        
        commands
    }
}

// ============= Solution: Event-Driven Executor =============

pub struct EventDrivenExecutor {
    hooks: Vec<Arc<dyn EventBasedHook>>,
    event_tx: mpsc::UnboundedSender<ExecutionEvent>,
    command_tx: mpsc::UnboundedSender<ExecutorCommand>,
}

impl EventDrivenExecutor {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<ExecutionEvent>, mpsc::UnboundedReceiver<ExecutorCommand>) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        
        let executor = Self {
            hooks: Vec::new(),
            event_tx,
            command_tx,
        };
        
        (executor, event_rx, command_rx)
    }
    
    pub fn add_hook(&mut self, hook: Arc<dyn EventBasedHook>) {
        self.hooks.push(hook);
    }
    
    /// Process events in a separate task
    pub async fn process_events(
        hooks: Vec<Arc<dyn EventBasedHook>>,
        mut event_rx: mpsc::UnboundedReceiver<ExecutionEvent>,
        command_tx: mpsc::UnboundedSender<ExecutorCommand>,
    ) {
        while let Some(event) = event_rx.recv().await {
            println!("\n[EventProcessor] Processing: {:?}", event);
            
            // Let each hook handle the event
            for hook in &hooks {
                let commands = hook.handle_event(&event).await;
                for cmd in commands {
                    println!("[EventProcessor] Sending command: {:?}", cmd);
                    let _ = command_tx.send(cmd);
                }
            }
        }
        println!("[EventProcessor] Shutting down");
    }
    
    /// Process commands in a separate task (with mutable state access)
    pub async fn process_commands(
        mut command_rx: mpsc::UnboundedReceiver<ExecutorCommand>,
        state: Arc<Mutex<ExecutorState>>,
    ) {
        while let Some(command) = command_rx.recv().await {
            let mut state = state.lock().await;
            match command {
                ExecutorCommand::AddNode { dag_name, node } => {
                    println!("[CommandProcessor] Adding node '{}' to DAG '{}'", node.id, dag_name);
                    state.pending_nodes.push(node);
                }
                ExecutorCommand::PauseExecution { reason } => {
                    println!("[CommandProcessor] Pausing execution: {}", reason);
                    state.is_paused = true;
                }
                ExecutorCommand::ResumeExecution => {
                    println!("[CommandProcessor] Resuming execution");
                    state.is_paused = false;
                }
            }
        }
        println!("[CommandProcessor] Shutting down");
    }
    
    /// Simulate parallel node execution
    pub async fn execute_nodes(
        nodes: Vec<Node>,
        event_tx: mpsc::UnboundedSender<ExecutionEvent>,
    ) {
        let mut handles = Vec::new();
        
        for node in nodes {
            let event_tx = event_tx.clone();
            let handle = tokio::spawn(async move {
                // Notify start
                let _ = event_tx.send(ExecutionEvent::NodeStarted { 
                    node: node.clone() 
                });
                
                // Simulate work
                println!("[Executor] Executing node '{}'", node.id);
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                
                // Notify completion (always success for demo)
                let _ = event_tx.send(ExecutionEvent::NodeCompleted {
                    node: node.clone(),
                    success: true,
                });
            });
            handles.push(handle);
        }
        
        // Wait for all nodes
        for handle in handles {
            let _ = handle.await;
        }
    }
}

/// Mutable executor state
pub struct ExecutorState {
    pub pending_nodes: Vec<Node>,
    pub is_paused: bool,
}

// ============= Demo =============

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Event-Driven Supervisor Hooks Solution ===\n");
    
    // Create executor and channels
    let (mut executor, event_rx, command_rx) = EventDrivenExecutor::new();
    
    // Add hooks
    executor.add_hook(Arc::new(LoggingHook));
    executor.add_hook(Arc::new(DynamicNodeHook));
    
    // Create shared state
    let state = Arc::new(Mutex::new(ExecutorState {
        pending_nodes: Vec::new(),
        is_paused: false,
    }));
    
    // Clone what we need
    let hooks = executor.hooks.clone();
    let event_tx = executor.event_tx.clone();
    let command_tx = executor.command_tx.clone();
    
    // Start event processor
    let event_processor = tokio::spawn(EventDrivenExecutor::process_events(
        hooks,
        event_rx,
        command_tx.clone(),
    ));
    
    // Start command processor
    let state_clone = state.clone();
    let command_processor = tokio::spawn(EventDrivenExecutor::process_commands(
        command_rx,
        state_clone,
    ));
    
    // Define initial nodes
    let nodes = vec![
        Node { id: "fetch".to_string(), action: "fetch".to_string() },
        Node { id: "process".to_string(), action: "process".to_string() },
        Node { id: "save".to_string(), action: "save".to_string() },
    ];
    
    println!("Starting execution with {} nodes\n", nodes.len());
    
    // Execute nodes (this would be in parallel in real implementation)
    EventDrivenExecutor::execute_nodes(nodes, event_tx.clone()).await;
    
    // Give time for events to process
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    
    // Check if new nodes were added
    {
        let state = state.lock().await;
        println!("\n=== Final State ===");
        println!("Pending nodes added by hooks:");
        for node in &state.pending_nodes {
            println!("  - {} (action: {})", node.id, node.action);
        }
        println!("Is paused: {}", state.is_paused);
    }
    
    // Cleanup
    drop(executor.event_tx);
    drop(executor.command_tx);
    
    // Wait for processors to finish
    let _ = tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        event_processor
    ).await;
    let _ = tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        command_processor
    ).await;
    
    println!("\n✅ Event-driven solution completed successfully!");
    println!("\n=== Key Insights ===");
    println!("1. Events and commands are processed in separate tasks");
    println!("2. No borrow checker conflicts because we use channels");
    println!("3. Hooks can't directly mutate state, only send commands");
    println!("4. Parallel execution works without issues");
    println!("5. State mutations are centralized and controlled");
    
    Ok(())
}