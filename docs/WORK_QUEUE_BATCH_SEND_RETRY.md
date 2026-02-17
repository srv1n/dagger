# Example: Batch send + partial failure + retry

This example demonstrates the intended **Work Queue** execution pattern:

- One Dagger run per user action (“Send” click)
- Per-item isolation (fanout item → step ids)
- Partial failure without stopping unrelated items
- Retrying **only failed items** deterministically

Artifacts:
- Example code: `examples/work_queue_batch_send_retry.rs`
- Sample spec: `examples/work_queue_batch_send_retry_spec.yaml`

## Run it

```bash
cargo run --example work_queue_batch_send_retry
```

Optional: pass an alternate spec path:

```bash
cargo run --example work_queue_batch_send_retry -- examples/work_queue_batch_send_retry_spec.yaml
```

## What it does

1) Loads a `BatchFanoutSpec` from YAML.
2) Runs the batch with `continue_on_error: true`.
3) Computes `failed_item_ids` by combining:
   - `BatchExecution.plan.item_steps` (item → step_id mapping)
   - `DagExecutionReport.node_outcomes` (step_id → success)
4) Re-runs with `selected_item_ids = failed_item_ids` and a larger `max_stdout_bytes` cap so the
   previously failing items succeed.

The included YAML spec intentionally sets a small `max_stdout_bytes`. Items with very long
`item_id`s produce longer JSON output, which gets truncated; JSON parsing fails; those items fail
while shorter items succeed. This gives a deterministic “partial failure” demo without shell hacks.

## Where approvals fit (stage vs approve+send)

Typical host flow:

1) **Stage** run (optional): compute what would be sent, validate payloads, write approval objects.
2) **Send** run: call `execute_batch(...)` with:
   - `selected_item_ids`: the UI selection
   - `approved_revision_ids`: approvals persisted by the host (or from the stage run)

If you need interactive human approval before side effects, insert a `hitl_checkpoint` step before
the `exec` send step. The host listens for `hitl.needs_approval` events and resumes via
`HitlRuntime::resume(token, decision)`.

