//! Human-in-the-loop (HITL) checkpoints for Work Queue execution.
//!
//! A checkpoint is a first-class node/action that can pause a *branch* (e.g. one item in a batch),
//! emit a structured approval request to the host, then resume deterministically with a resume
//! token + decision payload.

use crate::coord::action::{NodeAction, NodeCtx, NodeOutput};
use crate::dag_flow::events::{
    next_sequence, now_ms, EventSink, RuntimeEvent, RuntimeEventEnvelope,
};
use crate::dag_flow::{BranchRegistry, DagNodeContextData};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitlApprovalRequest {
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitlDecision {
    pub approved: bool,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HitlResumeToken {
    pub run_id: String,
    pub branch_id: String,
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_key: Option<String>,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitlCheckpointSpec {
    pub run_id: String,
    pub branch_id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(default)]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_key: Option<String>,
    /// Optional timeout for human response. If unset, the checkpoint waits indefinitely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum HitlError {
    #[error("hitl runtime missing from DagExecutor services")]
    MissingRuntime,
    #[error("invalid hitl checkpoint spec: {0}")]
    InvalidSpec(String),
    #[error("approval already pending for token")]
    AlreadyPending,
    #[error("no pending approval for token")]
    NotPending,
}

#[derive(Clone)]
pub struct HitlRuntime {
    branches: BranchRegistry,
    sink: Option<Arc<dyn EventSink>>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<HitlDecision>>>>,
}

impl HitlRuntime {
    pub fn new(branches: BranchRegistry, sink: Option<Arc<dyn EventSink>>) -> Self {
        Self {
            branches,
            sink,
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn resume(&self, token: &HitlResumeToken, decision: HitlDecision) -> Result<(), HitlError> {
        let key = token_key(token);

        let sender = self
            .pending
            .lock()
            .remove(&key)
            .ok_or(HitlError::NotPending)?;
        let _ = sender.send(decision.clone());

        self.resume_branch(&token.run_id, &token.branch_id);
        self.emit_domain_event(
            &token.run_id,
            "hitl.resumed",
            json!({
                "branch_id": token.branch_id,
                "node_id": token.node_id,
                "token": token,
                "decision": decision,
            }),
        );

        Ok(())
    }

    fn register_pending(
        &self,
        token: &HitlResumeToken,
    ) -> Result<(String, oneshot::Receiver<HitlDecision>), HitlError> {
        let key = token_key(token);
        let (tx, rx) = oneshot::channel();

        let mut pending = self.pending.lock();
        if pending.contains_key(&key) {
            return Err(HitlError::AlreadyPending);
        }
        pending.insert(key.clone(), tx);
        Ok((key, rx))
    }

    fn cleanup_pending_key(&self, key: &str) {
        self.pending.lock().remove(key);
    }

    fn pause_branch(&self, run_id: &str, branch_id: &str, reason: Option<&str>) {
        self.branches.register_branch(branch_id.to_string());
        self.branches.pause_branch(branch_id, reason);
        self.emit(
            run_id,
            RuntimeEvent::BranchStateUpdated {
                branch_id: branch_id.to_string(),
                status: "paused".to_string(),
                reason: reason.map(|s| s.to_string()),
            },
        );
    }

    fn resume_branch(&self, run_id: &str, branch_id: &str) {
        self.branches.resume_branch(branch_id);
        self.emit(
            run_id,
            RuntimeEvent::BranchStateUpdated {
                branch_id: branch_id.to_string(),
                status: "running".to_string(),
                reason: None,
            },
        );
    }

    fn emit_domain_event(&self, run_id: &str, name: &str, payload: Value) {
        self.emit(
            run_id,
            RuntimeEvent::DomainEvent {
                name: name.to_string(),
                payload,
            },
        );
    }

    fn emit(&self, run_id: &str, event: RuntimeEvent) {
        if let Some(sink) = &self.sink {
            let envelope = RuntimeEventEnvelope {
                version: 2,
                sequence: next_sequence(),
                run_id: run_id.to_string(),
                timestamp: now_ms(),
                event,
            };
            sink.emit(&envelope);
        }
    }
}

fn token_key(token: &HitlResumeToken) -> String {
    match token.invocation_key.as_ref() {
        Some(k) => format!(
            "{}|{}|{}|{}",
            token.run_id, token.branch_id, token.node_id, k
        ),
        None => format!("{}|{}|{}", token.run_id, token.branch_id, token.node_id),
    }
}

struct PendingGuard {
    runtime: Arc<HitlRuntime>,
    key: String,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.runtime.cleanup_pending_key(&self.key);
    }
}

/// NodeAction that pauses and awaits a human decision.
///
/// Inputs: `HitlCheckpointSpec` JSON.
/// Outputs: `{ resume_token, approved, payload }`
#[derive(Debug, Clone)]
pub struct HitlCheckpointAction;

#[async_trait]
impl NodeAction for HitlCheckpointAction {
    fn name(&self) -> &str {
        "hitl_checkpoint"
    }

    async fn execute(&self, ctx: &NodeCtx) -> anyhow::Result<NodeOutput> {
        let spec: HitlCheckpointSpec = serde_json::from_value(ctx.inputs.clone()).map_err(|e| {
            anyhow::anyhow!(
                "hitl_checkpoint inputs must match HitlCheckpointSpec: {}",
                e
            )
        })?;

        if spec.run_id.trim().is_empty() {
            anyhow::bail!(HitlError::InvalidSpec("run_id is required".into()));
        }
        if spec.branch_id.trim().is_empty() {
            anyhow::bail!(HitlError::InvalidSpec("branch_id is required".into()));
        }
        if spec.prompt.trim().is_empty() {
            anyhow::bail!(HitlError::InvalidSpec("prompt is required".into()));
        }

        let data = DagNodeContextData::from_ctx(ctx)
            .ok_or_else(|| anyhow::anyhow!("DagNodeContextData missing from NodeCtx"))?;
        let runtime = data
            .services::<HitlRuntime>()
            .ok_or_else(|| anyhow::anyhow!(HitlError::MissingRuntime))?;

        let token = HitlResumeToken {
            run_id: spec.run_id.clone(),
            branch_id: spec.branch_id.clone(),
            node_id: ctx.node_id.clone(),
            invocation_key: spec.invocation_key.clone(),
            created_at_ms: now_ms(),
        };
        let request = HitlApprovalRequest {
            prompt: spec.prompt.clone(),
            schema: spec.schema.clone(),
            payload: spec.payload.clone(),
        };

        let (pending_key, rx) = runtime.register_pending(&token)?;
        let _guard = PendingGuard {
            runtime: runtime.clone(),
            key: pending_key,
        };

        runtime.pause_branch(&spec.run_id, &spec.branch_id, Some("needs_approval"));
        runtime.emit_domain_event(
            &spec.run_id,
            "hitl.needs_approval",
            json!({
                "branch_id": spec.branch_id,
                "node_id": ctx.node_id,
                "token": token,
                "request": request,
            }),
        );

        let decision = if let Some(ms) = spec.timeout_ms {
            match tokio::time::timeout(std::time::Duration::from_millis(ms), rx).await {
                Ok(Ok(d)) => d,
                Ok(Err(_closed)) => {
                    anyhow::bail!("hitl checkpoint cancelled (channel closed)");
                }
                Err(_elapsed) => {
                    anyhow::bail!("hitl checkpoint timed out after {}ms", ms);
                }
            }
        } else {
            rx.await
                .map_err(|_closed| anyhow::anyhow!("hitl checkpoint cancelled"))?
        };

        let outputs = json!({
            "resume_token": token,
            "approved": decision.approved,
            "payload": decision.payload,
        });

        Ok(NodeOutput::success(outputs))
    }
}

#[linkme::distributed_slice(crate::coord::registry::ACTION_REGISTRARS)]
pub static __HITL_CHECKPOINT_ACTION_REG: fn(&crate::coord::registry::ActionRegistry) = |registry| {
    registry.register(Arc::new(HitlCheckpointAction));
};
