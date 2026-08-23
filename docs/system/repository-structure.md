---
subject: Repository structure
keywords: [folders, crates, modules, layout]
part_of: overview
describes: [Cargo.toml, src, tests, examples, dagger-macros, dagger-workflow-core]
status: canonical
created: 2026-08-23
last_verified: "2026-08-23 @ bdd41fa"
read_when: "You need to find the owner of code or tests."
skip_when: "You already know the target module."
---

# Repository structure

This page shows the owner of each repository area.

| Path | Owner | Notes |
| --- | --- | --- |
| `src/` | Dagger runtime | The older runtime and its public exports. |
| `src/dag_flow/` | DAG Flow | YAML graphs, scheduling, cache use, and runtime events. |
| `src/taskagent/` | Task Agent | The wrapper around the `task-core` package. |
| `src/taskagent/task-core/` | Task Core | Durable tasks, the scheduler, agents, and SQLite storage. |
| `src/pubsub/` | Pub/Sub | Messages, agents, channels, and execution. |
| `src/work_queue/` | Work Queue | Command execution, batches, pipelines, and approval checkpoints. |
| `src/storage/` | Dagger storage | SQLite records for DAG Flow and related state. |
| `dagger-macros/` | Macros | `action`, `task_agent`, and `pubsub_agent`. |
| `dagger-workflow-core/` | Workflow core | The bounded workflow engine. |
| `examples/` | Dagger examples | Examples for the older runtime. |
| `dagger-workflow-core/examples/` | Workflow-core examples | Examples for the bounded engine. |
| `tests/` | Dagger tests | Integration tests for the older runtime. |
| `dagger-workflow-core/tests/` | Workflow-core tests | Contract and durability tests. |
| `docs/system/` | Human documentation | Canonical documentation with Tusker metadata. |
| `.tusker/` | Task and project knowledge | Tusker records and canonical project knowledge. |

## Workspace packages

The workspace manifest defines four packages:

- `dagger` version 0.0.1.
- `dagger-workflow-core` version 0.1.0.
- `dagger-macros` version 0.1.0.
- `task-core` version 0.1.0.

The `examples/*` directories are not workspace members. Cargo still builds the root examples through the root package.

## Change rules

Select one engine before you change code.

Change both engines only when the request names both engines or when a shared build rule requires the change.

Add a workflow-core test in `dagger-workflow-core/tests/`. Add a Dagger runtime test in `tests/` or next to the owning module.
