---
subject: Operations and limits
keywords: [limits, budgets, approvals, deployment]
part_of: Workflow engine
describes: [dagger-workflow-core/src/run.rs, dagger-workflow-core/src/engine.rs, dagger-workflow-core/src/approval.rs, dagger-workflow-core/src/sqlite/reducer.rs]
status: canonical
created: 2026-08-23
last_verified: 2026-08-23 @ ef3df5d2232f1a7b2365b99287e80f31b7d510ee
read_when: "You configure a host or set safety limits."
skip_when: "You only need build commands."
---

# Operations and limits

The host must set all run limits. Dagger does not supply production defaults.

## Run limits

`RunLimits` has exactly seven values:

| Limit | Valid range |
| --- | --- |
| `max_dynamic_node_instances` | 0 through 100,000. |
| `max_total_attempts` | 1 through 1,000,000. |
| `max_total_events` | 1 through 10,000,000. |
| `max_inline_json_bytes_per_value` | 1 through 16,777,216. |
| `max_artifacts_per_attempt` | 0 through 1,024. |
| `max_aggregate_object_bytes_per_run` | 1 through 68,719,476,736. |
| `max_run_lifetime_ms` | 1 through 31,536,000,000. |

The store validates these values when it creates the run. The values do not change during the run.

Set `EngineConfig.max_concurrency` to a value greater than zero. This value limits action calls in one process.

## Budget

Set the run budget when you create the run.

An action declares its maximum cost. The store reserves cost before execution. It settles the reservation after completion. Unknown outcomes also settle through the durable budget ledger.

## Approvals

An approval policy has an expiry time, an expiry result, and an allowlist of principal IDs or role IDs.

The host authenticates the principal. Dagger checks the durable allowlist. The first valid decision closes the gate.

Do not accept an unauthenticated name from workflow input as identity.

## Secrets

Dagger has no secret store and no secret scanner.

Use a host-owned credential system. Do not put raw secrets in definitions, ordinary inputs, diagnostics, events, or ordinary artifacts.

## Deployment boundary

SQLite with `FsObjectStore` supports an embedded local deployment shape.

The store traits permit other implementations. Add a backend and backend-specific evidence before you claim support for another topology.
