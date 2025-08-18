# PubSub Agent Architecture

## Core Implementation Details

The PubSub system is built on `async-broadcast` channels with autonomous agents that communicate through named channels. Each agent runs in its own tokio task, listening for messages and processing them asynchronously.

## Memory Model

### Channel Architecture
```rust
struct ChannelInfo {
    sender: Sender<Message>,      // async-broadcast sender
    _receiver: Receiver<Message>,  // Kept alive to prevent channel closure
}

// Channels stored in Arc<RwLock<HashMap>>
channels: Arc<RwLock<HashMap<String, ChannelInfo>>>
```

The system uses async-broadcast (not tokio channels) because:
- Multiple agents can subscribe to the same channel
- Messages are automatically cloned to all receivers
- Backpressure through bounded capacity (default: 1000 messages)
- No message loss when receivers are slow

### Message Flow
```rust
pub struct Message {
    pub timestamp: NaiveDateTime,    // Chrono UTC timestamp
    pub source: String,               // Node ID of sender (unique per agent instance)
    pub channel: Option<String>,      // Set during publish
    pub task_id: Option<String>,      // Links to task system
    pub payload: serde_json::Value,   // Actual message data
    pub message_id: String,           // UUID v4 for tracking
}
```

Messages are **immutable** once created. The channel field is only set during publishing to prevent tampering.

## Agent Lifecycle

### Registration Process
```rust
pub async fn register_agent(&mut self, agent: Arc<dyn PubSubAgent>) -> Result<()> {
    // 1. Generate unique node_id with agent name + 8 char UUID
    let node_id = format!("{}_{}", name, Uuid::new_v4()[..8]);
    
    // 2. Create channels for subscriptions
    for channel in &agent.subscriptions() {
        self.ensure_channel_exists(channel).await?;
    }
    
    // 3. Create channels for publications
    for channel in &agent.publications() {
        self.ensure_channel_exists(channel).await?;
    }
    
    // 4. Store agent with metadata
    agents.insert(node_id, AgentEntry {
        agent,
        metadata,
        node_id: node_id.clone(),
    });
}
```

### Listener Task Spawning
Each agent subscription spawns a dedicated tokio task:
```rust
fn start_agent_listener(...) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut receiver = channel_info.sender.new_receiver();
        
        loop {
            tokio::select! {
                message_result = receiver.recv() => {
                    // Skip self-messages
                    if message.source == node_id { continue; }
                    
                    // Create fresh executor for agent
                    let mut executor = PubSubExecutor::new(config);
                    
                    // Copy channels to new executor (agent can publish)
                    executor.channels = channels.clone();
                    
                    // Process message
                    agent.process_message(&node_id, &channel, 
                                         &message, &mut executor, &cache).await;
                }
                _ = check_stopped() => break;
            }
        }
    })
}
```

Key design decisions:
- **One task per subscription**: Prevents blocking between channels
- **Fresh executor per message**: Isolation between message processing
- **Self-message filtering**: Prevents infinite loops
- **Graceful shutdown**: Through stopped flag checking

## Concurrency Model

### Lock Hierarchy
```
1. channels (RwLock) - Channel registry
2. agents (RwLock) - Agent registry  
3. stopped (RwLock) - Global stop flag
4. Cache::data (DashMap) - Lock-free concurrent hashmap
```

### Race Condition Prevention
- Channels created before agent registration completes
- Receiver cloned before releasing channel lock
- Message source checked to prevent self-loops
- Stopped flag checked in select! to ensure clean shutdown

## Schema Validation

Agents define JSON schemas for type safety:
```rust
impl PubSubAgent for DataProcessor {
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "data": {
                    "type": "array",
                    "items": {"type": "number"}
                }
            },
            "required": ["data"]
        })
    }
    
    fn validate_input(&self, message: &Value) -> Result<()> {
        let compiled = jsonschema::validator_for(&self.input_schema())?;
        compiled.validate(message)
            .map_err(|e| anyhow!("Validation failed: {:?}", e))?;
        Ok(())
    }
}
```

The validation uses `jsonschema` crate which:
- Compiles schemas for performance
- Provides detailed error messages
- Supports full JSON Schema draft 7

## Error Handling

### Message Processing Failures
```rust
if let Err(e) = agent.process_message(...).await {
    error!("Agent {} failed to process message {}: {}", 
           node_id, message.message_id, e);
    // Message is dropped, no retry
    // Other agents on same channel still receive it
}
```

### Channel Failures
```rust
Err(e) => {
    error!("Error receiving on channel {}: {}", channel, e);
    break;  // Listener task terminates
}
```

Error philosophy:
- **Fail-fast**: Bad messages are logged and dropped
- **Isolation**: One agent failure doesn't affect others
- **Observability**: All failures logged with context
- **No automatic retry**: Agents must implement their own retry logic

## Cache Integration

The shared cache uses DashMap for lock-free concurrent access:
```rust
pub struct Cache {
    pub data: Arc<DashMap<String, DashMap<String, Value>>>,
}

// Usage in agent
cache.insert_value(&node_id, "processed_count", &count)?;
let previous = cache.get_value(&node_id, "processed_count");
```

Cache patterns:
- **Node-scoped storage**: Each agent has its own namespace
- **Nested hashmaps**: `node_id -> key -> value`
- **Lock-free reads**: Multiple agents can read simultaneously
- **Serialization built-in**: Automatic serde conversion

