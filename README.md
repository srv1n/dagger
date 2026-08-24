# Dagger

Dagger is a Rust library for bounded workflow execution.

This repository has one Cargo package: `dagger-workflow-core`. This package is the canonical Dagger engine.

The engine provides these parts:

- Strict JSON and YAML workflow definitions.
- Immutable workflow revisions.
- Action, Map, Choice, Approval, Succeed, and Fail nodes.
- Tenant and namespace isolation.
- Run budgets and fixed run limits.
- Durable attempts, retries, approvals, events, and recovery.
- In-memory stores for tests and local process use.
- SQLite control state with the `sqlite` feature.
- In-memory or local file-system object storage.

## Check the code

You need Rust 1.82 or later.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test --doc --workspace --all-features
cargo doc --workspace --all-features --no-deps
```

## Run an example

```sh
cargo run -p dagger-workflow-core --example yaml_pipeline
cargo run -p dagger-workflow-core --example guardrails
cargo run -p dagger-workflow-core --example multi_tenant
cargo run -p dagger-workflow-core --features sqlite --example durable_demo
```

## Read the documentation

Start with the [system overview](docs/system/00-overview.md).

- [Getting started](docs/system/getting-started.md) gives build and example commands.
- [Workflow engine](docs/system/workflow-core.md) explains execution.
- [Workflow definitions](docs/system/workflow-definitions.md) explains the input format.
- [Storage and durability](docs/system/storage-and-durability.md) explains the stores.
- [Operations and limits](docs/system/operations-and-limits.md) lists host duties and hard limits.
- [Testing and development](docs/system/testing-and-development.md) lists the required checks.

## Task tracking

This repository uses Tusker for tasks, proof, gates, and project knowledge. Automation is off unless a human enables it.

```sh
tusker list
tusker show <TASK-ID> --capsule
tusker validate --vault ./.tusker --json
```
