# Upgrading / Downstream Adoption Notes

This note summarizes the changes you need to make if you are adopting this crate downstream or upgrading from older snapshots.

## High-level changes

- **Three supported modes** are now first-class: DAG Flow, Task Agent (task-core), and Pub/Sub. Legacy adapters and function-based DAG actions were removed.
- **Macro-driven registration** is the preferred path: `#[dagger::action]`, `#[dagger::task_agent]`, and `#[dagger::pubsub_agent]`.
- **Async everywhere**: most DAG loading and storage paths are async; update call sites accordingly.

## DAG Flow (YAML workflows)

### Action registration changes

- **Removed:** `register_action!` macro and legacy function-based actions.
- **Use instead:**
  - `#[dagger::action]` on async functions, or
  - implement `NodeAction` and call `ActionRegistry::register`.

`DagExecutor::new(...)` now calls `register_global_actions(&registry)` internally, so actions registered via `#[action]` are picked up automatically.

### API updates you may need

- `DagExecutor::load_yaml_dir(...)` is now **async**.
- `NodeAction::execute` is **compute-only** and receives a `NodeCtx` (immutable, clonable).
- `NodeOutput` controls success/failure; `success = false` now propagates failure.

## Task Agent (task-core)

### Legacy module removal

- `src/taskagent/*` legacy wrappers were removed. Use the task-core API directly (re-exported at the crate root).
- Import path updates:
  - Old: `dagger::taskagent::...`
  - New: `dagger::{TaskSystem, TaskSystemBuilder, AgentRegistry, Task, TaskContext, ...}`

### Agent signature / output changes

- Agents return `Bytes` internally. Use any type that implements `IntoBytes`:
  - `serde_json::Value`, `Bytes`, `Vec<u8>`, `String`, `&str`.
- Example signature:
  ```rust
  #[dagger::task_agent(name = "echo")]
  async fn echo(task: Task, ctx: std::sync::Arc<TaskContext>) -> anyhow::Result<serde_json::Value> {
      Ok(serde_json::json!({ "echo": task.payload.input }))
  }
  ```

### Storage trait changes

If you implemented `Storage`, update for:
- `list_tasks(...)` and `list_tasks_by_job(...)` signatures (now return `Vec<Task>`).
- Dependency storage uses JSON serialization.

## Pub/Sub

- `process_message` now receives `&Message` and `&Cache` (borrowed), not owned values.
- The macro supports `PubSubContext` for publish helpers.
- `PubSubExecutor::execute` now ensures listeners are created before the initial publish.

## Root re-exports

The crate root now re-exports common types for downstream ergonomics:

```rust
use dagger::{
  action, task_agent, pubsub_agent,
  DagExecutor, Cache,
  TaskSystemBuilder, AgentRegistry,
  PubSubExecutor, PubSubWorkflowSpec,
};
```

## Migration checklist

1. Replace `register_action!` / legacy DAG actions with `#[dagger::action]` or `NodeAction`.
2. Update `load_yaml_dir(...)` call sites to `await`.
3. Remove references to legacy `taskagent` modules.
4. Update task agent return types to something `IntoBytes`.
5. Update Pub/Sub agent signatures to match `&Message` / `&Cache`.

## Need help?

If you run into a mismatch, open the examples in `examples/` as the canonical reference for each mode.
