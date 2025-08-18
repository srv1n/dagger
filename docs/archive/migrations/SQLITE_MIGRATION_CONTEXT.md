# SQLite Migration Context for Dagger Workflow Engine

## Executive Summary

We're migrating the Dagger workflow orchestration library from Sled (embedded KV store) to SQLite to enable better queryability, transactional guarantees, and support for pause/edit/resume workflows with exactly-once semantics.

## What is Dagger?

Dagger is a Rust library providing three workflow orchestration paradigms:

### 1. DAG Flow System
- **Purpose**: Execute YAML-defined directed acyclic graphs
- **Features**: Dependencies, retries, timeouts, parallel execution
- **Current Storage**: Sled trees for cache, execution history, and state

### 2. Task-Core System  
- **Purpose**: High-performance task execution with dynamic dependencies
- **Features**: Lock-free design, crash recovery, agent-based execution
- **Current Storage**: Sled with tasks, outputs, and shared state trees

### 3. Pub/Sub System
- **Purpose**: Event-driven multi-agent communication
- **Features**: Dynamic channels, message routing, schema validation
- **Current Storage**: Memory-only (no persistence currently)

## Current Sled Usage Patterns

### What We're Storing

1. **Task/Node Execution State**
   - Task ID, status, agent assignment
   - Input/output data (can be large JSON)
   - Dependencies between tasks
   - Retry counts and metadata

2. **Cache Data**
   - Intermediate computation results
   - Key-value pairs per node execution
   - Sometimes compressed with zstd

3. **Execution History**
   - Complete execution trees for debugging
   - Performance metrics
   - Error traces

4. **Shared State**
   - Inter-task communication
   - Scoped key-value pairs
   - Temporary coordination data

### Current Operations

- **Compare-and-swap** for atomic status updates
- **Range scans** to find tasks by status
- **Binary serialization** using bincode
- **Multiple namespaces** (Sled trees) for logical separation

## Requirements for New System

### Functional Requirements

1. **Pause/Edit/Resume Workflows**
   - Pause execution at any point
   - Edit intermediate results
   - Resume with invalidation of affected downstream nodes
   - Audit trail of edits

2. **Exactly-Once Semantics**
   - Idempotent operations
   - Outbox pattern for side effects
   - Transactional guarantees

3. **Debugging and Observability**
   - Query execution history
   - Analyze performance bottlenecks
   - Inspect intermediate states
   - Track lineage of results

4. **Integration with Downstream Systems**
   - Chat/conversation systems need to query workflow state
   - UI needs real-time updates
   - Analytics systems need to aggregate metrics
   - Enterprise telemetry requirements

### Non-Functional Requirements

- **Performance**: Handle thousands of tasks per workflow
- **Concurrency**: Multiple workflows executing simultaneously  
- **Durability**: Survive crashes without data loss
- **Portability**: Work on Windows, macOS, Linux
- **Queryability**: Ad-hoc SQL queries for debugging

## Proposed Schema Design

### Core Execution Tables

```sql
-- Main workflow execution tracking
CREATE TABLE flow_runs (
  run_id TEXT PRIMARY KEY,
  flow_name TEXT NOT NULL,
  flow_version TEXT NOT NULL,
  dag_definition TEXT,              -- YAML or JSON of the DAG
  status TEXT NOT NULL,             -- running|awaiting_input|succeeded|failed|paused|canceled
  config JSON,                      -- Execution configuration
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  started_at DATETIME,
  completed_at DATETIME,
  paused_at DATETIME,
  error_message TEXT,
  INDEX idx_flow_status (status),
  INDEX idx_flow_name (flow_name)
);

-- Individual node execution within a flow
CREATE TABLE node_runs (
  run_id TEXT NOT NULL,
  node_id TEXT NOT NULL,
  attempt INTEGER NOT NULL DEFAULT 1,
  state TEXT NOT NULL,              -- pending|running|awaiting_input|succeeded|failed|canceled|skipped
  agent_type TEXT,                  -- Which agent/action executes this
  input_hash TEXT NOT NULL,         -- For cache invalidation
  output_artifact_id TEXT,          -- Reference to artifacts table
  error TEXT,
  retry_count INTEGER DEFAULT 0,
  max_retries INTEGER,
  timeout_seconds INTEGER,
  started_at DATETIME,
  finished_at DATETIME,
  metadata JSON,                    -- Additional node-specific data
  PRIMARY KEY (run_id, node_id, attempt),
  FOREIGN KEY (run_id) REFERENCES flow_runs(run_id),
  INDEX idx_node_state (state),
  INDEX idx_node_agent (agent_type)
);

-- Dependencies between nodes
CREATE TABLE node_dependencies (
  run_id TEXT NOT NULL,
  node_id TEXT NOT NULL,
  depends_on_node_id TEXT NOT NULL,
  PRIMARY KEY (run_id, node_id, depends_on_node_id),
  FOREIGN KEY (run_id, node_id) REFERENCES node_runs(run_id, node_id)
);
```

