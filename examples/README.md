# Dagger runtime examples

These examples use the root `dagger` package.

| Example | Purpose |
| --- | --- |
| `dag_flow_basic` | Run a small YAML DAG. |
| `dag_flow_pipeline` | Run an order pipeline. |
| `dag_flow_cli` | Select and run a YAML pipeline. |
| `dag_flow_dot` | Write a DOT graph. |
| `task_agent_basic` | Run one durable task agent. |
| `pubsub_basic` | Send one pub/sub message. |
| `work_queue_batch_exec` | Run an allowlisted batch command. |
| `work_queue_batch_send_retry` | Show partial failure and retry. |

Run an example from the repository root:

```sh
cargo run --example dag_flow_basic
```

These examples do not use `dagger-workflow-core`.
