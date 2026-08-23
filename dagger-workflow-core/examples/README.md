# Workflow-core examples

| Example | Purpose | Command |
| --- | --- | --- |
| `yaml_pipeline` | Parse and run an in-memory YAML workflow. | `cargo run -p dagger-workflow-core --example yaml_pipeline` |
| `guardrails` | Show limits and refused transitions. | `cargo run -p dagger-workflow-core --example guardrails` |
| `multi_tenant` | Show scope isolation for two tenants. | `cargo run -p dagger-workflow-core --example multi_tenant` |
| `durable_demo` | Use SQLite and the file-system object store. | `cargo run -p dagger-workflow-core --features sqlite --example durable_demo` |

The examples contain full host setup. Copy only the parts that your host needs.
