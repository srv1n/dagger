//! Exec primitive for Work Queue execution.
//!
//! Goals:
//! - Structured spec/result (no implicit shell interpolation).
//! - Strict caps (timeouts, output sizes) enforced centrally.
//! - Host-controlled policy: allowlists, sidecar resolution, approval gating.

use crate::coord::action::{NodeAction, NodeCtx, NodeOutput};
use crate::dag_flow::DagNodeContextData;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecKind {
    /// Execute using the system environment (typically requires an allowlist).
    System,
    /// Execute a "sidecar" binary managed by the host app.
    Sidecar,
}

fn default_timeout_ms() -> u64 {
    30_000
}

fn default_max_stdout_bytes() -> u64 {
    64 * 1024
}

fn default_max_stderr_bytes() -> u64 {
    64 * 1024
}

/// Structured exec specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecSpec {
    pub kind: ExecKind,
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub stdin: Option<String>,
    /// JSON stdin convenience for pipeline wiring.
    ///
    /// If set, this value is serialized as JSON and provided to the process stdin.
    /// Mutually exclusive with `stdin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin_json: Option<serde_json::Value>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_max_stdout_bytes")]
    pub max_stdout_bytes: u64,
    #[serde(default = "default_max_stderr_bytes")]
    pub max_stderr_bytes: u64,
    #[serde(default)]
    pub parse_stdout_as_json: bool,
}

impl ExecSpec {
    pub fn validate(&self) -> Result<(), ExecError> {
        if self.executable.trim().is_empty() {
            return Err(ExecError::InvalidSpec("executable is required".into()));
        }
        if self.stdin.is_some() && self.stdin_json.is_some() {
            return Err(ExecError::InvalidSpec(
                "stdin and stdin_json are mutually exclusive".into(),
            ));
        }
        if self.timeout_ms == 0 {
            return Err(ExecError::InvalidSpec("timeout_ms must be > 0".into()));
        }
        Ok(())
    }
}

/// Structured exec result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_json: Option<serde_json::Value>,
    pub truncated: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ResolvedExec {
    pub executable: OsString,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("exec denied: {0}")]
    Denied(String),
    #[error("unknown sidecar: {0}")]
    SidecarNotFound(String),
    #[error("invalid exec spec: {0}")]
    InvalidSpec(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("stdout is not valid utf-8")]
    StdoutNotUtf8,
    #[error("stdout is not valid json: {0}")]
    StdoutNotJson(String),
}

/// Host-controlled policy and resolution for exec.
pub trait ExecHost: Send + Sync {
    fn resolve(&self, spec: &ExecSpec) -> Result<ResolvedExec, ExecError>;
}

/// Simple host implementation: allowlisted system commands + registered sidecars.
#[derive(Debug, Default, Clone)]
pub struct LocalExecHost {
    system_allowlist: HashSet<OsString>,
    sidecars: HashMap<String, PathBuf>,
}

impl LocalExecHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow_system(mut self, executable: impl AsRef<OsStr>) -> Self {
        self.system_allowlist
            .insert(executable.as_ref().to_os_string());
        self
    }

    pub fn register_sidecar(mut self, name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        self.sidecars.insert(name.into(), path.into());
        self
    }

    fn is_system_allowed(&self, executable: &OsStr) -> bool {
        self.system_allowlist.contains(executable)
    }
}

impl ExecHost for LocalExecHost {
    fn resolve(&self, spec: &ExecSpec) -> Result<ResolvedExec, ExecError> {
        spec.validate()?;

        match spec.kind {
            ExecKind::System => {
                let exe = OsString::from(&spec.executable);
                if !self.is_system_allowed(OsStr::new(&spec.executable)) {
                    return Err(ExecError::Denied(format!(
                        "system executable not allowlisted: {}",
                        spec.executable
                    )));
                }
                Ok(ResolvedExec { executable: exe })
            }
            ExecKind::Sidecar => {
                let path = self
                    .sidecars
                    .get(&spec.executable)
                    .cloned()
                    .ok_or_else(|| ExecError::SidecarNotFound(spec.executable.clone()))?;
                Ok(ResolvedExec {
                    executable: path.into_os_string(),
                })
            }
        }
    }
}

