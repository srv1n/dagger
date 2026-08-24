# Dagger examples

| Example | Purpose | Command |
| --- | --- | --- |
| `yaml_pipeline` | Parse and run a YAML workflow with in-memory stores. | `cargo run -p dagger-workflow-core --example yaml_pipeline` |
| `guardrails` | Show refused transitions and limit checks. | `cargo run -p dagger-workflow-core --example guardrails` |
| `multi_tenant` | Show scope isolation for two tenants. | `cargo run -p dagger-workflow-core --example multi_tenant` |
| `durable_demo` | Use SQLite and the local file-system object store. | `cargo run -p dagger-workflow-core --features sqlite --example durable_demo` |

Each example contains its host setup. Copy only the parts that your host needs.
