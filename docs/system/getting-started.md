---
subject: Getting started
keywords: [build, install, run, examples]
part_of: overview
describes: [Cargo.toml, .github/workflows/ci.yml, examples, dagger-workflow-core/examples]
status: canonical
created: 2026-08-23
last_verified: "2026-08-23 @ bdd41fa"
read_when: "You want to build, test, or run an example."
skip_when: "You need the architecture; read System overview."
---

# Getting started

## Requirements

Install Rust 1.74 or later. Use Cargo from the same Rust toolchain.

This repository uses local workspace packages. The Cargo manifests do not claim a published release.

## Check the workspace

Run these commands from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test --doc --workspace --all-features
cargo doc --workspace --all-features --no-deps
```

These are the same checks that CI runs.

## Run the Dagger runtime

```sh
cargo run --example dag_flow_basic
cargo run --example task_agent_basic
cargo run --example pubsub_basic
```

## Run workflow core

```sh
cargo run -p dagger-workflow-core --example yaml_pipeline
cargo run -p dagger-workflow-core --example guardrails
cargo run -p dagger-workflow-core --example multi_tenant
cargo run -p dagger-workflow-core --features sqlite --example durable_demo
```

The durable example creates temporary data. It restarts the stores and checks the result.

## Use a path dependency

Use the root runtime:

```toml
[dependencies]
dagger = { path = "../dagger" }
```

Use workflow core:

```toml
[dependencies]
dagger-workflow-core = { path = "../dagger/dagger-workflow-core" }
```

Enable `sqlite` when you use `SqliteWorkflowStore`:

```toml
dagger-workflow-core = { path = "../dagger/dagger-workflow-core", features = ["sqlite"] }
```
