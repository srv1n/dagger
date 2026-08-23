---
schema: "tusker.domain-canon/v7"
kind: "domain_canon"
id: "project/canon"
project: "dagger"
domain: "project"
title: "Project Canon"
status: "current"
summary: "Current durable truth for the Dagger workspace."
capsule:
  skip_when: "Skip when you only need Tusker task state or proof."
  use_when: "Use before you change behavior, public APIs, storage, or documentation."
  what: "The engine boundary, source owners, and required checks."
source_of_truth:
  - "knowledge/domains/project/CANON.md"
created_at: "2026-08-23T12:08:23Z"
updated_at: "2026-08-23T12:36:03Z"
state_rev: "sha256:c1a60856211def47c9cb7661f2d6b8ce70131370365638dfa7aab8bf22b7c29d"
---

# Project Canon

## Current truth

- Dagger is one Rust workspace with two separate workflow engines.
- The root `dagger` package owns DAG Flow, Task Agent, Pub/Sub, and Work Queue.
- `dagger-workflow-core` owns the bounded workflow engine.
- The two engines do not share a runtime or data model.
- `docs/system/00-overview.md` is the human documentation entry point.
- Tusker owns repository task records. Tusker automation is off by default.

## Stable interfaces

- Root public exports come from `src/lib.rs`.
- Workflow-core public modules come from `dagger-workflow-core/src/lib.rs`.
- Workflow definition format `0.1` uses `dagger-workflow-core/schema/workflow-definition-0.1.json`.
- CI runs format, Clippy, workspace tests, doc tests, and Rust documentation.

## Constraints

- Select the target engine before you change code.
- Do not claim that workflow core replaces the root runtime.
- Keep each workflow-core durable key inside one tenant and namespace scope.
- Keep secrets outside definitions, events, diagnostics, and ordinary artifacts.
- Use Simplified Technical English in human documentation.
- Run `tusker docs map` and `tusker validate --vault ./.tusker --json` after a documentation change.

## Deprecated or stale

- The deleted architecture plans and status reviews are available only in Git history.

## Open questions

- The Cargo manifests do not contain complete publishing and license metadata.
- The repository does not contain a migration adapter between the two engines.
