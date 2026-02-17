//! Pipeline compiler for Work Queue execution.
//!
//! This module provides a Lobster-style ergonomics layer:
//! - Users specify a list of exec steps (no `sh -c`)
//! - Dagger compiles steps + pipes into a DAG (`Graph` / `Node`s)
//! - Piping is explicit and auditable via stable node ids + cache references
//!
//! Supported piping modes:
//! - Text: `prev.stdout` → `next.stdin`
//! - JSON: `prev.stdout_json` → `next.stdin_json`

use crate::dag_flow::{DagExecutionReport, DagExecutor, Graph, Node};
use crate::work_queue::exec::ExecSpec;
use crate::Cache;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipeMode {
    Text,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipeSpec {
    /// Step index to pipe from. Defaults to the previous step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_step: Option<usize>,
    pub mode: PipeMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStep {
    /// Human-readable step name (used for deterministic id generation).
    pub name: String,
    /// Exec specification for this step.
    pub exec: ExecSpec,
    /// Optional node timeout in seconds.
    #[serde(default)]
    pub timeout_s: Option<u64>,
    /// Optional retry count (node-level).
    #[serde(default)]
    pub try_count: Option<u8>,
    /// Optional explicit dependencies by step index.
    ///
    /// - `None` (default): depends on the previous step (linear pipeline).
    /// - `Some([])`: no dependencies (fanout root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deps: Option<Vec<usize>>,
    /// Optional pipe wiring from an upstream step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipe: Option<PipeSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineSpec {
    pub dag_name: String,
    pub steps: Vec<PipelineStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStepRef {
    pub index: usize,
    pub step_name: String,
    pub step_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelinePlan {
    pub dag_name: String,
    pub steps: Vec<PipelineStepRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineExecution {
    pub plan: PipelinePlan,
    pub report: DagExecutionReport,
}

pub fn build_pipeline_plan(spec: &PipelineSpec) -> anyhow::Result<(Graph, PipelinePlan)> {
    validate_pipeline_spec(spec)?;

    let step_ids: Vec<String> = spec
        .steps
        .iter()
        .enumerate()
        .map(|(idx, step)| deterministic_pipeline_step_id(idx, &step.name))
        .collect();

    let mut nodes = Vec::with_capacity(spec.steps.len());
    let mut steps = Vec::with_capacity(spec.steps.len());

    for (idx, step) in spec.steps.iter().enumerate() {
        let step_id = step_ids[idx].clone();
        let deps = compute_step_deps(idx, step, &step_ids)?;

        nodes.push(Node {
            id: step_id.clone(),
            dependencies: deps,
            inputs: Vec::new(),
            outputs: vec![
                crate::dag_flow::OField {
                    name: "exit_code".to_string(),
                    description: None,
                },
                crate::dag_flow::OField {
                    name: "stdout".to_string(),
                    description: None,
                },
                crate::dag_flow::OField {
                    name: "stderr".to_string(),
                    description: None,
                },
                crate::dag_flow::OField {
                    name: "stdout_json".to_string(),
                    description: None,
                },
                crate::dag_flow::OField {
                    name: "truncated".to_string(),
                    description: None,
                },
                crate::dag_flow::OField {
                    name: "duration_ms".to_string(),
                    description: None,
                },
            ],
            action: "exec".to_string(),
            failure: String::new(),
            onfailure: true,
            description: format!("Pipeline step {}: {}", idx, step.name),
            timeout: step.timeout_s.unwrap_or(300),
            try_count: step.try_count.unwrap_or(1),
            instructions: None,
        });

        steps.push(PipelineStepRef {
            index: idx,
            step_name: step.name.clone(),
            step_id,
        });
    }

    let graph = Graph {
        name: spec.dag_name.clone(),
        description: format!("Compiled pipeline: {}", spec.dag_name),
        author: "dagger".to_string(),
        version: "1.0".to_string(),
        signature: "generated".to_string(),
        tags: vec!["work_queue".to_string(), "pipeline".to_string()],
        instructions: None,
        nodes,
        config: None,
    };

    Ok((
        graph,
        PipelinePlan {
            dag_name: spec.dag_name.clone(),
            steps,
        },
    ))
}

pub async fn execute_pipeline(
    executor: &mut DagExecutor,
    cache: &Cache,
    spec: PipelineSpec,
    cancel_rx: oneshot::Receiver<()>,
) -> anyhow::Result<PipelineExecution> {
    let (graph, plan) = build_pipeline_plan(&spec)?;

    executor.load_graph(graph).await?;

    for (idx, step) in spec.steps.iter().enumerate() {
        let step_id = &plan.steps[idx].step_id;

        let mut inputs = serde_json::to_value(&step.exec)
            .map_err(|e| anyhow::anyhow!("failed to serialize ExecSpec: {}", e))?;

        // For piped steps, do not store a literal stdin/stdin_json value. We wire the input
        // reference directly to an upstream node output.
        if step.pipe.is_some() {
            if let Some(obj) = inputs.as_object_mut() {
                obj.remove("stdin");
                obj.remove("stdin_json");
            }
        }

        executor
            .set_node_inputs_json(&spec.dag_name, step_id, inputs, cache)
            .await?;

        if let Some(pipe) = step.pipe.as_ref() {
            let from_idx = pipe.from_step.unwrap_or_else(|| idx.saturating_sub(1));
            let from_id = &plan.steps[from_idx].step_id;

            match pipe.mode {
                PipeMode::Text => {
                    executor
                        .set_node_input_ref(
                            &spec.dag_name,
                            step_id,
                            "stdin",
                            format!("{}.stdout", from_id),
                        )
                        .await?;
                }
                PipeMode::Json => {
                    executor
                        .set_node_input_ref(
                            &spec.dag_name,
                            step_id,
                            "stdin_json",
                            format!("{}.stdout_json", from_id),
                        )
                        .await?;
                }
            }
        }
    }

    let report = executor
        .execute_static_dag(&spec.dag_name, cache, cancel_rx)
        .await?;

    Ok(PipelineExecution { plan, report })
}

fn validate_pipeline_spec(spec: &PipelineSpec) -> anyhow::Result<()> {
    if spec.dag_name.trim().is_empty() {
        anyhow::bail!("dag_name is required");
    }
    if spec.steps.is_empty() {
        anyhow::bail!("steps must be non-empty");
    }

    for (idx, step) in spec.steps.iter().enumerate() {
        if step.name.trim().is_empty() {
            anyhow::bail!("steps[{}].name is required", idx);
        }
        step.exec
            .validate()
            .map_err(|e| anyhow::anyhow!("steps[{}].exec invalid: {}", idx, e))?;

        if step.pipe.is_some() && (step.exec.stdin.is_some() || step.exec.stdin_json.is_some()) {
            anyhow::bail!(
                "steps[{}] cannot set exec.stdin/exec.stdin_json when pipe is configured",
                idx
            );
        }

        if let Some(pipe) = step.pipe.as_ref() {
            let from_idx = pipe
                .from_step
                .unwrap_or_else(|| idx.checked_sub(1).unwrap_or(usize::MAX));
            if from_idx == usize::MAX {
                anyhow::bail!("steps[{}].pipe requires from_step for the first step", idx);
            }
            if from_idx >= idx {
                anyhow::bail!(
                    "steps[{}].pipe.from_step must refer to an earlier step (got {})",
                    idx,
                    from_idx
                );
            }

            if pipe.mode == PipeMode::Json && !spec.steps[from_idx].exec.parse_stdout_as_json {
                anyhow::bail!(
                    "steps[{}] json pipe requires steps[{}].exec.parse_stdout_as_json = true",
                    idx,
                    from_idx
                );
            }
        }
    }

    Ok(())
}

fn compute_step_deps(
    idx: usize,
    step: &PipelineStep,
    step_ids: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut dep_indices: Vec<usize> = match step.deps.as_ref() {
        Some(v) => v.clone(),
        None => {
            if idx == 0 {
                Vec::new()
            } else {
                vec![idx - 1]
            }
        }
    };

    if let Some(pipe) = step.pipe.as_ref() {
        let from_idx = pipe.from_step.unwrap_or_else(|| idx.saturating_sub(1));
        if !dep_indices.contains(&from_idx) {
            dep_indices.push(from_idx);
        }
    }

    dep_indices.sort_unstable();
    dep_indices.dedup();

    for &d in &dep_indices {
        if d >= idx {
            anyhow::bail!(
                "steps[{}] deps must refer to earlier steps (got {})",
                idx,
                d
            );
        }
        if d >= step_ids.len() {
            anyhow::bail!(
                "steps[{}] deps index out of range (got {}, steps={})",
                idx,
                d,
                step_ids.len()
            );
        }
    }

    Ok(dep_indices
        .into_iter()
        .map(|d| step_ids[d].clone())
        .collect())
}

fn deterministic_pipeline_step_id(index: usize, step_name: &str) -> String {
    let name_frag = sanitize_fragment(step_name);
    format!("pipe_{}_{}", index, name_frag)
}

fn sanitize_fragment(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_underscore = false;

    for ch in input.chars() {
        // NOTE: Node IDs are used in `node_id.output` references, so '.' is not allowed.
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_');
        let mapped = if ok { ch } else { '_' };

        if mapped == '_' {
            if prev_underscore {
                continue;
            }
            prev_underscore = true;
        } else {
            prev_underscore = false;
        }

        out.push(mapped);
    }

    let trimmed = out.trim_matches('_');
    let mut out = if trimmed.is_empty() {
        "x".to_string()
    } else {
        trimmed.to_string()
    };

    const MAX_LEN: usize = 32;
    if out.len() > MAX_LEN {
        out.truncate(MAX_LEN);
    }

    out
}