/// Services wrapper to make an `ExecHost` accessible through `DagExecutor::set_services(...)`.
#[derive(Clone)]
pub struct ExecServices {
    pub host: Arc<dyn ExecHost>,
}

async fn read_stream_limited<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> Result<(Vec<u8>, bool), std::io::Error> {
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;

    let mut scratch = [0u8; 8192];
    loop {
        let n = reader.read(&mut scratch).await?;
        if n == 0 {
            break;
        }

        if limit == 0 {
            truncated = true;
            continue;
        }

        if buf.len() < limit {
            let remaining = limit - buf.len();
            let take = remaining.min(n);
            buf.extend_from_slice(&scratch[..take]);
            if take < n {
                truncated = true;
            }
        } else {
            truncated = true;
        }
    }

    Ok((buf, truncated))
}

fn effective_timeout_ms(spec: &ExecSpec, ctx: &NodeCtx) -> u64 {
    let mut timeout_ms = spec.timeout_ms;

    if let Some(data) = DagNodeContextData::from_ctx(ctx) {
        // Node.timeout is seconds; always enforce the smaller limit.
        let node_timeout_ms = data.node.timeout.saturating_mul(1000);
        timeout_ms = timeout_ms.min(node_timeout_ms.max(1));
    }

    timeout_ms.max(1)
}

async fn run_exec(host: &dyn ExecHost, spec: &ExecSpec, ctx: &NodeCtx) -> ExecResult {
    let started_at = Instant::now();

    let stdin_bytes = match (spec.stdin.as_ref(), spec.stdin_json.as_ref()) {
        (Some(s), None) => Some(s.as_bytes().to_vec()),
        (None, Some(v)) => match serde_json::to_vec(v) {
            Ok(mut bytes) => {
                // Newline is a convenience for many CLI tools and makes logs nicer.
                bytes.push(b'\n');
                Some(bytes)
            }
            Err(e) => {
                return ExecResult {
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("stdin_json serialize failed: {}", e),
                    stdout_json: None,
                    truncated: false,
                    duration_ms: started_at.elapsed().as_millis() as u64,
                };
            }
        },
        (None, None) => None,
        (Some(_), Some(_)) => {
            return ExecResult {
                exit_code: -1,
                stdout: String::new(),
                stderr: "invalid ExecSpec: stdin and stdin_json are mutually exclusive".to_string(),
                stdout_json: None,
                truncated: false,
                duration_ms: started_at.elapsed().as_millis() as u64,
            };
        }
    };

    let resolved = match host.resolve(spec) {
        Ok(resolved) => resolved,
        Err(e) => {
            return ExecResult {
                exit_code: -1,
                stdout: String::new(),
                stderr: e.to_string(),
                stdout_json: None,
                truncated: false,
                duration_ms: started_at.elapsed().as_millis() as u64,
            };
        }
    };

    let max_stdout_bytes = spec.max_stdout_bytes as usize;
    let max_stderr_bytes = spec.max_stderr_bytes as usize;
    let timeout_ms = effective_timeout_ms(spec, ctx);

    let mut cmd = Command::new(&resolved.executable);
    cmd.args(&spec.args);

    if let Some(cwd) = spec.cwd.as_ref() {
        cmd.current_dir(cwd);
    }

    for (k, v) in &spec.env {
        cmd.env(k, v);
    }

    if stdin_bytes.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    } else {
        cmd.stdin(std::process::Stdio::null());
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return ExecResult {
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("spawn failed: {}", e),
                stdout_json: None,
                truncated: false,
                duration_ms: started_at.elapsed().as_millis() as u64,
            };
        }
    };

    // Write stdin, if provided.
    if let Some(stdin_bytes) = stdin_bytes {
        if let Some(mut stdin) = child.stdin.take() {
            tokio::spawn(async move {
                let _ = stdin.write_all(&stdin_bytes).await;
                let _ = stdin.shutdown().await;
            });
        }
    }

    type StreamReadTask = Option<JoinHandle<Result<(Vec<u8>, bool), std::io::Error>>>;

    let stdout_task: StreamReadTask = child
        .stdout
        .take()
        .map(|stdout| tokio::spawn(read_stream_limited(stdout, max_stdout_bytes)));
    let stderr_task: StreamReadTask = child
        .stderr
        .take()
        .map(|stderr| tokio::spawn(read_stream_limited(stderr, max_stderr_bytes)));

    let wait_result = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await;

    let (exit_code, stderr_extra) = match wait_result {
        Ok(Ok(status)) => (status.code().unwrap_or(-1), None),
        Ok(Err(e)) => (-1, Some(format!("wait failed: {}", e))),
        Err(_) => {
            // Timeout: kill best-effort.
            let _ = child.kill().await;
            let _ = child.wait().await;
            (-1, Some(format!("timed out after {}ms", timeout_ms)))
        }
    };

    let (stdout_bytes, stdout_truncated) = match stdout_task {
        Some(handle) => match handle.await {
            Ok(Ok(v)) => v,
            Ok(Err(_e)) => (Vec::new(), true),
            Err(_) => (Vec::new(), true),
        },
        None => (Vec::new(), false),
    };
    let (stderr_bytes, stderr_truncated) = match stderr_task {
        Some(handle) => match handle.await {
            Ok(Ok(v)) => v,
            Ok(Err(_e)) => (Vec::new(), true),
            Err(_) => (Vec::new(), true),
        },
        None => (Vec::new(), false),
    };

    let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
    let mut stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
    if let Some(extra) = stderr_extra {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&extra);
    }

    // Optional stdout_json parsing.
    let stdout_json = if spec.parse_stdout_as_json {
        match std::str::from_utf8(&stdout_bytes) {
            Ok(s) => match serde_json::from_str::<serde_json::Value>(s) {
                Ok(v) => Some(v),
                Err(e) => {
                    if !stderr.is_empty() && !stderr.ends_with('\n') {
                        stderr.push('\n');
                    }
                    stderr.push_str(&format!("stdout_json parse failed: {}", e));
                    None
                }
            },
            Err(_) => {
                if !stderr.is_empty() && !stderr.ends_with('\n') {
                    stderr.push('\n');
                }
                stderr.push_str("stdout_json parse failed: stdout not valid utf-8");
                None
            }
        }
    } else {
        None
    };

    let truncated = stdout_truncated || stderr_truncated;

    ExecResult {
        exit_code,
        stdout,
        stderr,
        stdout_json,
        truncated,
        duration_ms: started_at.elapsed().as_millis() as u64,
    }
}

