//! Coordinator - the heart of parallel execution
//! 
//! The Coordinator orchestrates parallel node execution using channels,
//! ensuring no borrow checker conflicts while maintaining high parallelism.

use tokio::sync::{mpsc, oneshot, Semaphore};
use std::sync::Arc;
use std::collections::{HashMap, HashSet};
use anyhow::Result;
use petgraph::graph::NodeIndex;

use crate::dag_flow::{DagExecutor, Cache, ExecutionObserver};
use crate::coord::types::{ExecutionEvent, ExecutorCommand, NodeRef, NodeOutcome, NodeSpec};
use crate::coord::hooks::{EventHook, HookContext};
use crate::coord::action::{NodeCtx, NodeAction};

/// Coordinator for parallel DAG execution
pub struct Coordinator {
    // Channels for events and commands
    evt_tx: mpsc::Sender<ExecutionEvent>,
    evt_rx: mpsc::Receiver<ExecutionEvent>,
    cmd_tx: mpsc::Sender<ExecutorCommand>,
    cmd_rx: mpsc::Receiver<ExecutorCommand>,
    // Hooks that process events
    hooks: Vec<Arc<dyn EventHook>>,
    // Maximum in-flight events for backpressure
    max_inflight_events: usize,
}

impl Coordinator {
    /// Create a new coordinator
    pub fn new(hooks: Vec<Arc<dyn EventHook>>, cap_events: usize, cap_cmds: usize) -> Self {
        let (evt_tx, evt_rx) = mpsc::channel(cap_events);
        let (cmd_tx, cmd_rx) = mpsc::channel(cap_cmds);
        Self {
            evt_tx,
            evt_rx,
            cmd_tx,
            cmd_rx,
            hooks,
            max_inflight_events: cap_events,
        }
    }
    
    /// Get a sender for events (for workers to use)
    pub fn event_sender(&self) -> mpsc::Sender<ExecutionEvent> {
        self.evt_tx.clone()
    }
    
    /// Get a sender for commands (for hooks to use)
    pub fn command_sender(&self) -> mpsc::Sender<ExecutorCommand> {
        self.cmd_tx.clone()
    }
    
