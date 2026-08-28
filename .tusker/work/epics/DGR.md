---
schema: "tusker.epic/v7"
kind: "epic"
id: "DGR"
project: "dagger"
title: "Honest multi-root DAG support"
status: "ready"
owner: "sarav"
priority: "p2"
domains: []
spec_refs:
  - "docs/system/workflow-definitions.md"
  - "docs/system/workflow-core.md"
next_task_number: 1
next_gate_number: 1
next_decision_number: 1
created_at: "2026-08-28T14:04:36Z"
updated_at: "2026-08-28T14:51:06Z"
state_rev: "sha256:dd78f775ac206f1210fc9769b6812d3a5603f71fa33a8a0f7e43fe82bb0a9262"
capsule:
  skip_when: "Skip when you need a specific task contract, proof row, gate, or attempt."
  use_when: "Use to triage this workstream's scope, active tasks, and durable direction."
  what: "DGR epic: Honest multi-root DAG support."
---

# DGR · Honest multi-root DAG support

## Thesis

Extend dagger-workflow-core so references define a deterministic DAG with multiple true roots; no compiler-injected start node.

## Success criteria

- [ ] A single published contract permits multiple true graph roots derived from
      typed output references, with no compiler-created first node.
- [ ] Readiness, persisted ranks, execution, and recovery remain deterministic
      and fail closed for bad references and cycles.
- [ ] The RZN backend can pin the exact landed Dagger revision as a downstream
      prerequisite without duplicating the graph contract.

## Current decision

Keep triggers and schedulers outside Dagger. Dagger receives one validated graph
and executes its authored nodes; manual, cron, webhook, parent, and agent callers
are downstream host concerns.

## Open gates

<!-- tusker:generated open-gates -->

| Gate | Owner | Blocks | Action |
|---|---|---|---|
| _None._ |  |  |  |

## Active work

<!-- tusker:generated active-work -->

| Task | Status | Next owner | Next action |
|---|---|---|---|
| [[DGR-T-0001]] | ready | agent | Execute the task contract and satisfy proof mode. |

## Recently completed

<!-- tusker:generated recently-completed -->

| Task | Accepted by | Closed at |
|---|---|---|
| _None._ |  | |