## Workflow Execution

### Static Workflow
```rust
PubSubWorkflowSpec::StartWith { channel, message } => {
    // Publish initial message to kick off processing
    executor.publish(&channel, message, cache, None).await?;
}
```

### Reactive Workflow
```rust
PubSubWorkflowSpec::Reactive => {
    // Just start listeners, wait for external messages
}
```

### Cancellation Handling
```rust
tokio::select! {
    _ = cancel_rx => {
        *self.stopped.write().await = true;
        // All listener tasks check stopped flag
    }
    _ = futures::future::join_all(tasks) => {
        // Natural completion
    }
}
```

## Performance Characteristics

### Memory Usage
- **Channel buffers**: 1000 messages * sizeof(Message) per channel
- **Agent metadata**: ~1KB per registered agent
- **Cache growth**: Unbounded, application must manage

### CPU Usage
- **Listener tasks**: One OS thread per tokio worker * subscribers
- **Message cloning**: O(n) for n subscribers on a channel
- **Schema validation**: Cached compilation, O(1) after first use

### Throughput
- **Broadcast limit**: async-broadcast bounded at 1000 pending
- **Processing parallelism**: Limited by tokio worker threads
- **No batching**: Each message processed individually

## Example: Multi-Stage Pipeline

```rust
// Stage 1: Data Ingestion
struct Ingester;

#[async_trait]
impl PubSubAgent for Ingester {
    fn subscriptions(&self) -> Vec<String> {
        vec!["raw_input".to_string()]
    }
    
    fn publications(&self) -> Vec<String> {
        vec!["parsed_data".to_string()]
    }
    
    async fn process_message(
        &self,
        node_id: &str,
        _channel: &str,
        message: &Message,
        executor: &mut PubSubExecutor,
        cache: &Cache,
    ) -> Result<()> {
        // Parse raw data
        let raw = message.payload["content"].as_str().unwrap();
        let parsed = raw.split(',')
            .filter_map(|s| s.parse::<f64>().ok())
            .collect::<Vec<_>>();
        
        // Track in cache
        let count = cache.get_value(node_id, "processed_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) + 1;
        cache.insert_value(node_id, "processed_count", &count)?;
        
        // Publish to next stage
        let output = Message::new(
            node_id.to_string(),
            json!({
                "data": parsed,
                "count": count,
                "timestamp": chrono::Utc::now()
            })
        );
        executor.publish("parsed_data", output, cache, None).await?;
        
        Ok(())
    }
}

// Stage 2: Analysis
struct Analyzer;

#[async_trait]
impl PubSubAgent for Analyzer {
    fn subscriptions(&self) -> Vec<String> {
        vec!["parsed_data".to_string()]
    }
    
    fn publications(&self) -> Vec<String> {
        vec!["analysis_results".to_string()]
    }
    
    async fn process_message(
        &self,
        node_id: &str,
        _channel: &str,
        message: &Message,
        executor: &mut PubSubExecutor,
        cache: &Cache,
    ) -> Result<()> {
        let data = message.payload["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_f64())
            .collect::<Vec<_>>();
        
        // Compute statistics
        let mean = data.iter().sum::<f64>() / data.len() as f64;
        let variance = data.iter()
            .map(|x| (x - mean).powi(2))
            .sum::<f64>() / data.len() as f64;
        
        // Check cache for running statistics
        let prev_mean = cache.get_value(node_id, "running_mean")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        
        let alpha = 0.1;  // Exponential moving average
        let new_mean = alpha * mean + (1.0 - alpha) * prev_mean;
        cache.insert_value(node_id, "running_mean", &new_mean)?;
        
        // Publish results
        let output = Message::new(
            node_id.to_string(),
            json!({
                "mean": mean,
                "variance": variance,
                "running_mean": new_mean,
                "sample_size": data.len()
            })
        );
        executor.publish("analysis_results", output, cache, None).await?;
        
        Ok(())
    }
}
```

## Critical Implementation Details

### Why Agents Get Fresh Executors
Each message processing gets a new executor instance to:
1. Prevent state leakage between messages
2. Allow concurrent message processing
3. Isolate failures
4. Simplify the executor's internal state management

### Why Self-Messages Are Filtered
```rust
if message.source == node_id { continue; }
```
Without this check, agents that subscribe and publish to the same channel would create infinite loops.

### Why Receivers Are Kept Alive
```rust
struct ChannelInfo {
    _receiver: Receiver<Message>,  // Never used but kept
}
```
async-broadcast channels close when all receivers are dropped. Keeping one ensures the channel stays open even if no agents are currently subscribed.

### Task Spawning Strategy
One task per agent per channel subscription because:
- Blocking in one subscription doesn't affect others
- Natural parallelism for multi-channel agents
- Clean shutdown per subscription
- Better error isolation

## Limitations and Trade-offs

1. **No persistence**: Messages lost on crash
2. **No delivery guarantees**: At-most-once delivery
3. **No message ordering**: Between different publishers
4. **Memory pressure**: All messages kept in channel buffers
5. **No backpressure propagation**: Slow consumers can cause OOM

These are intentional trade-offs for simplicity and performance. For reliability, combine with the Task Agent system which provides persistence.