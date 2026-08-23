# Task Core

Task Core is the durable task engine used by the root `dagger` package.

It provides these main types:

- `TaskSystemBuilder` builds the system.
- `AgentRegistry` holds registered agents.
- `NewTaskSpec` describes a new task.
- `Scheduler` selects ready tasks.
- `SqliteStorage` stores tasks, dependencies, outputs, events, and shared state.

The builder accepts a SQLite file path or a caller-owned `SqlitePool`.

Start with `examples/task_agent_basic.rs` in the repository root.

Task Core is not the Tusker task tracker. Task Core runs product tasks. Tusker tracks repository work.