/// NodeAction that executes an `ExecSpec` from inputs and returns `ExecResult`.
///
/// Inputs: JSON object matching `ExecSpec`.
/// Outputs: JSON object matching `ExecResult`.
#[derive(Debug, Clone)]
pub struct ExecAction;

#[async_trait]
impl NodeAction for ExecAction {
    fn name(&self) -> &str {
        "exec"
    }

    async fn execute(&self, ctx: &NodeCtx) -> anyhow::Result<NodeOutput> {
        let spec: ExecSpec = serde_json::from_value(ctx.inputs.clone())
            .map_err(|e| anyhow::anyhow!("exec inputs must match ExecSpec: {}", e))?;

        let data = DagNodeContextData::from_ctx(ctx)
            .ok_or_else(|| anyhow::anyhow!("DagNodeContextData missing from NodeCtx"))?;
        let services = data
            .services::<ExecServices>()
            .ok_or_else(|| anyhow::anyhow!("ExecServices missing from DagExecutor services"))?;

        let result = run_exec(services.host.as_ref(), &spec, ctx).await;
        let ok =
            result.exit_code == 0 && (!spec.parse_stdout_as_json || result.stdout_json.is_some());

        let outputs = serde_json::to_value(&result)
            .map_err(|e| anyhow::anyhow!("failed to serialize ExecResult: {}", e))?;

        if ok {
            Ok(NodeOutput::success(outputs))
        } else {
            Ok(NodeOutput {
                outputs: Some(outputs),
                success: false,
                metadata: Some(serde_json::json!({
                    "error": "exec failed",
                    "exit_code": result.exit_code,
                    "executable": spec.executable,
                    "kind": spec.kind,
                    "cwd": spec.cwd,
                    "args": spec.args,
                    "truncated": result.truncated,
                })),
            })
        }
    }
}

#[linkme::distributed_slice(crate::coord::registry::ACTION_REGISTRARS)]
pub static __EXEC_ACTION_REG: fn(&crate::coord::registry::ActionRegistry) = |registry| {
    registry.register(Arc::new(ExecAction));
};
