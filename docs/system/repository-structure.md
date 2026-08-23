---
subject: Repository structure
keywords: [folders, crate, modules, layout]
part_of: System overview
describes: [Cargo.toml, dagger-workflow-core, docs/system, .tusker]
status: canonical
created: 2026-08-23
last_verified: 2026-08-23 @ 94e8bc4543ccf2c8f57c30715071e7c0b9352b57
read_when: "You need to find the owner of code or tests."
skip_when: "You already know the target module."
---

# Repository structure

The workspace has one Cargo package.

| Path | Contents |
| --- | --- |
| `dagger-workflow-core/src/` | Public types, the scheduler, store traits, and store implementations. |
| `dagger-workflow-core/schema/` | The workflow definition JSON Schema. |
| `dagger-workflow-core/examples/` | Complete host examples. |
| `dagger-workflow-core/tests/` | Integration tests for definitions, execution, storage, recovery, approvals, budgets, and integrity. |
| `docs/reference_workflows/` | YAML fixtures that tests load. |
| `docs/system/` | Canonical human documentation. |
| `.tusker/` | Task records, proof, gates, and project knowledge. |
| `.github/workflows/ci.yml` | CI checks. |

## Source modules

| Module | Responsibility |
| --- | --- |
| `definition` | Parse, validate, resolve, and publish definitions. |
| `engine` | Start and advance runs. |
| `action` | Register and call exact action implementations. |
| `run` | Define run, node, attempt, and limit state. |
| `store` | Define atomic workflow-store commands and reads. |
| `memory` | Provide process-local stores. |
| `sqlite` | Provide the optional SQLite control store. |
| `artifact` | Define object references and the object-store boundary. |
| `fs_object_store` | Store immutable objects on a local file system. |
| `committed_read` | Read committed objects and handle proven corruption. |
| `approval` | Define approval policies and decisions. |
| `event` | Define durable workflow events. |
| `budget` | Define budget reservations and settlements. |
| `scope` | Define tenant and namespace isolation. |

Add a test in `dagger-workflow-core/tests/`. Add a small unit test next to a module only when the behavior is local to that module.