    /// Run parallel execution
    /// 
    /// This is the main entry point that replaces execute_dag_parallel.
    /// It handles:
    /// - Scheduling ready nodes for parallel execution
    /// - Processing events from workers
    /// - Running hooks to generate commands
    /// - Applying commands to mutate the DAG (only place with &mut DagExecutor)
    /// - Backpressure and graceful shutdown
    pub async fn run_parallel(
        mut self,
        exec: &mut DagExecutor,
        cache: &Cache,
        dag_name: &str,
        run_id: &str,
        mut cancel_rx: oneshot::Receiver<()>,
    ) -> Result<()> {
        // Snapshot config to avoid long-lived borrows
        let max_parallel = exec.config.max_parallel_nodes.max(1);
        let semaphore = Arc::new(Semaphore::new(max_parallel));
        
        // Track execution state
        let mut in_flight: HashSet<String> = HashSet::new();
        let mut completed: HashSet<String> = HashSet::new();
        
        // Initial scheduling
        let initial_nodes = get_ready_nodes(exec, dag_name, &completed).await?;
        for node_id in initial_nodes {
            spawn_worker(
                exec,
                dag_name,
                node_id.clone(),
                semaphore.clone(),
                self.evt_tx.clone(),
                cache.clone(),
            ).await?;
            in_flight.insert(node_id);
        }
        
        // Create the hook context
        let hook_ctx = HookContext::new(run_id.to_string(), dag_name.to_string())
            .with_app_data(Arc::new(cache.clone()));
        
        // Call on_start hooks
        for hook in &self.hooks {
            let cmds = hook.on_start(&hook_ctx).await;
            for cmd in cmds {
                let _ = self.cmd_tx.send(cmd).await;
            }
        }
        
        // Main command/mutation loop with graceful shutdown
        let mut cmd_rx = self.cmd_rx;
        let mut evt_rx = self.evt_rx;
        
        loop {
            // Check completion condition first, before select!
            if in_flight.is_empty() && is_complete(exec, dag_name, &completed).await? {
                tracing::info!("DAG execution complete - all nodes finished");
                break;
            }
            
            tokio::select! {
                // Process commands from hooks
                Some(cmd) = cmd_rx.recv() => {
                    apply_command(exec, dag_name, cache, cmd).await?;
                    
                    // Schedule any newly ready nodes
                    let ready = get_ready_nodes(exec, dag_name, &completed).await?;
                    for node_id in ready {
                        if !in_flight.contains(&node_id) && !completed.contains(&node_id) {
                            spawn_worker(
                                exec,
                                dag_name,
                                node_id.clone(),
                                semaphore.clone(),
                                self.evt_tx.clone(),
                                cache.clone(),
                            ).await?;
                            in_flight.insert(node_id);
                        }
                    }
                }
                
                // Process events from workers
                Some(event) = evt_rx.recv() => {
                    // First, call hooks to generate any commands
                    for hook in &self.hooks {
                        let cmds = hook.handle(&hook_ctx, &event).await;
                        for cmd in cmds {
                            // Apply commands immediately
                            apply_command(exec, dag_name, cache, cmd).await?;
                        }
                    }
                    
                    // Update execution state and call observers
                    match &event {
                        ExecutionEvent::NodeStarted { node } => {
                            for observer in &exec.observers {
                                observer.on_node_started(run_id, dag_name, &node.node_id).await;
                            }
                        }
                        ExecutionEvent::NodeCompleted { node, .. } => {
                            tracing::debug!("Node completed: {}", node.node_id);
                            in_flight.remove(&node.node_id);
                            completed.insert(node.node_id.clone());
                            for observer in &exec.observers {
                                observer.on_node_completed(run_id, dag_name, &node.node_id).await;
                            }
                            
                            // IMPORTANT: Schedule dependent nodes after completion
                            let ready = get_ready_nodes(exec, dag_name, &completed).await?;
                            tracing::debug!("Ready nodes after {}: {:?}", node.node_id, ready);
                            for node_id in ready {
                                if !in_flight.contains(&node_id) && !completed.contains(&node_id) {
                                    tracing::info!("Scheduling dependent node: {}", node_id);
                                    spawn_worker(
                                        exec,
                                        dag_name,
                                        node_id.clone(),
                                        semaphore.clone(),
                                        self.evt_tx.clone(),
                                        cache.clone(),
                                    ).await?;
                                    in_flight.insert(node_id);
                                }
                            }
                            
                            // Check if this was the last node
                            if in_flight.is_empty() && is_complete(exec, dag_name, &completed).await? {
                                tracing::info!("DAG execution complete after processing node: {}", node.node_id);
                                // Process completion in next iteration to ensure all events are handled
                            }
                        }
                        ExecutionEvent::NodeFailed { node, error } => {
                            in_flight.remove(&node.node_id);
                            completed.insert(node.node_id.clone());
                            for observer in &exec.observers {
                                observer.on_node_failed(run_id, dag_name, &node.node_id, error).await;
                            }
                            
                            // Check if this was the last node (even if failed)
                            if in_flight.is_empty() && is_complete(exec, dag_name, &completed).await? {
                                tracing::info!("DAG execution complete after node failure: {}", node.node_id);
                            }
                        }
                        _ => {}
                    }
                }
                
                // Handle cancellation
                _ = &mut cancel_rx => {
                    tracing::info!("Coordinator received cancel signal");
                    break;
                }
                
                // Add a timeout to prevent infinite waiting on empty channels
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    // This acts as a periodic check for completion
                    // Continue to next iteration where completion will be checked
                }
            }
        }
        
        // Call on_complete hooks
        for hook in &self.hooks {
            let cmds = hook.on_complete(&hook_ctx, true).await;
            for cmd in cmds {
                let _ = apply_command(exec, dag_name, cache, cmd).await;
            }
        }
        
        // Shutdown by dropping event sender
        drop(self.evt_tx);
        