### Artifact Storage

```sql
-- Content-addressed storage for node outputs
CREATE TABLE artifacts (
  artifact_id TEXT PRIMARY KEY,     -- sha256(content)
  kind TEXT NOT NULL,               -- json|text|binary|passages|topics|citations
  mime_type TEXT,
  schema_version INTEGER NOT NULL,
  size_bytes INTEGER NOT NULL,
  is_compressed BOOLEAN DEFAULT FALSE,
  compression_type TEXT,            -- zstd|gzip|none
  
  -- Storage strategy based on size
  storage_location TEXT NOT NULL,   -- inline|file|s3
  inline_data BLOB,                 -- When small (<100KB)
  file_path TEXT,                   -- When medium (100KB-10MB)
  s3_url TEXT,                      -- When large (>10MB)
  
  -- Lineage and provenance
  created_by_run_id TEXT,
  created_by_node_id TEXT,
  provenance JSON,                  -- inputs, model, parameters
  
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  accessed_at DATETIME,
  access_count INTEGER DEFAULT 0,
  
  INDEX idx_artifact_kind (kind),
  INDEX idx_artifact_created (created_by_run_id, created_by_node_id)
);

-- Track user edits to artifacts
CREATE TABLE artifact_patches (
  patch_id TEXT PRIMARY KEY,
  base_artifact_id TEXT NOT NULL,
  new_artifact_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  node_id TEXT NOT NULL,
  
  editor_type TEXT NOT NULL,       -- user|system|auto
  editor_id TEXT,
  reason TEXT,
  
  -- Different patch types
  patch_type TEXT NOT NULL,        -- json_patch|text_diff|full_replacement
  patch_data JSON,                  -- JSON Patch or diff format
  
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  applied_at DATETIME,
  
  FOREIGN KEY (base_artifact_id) REFERENCES artifacts(artifact_id),
  FOREIGN KEY (new_artifact_id) REFERENCES artifacts(artifact_id),
  INDEX idx_patch_base (base_artifact_id)
);
```

### Cache and State Management

```sql
-- Fast key-value cache for intermediate computations
CREATE TABLE computation_cache (
  cache_key TEXT PRIMARY KEY,       -- Composite of node_id + input_hash
  run_id TEXT NOT NULL,
  node_id TEXT NOT NULL,
  value_artifact_id TEXT,
  computation_time_ms INTEGER,
  
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  expires_at DATETIME,
  hit_count INTEGER DEFAULT 0,
  last_accessed DATETIME,
  
  FOREIGN KEY (value_artifact_id) REFERENCES artifacts(artifact_id),
  INDEX idx_cache_run (run_id),
  INDEX idx_cache_expires (expires_at)
);

-- Shared state for inter-node communication
CREATE TABLE shared_state (
  run_id TEXT NOT NULL,
  scope TEXT NOT NULL,              -- Namespace for isolation
  key TEXT NOT NULL,
  value BLOB,
  value_type TEXT,                  -- json|text|binary
  
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  expires_at DATETIME,
  
  PRIMARY KEY (run_id, scope, key),
  INDEX idx_shared_scope (run_id, scope)
);
```

### Side Effects and Events

