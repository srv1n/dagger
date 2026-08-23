# Dagger macros

This package provides three procedural macros:

- `#[action]` registers a DAG Flow action.
- `#[task_agent]` registers a Task Agent implementation.
- `#[pubsub_agent]` registers a Pub/Sub agent.

The macros generate registration code for the root `dagger` package.

Use the examples in the repository root as the public usage reference.
