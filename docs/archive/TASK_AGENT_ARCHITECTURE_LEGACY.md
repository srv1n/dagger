# Task Agent Architecture

## Core Implementation Details

The Task Agent system is a two-tier architecture: a legacy compatibility layer (`taskagent`) wrapping a high-performance lock-free core (`task-core`). The system uses SQLite for persistence, DashMap for concurrent state, and a lock-free ready queue for task scheduling.

## Memory Model

### Task Storage Architecture
```rust
// Two-level storage: in-memory index + persistent SQLite
pub struct TaskManager {
    tasks_by_id: Arc<DashMap<String, Task>>,        // O(1) lookups
    tasks_by_job: Arc<DashMap<String, DashSet<String>>>,  // Job grouping
    agent_registry: Arc<TaskAgentRegistry>,          // Agent lookup
    cache: Arc<Cache>,                               // Shared cache (DashMap)
    db_path: String,                                 // SQLite persistence
}

// Task-core uses lock-free architecture
pub struct TaskSystem {
    storage: Arc<dyn Storage>,           // SQLite backend
    ready_queue: Arc<ReadyQueue<TaskId>>, // Lock-free MPMC queue
    scheduler: Arc<Scheduler>,           // Dependency resolution
    executor: Arc<Executor>,             // Worker pool
    shared_state: Arc<SharedState>,      // Cross-task communication
}
```

### Lock-Free Ready Queue
```rust
pub struct ReadyQueue<T> {
    inner: Arc<ArrayQueue<T>>,  // crossbeam lock-free queue
    notifier: Arc<Notify>,       // tokio notify for wakeups
}

impl<T> ReadyQueue<T> {
    pub fn push(&self, item: T) -> bool {
        if self.inner.push(item).is_ok() {
            self.notifier.notify_one();  // Wake a waiting worker
            true
        } else {
            false  // Queue full
        }
    }
    
    pub async fn pop(&self) -> Option<T> {
        loop {
            if let Some(item) = self.inner.pop() {
                return Some(item);
            }
            self.notifier.notified().await;  // Sleep until notified
        }
    }
}
```

## Task Lifecycle

### Task State Machine
```rust
pub enum TaskStatus {
    Pending,     // Waiting for dependencies
    InProgress,  // Being executed by agent
    Completed,   // Successfully finished
    Failed,      // Execution failed
    Blocked,     // Dependencies failed
    Accepted,    // Human approved (if needed)
    Rejected,    // Human rejected
}

// State transitions enforced by storage layer
impl Storage for SqliteStorage {
    async fn update_status(&self, id: TaskId, status: TaskStatus) -> Result<()> {
        // Atomic compare-and-swap in SQLite
        sqlx::query!(
            "UPDATE tasks SET status = ? WHERE id = ? AND status != ?",
            status as i32,
            id.to_string(),
            TaskStatus::Completed as i32  // Can't update completed tasks
        ).execute(&self.pool).await?;
        Ok(())
    }
}
```

### Task Claiming Protocol
```rust
pub fn claim_task(&self, task_id: &str, agent: String) -> Result<()> {
    // Atomic claim using DashMap entry API
    self.tasks_by_id.entry(task_id.to_string())
        .and_modify(|task| {
            if task.status == TaskStatus::Pending {
                task.status = TaskStatus::InProgress;
                task.updated_at = Utc::now().naive_utc();
                // Persist to SQLite
                self.persist_task(task);
            }
        });
}
```

## Dependency Resolution

### Scheduler Algorithm
```rust
impl Scheduler {
    pub async fn check_ready(&self) -> Result<Vec<TaskId>> {
        let mut ready = Vec::new();
        
        // Get all pending tasks
        let pending = self.storage.list_pending().await?;
        
        for task in pending {
            // Check if all dependencies completed
            let deps_ready = stream::iter(&task.dependencies)
                .all(|dep| async {
                    matches!(
                        self.storage.get_status(*dep).await,
                        Ok(Some(TaskStatus::Completed))
                    )
                })
                .await;
            
            if deps_ready {
                ready.push(task.task_id);
                // Enqueue to ready queue
                self.ready_queue.push(task.task_id);
            }
        }
        
        Ok(ready)
    }
}
```

