---
subject: Storage and durability
keywords: [sqlite, object store, crash, recovery]
part_of: Workflow engine
describes: [dagger-workflow-core/src/memory, dagger-workflow-core/src/sqlite, dagger-workflow-core/src/fs_object_store.rs, dagger-workflow-core/src/committed_read.rs]
status: canonical
created: 2026-08-23
last_verified: 2026-08-23 @ 94e8bc4543ccf2c8f57c30715071e7c0b9352b57
read_when: "You select a store or review durability behavior."
skip_when: "You only write a definition."
---

# Storage and durability

Dagger separates control state from object bytes.

## Control store

The control store keeps definitions, revisions, runs, nodes, attempts, gates, budgets, receipts, object metadata, and events.

`InMemoryStore` keeps state in one process.

`SqliteWorkflowStore` is available with the `sqlite` feature. Its standalone open path enables WAL mode, full synchronous writes, foreign keys, a five-second busy timeout, and a bounded connection pool. `from_pool` silently normalizes a caller-owned file database to WAL; it accepts SQLite's unavoidable in-memory journal mode. The store re-enables foreign keys and the five-second busy timeout every time it acquires a connection, so a caller-owned pool cannot silently supply a wrongly configured connection.

Each SQLite command starts an immediate transaction. A BUSY or LOCKED transaction start retries at most six times with capped, jittered 10–640 ms delays; other database errors remain typed failures. The command uses the database clock, applies one reducer operation, saves the new projections, and commits. A command with no applied state change rolls back. Some contract and run-limit errors commit a terminal state change and return a typed error.

The SQLite database stores object metadata and typed references. It does not store object bytes.

## Object store

The object store keeps immutable bytes. SHA-256 identifies the bytes.

`InMemoryObjectStore` keeps bytes in one process.

`FsObjectStore` stores bytes on a local file system. It canonicalizes JSON, writes a temporary file, syncs the file, publishes immutable content and media metadata, and syncs the parent directories. It verifies bytes before it returns `VerifiedObjectRef`.

## Commit order

The object store publishes bytes first. The control store then commits a typed reference.

A crash can leave an object with no durable reference. This is safe. A committed reference must not point to bytes that were not published.

## Large binary files

`FsObjectStore::import_file` copies and hashes a local binary file with a bounded
buffer and retains the local publication sync barriers. It does not hardlink a
mutable source. JSON uses canonical JSON publication. Binary capabilities retain
metadata without retaining file contents in control state.
Copying and binary verification run on blocking workers so they do not stall
the scheduler runtime during a large read.

`ObjectStore::verify` returns a verified capability. FsObjectStore streams binary
verification. The default trait method uses an ordinary in-memory read; other
backends need their own override for bounded memory.

`FsObjectStore::get_file` returns a verified private file snapshot. Its temporary
disk copy supports reading and seeking without trusting a mutable object path.
`CommittedObjectReader::read_file` adds committed-reference and corruption checks.
These APIs bound memory, not disk traffic. Byte-returning reads remain available
for small objects.

`ObjectStore::put_repairing` restores a regenerated candidate only after the
backend proves that its existing same-digest bytes are missing or invalid.
FsObjectStore checks its live store identity, candidate digest, metadata, and
publication barriers before replacement. Healthy bytes and metadata conflicts
remain immutable. Ordinary `put` stays strict. Memoizable binary actions using
byte publication should use the repairing method; local `import_file` supports
verified regeneration directly. A storage outage does not authorize repair.

## Read failures

`CommittedObjectReader` has three important results:

- Verified bytes.
- Storage unavailable, with no state change.
- Proven missing or invalid bytes, followed by a durable corruption mark.

A temporary I/O error is not corruption. Only a valid failed-read proof can move a run to `CorruptStorage`.

## Recovery

The store keeps scheduler claims, generations, attempt fences, retries, deadlines, and command receipts.

A new generation can recover attempts from an older generation. The recovery path records unknown outcome and applies the retry policy.

## Evidence boundary

The tests include a stateful model of the local file-system publication barriers. The model also has negative controls.

This evidence applies to the implemented local protocol and its model. It does not prove durability on each device, network file system, or cloud object store.

The repository does not contain a cloud object-store backend or a distributed control store.
