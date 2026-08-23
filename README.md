# Dagger

Dagger is a Rust workspace for workflow execution.

The workspace has two workflow engines. They do not share one runtime.

| Engine | Use it for | Package |
| --- | --- | --- |
| Dagger runtime | Existing DAG, task-agent, pub/sub, and work-queue code | `dagger` |
| Workflow core | New bounded workflows with strict state, scope, and durability rules | `dagger-workflow-core` |

Do not mix the two engines in one design unless you write an adapter.

## Start

You need Rust 1.74 or later.

Run the full local checks:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test --doc --workspace --all-features
```

Run a small example:

```sh
cargo run --example dag_flow_basic
cargo run -p dagger-workflow-core --example yaml_pipeline
```

Run the durable workflow-core example:

```sh
cargo run -p dagger-workflow-core --features sqlite --example durable_demo
```

## Read the documentation

Start with the [system overview](docs/system/00-overview.md).

Use [getting started](docs/system/getting-started.md) for build and example commands.
Use [repository structure](docs/system/repository-structure.md) to select the correct crate.
Use [workflow core](docs/system/workflow-core.md) for the bounded engine.
Use [legacy Dagger runtime](docs/system/legacy-dagger-runtime.md) for the older engine.

## Project status

This repository has active development code. The manifests do not declare a published crate release. Treat the path-dependency examples as the supported setup for this checkout.

The workflow core has memory and SQLite control-plane stores. It also has memory and file-system object stores. The tests cover the implemented protocol. They do not prove behavior for all hardware, network file systems, or cloud object stores.

## Task tracking

This repository uses Tusker. Automation is off by default.

```sh
tusker list
tusker show <TASK-ID> --capsule
tusker validate --vault ./.tusker --json
```

## License

The root package declares MIT in this README. The Cargo manifests do not yet contain complete package license metadata.