        Ok(())
    }
}

/// Apply a command to mutate the executor
/// 
/// This is the ONLY place where we have &mut DagExecutor
async fn apply_command(exec: &mut DagExecutor, dag_name: &str, cache: &Cache, cmd: ExecutorCommand) -> Result<()> {
    use ExecutorCommand::*;
    
    match cmd {
        AddNode { dag_name: dn, spec } => {
            if dn != dag_name {
                anyhow::bail!("Command for wrong DAG: {} vs {}", dn, dag_name);
            }
            let id = spec.id.clone().unwrap_or_else(|| exec.gen_node_id(&spec.action));
            exec.add_node(dag_name, id.clone(), spec.action, spec.deps).await?;
            // Store inputs if provided
            if !spec.inputs.is_null() {
                exec.set_node_inputs_json(dag_name, &id, spec.inputs, cache).await?;
            }
            // Notify observers about added node
            for observer in &exec.observers {
                observer.on_nodes_added("", dag_name, &[id.clone()]).await;
            }
        }
        AddNodes { dag_name: dn, specs } => {
            if dn != dag_name {
                anyhow::bail!("Command for wrong DAG: {} vs {}", dn, dag_name);
            }
            let mut node_ids = Vec::new();
            for spec in specs {
                let id = spec.id.clone().unwrap_or_else(|| exec.gen_node_id(&spec.action));
                exec.add_node(dag_name, id.clone(), spec.action, spec.deps).await?;
                if !spec.inputs.is_null() {
                    exec.set_node_inputs_json(dag_name, &id, spec.inputs, cache).await?;
                }
                node_ids.push(id);
            }
            // Notify observers about added nodes
            if !node_ids.is_empty() {
                for observer in &exec.observers {
                    observer.on_nodes_added("", dag_name, &node_ids).await;
                }
            }
        }
        SetNodeInputs { dag_name: dn, node_id, inputs } => {
            if dn != dag_name {
                anyhow::bail!("Command for wrong DAG: {} vs {}", dn, dag_name);
            }
            exec.set_node_inputs_json(dag_name, &node_id, inputs, cache).await?;
        }
        PauseBranch { branch_id, reason } => {
            exec.pause_branch(&branch_id, reason.as_deref());
        }
        ResumeBranch { branch_id } => {
            exec.resume_branch(&branch_id);
        }
        CancelBranch { branch_id } => {
            exec.cancel_branch(&branch_id, None);
        }
        EmitEvent { event } => {
            // Route to EventStore if configured
            if let Some(ref sink) = exec.event_sink {
                // Convert to the right event type
                // This is a placeholder - adapt to your event system
                tracing::debug!("Emitting event: {:?}", event);
            }
        }
    }
    
    Ok(())
}

/// Get nodes that are ready to execute
async fn get_ready_nodes(exec: &DagExecutor, dag_name: &str, completed: &HashSet<String>) -> Result<Vec<String>> {
    let dags = exec.prebuilt_dags.read().await;
    
    let (graph, node_map) = dags.get(dag_name)
        .ok_or_else(|| anyhow::anyhow!("DAG not found: {}", dag_name))?;
    
    let mut ready = Vec::new();
    
    for (node_id, &node_idx) in node_map.iter() {
        // Skip if already completed
        if completed.contains(node_id) {
            continue;
        }
        
        // Check if all dependencies are completed
        let deps_complete = graph
            .neighbors_directed(node_idx, petgraph::Direction::Incoming)
            .all(|dep_idx| {
                // Find the node ID for this index
                node_map.iter()
                    .find(|(_, &idx)| idx == dep_idx)
                    .map(|(id, _)| completed.contains(id))
                    .unwrap_or(false)
            });
        
        if deps_complete {
            ready.push(node_id.clone());
        }
    }
    
    Ok(ready)
}