### Cycle Detection
```rust
// Using Tarjan's algorithm for strongly connected components
pub fn detect_cycles(&self, job_id: &str) -> Result<bool> {
    let tasks = self.get_tasks_for_job(job_id);
    let mut graph = DiGraph::<String, ()>::new();
    let mut node_map = HashMap::new();
    
    // Build graph
    for task in &tasks {
        let node = graph.add_node(task.task_id.clone());
        node_map.insert(task.task_id.clone(), node);
    }
    
    // Add edges for dependencies
    for task in &tasks {
        let to = node_map[&task.task_id];
        for dep in &task.dependencies {
            if let Some(&from) = node_map.get(dep) {
                graph.add_edge(from, to, ());
            }
        }
    }
    
    // Check for cycles
    !is_cyclic_directed(&graph)
}
```

## Agent Execution

### Worker Pool Architecture
```rust
pub struct Executor {
    workers: usize,                      // Number of worker threads
    agent_registry: Arc<AgentRegistry>,  // Agent implementations
    semaphore: Arc<Semaphore>,          // Worker limiting
}

impl Executor {
    pub async fn run(self, shutdown_rx: Receiver<()>) -> Result<()> {
        loop {
            tokio::select! {
                _ = shutdown_rx => break,
                
                Some(task_id) = self.ready.pop() => {
                    // Acquire worker permit
                    let permit = self.semaphore.clone().acquire_owned().await?;
                    
                    // Spawn task execution
                    tokio::spawn(async move {
                        let _permit = permit;  // Held until task completes
                        self.execute_task(task_id).await;
                    });
                }
            }
        }
    }
    
    async fn execute_task(&self, task_id: TaskId) -> Result<()> {
        // Load task from storage
        let task = self.storage.get(task_id).await?;
        
        // Get agent
        let agent = self.agent_registry.get(task.agent_id)?;
        
        // Create context
        let ctx = Arc::new(TaskContext {
            handle: Arc::new(self.clone()),
            shared: self.shared_state.clone(),
            parent: task.parent,
            dependencies: task.dependencies.into(),
        });
        
        // Execute with timeout
        let result = timeout(
            task.timeout.unwrap_or(Duration::from_secs(300)),
            agent.execute(task.input, ctx)
        ).await;
        
        // Update status based on result
        match result {
            Ok(Ok(output)) => {
                self.storage.set_output(task_id, output).await?;
                self.storage.update_status(task_id, TaskStatus::Completed).await?;
            }
            Ok(Err(e)) => {
                self.handle_failure(task_id, e).await?;
            }
            Err(_) => {
                self.handle_timeout(task_id).await?;
            }
        }
        
        Ok(())
    }
}
```

### Dynamic Task Creation
```rust
#[async_trait]
impl TaskHandle for ExecutorHandle {
    async fn spawn_task(&self, spec: NewTaskSpec) -> Result<TaskId> {
        // Generate new task ID
        let task_id = TaskId::new();
        
        // Create task
        let task = Task {
            task_id,
            job_id: spec.job_id,
            parent: None,
            dependencies: spec.dependencies,
            agent_id: spec.agent_id,
            input: spec.input,
            status: TaskStatus::Pending,
            created_at: Utc::now(),
            // ...
        };
        
        // Store in database
        self.storage.insert(task).await?;
        
        // Schedule for dependency check
        self.scheduler.schedule(task_id).await?;
        
        Ok(task_id)
    }
    
    async fn spawn_dependent(
        &self, 
        parent: TaskId, 
        mut spec: NewTaskSpec
    ) -> Result<TaskId> {
        // Add parent as dependency
        spec.dependencies.push(parent);
        
        // Create with parent link
        let task_id = self.spawn_task(spec).await?;
        
        // Update parent's children list
        self.storage.add_child(parent, task_id).await?;
        
        Ok(task_id)
    }
}
```

## Persistence Layer

### SQLite Schema
```sql
CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    parent_id TEXT,
    agent_id INTEGER NOT NULL,
    status INTEGER NOT NULL,
    input BLOB NOT NULL,
    output BLOB,
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    completed_at INTEGER,
    error TEXT,
    retry_count INTEGER DEFAULT 0,
    INDEX idx_job_id (job_id),
    INDEX idx_status (status),
    INDEX idx_parent (parent_id)
);

CREATE TABLE task_dependencies (
    task_id TEXT NOT NULL,
    dependency_id TEXT NOT NULL,
    PRIMARY KEY (task_id, dependency_id),
    FOREIGN KEY (task_id) REFERENCES tasks(id),
    FOREIGN KEY (dependency_id) REFERENCES tasks(id)
);

CREATE TABLE shared_state (
    scope TEXT NOT NULL,
    key TEXT NOT NULL,
    value BLOB NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (scope, key)
);
```

