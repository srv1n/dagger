---
schema: "tusker.domain-canon/v7"
kind: "domain_canon"
id: "project/canon"
project: "dagger"
domain: "project"
title: "Project Canon"
status: "current"
summary: "Current durable truth for the Dagger workflow engine."
capsule:
  skip_when: "Skip when you only need Tusker task state or proof."
  use_when: "Use before you change behavior, public APIs, storage, or documentation."
  what: "The engine boundary, source owners, and required checks."
source_of_truth:
  - "knowledge/domains/project/CANON.md"
created_at: "2026-08-23T12:08:23Z"
updated_at: "2026-08-28T14:50:58Z"
state_rev: "sha256:771bd3601378b0338821268604f72ca36ed7df2e5788e877c3cd612d03b514f7"
---

# Project Canon

## Current truth

- Dagger has one Cargo package: `dagger-workflow-core`.
- This package is the canonical workflow engine.
- `dagger-workflow-core/src/lib.rs` defines the public module surface.
- `dagger-workflow-core/schema/workflow-definition-0.1.json` defines the current workflow input shape.
- The format value `0.1` is a data contract. It is not a Dagger product generation.
- `docs/system/00-overview.md` is the human documentation entry point.
- Tusker owns task records, proof, gates, and project knowledge.
- Tusker automation is off unless a human enables it.

## Engine boundary

- `WorkflowEngine` changes workflow state through `WorkflowStore` commands.
- `ActionRegistry` owns executable action implementations and exact pins.
- `WorkflowStore` owns control state.
- `ObjectStore` owns immutable bytes.
- `ExecutionScope` isolates every tenant and namespace.
- The host owns identity, secrets, process startup, and deployment policy.

## Storage boundary

- `InMemoryStore` and `InMemoryObjectStore` are process-local.
- `SqliteWorkflowStore` is the optional durable control store.
- `FsObjectStore` is the local file-system object store.
- The repository has no cloud object-store backend and no distributed control store.

## Required checks

- Run format, Clippy, workspace tests, doc tests, and Rust documentation checks after a code change.
- Run `tusker docs map` and `tusker validate --vault ./.tusker --json` after a documentation change.
- Use Simplified Technical English in human documentation.
