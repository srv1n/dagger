-- Dagger SQLite Schema
-- This schema supports flow runs, node runs, artifacts, tasks, and shared state

-- Flow runs table
CREATE TABLE IF NOT EXISTS flow_runs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'failed', 'cancelled')),
    created_at TEXT NOT NULL DEFAULT (datetime('now', 'utc')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now', 'utc')),
    started_at TEXT,
    finished_at TEXT,
    parameters TEXT, -- JSON
    error_message TEXT
);

-- Node runs table
CREATE TABLE IF NOT EXISTS node_runs (
    id TEXT PRIMARY KEY,
    flow_run_id TEXT NOT NULL,
    node_name TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'failed', 'skipped', 'cancelled')),
    created_at TEXT NOT NULL DEFAULT (datetime('now', 'utc')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now', 'utc')),
    started_at TEXT,
    finished_at TEXT,
    inputs TEXT, -- JSON
    outputs TEXT, -- JSON
    error_message TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (flow_run_id) REFERENCES flow_runs(id) ON DELETE CASCADE
);

-- Artifacts table - supports both inline data and file references
CREATE TABLE IF NOT EXISTS artifacts (
    id TEXT PRIMARY KEY,
    flow_run_id TEXT,
    node_run_id TEXT,
    key TEXT NOT NULL,
    artifact_type TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now', 'utc')),
    data TEXT, -- JSON data for small artifacts
    file_path TEXT, -- File path for large artifacts
    metadata TEXT, -- JSON metadata
    FOREIGN KEY (flow_run_id) REFERENCES flow_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (node_run_id) REFERENCES node_runs(id) ON DELETE CASCADE,
    CHECK ((data IS NOT NULL AND file_path IS NULL) OR (data IS NULL AND file_path IS NOT NULL))
);

-- Outbox table for event sourcing and external integration
CREATE TABLE IF NOT EXISTS outbox (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL, -- JSON
    created_at TEXT NOT NULL DEFAULT (datetime('now', 'utc')),
    processed BOOLEAN NOT NULL DEFAULT FALSE,
    processed_at TEXT
);

-- Tasks table for task management
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    flow_run_id TEXT,
    name TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'failed', 'cancelled')),
    created_at TEXT NOT NULL DEFAULT (datetime('now', 'utc')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now', 'utc')),
    scheduled_at TEXT,
    started_at TEXT,
    finished_at TEXT,
    priority INTEGER NOT NULL DEFAULT 0,
    max_retries INTEGER NOT NULL DEFAULT 3,
    retry_count INTEGER NOT NULL DEFAULT 0,
    payload TEXT, -- JSON
    result TEXT, -- JSON
    error_message TEXT,
    FOREIGN KEY (flow_run_id) REFERENCES flow_runs(id) ON DELETE SET NULL
);

-- Task dependencies table
CREATE TABLE IF NOT EXISTS task_dependencies (
    task_id TEXT NOT NULL,
    depends_on_task_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now', 'utc')),
    PRIMARY KEY (task_id, depends_on_task_id),
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (depends_on_task_id) REFERENCES tasks(id) ON DELETE CASCADE
);

-- Shared state table for cross-flow communication
CREATE TABLE IF NOT EXISTS shared_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL, -- JSON
    created_at TEXT NOT NULL DEFAULT (datetime('now', 'utc')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now', 'utc')),
    version INTEGER NOT NULL DEFAULT 1,
    ttl TEXT -- Optional TTL as ISO datetime
);

-- Indexes for performance

-- Flow runs indexes
CREATE INDEX IF NOT EXISTS idx_flow_runs_status ON flow_runs(status);
CREATE INDEX IF NOT EXISTS idx_flow_runs_created_at ON flow_runs(created_at);
CREATE INDEX IF NOT EXISTS idx_flow_runs_name ON flow_runs(name);