### Transaction Guarantees
```rust
impl SqliteStorage {
    pub async fn atomic_update<F>(&self, f: F) -> Result<()> 
    where
        F: FnOnce(&mut Transaction) -> Result<()>
    {
        let mut tx = self.pool.begin().await?;
        
        // Set isolation level
        sqlx::query!("PRAGMA read_uncommitted = 0")
            .execute(&mut tx).await?;
        
        // Execute updates
        f(&mut tx)?;
        
        // Commit atomically
        tx.commit().await?;
        
        Ok(())
    }
}
```

## Retry & Recovery

### Retry Strategy
```rust
pub enum RetryStrategy {
    Fixed { delay: Duration },
    Exponential { base: Duration, max: Duration },
    Custom(Box<dyn Fn(u32) -> Duration>),
}

impl TaskManager {
    async fn handle_failure(&self, task_id: TaskId, error: AgentError) -> Result<()> {
        let task = self.storage.get(task_id).await?;
        
        if task.retry_count < task.max_retries {
            // Calculate retry delay
            let delay = match &task.retry_strategy {
                RetryStrategy::Fixed { delay } => *delay,
                RetryStrategy::Exponential { base, max } => {
                    (*base * 2_u32.pow(task.retry_count)).min(*max)
                }
                RetryStrategy::Custom(f) => f(task.retry_count),
            };
            
            // Schedule retry
            tokio::spawn(async move {
                sleep(delay).await;
                self.storage.update_status(task_id, TaskStatus::Pending).await?;
                self.storage.increment_retry(task_id).await?;
                self.ready_queue.push(task_id);
            });
        } else {
            // Max retries exceeded
            self.storage.update_status(task_id, TaskStatus::Failed).await?;
            self.propagate_failure(task_id).await?;
        }
        
        Ok(())
    }
}
```

### Crash Recovery
```rust
impl Recovery {
    pub async fn recover(&self) -> Result<RecoveryStats> {
        let mut stats = RecoveryStats::default();
        
        // Find tasks that were running
        let running = self.storage.list_running().await?;
        stats.total_running = running.len();
        
        for task in running {
            match task.durability {
                Durability::BestEffort => {
                    // Reset to pending for retry
                    self.storage.update_status(task.task_id, TaskStatus::Pending).await?;
                    self.ready_queue.push(task.task_id);
                    stats.retried += 1;
                }
                Durability::AtMostOnce => {
                    // Mark as blocked, needs manual intervention
                    self.storage.update_status(task.task_id, TaskStatus::Blocked).await?;
                    stats.paused += 1;
                }
            }
        }
        
        Ok(stats)
    }
}
```

## Shared State Management

### Cross-Task Communication
```rust
pub struct SharedState {
    tree: Arc<dyn SharedTree>,  // SQLite-backed key-value store
}

impl SharedState {
    // Scoped key-value operations
    pub async fn put(&self, scope: &str, key: &str, val: &[u8]) -> Result<()> {
        sqlx::query!(
            "INSERT OR REPLACE INTO shared_state (scope, key, value, updated_at) 
             VALUES (?, ?, ?, ?)",
            scope, key, val, Utc::now().timestamp()
        ).execute(&self.pool).await?;
        Ok(())
    }
    
    pub async fn get(&self, scope: &str, key: &str) -> Result<Option<Bytes>> {
        let row = sqlx::query!(
            "SELECT value FROM shared_state WHERE scope = ? AND key = ?",
            scope, key
        ).fetch_optional(&self.pool).await?;
        
        Ok(row.map(|r| Bytes::from(r.value)))
    }
}

// Usage in agent
impl Agent for DataProcessor {
    async fn execute(&self, input: Bytes, ctx: Arc<TaskContext>) -> Result<Bytes> {
        // Get dependency outputs
        for dep_id in ctx.dependencies.iter() {
            if let Some(output) = ctx.dependency_output(*dep_id).await? {
                // Process dependency data
            }
        }
        
        // Store intermediate result
        ctx.shared.put("analytics", "last_run", &timestamp).await?;
        
        // Spawn sub-task
        let subtask = ctx.handle.spawn_dependent(
            ctx.task_id,
            NewTaskSpec {
                agent_id: VALIDATOR_AGENT,
                input: processed_data,
                // ...
            }
        ).await?;
        
        Ok(output)
    }
}
```

