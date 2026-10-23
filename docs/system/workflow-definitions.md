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

Maps accept optional `on_child_failure: continue`, `min_successful_children`,
and `item_key_pointer`. The minimum defaults to the actual item count. The key
pointer selects a unique string within each item for part locks. Omit these
fields to keep legacy behavior.

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

## Action manifests

The action manifest loader accepts JSON and YAML. Entries declare a name,
contract version, input and output schema documents, implementation digest,
optional `memoize`, and optional `required_capabilities`. Canonical JSON supplies
schema digests. The loader checks the publication schema subset by default. A host factory
supplies implementations whose descriptors match each entry's five pins.
Multiple entries can share command code while keeping distinct descriptors.

Bulk registration validates all entries before changing the registry. Schema
publication supplies objects for the ordinary publication resolver. Capability
tags are metadata in this local milestone. They do not dispatch remote workers.

## Host-validated schema bundles

The host can explicitly select `SchemaValidationMode::Opaque` for JSON Schema
2020-12 documents outside Dagger's supported validation subset. The canonical
`dagger.opaque-schema-bundle/v1` envelope retains the original documents and
their complete reference closure. No schema text is rewritten and no network
resolution is performed. An unresolved reference rejects publication. Changing
a referenced document changes every consuming bundle digest. Unused documents
are excluded.

Dagger verifies the bundle structure, bytes, and digest. **The host action must
validate input and output payloads.** Static binding checks treat opaque shapes
as unknown. The same responsibility applies to host-validated run input/output
and approval payload schemas if the host selects this mode for them. Unsupported
unmarked schemas still reject; the existing default validation is unchanged.

Bundle digests differ from raw-document digests. The loader checks any declared
raw schema digests, then returns action references and run-schema digests for
the canonical bundles. Use these returned pins when publishing.

## Structured decisions

An Approval gate can enable `decision_output` and declare a
`decision_schema_digest` for its human JSON payload. Publish that schema through
the approval-schema API. Both approve and reject produce a successful decision
result in this mode. Bindings can read its payload directly; Choice can route on
the verdict. Expiry produces its configured decision with a null payload and
does not apply the human payload schema. Default rejection still fails the node.
Identity, allowlists, first-valid resolution, and replay checks still apply.

## Opaque schema values

Use the empty schema object `{}` as the official opaque marker. It accepts any JSON value. It means Dagger does not statically check the value's shape at that schema node.

## Reference definitions

Tests load these files:

- `docs/reference_workflows/intel_digest.yaml`.
- `docs/reference_workflows/legal_research.yaml`.

Treat these files as executable fixtures. Run their tests after a change.