```sql
-- Outbox pattern for exactly-once delivery
CREATE TABLE outbox (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id TEXT NOT NULL,
  node_id TEXT NOT NULL,
  
  -- Idempotency
  idempotency_key TEXT NOT NULL UNIQUE,
  
  -- Event details
  event_type TEXT NOT NULL,        -- task_started|task_completed|message_sent|api_call
  target_system TEXT,               -- chat|email|webhook|database
  payload JSON NOT NULL,
  
  -- Delivery tracking
  status TEXT DEFAULT 'pending',    -- pending|dispatched|confirmed|failed
  retry_count INTEGER DEFAULT 0,
  max_retries INTEGER DEFAULT 3,
  
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  dispatched_at DATETIME,
  confirmed_at DATETIME,
  next_retry_at DATETIME,
  
  INDEX idx_outbox_status (status),
  INDEX idx_outbox_idempotent (idempotency_key)
);

-- Event streaming for UI and monitoring
CREATE TABLE workflow_events (
  event_id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id TEXT NOT NULL,
  node_id TEXT,
  
  event_type TEXT NOT NULL,        -- Granular event types
  severity TEXT,                    -- info|warning|error|critical
  
  -- Event data
  message TEXT,
  details JSON,
  
  -- Timing
  occurred_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  
  -- Consumption tracking
  consumed_by TEXT,                 -- Which system consumed this
  consumed_at DATETIME,
  
  INDEX idx_events_run (run_id),
  INDEX idx_events_type (event_type),
  INDEX idx_events_unconsumed (consumed_at) WHERE consumed_at IS NULL
);
```

### Task Agent Specific Tables

```sql
-- Task definitions for the task-core system
CREATE TABLE tasks (
  task_id INTEGER PRIMARY KEY,
  job_id TEXT NOT NULL,
  
  -- Task details
  name TEXT NOT NULL,
  description TEXT,
  agent_type TEXT NOT NULL,
  
  -- Status tracking
  status TEXT NOT NULL,             -- pending|ready|running|blocked|completed|failed
  
  -- Input/Output
  input_data BLOB,
  output_artifact_id TEXT,
  
  -- Dependencies
  dependencies JSON,                -- Array of task_ids
  blocking_on JSON,                 -- Current blockers
  
  -- Execution details
  assigned_to TEXT,                 -- Worker/agent ID
  priority INTEGER DEFAULT 0,
  retry_count INTEGER DEFAULT 0,
  max_retries INTEGER DEFAULT 3,
  
  -- Timing
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  ready_at DATETIME,
  started_at DATETIME,
  completed_at DATETIME,
  
  -- Metrics
  execution_time_ms INTEGER,
  queue_time_ms INTEGER,
  
  INDEX idx_task_status (status),
  INDEX idx_task_job (job_id),
  INDEX idx_task_agent (agent_type)
);
```

## Query Patterns We Need to Support

### Operational Queries

```sql
-- Find all paused workflows awaiting user input
SELECT * FROM flow_runs 
WHERE status = 'awaiting_input' 
ORDER BY created_at DESC;

-- Get next ready task for an agent
SELECT * FROM tasks 
WHERE status = 'ready' 
  AND agent_type = ? 
  AND retry_count < max_retries
ORDER BY priority DESC, created_at ASC
LIMIT 1;

-- Find stuck nodes (running > timeout)
SELECT * FROM node_runs 
WHERE state = 'running' 
  AND datetime(started_at, '+' || timeout_seconds || ' seconds') < datetime('now');
```

### Analytics Queries

```sql
-- Node execution performance by type
SELECT 
  agent_type,
  COUNT(*) as executions,
  AVG(julianday(finished_at) - julianday(started_at)) * 86400 as avg_duration_seconds,
  SUM(CASE WHEN state = 'failed' THEN 1 ELSE 0 END) as failures
FROM node_runs
WHERE finished_at IS NOT NULL
GROUP BY agent_type;

-- Cache hit rates
SELECT 
  node_id,
  COUNT(*) as total_executions,
  SUM(hit_count) as cache_hits,
  CAST(SUM(hit_count) AS FLOAT) / COUNT(*) as hit_rate
FROM computation_cache
GROUP BY node_id;
```