## Performance Optimizations

### Lock-Free Operations
```rust
// DashMap for concurrent access without locks
pub struct Cache {
    data: Arc<DashMap<String, DashMap<String, Value>>>,
}

// Crossbeam ArrayQueue for lock-free MPMC
pub struct ReadyQueue<T> {
    inner: Arc<ArrayQueue<T>>,
}

// Atomic operations for status updates
use std::sync::atomic::{AtomicU8, Ordering};
status: AtomicU8,  // Can update without locks
```

### Batch Operations
```rust
impl Storage for SqliteStorage {
    async fn batch_insert(&self, tasks: Vec<Task>) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        
        // Prepare statement once
        let stmt = tx.prepare(
            "INSERT INTO tasks (id, job_id, ...) VALUES (?, ?, ...)"
        ).await?;
        
        // Execute in batch
        for task in tasks {
            stmt.execute(&[task.id, task.job_id, ...]).await?;
        }
        
        tx.commit().await?;
        Ok(())
    }
}
```

### Connection Pooling
```rust
// SQLx connection pool configuration
let pool = SqlitePoolOptions::new()
    .max_connections(32)           // Worker threads * 2
    .min_connections(4)            // Keep warm connections
    .acquire_timeout(Duration::from_secs(3))
    .idle_timeout(Duration::from_secs(300))
    .connect(&database_url).await?;
```

## Monitoring & Observability

### DOT Graph Generation
```rust
pub fn get_dot_graph(&self, job_id: Option<&str>) -> Result<String> {
    let mut dot = String::from("digraph TaskGraph {\n");
    dot.push_str("  rankdir=TB;\n");
    dot.push_str("  node [shape=box];\n");
    
    let tasks = if let Some(id) = job_id {
        self.get_tasks_for_job(id)
    } else {
        self.get_all_tasks()
    };
    
    for task in tasks {
        // Node with status coloring
        let color = match task.status {
            TaskStatus::Completed => "green",
            TaskStatus::Failed => "red",
            TaskStatus::InProgress => "yellow",
            TaskStatus::Blocked => "orange",
            _ => "white",
        };
        
        dot.push_str(&format!(
            "  \"{}\" [label=\"{}\\n{}\\n{}\", fillcolor={}, style=filled];\n",
            task.task_id, task.task_id, task.agent, task.status, color
        ));
        
        // Edges for dependencies
        for dep in &task.dependencies {
            dot.push_str(&format!("  \"{}\" -> \"{}\";\n", dep, task.task_id));
        }
    }
    
    dot.push_str("}\n");
    Ok(dot)
}
```

### Metrics Collection
```rust
#[cfg(feature = "metrics")]
pub struct TaskMetrics {
    tasks_created: Counter,
    tasks_completed: Counter,
    tasks_failed: Counter,
    task_duration: Histogram,
    queue_depth: Gauge,
}

impl TaskMetrics {
    pub fn record_completion(&self, duration: Duration) {
        self.tasks_completed.increment(1);
        self.task_duration.record(duration.as_secs_f64());
    }
}
```

## Critical Design Decisions

### Why Two-Tier Architecture
The system has both `taskagent` (legacy) and `task-core` (modern) because:
1. **Backward compatibility**: Existing code uses the taskagent API
2. **Performance**: task-core provides lock-free operations
3. **Migration path**: Gradual transition to the new system
4. **Feature parity**: Both systems work together

### Why SQLite Over Sled
Originally used sled, migrated to SQLite because:
1. **ACID transactions**: SQLite provides true ACID guarantees
2. **SQL queries**: Complex filtering and joins
3. **Mature ecosystem**: Better tooling and debugging
4. **Performance**: SQLite is faster for our workload
5. **Stability**: No corruption issues

### Why Lock-Free Queues
Using crossbeam ArrayQueue instead of channels because:
1. **Bounded capacity**: Prevents memory exhaustion
2. **MPMC**: Multiple producers and consumers
3. **No allocations**: Fixed-size array backing
4. **Cache-friendly**: Better CPU cache utilization

## Limitations

1. **SQLite write bottleneck**: Single writer limitation
2. **No distributed execution**: Single-node only
3. **Memory usage**: All task metadata in memory
4. **No streaming**: Tasks must fit in memory
5. **Recovery time**: Proportional to number of tasks

These trade-offs prioritize reliability and simplicity over scale. For distributed execution, consider combining with the DAG Flow system.