-- Node runs indexes
CREATE INDEX IF NOT EXISTS idx_node_runs_flow_run_id ON node_runs(flow_run_id);
CREATE INDEX IF NOT EXISTS idx_node_runs_status ON node_runs(status);
CREATE INDEX IF NOT EXISTS idx_node_runs_node_name ON node_runs(node_name);
CREATE INDEX IF NOT EXISTS idx_node_runs_created_at ON node_runs(created_at);

-- Artifacts indexes
CREATE INDEX IF NOT EXISTS idx_artifacts_flow_run_id ON artifacts(flow_run_id);
CREATE INDEX IF NOT EXISTS idx_artifacts_node_run_id ON artifacts(node_run_id);
CREATE INDEX IF NOT EXISTS idx_artifacts_key ON artifacts(key);
CREATE INDEX IF NOT EXISTS idx_artifacts_type ON artifacts(artifact_type);
CREATE INDEX IF NOT EXISTS idx_artifacts_created_at ON artifacts(created_at);

-- Outbox indexes
CREATE INDEX IF NOT EXISTS idx_outbox_processed ON outbox(processed);
CREATE INDEX IF NOT EXISTS idx_outbox_created_at ON outbox(created_at);
CREATE INDEX IF NOT EXISTS idx_outbox_event_type ON outbox(event_type);

-- Tasks indexes
CREATE INDEX IF NOT EXISTS idx_tasks_flow_run_id ON tasks(flow_run_id);
CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_priority ON tasks(priority DESC);
CREATE INDEX IF NOT EXISTS idx_tasks_scheduled_at ON tasks(scheduled_at);
CREATE INDEX IF NOT EXISTS idx_tasks_created_at ON tasks(created_at);
CREATE INDEX IF NOT EXISTS idx_tasks_name ON tasks(name);

-- Task dependencies indexes
CREATE INDEX IF NOT EXISTS idx_task_dependencies_task_id ON task_dependencies(task_id);
CREATE INDEX IF NOT EXISTS idx_task_dependencies_depends_on ON task_dependencies(depends_on_task_id);

-- Shared state indexes
CREATE INDEX IF NOT EXISTS idx_shared_state_created_at ON shared_state(created_at);
CREATE INDEX IF NOT EXISTS idx_shared_state_updated_at ON shared_state(updated_at);
CREATE INDEX IF NOT EXISTS idx_shared_state_ttl ON shared_state(ttl);

-- Triggers for automatic updated_at timestamps

-- Flow runs updated_at trigger
CREATE TRIGGER IF NOT EXISTS update_flow_runs_updated_at
    AFTER UPDATE ON flow_runs
    BEGIN
        UPDATE flow_runs SET updated_at = datetime('now', 'utc') WHERE id = NEW.id;
    END;

-- Node runs updated_at trigger
CREATE TRIGGER IF NOT EXISTS update_node_runs_updated_at
    AFTER UPDATE ON node_runs
    BEGIN
        UPDATE node_runs SET updated_at = datetime('now', 'utc') WHERE id = NEW.id;
    END;

-- Tasks updated_at trigger
CREATE TRIGGER IF NOT EXISTS update_tasks_updated_at
    AFTER UPDATE ON tasks
    BEGIN
        UPDATE tasks SET updated_at = datetime('now', 'utc') WHERE id = NEW.id;
    END;

-- Shared state updated_at and version trigger
CREATE TRIGGER IF NOT EXISTS update_shared_state_updated_at
    AFTER UPDATE ON shared_state
    BEGIN
        UPDATE shared_state 
        SET updated_at = datetime('now', 'utc'), version = version + 1 
        WHERE key = NEW.key;
    END;

-- Cleanup trigger for expired shared state
CREATE TRIGGER IF NOT EXISTS cleanup_expired_shared_state
    AFTER INSERT ON shared_state
    BEGIN
        DELETE FROM shared_state 
        WHERE ttl IS NOT NULL AND ttl < datetime('now', 'utc');
    END;