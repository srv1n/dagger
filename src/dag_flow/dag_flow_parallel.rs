use super::dag_flow::{
    create_execution_report, execute_node_with_context, parse_input_from_name, Cache,
    DagExecutionReport, DagExecutor, ExecutionContext, Node, NodeExecutionOutcome, OnFailure,
};
use futures::stream::{FuturesUnordered, StreamExt};
use petgraph::graph::DiGraph;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

/// Executes the DAG asynchronously with parallel node execution support
pub async fn execute_dag_parallel(
    executor: &mut DagExecutor,
    dag: &DiGraph<Node, ()>,
    cache: &Cache,
    dag_name: &str,
) -> (DagExecutionReport, bool) {
    let mut node_outcomes = Vec::new();
    let mut overall_success = true;
    let mut executed_nodes = HashSet::new();

    let context = executor.execution_context.as_ref().unwrap();
    let semaphore = Arc::new(Semaphore::new(context.max_parallel_nodes));

    // Create a channel for results
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    // Track active tasks
    let mut active_tasks = 0;

    loop {
        // Check for completion conditions
        if *executor.stopped.read().await {
            info!("Execution stopped");
            break;
        }

        // Get the latest DAG state
        let current_dag = {
            let dags = executor.prebuilt_dags.read().await;

            match dags.get(dag_name) {
                Some((dag, _)) => dag.clone(),
                None => break,
            }
        };

        // Check limits and timeouts
        let supervisor_iteration: usize = if let Some(supervisor_idx) = current_dag
            .node_indices()
            .find(|&idx| current_dag[idx].action == "supervisor_step")
        {
            parse_input_from_name(
                cache,
                "iteration".to_string(),
                &current_dag[supervisor_idx].inputs,
            )
            .unwrap_or(0)
        } else {
            executed_nodes.len()
        };

        if let Some(max_iter) = executor.config.max_iterations {
            if supervisor_iteration >= max_iter as usize {
                return (
                    create_execution_report(
                        node_outcomes,
                        false,
                        Some(format!("Maximum iterations ({}) reached", max_iter)),
                    ),
                    true,
                );
            }
        }

        let elapsed = chrono::Local::now().naive_local() - executor.start_time;
        if elapsed.num_seconds() > executor.config.timeout_seconds.unwrap_or(3600) as i64 {
            return (
                create_execution_report(
                    node_outcomes,
                    false,
                    Some("DAG timeout exceeded".to_string()),
                ),
                false,
            );
        }

        // Find all nodes ready for execution
        let ready_nodes: Vec<Node> = current_dag
            .node_indices()
            .filter_map(|idx| {
                let node = &current_dag[idx];
                if !executed_nodes.contains(&node.id)
                    && node
                        .dependencies
                        .iter()
                        .all(|dep| executed_nodes.contains(dep))
                {
                    Some(node.clone())
                } else {
                    None
                }
            })
            .collect();

        // Launch new tasks for ready nodes
        let ready_count = ready_nodes.len();
        for node in ready_nodes {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let tx_clone = tx.clone();
            let node_clone = node.clone();
            let cache_clone = cache.clone();
            let registry = executor.function_registry.clone();
            let config = executor.config.clone();
            let sqlite_cache = executor.sqlite_cache.clone();
            let tree = executor.tree.clone();
            let graphs = executor.graphs.clone();
            let app_state = executor.app_state.clone();
            let event_tx = executor.event_tx.clone();
            let services = executor.services.clone();

            active_tasks += 1;
            // Call supervisor hooks before node starts
            // Note: We can't call hooks here due to borrow checker constraints
            // The hooks will be called after node completion instead
            
            info!(
                "Launching node {} (active tasks: {})",
                node.id, active_tasks
            );

            tokio::spawn(async move {
                let outcome = execute_node_with_context(
                    &node_clone,
                    &cache_clone,
                    registry,
                    config,
                    sqlite_cache,
                    tree,
                    graphs,
                    app_state,
                    event_tx,
                    services,
                )
                .await;

                drop(permit); // Release semaphore
                let _ = tx_clone.send(outcome);
            });
        }

        // If no active tasks and no more nodes, we're done
        if active_tasks == 0 && ready_count == 0 {
            info!("No active tasks and no ready nodes - execution complete");
            break;
        }

        // Wait for at least one task to complete
        if active_tasks > 0 {
            match rx.recv().await {
                Some(outcome) => {
                    active_tasks -= 1;
                    info!(
                        "Node {} completed (active tasks: {})",
                        outcome.node_id, active_tasks
                    );

                    executed_nodes.insert(outcome.node_id.clone());
                    
                    // Note: Supervisor hooks cannot be called here due to borrow checker constraints
                    // This would require a redesign of the execution flow to properly support hooks

                    if !outcome.success {
                        match executor.config.on_failure {
                            OnFailure::Stop => {
                                overall_success = false;
                                node_outcomes.push(outcome);
                                return (
                                    create_execution_report(node_outcomes, false, None),
                                    false,
                                );
                            }
                            OnFailure::Pause => {
                                if let Err(e) = executor.save_cache(&outcome.node_id, cache).await {
                                    error!("Failed to save cache before pause: {}", e);
                                }
                                overall_success = false;
                                node_outcomes.push(outcome);
                                return (
                                    create_execution_report(node_outcomes, false, None),
                                    false,
                                );
                            }
                            OnFailure::Continue => {
                                overall_success = false;
                                node_outcomes.push(outcome);
                            }
                        }
                    } else {
                        node_outcomes.push(outcome);
                    }

                    // Check if we need incremental cache save
                    if executor.config.enable_incremental_cache {
                        let should_snapshot = {
                            let last_snapshot = *context.cache_last_snapshot.read().await;
                            let delta_size = *context.cache_delta_size.read().await;

                            last_snapshot.elapsed().as_secs()
                                > executor.config.cache_snapshot_interval
                                || delta_size > 1000 // Threshold for delta size
                        };

                        if should_snapshot {
                            if let Err(e) = executor.save_cache(dag_name, cache).await {
                                warn!("Failed to save incremental cache: {}", e);
                            }
                            *context.cache_last_snapshot.write().await = Instant::now();
                            *context.cache_delta_size.write().await = 0;
                        } else {
                            *context.cache_delta_size.write().await += 1;
                        }
                    }
                }
                None => {
                    // Channel closed unexpectedly
                    break;
                }
            }
        }

        if *executor.paused.read().await {
            return (
                create_execution_report(node_outcomes, overall_success, None),
                false,
            );
        }
    }

    // Wait for remaining tasks
    drop(tx); // Close sender
    while let Some(outcome) = rx.recv().await {
        info!("Final collection: Node {} completed", outcome.node_id);
        node_outcomes.push(outcome);
    }

    if let Err(e) = executor.save_execution_tree(dag_name).await {
        error!("Failed to save execution tree: {}", e);
    }

    if let Err(e) = executor.save_cache(dag_name, cache).await {
        error!("Failed to save cache for '{}': {}", dag_name, e);
    }

    (
        create_execution_report(node_outcomes, overall_success, None),
        false,
    )
}
