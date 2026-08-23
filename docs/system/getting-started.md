---
subject: Getting started
keywords: [build, test, examples, dependency]
part_of: System overview
describes: [Cargo.toml, dagger-workflow-core/Cargo.toml, .github/workflows/ci.yml, dagger-workflow-core/examples]
status: canonical
created: 2026-08-23
last_verified: 2026-08-23 @ ef3df5d2232f1a7b2365b99287e80f31b7d510ee
read_when: "You want to build, test, or run Dagger."
skip_when: "You need engine behavior; read Workflow engine."
---

# Getting started

## Requirements

Install Rust 1.82 or later. Run commands from the repository root.

## Check the workspace

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test --doc --workspace --all-features
cargo doc --workspace --all-features --no-deps
```

CI runs these commands.

## Run an example

```sh
cargo run -p dagger-workflow-core --example yaml_pipeline
cargo run -p dagger-workflow-core --example guardrails
cargo run -p dagger-workflow-core --example multi_tenant
cargo run -p dagger-workflow-core --features sqlite --example durable_demo
```

`durable_demo` uses SQLite and `FsObjectStore`. It restarts both stores and checks the result.

## Use a path dependency

```toml
[dependencies]
dagger-workflow-core = { path = "../dagger/dagger-workflow-core" }
```

Enable SQLite only when the host uses `SqliteWorkflowStore`:

```toml
dagger-workflow-core = { path = "../dagger/dagger-workflow-core", features = ["sqlite"] }
```
