---
schema: "tusker.domain/v7"
kind: "domain"
id: "project"
project: "dagger"
title: "Project"
status: "current"
summary: "Durable Dagger workspace knowledge."
capsule:
  skip_when: "Skip when task proof or runtime events are the target."
  use_when: "Use when a task changes repository behavior or documentation."
  what: "Routes a reader to the project canon and human documentation."
source_of_truth:
  - "knowledge/domains/project/CANON.md"
canonical_files:
  - "INDEX.md"
  - "CANON.md"
created_at: "2026-08-23T12:08:23Z"
updated_at: "2026-08-23T12:36:03Z"
state_rev: "sha256:0ed8210081ecdc15fff8f58b93fb375bcee63c3a9e0738ede402b02a1359f0d2"
---

# Project

## Read this first

1. Read `CANON.md` for durable project truth.
2. Read `docs/system/00-overview.md` for the human system map.
3. Read the narrowest child page for the target engine.

## Human documentation

- `docs/system/legacy-dagger-runtime.md` owns the root runtime.
- `docs/system/workflow-core.md` owns the bounded engine.
- `docs/system/testing-and-development.md` owns validation commands.
- `docs/system/operations-and-limits.md` owns host safety rules.

## Current work

- Tusker epic `DOC` owns documentation maintenance.
