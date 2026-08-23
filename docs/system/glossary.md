---
subject: Glossary
keywords: [terms, definitions, vocabulary]
part_of: overview
describes: [src, dagger-workflow-core/src]
status: canonical
created: 2026-08-23
last_verified: "2026-08-23 @ bdd41fa"
read_when: "You need the meaning of a project term."
skip_when: "The term is already clear in the owning guide."
---

# Glossary

**Action**: Rust code that receives canonical input and returns one outcome.

**Action pin**: The exact version, schema digests, and compatibility digest for an action.

**Artifact reference**: A typed durable reference to immutable object bytes.

**Canonical JSON**: JSON bytes with one deterministic representation.

**Control plane**: Durable records that describe workflow state. Object bytes are not control-plane state.

**DAG**: A directed graph with no cycle.

**Engine claim**: The scoped lease that permits one scheduler generation to change workflow state.

**Execution scope**: The tenant ID and namespace that isolate workflow-core data.

**Host**: The application that embeds Dagger or workflow core.

**Idempotency key**: A stable key that lets an external action detect a repeated request.

**Object store**: Storage for immutable bytes addressed by a SHA-256 digest.

**Revision**: An immutable, validated, and pinned workflow definition.

**Run**: One execution of one published revision with immutable input and limits.

**Tusker**: The repository task and project-knowledge system. It does not execute product code unless a human enables automation.