### Debugging Queries

```sql
-- Full execution trace for a workflow
SELECT 
  nr.*,
  a.kind as output_type,
  a.size_bytes
FROM node_runs nr
LEFT JOIN artifacts a ON nr.output_artifact_id = a.artifact_id
WHERE nr.run_id = ?
ORDER BY nr.started_at;

-- Find all artifacts affected by an edit
WITH RECURSIVE affected AS (
  SELECT new_artifact_id as artifact_id
  FROM artifact_patches
  WHERE base_artifact_id = ?
  
  UNION
  
  SELECT ap.new_artifact_id
  FROM artifact_patches ap
  JOIN affected a ON ap.base_artifact_id = a.artifact_id
)
SELECT * FROM affected;
```

## Integration Points

### 1. Chat/Conversation System
- Needs to query workflow outputs
- Updates conversation state based on workflow events
- Requires transactional consistency with workflow state

### 2. UI/Frontend
- Real-time progress updates via workflow_events
- Pause/resume controls updating flow_runs
- Artifact viewing and editing

### 3. Monitoring/Telemetry
- Export metrics from aggregated queries
- Stream events to external systems
- Alert on stuck/failed workflows

### 4. Storage Systems
- Local file system for medium artifacts
- S3/blob storage for large artifacts
- Inline storage for small, frequently accessed data

## Migration Considerations

### From Sled's Perspective

**What Sled Did Well:**
- Simple key-value operations
- Built-in compression
- Multiple namespaces (trees)
- Compare-and-swap operations

**What We're Replacing With:**
- SQL transactions instead of CAS
- SQLite compression extensions or application-level compression
- Tables instead of trees
- WHERE clauses for conditional updates

### Performance Optimizations Needed

1. **Write Batching**: Group inserts/updates in transactions
2. **Connection Pooling**: Use SQLx pool with appropriate size
3. **Prepared Statements**: Cache frequently used queries
4. **Indexes**: Strategic indexes on status, timestamps, foreign keys
5. **WAL Mode**: Enable Write-Ahead Logging for concurrency
6. **Vacuum Schedule**: Regular maintenance for long-running systems

### Data Migration Strategy

1. **Dual-write period**: Write to both Sled and SQLite
2. **Verification**: Compare reads from both systems
3. **Cutover**: Switch reads to SQLite
4. **Cleanup**: Remove Sled after confidence period

## Questions for External Review

1. **Schema Design**
   - Is the artifact storage strategy (inline vs file) appropriate?
   - Should we denormalize any relationships for performance?
   - Are the indexes sufficient for the query patterns?

2. **Transactional Boundaries**
   - Which operations must be atomic together?
   - How to handle long-running transactions?

3. **Scaling Considerations**
   - At what data volume should we consider partitioning?
   - Should we implement logical sharding from the start?

4. **Event Streaming**
   - Is the workflow_events table sufficient for real-time updates?
   - Should we use SQLite's update hooks or poll?

5. **Cache Invalidation**
   - How aggressive should cache expiration be?
   - Should we implement LRU eviction in SQL?

## Specific Implementation Questions

### For Task-Core System
- How to map dynamic task dependencies to SQL?
- Best approach for the ready queue (polling vs triggers)?
- Should agent assignment be lease-based with expiration?

### For DAG Flow System
- How to store YAML definitions (normalized vs denormalized)?
- Best way to handle parallel node execution tracking?
- Should we version the DAG definitions separately?

### For Pub/Sub System
- Should messages be persisted at all?
- If yes, how long to retain them?
- How to handle fan-out patterns in SQL?

## Success Criteria

1. **Functional**: All existing examples and tests pass
2. **Performance**: No regression in throughput (target: 1000 tasks/second)
3. **Reliability**: Graceful handling of crashes with full recovery
4. **Queryability**: Sub-second responses for operational queries
5. **Maintainability**: Clear schema with documentation

---

This migration will transform Dagger from a simple workflow executor to a full workflow platform with enterprise-grade observability, debugging, and integration capabilities.