/// Check if DAG execution is complete
async fn is_complete(exec: &DagExecutor, dag_name: &str, completed: &HashSet<String>) -> Result<bool> {
    let dags = exec.prebuilt_dags.read().await;
    
    let (_, node_map) = dags.get(dag_name)
        .ok_or_else(|| anyhow::anyhow!("DAG not found: {}", dag_name))?;
    
    // Complete if all nodes are completed
    Ok(node_map.keys().all(|id| completed.contains(id)))
}

/// Spawn a worker task to execute a node
async fn spawn_worker(
    exec: &mut DagExecutor,
    dag_name: &str,
    node_id: String,
    sem: Arc<Semaphore>,
    evt_tx: mpsc::Sender<ExecutionEvent>,
    cache: Cache,
) -> Result<()> {
    // Get the node's action and inputs
    let dags = exec.prebuilt_dags.read().await;
    
    let (graph, node_map) = dags.get(dag_name)
        .ok_or_else(|| anyhow::anyhow!("DAG not found: {}", dag_name))?;
    
    let &node_idx = node_map.get(&node_id)
        .ok_or_else(|| anyhow::anyhow!("Node not found: {}", node_id))?;
    
    let node = &graph[node_idx];
    let action_name = node.action.clone();
    let node_inputs = node.inputs.clone();
    let node_timeout = node.timeout; // Get the timeout from the node config
    
    drop(dags);
    
    // Get the action from registry using the synchronous API
    let action = exec.function_registry.get(&action_name)
        .ok_or_else(|| anyhow::anyhow!("Action not found: {}", action_name))?;
    
    // Resolve node inputs from references
    let mut resolved_inputs = serde_json::json!({});
    for input_field in &node_inputs {
        let value = if input_field.reference.starts_with("inputs.") {
            // Reference to DAG inputs stored in cache
            let key = input_field.reference.strip_prefix("inputs.").unwrap();
            match crate::get_input::<serde_json::Value>(&cache, "inputs", key) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!("Failed to resolve input {}: {}", input_field.reference, e);
                    serde_json::Value::Null
                }
            }
        } else if input_field.reference.contains('.') {
            // Reference to another node's output
            let parts: Vec<&str> = input_field.reference.split('.').collect();
            if parts.len() == 2 {
                match crate::get_input::<serde_json::Value>(&cache, parts[0], parts[1]) {
                    Ok(v) => v,
                    Err(_) => serde_json::Value::Null
                }
            } else {
                serde_json::Value::Null
            }
        } else {
            // Direct value or default
            serde_json::Value::Null
        };
        
        resolved_inputs[input_field.name.clone()] = value;
    }
    
    // Create node context with resolved inputs
    let ctx = NodeCtx::new(
        dag_name,
        node_id.clone(),
        resolved_inputs,
        cache.clone(),
    );
    
    let node_ref = NodeRef {
        dag_name: dag_name.to_string(),
        node_id: node_id.clone(),
    };
    
    // Notify start
    let _ = evt_tx.send(ExecutionEvent::NodeStarted {
        node: node_ref.clone()
    }).await;
    
    // Spawn the worker
    tokio::spawn(async move {
        let _permit = sem.acquire_owned().await.ok()?;
        
        // Execute the action using the new NodeAction trait
        // Actions are compute-only and don't need executor
        
        match tokio::time::timeout(
            std::time::Duration::from_secs(node_timeout), // Use node's configured timeout
            action.execute(&ctx)
        ).await {
            Ok(Ok(output)) => {
                // The output is already stored in cache by the node implementation
                // We just need to send the completion event
                let _ = evt_tx.send(ExecutionEvent::NodeCompleted {
                    node: node_ref,
                    outcome: NodeOutcome::Success { outputs: output.outputs },
                }).await;
            }
            Ok(Err(e)) => {
                let _ = evt_tx.send(ExecutionEvent::NodeFailed {
                    node: node_ref,
                    error: e.to_string(),
                }).await;
            }
            Err(e) => {
                let _ = evt_tx.send(ExecutionEvent::NodeFailed {
                    node: node_ref,
                    error: format!("Worker task error: {}", e),
                }).await;
            }
        }
        
        Some(())
    });
    
    Ok(())
}