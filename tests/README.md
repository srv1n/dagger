# Dagger runtime tests

These tests cover the root `dagger` package.

Run all root-package tests:

```sh
cargo test -p dagger
```

Run one integration test:

```sh
cargo test -p dagger --test test_dag_flow_simple
```

Use `dagger-workflow-core/tests/` for workflow-core behavior.
