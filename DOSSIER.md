# Dagger — dossier
Last updated: 2026-08-23 (update this line every edit)

## One paragraph
Dagger is an embeddable Rust workspace for applications that need to define multi-step work, run independent steps in parallel, pass outputs between steps, pause for approval, retry failures, and recover durable work. It contains two separate engines: the older `dagger` runtime for YAML DAGs, task agents, pub/sub, and work queues, and `dagger-workflow-core` for strictly validated, tenant-scoped workflows with explicit budgets, state transitions, and durable storage boundaries.

## Status
Active build. The code is a library embedded by a host application; the checked-in implementations support process-local memory or local SQLite/file-system storage. The manifests do not declare a published crate release, and the repository contains no deployment or app-store configuration.

- Shipped/deployed/store status: UNKNOWN — ask Sarav
- Real users and count: UNKNOWN — ask Sarav

## What it does (features, user-facing)
- Define DAGs in YAML, register Rust actions, run dependency-ready nodes in parallel, and pass JSON outputs between nodes.
- Create durable task graphs at runtime, register task agents, persist tasks and dependencies in SQLite, and recover interrupted work.
- Publish messages to named channels and run registered agents that subscribe to those channels.
- Build allowlisted subprocess batches and pipelines with time/output limits, partial-failure retry, and human approval checkpoints.
- Define strict JSON or YAML workflow-core graphs with Action, Map, Choice, Approval, Succeed, and Fail nodes.
- Pin action contracts and schemas, isolate durable data by tenant and namespace, and enforce run budgets and limits.
- Persist workflow-core control state in memory or SQLite and immutable objects in memory or the local file system.
- Resume workflow-core runs after lease takeover without treating an unknown external action outcome as success.

## Who it's for
UNKNOWN — ask Sarav

## Numbers that are true
- Declared package versions: `dagger` 0.0.1; `dagger-workflow-core`, `dagger-macros`, and `task-core` 0.1.0. Reproduce from the four `Cargo.toml` manifests.
- Current-tree compile check: PASS on 2026-08-23 with `cargo check --workspace --all-features`. This is not test, runtime, release, or production proof.
- Stars: UNKNOWN — ask Sarav
- Installs/downloads: UNKNOWN — ask Sarav
- Revenue and paying customers: UNKNOWN — ask Sarav
- Reproducible benchmark results: UNKNOWN — ask Sarav; no benchmark result is checked in.
- Observed real-world failure or data-loss counts: UNKNOWN — ask Sarav

## Tech shape (short)
- Rust 2021 workspace using Tokio, Serde, Petgraph, SQLx/SQLite, JSON Schema, and procedural macros.
- Four workspace packages; reproduce with the root `[workspace].members` plus the root package manifest.
- The root runtime and workflow core have separate APIs, state models, stores, and definition formats; neither calls the other.
- Workflow core separates transactional control-plane rows from content-addressed immutable object bytes.
- SQLite is feature-gated in workflow core; there is no cloud object-store or distributed control-plane backend.
- CI declares format, Clippy, workspace tests, doc tests, and Rust documentation checks in `.github/workflows/ci.yml`.

## Recent changes (rolling, newest first, keep last ~10)
- 2026-08-02: Workflow core began enforcing the per-value byte ceiling when an action commits its own output.
- 2026-08-01: The in-memory store began re-deriving canonical revision structure instead of trusting caller-supplied parsed data.
- 2026-08-01: Applied contract failures began durably applying their state transition; a stateful crash-protocol model was added.
- 2026-08-01: Succeed-node outputs gained schema, inline-size, and aggregate-budget checks; recovery, budget, and event coverage expanded.
- 2026-08-01: Committed-object reads began preserving corruption proofs, scoping corruption marks, and redacting capability data from debug output.
- 2026-08-01: Temporary object-store unavailability was separated from proven corruption, and file publication barriers were hardened.
- 2026-07-29: Memory/SQLite canonical-input behavior was aligned and SQLite history-dependent command work was bounded.
- 2026-07-29: A durable SQLite plus file-system example was added with simulated restart and recovery.
- 2026-07-29: SQLite recovery, expiry, and compatibility scans became scoped, indexed, and keyset-paginated.
- 2026-07-29: SQLite durable authority moved from a reducer snapshot to versioned relational rows and transactional row diffs.

## Deliberate exclusions
- The engines have no migration adapter and must not be mixed directly. Why this boundary was chosen: UNKNOWN — ask Sarav
- The host, not this repository, owns process startup, authenticated identity, secrets, and deployment policy. Why this product boundary was chosen: UNKNOWN — ask Sarav
- No cloud object-store or distributed control-plane backend is checked in. Whether this is deliberate and why: UNKNOWN — ask Sarav
- `enhanced_pubsub.rs` is excluded from compilation; its persistence, compression, and stored-message polling remain placeholders. Whether abandoning it was deliberate and why: UNKNOWN — ask Sarav

## Open questions / embarrassments
- The current checkout contains broad pre-existing uncommitted workflow-core and documentation changes; they are current-tree evidence, not shipped-history proof.
- The committed README at `HEAD` claims 3–10x SQLite compression, but no checked-in benchmark supports that ratio; reproduce the claim with `git show HEAD:README.md`.
- The current README declares MIT, but there is no license file and no Cargo manifest has complete license metadata.
- The README declares Rust 1.74 or later, but CI tests only the stable toolchain and the repository does not track a `Cargo.lock`; minimum-version compatibility is not proved.
- Task Core's crate docs call it “high-performance” and “lock-free”; no benchmark is checked in, and the implementation uses locks plus SQLite.
- The latest committed workflow-core history names unresolved Choice input and Approval request/replay validation paths; current source still has no applied `ChoiceInputInvalid` branch in `record_choice`.
- The file-system crash model proves the code against the model, not power-loss behavior on real devices, network file systems, or cloud stores.
- Full tests, examples, deployment, production durability, and human acceptance were not run or proved during this dossier audit.
