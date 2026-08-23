---
subject: Storage and durability
keywords: [sqlite, object store, crash, recovery]
part_of: Workflow core
describes: [dagger-workflow-core/src/memory, dagger-workflow-core/src/sqlite, dagger-workflow-core/src/fs_object_store.rs, dagger-workflow-core/src/committed_read.rs]
status: canonical
created: 2026-08-23
last_verified: "2026-08-23 @ bdd41fa"
read_when: "You select a store or review durability behavior."
skip_when: "You only write a definition."
---

# Storage and durability

Workflow core separates control state from object bytes.

## Control-plane store

The control-plane store keeps definitions, revisions, runs, nodes, attempts, gates, budget entries, receipts, and events.

`InMemoryStore` is for tests and process-local use.

`SqliteWorkflowStore` is available with the `sqlite` feature. It uses relational rows as durable state. Each command uses a transaction. Scope and version checks prevent an unsafe update.

## Object store

The object store keeps immutable bytes. A SHA-256 digest identifies the bytes.

`InMemoryObjectStore` is for tests and process-local use.

`FsObjectStore` stores bytes on a local file system. It verifies content before it returns a reference. It uses file and directory sync operations in its publication sequence.

## Two-store rule

The object store publishes bytes first. The control-plane store then commits a typed reference to those bytes.

An unreferenced object can remain after a crash. This is safe. A committed reference must not point to bytes that were never published.

The engine uses `VerifiedObjectRef` as an in-process capability. Logs redact the store nonce and verified bytes.

## Read failures

The object store can report proven missing or invalid content. It can also report storage unavailability without a corruption proof.

Only a valid failed-read proof can move a run to `CorruptStorage`.

A temporary I/O failure must not become permanent corruption.

## Evidence boundary

The test suite has a stateful crash model. The model checks each file-system barrier and includes negative controls.

This model proves the code protocol against the model. It does not prove power-loss behavior on each real device or file system.

Do not claim support for a network file system or a cloud object store from the local file-system tests. Add a backend and backend-specific evidence first.
