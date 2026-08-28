---
schema: "tusker.domain/v7"
kind: "domain"
id: "project"
project: "dagger"
title: "Project"
status: "current"
summary: "Durable knowledge for the Dagger workflow engine."
capsule:
  skip_when: "Skip when task proof or runtime events are the target."
  use_when: "Use when a task changes repository behavior or documentation."
  what: "Routes a reader to project canon and human documentation."
source_of_truth:
  - "knowledge/domains/project/CANON.md"
canonical_files:
  - "INDEX.md"
  - "CANON.md"
created_at: "2026-08-23T12:08:23Z"
updated_at: "2026-08-28T14:50:58Z"
state_rev: "sha256:82155c77439f3ac254e005f1be810ab58a0eeaf120db6572a06f07ea03deaa54"
---

# Project

## Read this first

1. Read `CANON.md` for durable project truth.
2. Read `docs/system/00-overview.md` for the system map.
3. Read the narrowest child page for the change.

## Human documentation

- `docs/system/workflow-core.md` owns engine behavior.
- `docs/system/workflow-definitions.md` owns the workflow input contract.
- `docs/system/storage-and-durability.md` owns store behavior.
- `docs/system/operations-and-limits.md` owns host duties and limits.
- `docs/system/testing-and-development.md` owns validation commands.

## Current work

- Tusker epic `DOC` owns documentation maintenance.
