# Dagger Pub/Sub Guide

Dagger's Pub/Sub mode is an in-memory, event-driven system for decoupled agents that communicate over named channels. It is designed for lightweight message routing and coordination; persistence is handled by DAG Flow or Task Agent mode.

## Core Concepts

### Agents
Agents implement `PubSubAgent` and react to messages on subscribed channels.

```rust
#[async_trait::async_trait]
pub trait PubSubAgent: Send + Sync + 'static {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn subscriptions(&self) -> Vec<String>;
    fn publications(&self) -> Vec<String>;
    fn input_schema(&self) -> serde_json::Value;
    fn output_schema(&self) -> serde_json::Value;

    async fn process_message(
        &self,
        node_id: &str,
        channel: &str,
        message: &Message,
        executor: &mut PubSubExecutor,
        cache: &Cache,
    ) -> anyhow::Result<()>;
}
```

### Channels
Channels are `async-broadcast` queues keyed by name. Multiple agents can subscribe to the same channel; each message is cloned to all receivers.

### Messages
Messages are immutable records sent across channels.

```rust
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    pub timestamp: NaiveDateTime,
    pub source: String,
    pub channel: Option<String>,
    pub task_id: Option<String>,
    pub payload: serde_json::Value,
    pub message_id: String,
}
```

### Executor
`PubSubExecutor` manages channels, agent registration, and workflow execution.

```rust
let mut executor = PubSubExecutor::new(None);
executor.register_agent(Arc::new(MyAgent)).await?;

let cache = dagger::Cache::new();
let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

let workflow = PubSubWorkflowSpec::StartWith { channel, message };
executor.execute(workflow, &cache, cancel_rx).await?;
let _ = cancel_tx.send(());
```

## Macro-Based Agents

Use `#[dagger::pubsub_agent]` for concise agents and auto-registration.

```rust
#[dagger::pubsub_agent(name = "printer", subscribe = "events", publish = "results")]
async fn printer(msg: Message) -> anyhow::Result<()> {
    println!("printer received: {}", msg.payload);
    Ok(())
}
```

If you accept a second argument, the macro provides a `PubSubContext` with publish helpers:

```rust
#[dagger::pubsub_agent(subscribe = "events", publish = "results")]
async fn handler(msg: Message, ctx: &mut dagger::PubSubContext) -> anyhow::Result<()> {
    ctx.publish_payload("results", serde_json::json!({"ok": true})).await?;
    Ok(())
}
```

## Cache Usage

The shared cache uses `DashMap` under the hood. Use the helper functions to read/write values:

```rust
let count: u64 = dagger::get_input(&cache, node_id, "processed_count").unwrap_or(0) + 1;
dagger::insert_value(&cache, node_id, "processed_count", count)?;
```

## Notes

- Listeners are started before the initial `StartWith` message is published to avoid dropped messages.
- Each subscription runs in its own task; messages from the same agent are filtered to prevent loops.
- Pub/Sub is in-memory. For durable workflows, use DAG Flow or Task Agent mode.
