---
subject: overview
keywords: [dagger, architecture, documentation]
part_of:
describes: [Cargo.toml, src, dagger-workflow-core, dagger-macros]
status: canonical
created: 2026-08-23
last_verified: "2026-08-23 @ bdd41fa"
read_when: "You need the top-level map of this repository."
skip_when: "You need a command only; read Getting started."
---

# System overview

Dagger is one Rust workspace. The workspace contains two workflow engines.

The engines have different data models. The engines have different public APIs. They do not call each other.

```text
Host application
    |
    +-- dagger 0.0.1
    |     +-- DAG Flow
    |     +-- Task Agent
    |     +-- Pub/Sub
    |     +-- Work Queue
    |
    +-- dagger-workflow-core 0.1.0
          +-- strict workflow definitions
          +-- bounded scheduler
          +-- memory or SQLite control state
          +-- memory or file-system objects
```

## Select an engine

Use the Dagger runtime when you maintain existing code in `src/`.

Use workflow core when you need these properties:

- A strict JSON or YAML definition.
- A closed set of node types.
- An immutable published revision.
- A tenant and namespace scope on each durable key.
- A budget and explicit run limits.
- Durable retries, approvals, events, and recovery.
- Content-addressed objects.

Do not call workflow core a replacement for the Dagger runtime. The repository does not contain a migration adapter.

## Main boundaries

The host owns process startup, identity, secrets, and deployment policy.

The engine owns workflow state transitions.

The action registry owns executable action implementations.

The control-plane store owns definitions, runs, nodes, attempts, gates, receipts, and events.

The object store owns immutable bytes. A SHA-256 digest identifies these bytes.

## Documentation rules

These pages use Simplified Technical English as a practical writing rule.

- Use short sentences.
- Use active voice.
- Put one action in each instruction.
- Define a term before you use it.
- Use the same word for the same thing.
- State a limit with a number or a source path.
- Separate implemented behavior from planned behavior.

The source code and tests are the final authority for implemented behavior.

<!-- tusker:docs-map:begin -->
```mermaid
graph TD
  n_Getting_started["Getting started"]
  n_Glossary["Glossary"]
  n_Legacy_Dagger_runtime["Legacy Dagger runtime"]
  n_Operations_and_limits["Operations and limits"]
  n_Repository_structure["Repository structure"]
  n_Storage_and_durability["Storage and durability"]
  n_Testing_and_development["Testing and development"]
  n_Workflow_core["Workflow core"]
  n_Workflow_definitions["Workflow definitions"]
  n_overview["overview"]
  n_overview --> n_Getting_started
  n_overview --> n_Glossary
  n_overview --> n_Legacy_Dagger_runtime
  n_Workflow_core --> n_Operations_and_limits
  n_overview --> n_Repository_structure
  n_Workflow_core --> n_Storage_and_durability
  n_overview --> n_Testing_and_development
  n_overview --> n_Workflow_core
  n_Workflow_core --> n_Workflow_definitions
```
<!-- tusker:docs-map:end -->
