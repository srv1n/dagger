---
subject: Workflow definitions
keywords: [yaml, json, schema, nodes, bindings]
part_of: Workflow core
describes: [dagger-workflow-core/src/definition, dagger-workflow-core/schema, docs/reference_workflows]
status: canonical
created: 2026-08-23
last_verified: "2026-08-23 @ bdd41fa"
read_when: "You write or validate a workflow-core definition."
skip_when: "You need scheduler state; read Workflow core."
---

# Workflow definitions

Workflow core accepts JSON and YAML. Both formats describe the same workflow data.

The code stores the data in the `WorkflowDefinition` type.

The format version is `0.1`.

The machine-readable schema is `dagger-workflow-core/schema/workflow-definition-0.1.json`.

## Required root fields

A definition contains these fields:

- `definition_format_version`.
- `definition_id`.
- `name`.
- Optional `description`.
- `run_input_schema_digest`.
- `run_output_schema_digest`.
- `entry_node_id`.
- `nodes`.

The parser rejects an unknown structural field. The semantic validator also checks graph rules that JSON Schema cannot check.

## Bind data

An ordinary binding can read from these sources:

- A constant JSON value.
- The immutable run input.
- A successful upstream node output.
- A typed artifact reference.

A Map child can also read the current item and its zero-based index.

Use RFC 6901 JSON pointers. The validator rejects overlapping target pointers and invalid upstream references.

## Pin actions

Each `Action` and `Map` action has five pin fields. The engine requires an exact match before it starts or resumes work.

The example YAML files contain zero digests as authoring placeholders. Example code replaces them before publication. Do not use zero digests in a deployed definition.

## Minimal shape

```yaml
definition_format_version: "0.1"
definition_id: sample
name: Sample
run_input_schema_digest: sha256:<64 lowercase hex characters>
run_output_schema_digest: sha256:<64 lowercase hex characters>
entry_node_id: work
nodes:
  - id: work
    kind: Action
    action:
      name: sample.work
      contract_version: "1"
      input_schema_digest: sha256:<64 lowercase hex characters>
      output_schema_digest: sha256:<64 lowercase hex characters>
      compatible_implementation_requirement: sha256:<64 lowercase hex characters>
    bindings: []
    retry:
      max_attempts: 1
      backoff:
        kind: fixed
        delay_ms: 0
    timeout:
      timeout_ms: 10000
    declared_max_cost_units: "1"
    next: [done]
  - id: done
    kind: Succeed
    output:
      kind: node_output
      node_id: work
      pointer: ""
```

Replace each placeholder digest with a real SHA-256 digest before publication.

## Reference definitions

Use these files as parser and validation fixtures:

- `docs/reference_workflows/intel_digest.yaml`.
- `docs/reference_workflows/legal_research.yaml`.

These files are test inputs. Do not change them as prose-only edits.
