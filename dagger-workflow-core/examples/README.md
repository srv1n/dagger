# dagger-workflow-core examples

Every example is self-contained: it registers its own actions, publishes its own
definition, and prints a transcript you can read top to bottom.

| Example | What it demonstrates | Command |
| --- | --- | --- |
| `yaml_pipeline` | A graph defined entirely in `pipeline.yaml` and parsed at runtime: fan-out, a join, a Map fan-out, content-addressed node results, and the verified final artifact. | `cargo run -p dagger-workflow-core --example yaml_pipeline` |
| `guardrails` | What the engine refuses to commit: a retry after a retryable failure, the retry ceiling, three run-limit ceilings, and the pinned root output schema. Each refusal is a durable terminal state with a closed failure kind. | `cargo run -p dagger-workflow-core --example guardrails` |
| `multi_tenant` | Scope isolation: the same definition and the same run id executed under two `ExecutionScope`s against one shared store, with runs, events, budgets, attempts, and artifacts disjoint. | `cargo run -p dagger-workflow-core --example multi_tenant` |
| `durable_demo` | The SQLite store and the filesystem object store: publish, run, simulated kill, real lease expiry, recovery at a new claim generation, and completion without replaying finished work. Sleeps ~20s waiting for the lease. | `cargo run -p dagger-workflow-core --example durable_demo --features sqlite` |

`durable_demo` is the only example that needs `--features sqlite`. The other
three use the in-memory control plane and object store and leave no state behind.
