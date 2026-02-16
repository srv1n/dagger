//! Batch execution helpers for Work Queue items.
//!
//! This module provides a deterministic "fanout" pattern:
//! - One run per user action (approve/send/retry)
//! - One isolated step-chain per selected item
//! - Deterministic `step_id` naming so UI/audit can map item → steps
//!
//! Retry strategy:
//! - Re-run a batch with `selected_item_ids = failed_item_ids` to target only failures.

use crate::dag_flow::{DagExecutionReport, DagExecutor, Graph, Node, OnFailure};
use crate::Cache;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchInput {
    pub container_id: String,
    pub selected_item_ids: Vec<String>,
    #[serde(default)]
    pub approved_revision_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerItemStepTemplate {
    /// Logical step name (used in deterministic step_id generation).
    pub name: String,
    /// Registered Dagger action name.
    pub action: String,
    /// Base inputs for the step (merged with item metadata).
    #[serde(default)]
    pub inputs: Value,
    /// Optional per-step timeout in seconds.
    #[serde(default)]
    pub timeout_s: Option<u64>,
    /// Optional per-step retry count.
    #[serde(default)]
    pub try_count: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchFanoutSpec {
    pub dag_name: String,
    pub input: BatchInput,
    pub steps: Vec<PerItemStepTemplate>,
    /// If true, failures in one item do not stop other items.
    #[serde(default)]
    pub continue_on_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchStepRef {
    pub step_name: String,
    pub step_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchPlan {
    pub dag_name: String,
    pub item_steps: BTreeMap<String, Vec<BatchStepRef>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchExecution {
    pub plan: BatchPlan,
    pub report: DagExecutionReport,
}

pub fn build_batch_plan(spec: &BatchFanoutSpec) -> anyhow::Result<(Graph, BatchPlan)> {
    if spec.input.container_id.trim().is_empty() {
        anyhow::bail!("container_id is required");
    }
    if spec.steps.is_empty() {
        anyhow::bail!("steps must be non-empty");
    }

    // Build nodes with deterministic IDs. Inputs are applied later via
    // `DagExecutor::set_node_inputs_json(...)` so we can use literal JSON without
    // references.
    let mut nodes: Vec<Node> = Vec::new();
    let mut item_steps: BTreeMap<String, Vec<BatchStepRef>> = BTreeMap::new();

    for item_id in &spec.input.selected_item_ids {
        let mut per_item_refs = Vec::with_capacity(spec.steps.len());
        let mut deps: Vec<String> = Vec::new();

        for step in &spec.steps {
            let step_id = deterministic_step_id(&spec.input.container_id, item_id, &step.name);

            nodes.push(Node {
                id: step_id.clone(),
                dependencies: deps.clone(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                action: step.action.clone(),
                failure: String::new(),
                onfailure: true,
                description: format!("Work queue item '{}' step '{}'", item_id, step.name),
                timeout: step.timeout_s.unwrap_or(300),
                try_count: step.try_count.unwrap_or(1),
                instructions: None,
            });

            per_item_refs.push(BatchStepRef {
                step_name: step.name.clone(),
                step_id: step_id.clone(),
            });

            deps = vec![step_id];
        }

        item_steps.insert(item_id.clone(), per_item_refs);
    }

    let graph = Graph {
        name: spec.dag_name.clone(),
        description: format!("Work queue batch fanout for container {}", spec.input.container_id),
        author: "dagger".to_string(),
        version: "1.0".to_string(),
        signature: "generated".to_string(),
        tags: vec!["work_queue".to_string(), "batch".to_string()],
        instructions: None,
        nodes,
        config: None,
    };

    let plan = BatchPlan {
        dag_name: spec.dag_name.clone(),
        item_steps,
    };

    Ok((graph, plan))
}

pub async fn execute_batch(
    executor: &mut DagExecutor,
    cache: &Cache,
    spec: BatchFanoutSpec,
    cancel_rx: oneshot::Receiver<()>,
) -> anyhow::Result<BatchExecution> {
    let (graph, plan) = build_batch_plan(&spec)?;

    // Load the graph into the executor.
    executor.load_graph(graph).await?;

    // Apply literal JSON inputs per node using the helper that writes to cache
    // and updates IField references.
    for item_id in &spec.input.selected_item_ids {
        for step in &spec.steps {
            let step_id = deterministic_step_id(&spec.input.container_id, item_id, &step.name);
            let merged = merge_step_inputs(&spec, item_id, step)?;
            executor
                .set_node_inputs_json(&spec.dag_name, &step_id, merged, cache)
                .await?;
        }
    }

    // Configure failure mode for this run.
    let prev_on_failure = executor.config.on_failure.clone();
    executor.config.on_failure = if spec.continue_on_error {
        OnFailure::Continue
    } else {
        OnFailure::Stop
    };

    let report = executor
        .execute_static_dag(&spec.dag_name, cache, cancel_rx)
        .await?;

    executor.config.on_failure = prev_on_failure;

    Ok(BatchExecution { plan, report })
}

fn merge_step_inputs(
    spec: &BatchFanoutSpec,
    item_id: &str,
    step: &PerItemStepTemplate,
) -> anyhow::Result<Value> {
    let rendered = render_placeholders(&step.inputs, &spec.input.container_id, item_id, &step.name);

    let mut inputs = match rendered {
        Value::Null => json!({}),
        Value::Object(_) => rendered,
        _ => anyhow::bail!("step.inputs must be a JSON object (or null)"),
    };

    let obj = inputs
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("step.inputs must be a JSON object"))?;

    obj.insert(
        "container_id".to_string(),
        Value::String(spec.input.container_id.clone()),
    );
    obj.insert("item_id".to_string(), Value::String(item_id.to_string()));
    obj.insert(
        "approved_revision_ids".to_string(),
        serde_json::to_value(&spec.input.approved_revision_ids)?,
    );
    obj.insert("step_name".to_string(), Value::String(step.name.clone()));

    Ok(inputs)
}

fn render_placeholders(value: &Value, container_id: &str, item_id: &str, step_name: &str) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(*b),
        Value::Number(n) => Value::Number(n.clone()),
        Value::String(s) => Value::String(
            s.replace("{{container_id}}", container_id)
                .replace("{{item_id}}", item_id)
                .replace("{{step_name}}", step_name),
        ),
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|v| render_placeholders(v, container_id, item_id, step_name))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(
                    k.clone(),
                    render_placeholders(v, container_id, item_id, step_name),
                );
            }
            Value::Object(out)
        }
    }
}

fn deterministic_step_id(container_id: &str, item_id: &str, step_name: &str) -> String {
    let container_frag = sanitize_fragment(container_id);
    let item_frag = sanitize_fragment(item_id);
    let step_frag = sanitize_fragment(step_name);

    let seed = format!("{}:{}:{}", container_id, item_id, step_name);
    let hash = stable_hash8(&seed);

    format!(
        "item_{}_{}_{}_{}",
        container_frag, item_frag, step_frag, hash
    )
}

fn sanitize_fragment(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_underscore = false;

    for ch in input.chars() {
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.');
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

fn stable_hash8(s: &str) -> String {
    let hash = fnv1a64(s.as_bytes());
    format!("{:08x}", (hash & 0xffff_ffff) as u32)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}
