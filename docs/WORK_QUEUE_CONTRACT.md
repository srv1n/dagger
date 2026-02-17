# Work Queue Contract (Dagger ↔ Host)

This document defines the **stable integration contract** between Dagger and a host application
that drives *Work Queue* style execution (e.g. multi-select approve/send/retry).

Scope:
- Deterministic batch fanout (one run → per-item branches/steps)
- Structured exec primitive with guardrails (system vs sidecar)
- Runtime events semantics (`run_id` vs `branch_id`) + domain events
- HITL checkpoints + resume token shape (host responsibilities)

The contract is intentionally **host-agnostic**: Dagger provides primitives and extension points;
the host enforces enterprise policy (allowlists, sidecar distribution, approvals).

## Identifiers (audit + determinism)

- `run_id`: a single orchestration run (one user action, one “Send” click).
- `branch_id`: a sub-run identifier (e.g. a per-item fanout branch).
- `step_id`: a deterministic identifier for nodes/steps inside a branch.

Host guidance:
- Persist `run_id`, `branch_id`, `step_id` in your audit log to support retries and timelines.
- Prefer deterministic IDs (same inputs → same ids) so retries are easy to correlate.

## Exec primitive (`work_queue::exec`)

Exec is a structured alternative to `"sh -c"` pipelines. It runs one executable with explicit args and
enforced resource caps.

### Spec

`ExecSpec` (JSON shape; field names are stable):
- `kind`: `"system"` or `"sidecar"`
- `executable`: string (path or sidecar name)
- `args`: string array (passed verbatim, no shell interpolation)
- `cwd`: optional string
- `env`: optional `{ string: string }`
- `stdin`: optional string
- `stdin_json`: optional JSON value (serialized to stdin; mutually exclusive with `stdin`)
- `timeout_ms`: optional integer
- `max_stdout_bytes`: optional integer
- `max_stderr_bytes`: optional integer
- `parse_stdout_as_json`: optional bool

### Result

`ExecResult`:
- `exit_code`: integer
- `stdout`: string (possibly truncated)
- `stderr`: string (possibly truncated)
- `stdout_json`: optional JSON value (when `parse_stdout_as_json=true`)
- `truncated`: bool (true if stdout/stderr caps were hit)
- `duration_ms`: integer

### Host policy hooks

The host provides an `ExecHost` implementation (via `DagExecutor::set_services(...)`) to enforce:
- **System command allowlisting** (which binaries are permitted).
- **Sidecar resolution** (mapping a sidecar name → an executable path + args prefix).
- Optional approval gating prior to exec (policy lives in the host).

If you need multiple independent services (e.g. `ExecServices` + `HitlRuntime`), store a
`ServiceRegistry` in `DagExecutor::set_services(...)` and insert each typed service into it.

## Batch fanout helper (`work_queue::batch`)

Batch execution turns “selected items” into deterministic per-item step chains.

### Inputs

`BatchFanoutSpec`:
- `container_id`: string
- `selected_item_ids`: string array
- `approved_revision_ids`: string array (optional host-provided context)
- `steps`: array of `PerItemStepTemplate`
  - `name`: string (human name; used for stable id generation)
  - `action`: string (e.g. `"exec"`)
  - `inputs`: JSON object (templated strings allowed)

Templating:
- String values in `inputs` may include placeholders:
  - `{{container_id}}`
  - `{{item_id}}`
  - `{{step_name}}`

### Outputs

`BatchExecution.plan.item_steps` provides a stable mapping:
`item_id -> [ { step_name, step_id } ]`

Retry contract:
- To retry only failures, re-run `execute_batch(...)` with `selected_item_ids` set to the failed item ids.
- Because step ids are deterministic, host timelines can correlate original attempts and retries.

## Pipeline compiler (`work_queue::pipeline`)

Pipelines compile a list of exec steps into a DAG with explicit dependencies and auditable piping.

Key properties:
- No opaque shell pipelines: every step is an `exec` node.
- Deterministic node ids based on step index + name.
- Supports explicit fanout via `deps` (parallel steps when no dependency exists).

Piping modes:
- Text: `prev.stdout` → `next.stdin`
- JSON: `prev.stdout_json` → `next.stdin_json` (requires `prev.exec.parse_stdout_as_json = true`)

Node id mapping:
- Step at index `i` compiles to node id `pipe_{i}_{sanitized_step_name}`.
- The index is always present to make audits and retries easy to correlate with the pipeline spec.

## Runtime events (V2)

Events are emitted via `dag_flow::events::EventSink` for host UI timelines and audit.

Semantics:
- `RuntimeEventEnvelope.run_id` is the *flow/run identifier*.
- `RuntimeEvent::BranchStateUpdated.branch_id` is the *branch identifier*.
- `RuntimeEvent::*step_id*` fields should be deterministic and stable where possible.

Host-defined domain events:
- Use `DagExecutor::emit_domain_event(run_id, name, payload)` for audited application events
  (e.g. `work_item.state_changed`) without overloading tool/step events.

## HITL checkpoints + resume tokens (contract)

Some workflows require **human-in-the-loop** approval before side effects occur.

Contract:
- Use the `hitl_checkpoint` action to pause a branch and request approval.
- Dagger pauses the relevant branch before executing the side-effectful operation (place checkpoint before side effects).
- The host persists the request payload (for UI) and calls resume with a **resume token** plus the user’s decision/input.

Resume token shape (stable JSON fields):
- `run_id`: string
- `branch_id`: string
- `node_id`: string (the checkpoint node that requested approval)
- `invocation_key`: optional string (to disambiguate deterministic invocations)
- `created_at_ms`: integer (for auditing/expiry policies)

Host responsibilities:
- Persist the resume token and approval payload durably.
- Render a UI for approval (and optional editable inputs).
- Call `HitlRuntime::resume(token, decision)` with the same token and the approval decision payload.
- Enforce expiration / replay policies if required by your security model.
