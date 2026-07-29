//! Versioned, restartable SQLite schema migrations.

use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::store::StoreError;

/// Current durable schema version.
///
/// This pre-release adapter has one coherent baseline. Earlier local W6
/// migration numbers never represented independently usable schemas and were
/// deliberately collapsed rather than retained as fictional compatibility.
pub const SCHEMA_VERSION: i64 = 1;

const MIGRATION_1: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_definitions (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, definition_id TEXT NOT NULL,
        display_name TEXT NOT NULL, description TEXT NOT NULL, created_at_ms INTEGER NOT NULL,
        created_by TEXT NOT NULL, latest_revision_hash TEXT, version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, definition_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_revisions (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, definition_id TEXT NOT NULL,
        revision_hash TEXT NOT NULL, canonical_definition_digest TEXT NOT NULL,
        run_input_schema_digest TEXT NOT NULL, run_output_schema_digest TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL, created_by TEXT NOT NULL,
        PRIMARY KEY (tenant_id, namespace, definition_id, revision_hash)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_revision_nodes (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, definition_id TEXT NOT NULL,
        revision_hash TEXT NOT NULL, node_id TEXT NOT NULL, node_kind TEXT NOT NULL,
        topological_rank INTEGER NOT NULL CHECK(topological_rank >= 0),
        PRIMARY KEY (tenant_id, namespace, definition_id, revision_hash, node_id)
    ) STRICT"#,
    // Each authority table stores one independently CAS-protected domain row.
    // The payload is the control-plane entity codec, never object bytes.  The
    // canonical revision row is also the durable home of its parsed revision.
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_definition_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_revision_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_engine_claim_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_run_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_node_run_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_edge_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_attempt_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_approval_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_invocation_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_idempotency_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_artifact_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_object_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_event_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_event_batch_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_budget_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_stale_observation_rows (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_scope_counters (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
        sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, entity_id, sub_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_engine_claims (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, control_plane_id TEXT NOT NULL,
        instance_id TEXT NOT NULL, generation INTEGER NOT NULL CHECK(generation > 0),
        session_token_digest TEXT NOT NULL, released_token_digest TEXT,
        claimed_at_ms INTEGER NOT NULL, heartbeat_at_ms INTEGER NOT NULL,
        expires_at_ms INTEGER NOT NULL, version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, control_plane_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_runs (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, run_id TEXT NOT NULL,
        definition_id TEXT NOT NULL, revision_hash TEXT NOT NULL, status TEXT NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0), event_seq INTEGER NOT NULL CHECK(event_seq >= 0),
        aggregate_object_bytes INTEGER NOT NULL CHECK(aggregate_object_bytes >= 0),
        total_attempts INTEGER NOT NULL CHECK(total_attempts >= 0),
        budget_limit INTEGER NOT NULL CHECK(budget_limit >= 0),
        budget_reserved INTEGER NOT NULL CHECK(budget_reserved >= 0),
        budget_spent INTEGER NOT NULL CHECK(budget_spent >= 0),
        lifetime_deadline_at_ms INTEGER NOT NULL, created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY (tenant_id, namespace, run_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_run_limits (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, run_id TEXT NOT NULL,
        max_dynamic_node_instances INTEGER NOT NULL, max_total_attempts INTEGER NOT NULL,
        max_total_events INTEGER NOT NULL, max_inline_json_bytes_per_value INTEGER NOT NULL,
        max_artifacts_per_attempt INTEGER NOT NULL, max_aggregate_object_bytes_per_run INTEGER NOT NULL,
        max_run_lifetime_ms INTEGER NOT NULL,
        PRIMARY KEY (tenant_id, namespace, run_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_nodes (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, run_id TEXT NOT NULL, node_id TEXT NOT NULL,
        node_kind TEXT NOT NULL, status TEXT NOT NULL, topological_rank INTEGER NOT NULL,
        map_item_index INTEGER, attempt_count INTEGER NOT NULL CHECK(attempt_count >= 0),
        active_attempt_id TEXT, next_eligible_at_ms INTEGER, version INTEGER NOT NULL CHECK(version > 0),
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY (tenant_id, namespace, run_id, node_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_edges (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, run_id TEXT NOT NULL, edge_id TEXT NOT NULL,
        from_node_id TEXT NOT NULL, to_node_id TEXT NOT NULL, status TEXT NOT NULL,
        version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, run_id, edge_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_attempts (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, run_id TEXT NOT NULL, attempt_id TEXT NOT NULL,
        node_id TEXT NOT NULL, attempt_number INTEGER NOT NULL, status TEXT NOT NULL,
        engine_generation INTEGER NOT NULL, completion_credential_digest TEXT NOT NULL,
        reservation_units INTEGER NOT NULL, started_at_ms INTEGER NOT NULL, deadline_at_ms INTEGER NOT NULL,
        finished_at_ms INTEGER, output_digest TEXT, diagnostics_digest TEXT,
        version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, run_id, attempt_id),
        UNIQUE (tenant_id, namespace, run_id, node_id, attempt_number)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_action_invocations (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, run_id TEXT NOT NULL, invocation_id TEXT NOT NULL,
        attempt_id TEXT NOT NULL, node_id TEXT NOT NULL, input_digest TEXT NOT NULL,
        action_semantic_digest TEXT NOT NULL, idempotency_key TEXT NOT NULL,
        PRIMARY KEY (tenant_id, namespace, run_id, invocation_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_approval_gates (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, run_id TEXT NOT NULL, gate_id TEXT NOT NULL,
        node_id TEXT NOT NULL, status TEXT NOT NULL, request_digest TEXT NOT NULL,
        decision_payload_digest TEXT, output_digest TEXT, expires_at_ms INTEGER NOT NULL,
        version INTEGER NOT NULL CHECK(version > 0),
        PRIMARY KEY (tenant_id, namespace, run_id, gate_id)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_budget_ledger (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, run_id TEXT NOT NULL, ledger_seq INTEGER NOT NULL,
        attempt_id TEXT NOT NULL, node_id TEXT NOT NULL, kind TEXT NOT NULL,
        reserved_delta INTEGER NOT NULL, consumed_delta INTEGER NOT NULL CHECK(consumed_delta >= 0),
        reservation_amount INTEGER NOT NULL CHECK(reservation_amount >= 0),
        reason TEXT NOT NULL, created_at_ms INTEGER NOT NULL,
        PRIMARY KEY (tenant_id, namespace, run_id, ledger_seq)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_events (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, run_id TEXT NOT NULL, event_seq INTEGER NOT NULL,
        batch_id TEXT NOT NULL, batch_index INTEGER NOT NULL, batch_count INTEGER NOT NULL,
        event_type TEXT NOT NULL, actor_kind TEXT NOT NULL, actor_id TEXT NOT NULL,
        occurred_at_ms INTEGER NOT NULL, payload_json TEXT NOT NULL,
        PRIMARY KEY (tenant_id, namespace, run_id, event_seq),
        UNIQUE (tenant_id, namespace, run_id, batch_id, batch_index)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_command_receipts (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, command_kind TEXT NOT NULL,
        idempotency_token TEXT NOT NULL, request_fingerprint TEXT NOT NULL, run_id TEXT NOT NULL,
        outcome_json TEXT NOT NULL, batch_id TEXT NOT NULL, committed_at_ms INTEGER NOT NULL,
        PRIMARY KEY (tenant_id, namespace, command_kind, idempotency_token)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_objects (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, digest TEXT NOT NULL,
        size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0), object_key TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY (tenant_id, namespace, digest)
    ) STRICT"#,
    r#"CREATE TABLE IF NOT EXISTS dagger_workflow_artifact_refs (
        tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, artifact_ref_id TEXT NOT NULL,
        digest TEXT NOT NULL, size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
        media_type TEXT NOT NULL, kind TEXT NOT NULL, producer_run_id TEXT,
        producer_node_id TEXT, producer_attempt_id TEXT, ordinal INTEGER NOT NULL,
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY (tenant_id, namespace, artifact_ref_id)
    ) STRICT"#,
    r#"CREATE INDEX IF NOT EXISTS dagger_workflow_nodes_scheduler_scan
        ON dagger_workflow_nodes(
            tenant_id, namespace, status, run_id, node_id, updated_at_ms, next_eligible_at_ms
        )"#,
    r#"CREATE INDEX IF NOT EXISTS dagger_workflow_attempts_deadline_scan
        ON dagger_workflow_attempts(
            tenant_id, namespace, status, deadline_at_ms, run_id, node_id, attempt_id, started_at_ms
        )"#,
    r#"CREATE INDEX IF NOT EXISTS dagger_workflow_attempts_recovery_scan
        ON dagger_workflow_attempts(
            tenant_id, namespace, status, engine_generation, started_at_ms, run_id, attempt_id
        )"#,
    r#"CREATE INDEX IF NOT EXISTS dagger_workflow_gates_expiry_scan
        ON dagger_workflow_approval_gates(tenant_id, namespace, status, expires_at_ms, run_id, gate_id)"#,
    r#"CREATE INDEX IF NOT EXISTS dagger_workflow_runs_compatibility_scan
        ON dagger_workflow_runs(
            tenant_id, namespace, status, updated_at_ms, run_id
        )"#,
    r#"CREATE INDEX IF NOT EXISTS dagger_workflow_runs_lifetime_scan
        ON dagger_workflow_runs(
            tenant_id, namespace, status, lifetime_deadline_at_ms, run_id
        )"#,
];

/// Applies all pending migrations, committing each version independently.
pub async fn migrate(pool: &SqlitePool) -> Result<(), StoreError> {
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS dagger_workflow_schema_migrations (
            version INTEGER PRIMARY KEY, applied_at_ms INTEGER NOT NULL,
            clock_offset_ms INTEGER NOT NULL DEFAULT 0
        ) STRICT"#,
    )
    .execute(pool)
    .await
    .map_err(|_| StoreError::StorageUnavailable)?;

    let applied: Option<i64> =
        sqlx::query_scalar("SELECT MAX(version) FROM dagger_workflow_schema_migrations")
            .fetch_one(pool)
            .await
            .map_err(|_| StoreError::StorageUnavailable)?;
    if applied.is_some_and(|version| version > SCHEMA_VERSION) {
        return Err(StoreError::TransactionFailed);
    }
    if applied.unwrap_or(0) < 1 {
        apply_version(pool, 1, MIGRATION_1).await?;
    }
    validate_coherent_v1(pool).await?;
    Ok(())
}

async fn validate_coherent_v1(pool: &SqlitePool) -> Result<(), StoreError> {
    for (table, required_columns) in [
        (
            "dagger_workflow_schema_migrations",
            &["clock_offset_ms"][..],
        ),
        (
            "dagger_workflow_run_rows",
            &["tenant_id", "namespace", "row_data", "version"][..],
        ),
        (
            "dagger_workflow_runs",
            &["tenant_id", "namespace", "updated_at_ms"][..],
        ),
        (
            "dagger_workflow_nodes",
            &["tenant_id", "namespace", "updated_at_ms"][..],
        ),
    ] {
        let present: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info(?)
             WHERE name IN ('tenant_id', 'namespace', 'row_data', 'version',
                            'clock_offset_ms', 'updated_at_ms')",
        )
        .bind(table)
        .fetch_all(pool)
        .await
        .map_err(|_| StoreError::StorageUnavailable)?;
        if required_columns
            .iter()
            .any(|required| !present.iter().any(|column| column == required))
        {
            return Err(StoreError::TransactionFailed);
        }
    }
    Ok(())
}

async fn apply_version(
    pool: &SqlitePool,
    version: i64,
    statements: &[&str],
) -> Result<(), StoreError> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| StoreError::StorageUnavailable)?;
    for statement in statements {
        sqlx::query(statement)
            .execute(&mut *transaction)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
    }
    record_version(&mut transaction, version).await?;
    transaction
        .commit()
        .await
        .map_err(|_| StoreError::TransactionFailed)
}

async fn record_version(
    transaction: &mut Transaction<'_, Sqlite>,
    version: i64,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO dagger_workflow_schema_migrations(version, applied_at_ms)
         VALUES (?, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))",
    )
    .bind(version)
    .execute(&mut **transaction)
    .await
    .map_err(|_| StoreError::TransactionFailed)?;
    Ok(())
}
