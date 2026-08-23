---
subject: Glossary
keywords: [terms, definitions, vocabulary]
part_of: System overview
describes: [dagger-workflow-core/src]
status: canonical
created: 2026-08-23
last_verified: 2026-08-23 @ 94e8bc4543ccf2c8f57c30715071e7c0b9352b57
read_when: "You need the meaning of a Dagger term."
skip_when: "The owning guide already defines the term."
---

# Glossary

**Action**: Rust code that receives canonical input and returns one outcome.

**Action pin**: The exact action name, contract version, schema digests, and compatibility digest.

**Artifact reference**: A typed reference to immutable object bytes.

**Canonical JSON**: JSON bytes that have one deterministic representation.

**Control store**: Storage for workflow state. It does not store object bytes.

**Engine claim**: The scoped lease that lets one scheduler generation change workflow state.

**Execution scope**: A tenant ID and namespace that isolate durable data.

**Host**: The application that embeds Dagger.

**Idempotency key**: A stable key that lets an external action detect a repeated request.

**Object store**: Storage for immutable bytes that a SHA-256 digest identifies.

**Revision**: An immutable and validated workflow definition with exact pins.

**Run**: One execution of one published revision with fixed input, limits, and budget.

**Tusker**: The repository system for tasks, proof, gates, and project knowledge.
