---
subject: Workflow definitions
keywords: [yaml, json, schema, nodes, bindings]
part_of: Workflow engine
describes: [dagger-workflow-core/src/definition, dagger-workflow-core/schema, docs/reference_workflows]
status: canonical
created: 2026-08-23
last_verified: 2026-08-23 @ 94e8bc4543ccf2c8f57c30715071e7c0b9352b57
read_when: "You write or validate a Dagger definition."
skip_when: "You need scheduler state; read Workflow engine."
---

# Workflow definitions

Dagger accepts JSON and YAML. Both inputs create the same workflow data.

The Rust type is `WorkflowDefinition`.

The required `definition_format_version` value is `0.1`. This value identifies the data format. It is not a Dagger product generation.

The machine-readable schema is `dagger-workflow-core/schema/workflow-definition-0.1.json`.

## Publication stages

Publication has two stages.

1. `validate_definition` checks syntax, IDs, fields, limits, graph edges, reachability, bindings, and terminal paths.
2. `resolve_publication` resolves exact schemas, literal artifacts, and action pins. The host then calls `WorkflowStore::publish_revision` with the immutable revision.

Parsing a file is not publication. The host must supply the resolver data for stage 2.

## Root fields

A definition has these root fields:

- `definition_format_version`.
- `definition_id`.
- `name`.
- Optional `description`.
- `run_input_schema_digest`.
- `run_output_schema_digest`.
- `nodes`.

The parser rejects unknown structural fields and duplicate keys. The YAML parser also rejects merge keys and more than one document.

## Graph rules

The validator requires these rules:

- Node IDs are unique.
- Each edge names an existing node.
- The graph has no cycle.
- A `node_output` source creates one dependency edge from its named node. That edge makes the
  consumer reachable and ready when its source output exists; it does not need a duplicate `next`.
- Constant and run-input sources do not create dependency edges.
- Literal and run-input artifact references do not create dependency edges.
- The root set contains every node with no incoming output-reference edge and no incoming
  control-activation edge.
- Each required node is reachable from the derived root set.
- Exactly one reachable node is `Succeed`.
- Every reachable path ends at `Succeed` or `Fail`.
- A binding can read only allowed upstream data.

The definition does not contain `entry_node_id` or `depends_on`. Dagger keeps every authored
root. It does not add a start node. Canonical topological ranks use lexical Kahn order over the
output-reference edges, so source file order does not change readiness order.

## Bindings

An ordinary binding can read from these sources:

- A constant JSON value.
- The immutable run input.
- A successful upstream node output.
- One selected field from every successful child output of a named Map.
- A deterministic object or array assembled from other closed sources.
- A typed artifact reference.

A Map child can also read the current item and its zero-based index.

Bindings use RFC 6901 JSON pointers. The validator rejects overlapping target pointers.

A Choice outcome names one guarded node. `kind: node` activates it; `kind: skip` marks it skipped.

## Definition limits

| Item | Enforced limit |
| --- | --- |
| Canonical definition bytes | At most 4 MiB. |
| Nodes | At most 1,024. |
| Map items | At most 10,000. |
| Map concurrency | At most 1,024 and not more than the item count. |
| Choice cases | At most 100. |
| Approval allowlist entries | At most 256 principal IDs and 256 role IDs. |
| `retry.max_attempts` | From 1 through 100. |
| Action timeout | From 1 ms through 86,400,000 ms. |

The parser also limits YAML depth, aliases, anchors, scalar bytes, events, and expansions. See the constants in `dagger-workflow-core/src/definition/mod.rs` for the exact parser ceilings.

## Action pins

Each `Action` and Map action has five exact pin fields. The engine requires an exact registry match before it starts or resumes the run.

The example YAML files use zero digests as authoring placeholders. Example code replaces these values before publication. Do not publish a definition with placeholder digests.

## Opaque schema values

Use the empty schema object `{}` as the official opaque marker. It accepts any JSON value. It means Dagger does not statically check the value's shape at that schema node.

## Reference definitions

Tests load these files:

- `docs/reference_workflows/intel_digest.yaml`.
- `docs/reference_workflows/legal_research.yaml`.

Treat these files as executable fixtures. Run their tests after a change.
