# Examples

All examples are runnable with `cargo run --example <name>`.

## Quick Summary (Sample Output)

Note: timestamps and ordering can vary due to parallel execution. The lines below are the key markers you should see.

- `dag_flow_basic`
  - YAML: `examples/fixtures/basic_pipeline.yaml`
  - Prints: `DAG success: true`

- `dag_flow_pipeline`
  - YAML: `examples/fixtures/order_pipeline.yaml`
  - Prints: `Pipeline success: true`
  - Prints a JSON summary with totals and counts

- `dag_flow_cli`
  - YAML: `examples/fixtures/pipeline.yaml`, `examples/fixtures/pipeline_parallel.yaml`
  - CLI runner with `--list`, `--dag`, `--parallel`, `--sequential`, `--input`

- `dag_flow_dot`
  - YAML: `examples/fixtures/basic_pipeline.yaml`
  - Emits a DOT graph for visualization (Graphviz)

- `task_agent_basic`
  - No stdout on success (runs a short-lived task system and exits cleanly)

- `pubsub_basic`
  - Prints: `printer received: {"hello":"world"}`

- `dynamic_nodes_demo`
  - Prints:
    - `=== Dynamic Node Addition Demo ===`
    - `=== Final Results ===` with `add_1`, `add_2`, `multiply_add_2`

- `coordinator_demo`
  - Prints:
    - `=== Coordinator-Based Parallel Execution Demo ===`
    - `✅ Execution completed successfully!`
    - `=== Final DAG State ===`

- `hooks_solution_simple`
  - Prints:
    - `=== Event-Driven Supervisor Hooks Solution ===`
    - `✅ Event-driven solution completed successfully!`

- `supervisor_hooks_refactored`
  - Prints:
    - `=== Refactored Supervisor Hooks Demo ===`
    - `✅ Refactored hooks demonstration completed successfully!`
