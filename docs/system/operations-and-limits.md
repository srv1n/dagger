---
subject: Operations and limits
keywords: [limits, budgets, approvals, security, deployment]
part_of: Workflow core
describes: [dagger-workflow-core/src/run.rs, dagger-workflow-core/src/engine.rs, dagger-workflow-core/src/approval.rs, src/core/limits.rs, src/work_queue/exec.rs]
status: canonical
created: 2026-08-23
last_verified: "2026-08-23 @ bdd41fa"
read_when: "You configure a host or set safety limits."
skip_when: "You only need build commands."
---

# Operations and limits

This page lists the limits and safety rules that a host must set.

## Workflow-core run limits

Set all seven `RunLimits` values when you create a run:

| Limit | Purpose |
| --- | --- |
| `max_dynamic_node_instances` | Limits Map children. |
| `max_total_attempts` | Limits started attempts. |
| `max_total_events` | Limits durable events. |
| `max_inline_json_bytes_per_value` | Limits one canonical JSON value. |
| `max_artifacts_per_attempt` | Limits artifacts from one attempt. |
| `max_aggregate_object_bytes_per_run` | Limits bytes charged to one run. |
| `max_run_lifetime_ms` | Limits run lifetime by the database clock. |

Set the run budget separately. An action declares its maximum cost. The store reserves cost before execution and settles cost after completion.

Set `EngineConfig.max_concurrency` to a positive value. This value limits action calls in one process. A Map also has `max_concurrency` and `max_items`.

## Root-runtime limits

The root runtime has `ResourceLimits`. The default values are code defaults, not universal production values.

Call `validate` after you change them. Use a smaller profile for tests.

## Approvals

An approval definition names allowed principal IDs or role IDs. It also sets an expiry time and an expiry result.

The host authenticates the principal. The engine checks the durable authorization policy.

Do not accept an unauthenticated user name from an action payload.

## Command execution

The Work Queue exec host denies a system executable until the host adds it to the allowlist.

Use argument arrays. Do not build a shell command string from user input.

Set time and output-byte limits for each command.

## Secrets

Keep secrets outside workflow definitions and durable events.

Pass a secret through a host-owned credential system. Do not put it in JSON input, diagnostics, or an artifact unless the product explicitly treats that storage as secret storage.

## Deployment boundary

SQLite and `FsObjectStore` support an embedded local deployment shape.

The traits permit other backends. The repository does not contain a cloud object-store backend or a distributed control-plane store.

Test the exact storage and failover topology before you claim production durability.
