//! SQLite workflow control plane.
//!
//! The database clock and every durable projection are scoped by tenant and
//! namespace. Object contents are intentionally never persisted here: only
//! verified object metadata and typed references belong to the control plane.

mod codec;
mod reducer;
mod schema;

use crate::approval::ApprovalGate;
use crate::artifact::ObjectRecord;
use crate::engine::Clock;
use crate::event::WorkflowEvent;
use crate::ids::{Digest, Id, NodeInstanceId, Timestamp};
use crate::revision::WorkflowRevision;
use crate::run::{NodeAttempt, NodeRun, WorkflowRun, WorkflowRunView};
use crate::scope::ExecutionScope;
use crate::store::*;
use reducer::{ReducerState, ReducerStore};
use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Sqlite, SqlitePool};
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

pub use schema::SCHEMA_VERSION;

#[derive(Clone)]
struct DatabaseTimestamp(Timestamp);

impl Clock for DatabaseTimestamp {
    fn now(&self) -> Timestamp {
        self.0
    }
}

/// SQLite-backed workflow control plane with no process-local domain state.
pub struct SqliteWorkflowStore<C> {
    pool: SqlitePool,
    /// Process-local material from `VerifiedObjectRef`. This is deliberately
    /// non-durable; bytes are owned by the object store, never SQLite.
    verified_bytes: Arc<Mutex<BTreeMap<(ExecutionScope, Digest), Vec<u8>>>>,
    marker: PhantomData<fn() -> C>,
}

impl<C> SqliteWorkflowStore<C> {
    /// Initializes the adapter on a caller-owned pool without disturbing host tables.
    pub async fn from_pool(pool: SqlitePool, _clock: Arc<C>) -> Result<Self, StoreError> {
        schema::migrate(&pool).await?;
        Ok(Self {
            pool,
            verified_bytes: Arc::new(Mutex::new(BTreeMap::new())),
            marker: PhantomData,
        })
    }

    /// Opens or creates a standalone WAL database and runs pending migrations.
    pub async fn open(path: impl AsRef<Path>, clock: Arc<C>) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|_| StoreError::StorageUnavailable)?;
        Self::from_pool(pool, clock).await
    }

    /// Opens a SQLite URL, including a single-connection in-memory database.
    pub async fn open_url(url: &str, clock: Arc<C>) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::from_str(url)
            .map_err(|_| StoreError::InvalidField)?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full);
        let pool = SqlitePoolOptions::new()
            .max_connections(if url.contains(":memory:") { 1 } else { 5 })
            .connect_with(options)
            .await
            .map_err(|_| StoreError::StorageUnavailable)?;
        Self::from_pool(pool, clock).await
    }

    /// Exposes the injected pool for host composition and diagnostics.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    async fn begin_immediate(&self) -> Result<PoolConnection<Sqlite>, StoreError> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| StoreError::StorageUnavailable)?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        Ok(connection)
    }

    async fn load_state(
        connection: &mut PoolConnection<Sqlite>,
        scope: &ExecutionScope,
    ) -> Result<(ReducerState, i64, Timestamp), StoreError> {
        let stored: Option<(String, i64)> = sqlx::query_as(
            "SELECT reducer_data, version
             FROM dagger_workflow_adapter_state
             WHERE tenant_id = ? AND namespace = ?",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .fetch_optional(&mut **connection)
        .await
        .map_err(|_| StoreError::StorageUnavailable)?;
        let (mut state, version) = match stored {
            Some((data, version)) => (
                serde_json::from_str(&data).map_err(|_| StoreError::CorruptControlPlane)?,
                version,
            ),
            None => (ReducerState::default(), 0),
        };
        // A scoped row can never hydrate another scope, even if a corrupt file
        // was edited out of band.
        if state
            .definitions
            .keys()
            .any(|(row_scope, _)| row_scope != scope)
            || state.runs.keys().any(|(row_scope, _)| row_scope != scope)
        {
            return Err(StoreError::CorruptControlPlane);
        }
        state.verified_object_bytes.clear();
        let now_ms: i64 = sqlx::query_scalar(
            "SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
                    + clock_offset_ms
             FROM dagger_workflow_schema_migrations
             WHERE version = ?",
        )
        .bind(SCHEMA_VERSION)
        .fetch_one(&mut **connection)
        .await
        .map_err(|_| StoreError::StorageUnavailable)?;
        Ok((state, version, Timestamp(now_ms)))
    }

    async fn persist_state(
        connection: &mut PoolConnection<Sqlite>,
        scope: &ExecutionScope,
        expected_state_version: i64,
        state: &ReducerState,
    ) -> Result<(), StoreError> {
        let data = serde_jcs::to_string(state).map_err(|_| StoreError::TransactionFailed)?;
        let result = sqlx::query(
            "INSERT INTO dagger_workflow_adapter_state
                (tenant_id, namespace, reducer_data, version)
             VALUES (?, ?, ?, 1)
             ON CONFLICT(tenant_id, namespace) DO UPDATE
             SET reducer_data = excluded.reducer_data, version = version + 1
             WHERE tenant_id = ? AND namespace = ? AND version = ?
               AND version < 9223372036854775807",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(data)
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(expected_state_version)
        .execute(&mut **connection)
        .await
        .map_err(|_| StoreError::TransactionFailed)?;
        if result.rows_affected() != 1 {
            return Err(StoreError::CasConflict);
        }
        Ok(())
    }

    fn sql_integer(value: u64) -> Result<i64, StoreError> {
        i64::try_from(value).map_err(|_| StoreError::ArithmeticOverflow)
    }

    async fn persist_projections(
        connection: &mut PoolConnection<Sqlite>,
        scope: &ExecutionScope,
        state: &ReducerState,
    ) -> Result<(), StoreError> {
        const TABLES: &[&str] = &[
            "dagger_workflow_artifact_refs",
            "dagger_workflow_objects",
            "dagger_workflow_command_receipts",
            "dagger_workflow_events",
            "dagger_workflow_budget_ledger",
            "dagger_workflow_approval_gates",
            "dagger_workflow_action_invocations",
            "dagger_workflow_attempts",
            "dagger_workflow_edges",
            "dagger_workflow_nodes",
            "dagger_workflow_run_limits",
            "dagger_workflow_runs",
            "dagger_workflow_engine_claims",
            "dagger_workflow_revision_nodes",
            "dagger_workflow_revisions",
            "dagger_workflow_definitions",
        ];
        for table in TABLES {
            let statement = format!("DELETE FROM {table} WHERE tenant_id = ? AND namespace = ?");
            sqlx::query(&statement)
                .bind(scope.tenant_id.as_str())
                .bind(scope.namespace.as_str())
                .execute(&mut **connection)
                .await
                .map_err(|_| StoreError::TransactionFailed)?;
        }

        for definition in state.definitions.values() {
            sqlx::query(
                "INSERT INTO dagger_workflow_definitions
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(definition.scope.tenant_id.as_str())
            .bind(definition.scope.namespace.as_str())
            .bind(definition.definition_id.as_str())
            .bind(&definition.display_name)
            .bind(&definition.description)
            .bind(definition.created_at.0)
            .bind(&definition.created_by)
            .bind(definition.latest_revision_hash.as_ref().map(Digest::as_str))
            .bind(Self::sql_integer(definition.version.0)?)
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        }
        for revision in state.revisions.values() {
            sqlx::query(
                "INSERT INTO dagger_workflow_revisions
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(revision.scope.tenant_id.as_str())
            .bind(revision.scope.namespace.as_str())
            .bind(revision.definition_id.as_str())
            .bind(revision.revision_hash.as_str())
            .bind(revision.canonical_definition_ref.0.digest.as_str())
            .bind(revision.run_input_schema_digest.as_str())
            .bind(revision.run_output_schema_digest.as_str())
            .bind(revision.published_at.0)
            .bind(&revision.published_by)
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
            for (node_id, rank) in &revision.node_topological_ranks {
                let node_kind = state
                    .parsed_revisions
                    .get(&(
                        revision.scope.clone(),
                        revision.definition_id.clone(),
                        revision.revision_hash.clone(),
                    ))
                    .and_then(|parsed| {
                        parsed.definition.nodes.iter().find_map(|node| {
                            let (id, kind) = match node {
                                crate::definition::NodeDefinition::Action { id, .. } => {
                                    (id, "Action")
                                }
                                crate::definition::NodeDefinition::Map { id, .. } => (id, "Map"),
                                crate::definition::NodeDefinition::Choice { id, .. } => {
                                    (id, "Choice")
                                }
                                crate::definition::NodeDefinition::Approval { id, .. } => {
                                    (id, "Approval")
                                }
                                crate::definition::NodeDefinition::Succeed { id, .. } => {
                                    (id, "Succeed")
                                }
                                crate::definition::NodeDefinition::Fail { id, .. } => (id, "Fail"),
                            };
                            (id == node_id).then_some(kind)
                        })
                    })
                    .ok_or(StoreError::CorruptControlPlane)?;
                sqlx::query(
                    "INSERT INTO dagger_workflow_revision_nodes
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(revision.scope.tenant_id.as_str())
                .bind(revision.scope.namespace.as_str())
                .bind(revision.definition_id.as_str())
                .bind(revision.revision_hash.as_str())
                .bind(node_id.as_str())
                .bind(node_kind)
                .bind(i64::from(rank.0))
                .execute(&mut **connection)
                .await
                .map_err(|_| StoreError::TransactionFailed)?;
            }
        }
        for stored in state.claims.values() {
            let claim = &stored.claim;
            sqlx::query(
                "INSERT INTO dagger_workflow_engine_claims
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(claim.scope.tenant_id.as_str())
            .bind(claim.scope.namespace.as_str())
            .bind(&claim.control_plane_id)
            .bind(claim.instance_id.as_str())
            .bind(Self::sql_integer(claim.generation)?)
            .bind(stored.token_digest.as_str())
            .bind(stored.released_token_digest.as_ref().map(Digest::as_str))
            .bind(claim.claimed_at.0)
            .bind(claim.heartbeat_at.0)
            .bind(claim.expires_at.0)
            .bind(Self::sql_integer(claim.version.0)?)
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        }
        for run in state.runs.values() {
            sqlx::query(
                "INSERT INTO dagger_workflow_runs
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(run.scope.tenant_id.as_str())
            .bind(run.scope.namespace.as_str())
            .bind(run.run_id.as_str())
            .bind(run.definition_id.as_str())
            .bind(run.revision_hash.as_str())
            .bind(format!("{:?}", run.status))
            .bind(Self::sql_integer(run.version.0)?)
            .bind(Self::sql_integer(run.last_event_seq)?)
            .bind(Self::sql_integer(run.aggregate_object_bytes)?)
            .bind(Self::sql_integer(run.total_attempt_count)?)
            .bind(Self::sql_integer(run.budget_limit.0)?)
            .bind(Self::sql_integer(run.budget_reserved.0)?)
            .bind(Self::sql_integer(run.budget_consumed.0)?)
            .bind(run.lifetime_deadline_at.0)
            .bind(run.created_at.0)
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
            sqlx::query(
                "INSERT INTO dagger_workflow_run_limits VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(run.scope.tenant_id.as_str())
            .bind(run.scope.namespace.as_str())
            .bind(run.run_id.as_str())
            .bind(Self::sql_integer(run.limits.max_dynamic_node_instances)?)
            .bind(Self::sql_integer(run.limits.max_total_attempts)?)
            .bind(Self::sql_integer(run.limits.max_total_events)?)
            .bind(Self::sql_integer(
                run.limits.max_inline_json_bytes_per_value,
            )?)
            .bind(Self::sql_integer(run.limits.max_artifacts_per_attempt)?)
            .bind(Self::sql_integer(
                run.limits.max_aggregate_object_bytes_per_run,
            )?)
            .bind(Self::sql_integer(run.limits.max_run_lifetime_ms)?)
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        }
        for node in state.nodes.values() {
            sqlx::query(
                "INSERT INTO dagger_workflow_nodes VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(node.scope.tenant_id.as_str())
            .bind(node.scope.namespace.as_str())
            .bind(node.run_id.as_str())
            .bind(node.node_instance_id.as_str())
            .bind(format!("{:?}", node.kind))
            .bind(format!("{:?}", node.status))
            .bind(i64::from(node.topological_rank.0))
            .bind(node.map_item_index.map(i64::from))
            .bind(i64::from(node.attempt_count))
            .bind(node.active_attempt_id.as_ref().map(Id::as_str))
            .bind(node.next_eligible_at.map(|time| time.0))
            .bind(Self::sql_integer(node.version.0)?)
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        }
        for edge in state.edges.values() {
            sqlx::query("INSERT INTO dagger_workflow_edges VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(edge.scope.tenant_id.as_str())
                .bind(edge.scope.namespace.as_str())
                .bind(edge.run_id.as_str())
                .bind(edge.edge_id.as_str())
                .bind(edge.from_node_id.as_str())
                .bind(edge.to_node_id.as_str())
                .bind(format!("{:?}", edge.state))
                .execute(&mut **connection)
                .await
                .map_err(|_| StoreError::TransactionFailed)?;
        }
        for attempt in state.attempts.values() {
            sqlx::query(
                "INSERT INTO dagger_workflow_attempts
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(attempt.scope.tenant_id.as_str())
            .bind(attempt.scope.namespace.as_str())
            .bind(attempt.run_id.as_str())
            .bind(attempt.attempt_id.as_str())
            .bind(attempt.node_instance_id.as_str())
            .bind(i64::from(attempt.attempt_number))
            .bind(format!("{:?}", attempt.status))
            .bind(Self::sql_integer(attempt.engine_generation)?)
            .bind(attempt.completion_credential_digest.as_str())
            .bind(Self::sql_integer(attempt.reserved_cost.0)?)
            .bind(attempt.started_at.0)
            .bind(attempt.deadline_at.0)
            .bind(attempt.finished_at.map(|time| time.0))
            .bind(
                attempt
                    .output_ref
                    .as_ref()
                    .map(|reference| reference.0.digest.as_str()),
            )
            .bind(
                attempt
                    .diagnostics_ref
                    .as_ref()
                    .map(|reference| reference.0.digest.as_str()),
            )
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        }
        for invocation in state.invocations.values() {
            sqlx::query(
                "INSERT INTO dagger_workflow_action_invocations
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(invocation.scope.tenant_id.as_str())
            .bind(invocation.scope.namespace.as_str())
            .bind(invocation.run_id.as_str())
            .bind(invocation.invocation_id.as_str())
            .bind(invocation.attempt_id.as_str())
            .bind(invocation.node_instance_id.as_str())
            .bind(invocation.bound_input_digest.as_str())
            .bind(invocation.compatible_implementation_requirement.as_str())
            .bind(
                state
                    .attempts
                    .get(&(
                        invocation.scope.clone(),
                        invocation.run_id.clone(),
                        invocation.attempt_id.clone(),
                    ))
                    .map(|attempt| attempt.idempotency_key.as_str())
                    .ok_or(StoreError::CorruptControlPlane)?,
            )
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        }
        for gate in state.gates.values() {
            sqlx::query(
                "INSERT INTO dagger_workflow_approval_gates
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(gate.scope.tenant_id.as_str())
            .bind(gate.scope.namespace.as_str())
            .bind(gate.run_id.as_str())
            .bind(gate.gate_id.as_str())
            .bind(gate.node_instance_id.as_str())
            .bind(format!("{:?}", gate.status))
            .bind(gate.request_ref.0.digest.as_str())
            .bind(
                gate.decision_payload_ref
                    .as_ref()
                    .map(|reference| reference.0.digest.as_str()),
            )
            .bind(
                state
                    .nodes
                    .get(&(
                        gate.scope.clone(),
                        gate.run_id.clone(),
                        gate.node_instance_id.clone(),
                    ))
                    .and_then(|node| node.result_ref.as_ref())
                    .map(|reference| reference.0.digest.as_str()),
            )
            .bind(gate.expires_at.0)
            .bind(Self::sql_integer(gate.version.0)?)
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        }
        for entries in state.ledger.values() {
            for entry in entries {
                let reserved_delta = i64::try_from(entry.reserved_delta)
                    .map_err(|_| StoreError::ArithmeticOverflow)?;
                sqlx::query(
                    "INSERT INTO dagger_workflow_budget_ledger
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(entry.scope.tenant_id.as_str())
                .bind(entry.scope.namespace.as_str())
                .bind(entry.run_id.as_str())
                .bind(Self::sql_integer(entry.ledger_seq)?)
                .bind(entry.attempt_id.as_str())
                .bind(entry.node_instance_id.as_str())
                .bind(format!("{:?}", entry.kind))
                .bind(reserved_delta)
                .bind(Self::sql_integer(entry.consumed_delta.0)?)
                .bind(Self::sql_integer(entry.reservation_amount.0)?)
                .bind(format!("{:?}", entry.reason))
                .bind(entry.created_at.0)
                .execute(&mut **connection)
                .await
                .map_err(|_| StoreError::TransactionFailed)?;
            }
        }
        for events in state.events.values() {
            for event in events {
                sqlx::query(
                    "INSERT INTO dagger_workflow_events
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(event.scope.tenant_id.as_str())
                .bind(event.scope.namespace.as_str())
                .bind(event.run_id.as_str())
                .bind(Self::sql_integer(event.event_seq)?)
                .bind(event.batch_id.as_str())
                .bind(i64::from(event.batch_index))
                .bind(i64::from(event.batch_count))
                .bind(format!("{:?}", event.event_type))
                .bind(format!("{:?}", event.actor_kind))
                .bind(&event.actor_id)
                .bind(event.occurred_at.0)
                .bind(
                    serde_jcs::to_string(&event.payload)
                        .map_err(|_| StoreError::TransactionFailed)?,
                )
                .execute(&mut **connection)
                .await
                .map_err(|_| StoreError::TransactionFailed)?;
            }
        }
        for receipt in state.receipts.values() {
            sqlx::query(
                "INSERT INTO dagger_workflow_command_receipts
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(receipt.scope.tenant_id.as_str())
            .bind(receipt.scope.namespace.as_str())
            .bind(format!("{:?}", receipt.command_kind))
            .bind(&receipt.idempotency_token)
            .bind(receipt.request_fingerprint.as_str())
            .bind(receipt.run_id.as_str())
            .bind(
                serde_jcs::to_string(&receipt.outcome)
                    .map_err(|_| StoreError::TransactionFailed)?,
            )
            .bind(receipt.batch_id.as_str())
            .bind(receipt.committed_at.0)
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        }
        for record in state.object_records.values() {
            sqlx::query("INSERT INTO dagger_workflow_objects VALUES (?, ?, ?, ?, ?, ?)")
                .bind(record.scope.tenant_id.as_str())
                .bind(record.scope.namespace.as_str())
                .bind(record.digest.as_str())
                .bind(Self::sql_integer(record.size_bytes)?)
                .bind(&record.object_key)
                .bind(record.created_at.0)
                .execute(&mut **connection)
                .await
                .map_err(|_| StoreError::TransactionFailed)?;
        }
        for reference in state.artifact_refs.values() {
            sqlx::query(
                "INSERT INTO dagger_workflow_artifact_refs
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(reference.scope.tenant_id.as_str())
            .bind(reference.scope.namespace.as_str())
            .bind(reference.artifact_ref_id.as_str())
            .bind(reference.digest.as_str())
            .bind(Self::sql_integer(reference.size_bytes)?)
            .bind(&reference.media_type)
            .bind(format!("{:?}", reference.kind))
            .bind(reference.producer_run_id.as_ref().map(Id::as_str))
            .bind(
                reference
                    .producer_node_id
                    .as_ref()
                    .map(NodeInstanceId::as_str),
            )
            .bind(reference.producer_attempt_id.as_ref().map(Id::as_str))
            .bind(i64::from(reference.ordinal))
            .bind(reference.created_at.0)
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        }
        Ok(())
    }

    async fn commit(mut connection: PoolConnection<Sqlite>) -> Result<(), StoreError> {
        sqlx::query("COMMIT")
            .execute(&mut *connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        Ok(())
    }

    async fn rollback(connection: &mut PoolConnection<Sqlite>) {
        let _ = sqlx::query("ROLLBACK").execute(&mut **connection).await;
    }

    fn restore_verified_bytes(&self, state: &mut ReducerState, scope: &ExecutionScope) {
        state.verified_object_bytes = self
            .verified_bytes
            .lock()
            .expect("verified-byte cache lock poisoned")
            .iter()
            .filter(|((cached_scope, _), _)| cached_scope == scope)
            .map(|(key, bytes)| (key.clone(), bytes.clone()))
            .collect();
    }

    fn remember_verified_bytes(&self, state: &ReducerState) {
        let mut cache = self
            .verified_bytes
            .lock()
            .expect("verified-byte cache lock poisoned");
        cache.extend(state.verified_object_bytes.clone());
    }

    async fn read_reducer(
        &self,
        scope: &ExecutionScope,
    ) -> Result<ReducerStore<DatabaseTimestamp>, StoreError> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| StoreError::StorageUnavailable)?;
        let (mut state, _, now) = Self::load_state(&mut connection, scope).await?;
        self.restore_verified_bytes(&mut state, scope);
        Ok(ReducerStore::from_state(
            Arc::new(DatabaseTimestamp(now)),
            state,
        ))
    }

    /// Reads exact registered object metadata from the committed SQL snapshot.
    pub async fn object_records(
        &self,
        scope: &ExecutionScope,
    ) -> Result<Vec<ObjectRecord>, StoreError> {
        Ok(self.read_reducer(scope).await?.object_records(scope))
    }

    /// Advances the durable database-clock offset for deterministic conformance.
    ///
    /// Production callers leave the offset at zero and therefore use Unix time
    /// evaluated by SQLite inside each command transaction.
    pub async fn advance_database_clock_ms(&self, milliseconds: i64) -> Result<(), StoreError> {
        let mut connection = self.begin_immediate().await?;
        let result = sqlx::query(
            "UPDATE dagger_workflow_schema_migrations
             SET clock_offset_ms = clock_offset_ms + ?
             WHERE version = ?
               AND ((? >= 0 AND clock_offset_ms <= 9223372036854775807 - ?)
                 OR (? < 0 AND clock_offset_ms >= -9223372036854775808 - ?))",
        )
        .bind(milliseconds)
        .bind(SCHEMA_VERSION)
        .bind(milliseconds)
        .bind(milliseconds)
        .bind(milliseconds)
        .bind(milliseconds)
        .execute(&mut *connection)
        .await
        .map_err(|_| StoreError::TransactionFailed)?;
        if result.rows_affected() != 1 {
            Self::rollback(&mut connection).await;
            return Err(StoreError::ArithmeticOverflow);
        }
        Self::commit(connection).await
    }
}

macro_rules! sql_command {
    ($name:ident ( $($arg:ident : $arg_type:ty),* $(,)? ) -> $output:ty) => {
        async fn $name(
            &self,
            scope: &ExecutionScope,
            $($arg: $arg_type),*
        ) -> Result<$output, StoreError> {
            let mut connection = self.begin_immediate().await?;
            let (mut state, state_version, now) = match Self::load_state(&mut connection, scope).await {
                Ok(value) => value,
                Err(error) => {
                    Self::rollback(&mut connection).await;
                    return Err(error);
                }
            };
            self.restore_verified_bytes(&mut state, scope);
            let reducer = ReducerStore::from_state(Arc::new(DatabaseTimestamp(now)), state);
            match reducer.$name(scope, $($arg),*).await {
                Ok(value) => {
                    let state = reducer.snapshot();
                    self.remember_verified_bytes(&state);
                    if let Err(error) = Self::persist_projections(&mut connection, scope, &state).await {
                        Self::rollback(&mut connection).await;
                        return Err(error);
                    }
                    if let Err(error) =
                        Self::persist_state(&mut connection, scope, state_version, &state).await
                    {
                        Self::rollback(&mut connection).await;
                        return Err(error);
                    }
                    Self::commit(connection).await?;
                    Ok(value)
                }
                Err(error) => {
                    Self::rollback(&mut connection).await;
                    Err(error)
                }
            }
        }
    };
}

macro_rules! sql_read {
    ($name:ident ( $($arg:ident : $arg_type:ty),* $(,)? ) -> $output:ty) => {
        async fn $name(
            &self,
            scope: &ExecutionScope,
            $($arg: $arg_type),*
        ) -> Result<$output, StoreError> {
            self.read_reducer(scope).await?.$name(scope, $($arg),*).await
        }
    };
}

impl<C: Send + Sync> WorkflowStore for SqliteWorkflowStore<C> {
    sql_command!(create_definition(command: CreateDefinition) -> DefinitionRecord);
    sql_command!(
        update_definition_metadata(command: UpdateDefinitionMetadata) -> DefinitionRecord
    );
    sql_command!(publish_revision(command: PublishRevision) -> WorkflowRevision);
    sql_command!(acquire_engine_claim(instance_id: Id) -> AcquiredEngineClaim);
    sql_command!(heartbeat_engine_claim(permit: &EnginePermit) -> EngineClaim);
    sql_command!(release_engine_claim(permit: &EnginePermit) -> ());
    sql_command!(create_run(command: CreateRun) -> CommandReceipt);
    sql_command!(start_run(command: StartRun) -> WorkflowRun);
    sql_command!(suspend_incompatible(command: SuspendIncompatible) -> WorkflowRun);
    sql_command!(resume_compatible(command: ResumeCompatible) -> WorkflowRun);
    sql_command!(claim_node_attempt(command: ClaimNodeAttempt) -> ClaimNodeAttemptResult);
    sql_command!(complete_attempt(command: CompleteAttempt) -> CompleteAttemptResult);
    sql_command!(timeout_attempt(command: TimeoutAttempt) -> NodeAttempt);
    sql_command!(
        recover_abandoned_attempts_for_run(command: RecoverAbandonedAttemptsForRun)
            -> Vec<NodeAttempt>
    );
    sql_command!(release_retry(command: ReleaseRetry) -> NodeRun);
    sql_command!(record_choice(command: RecordChoice) -> NodeRun);
    sql_command!(expand_map(command: ExpandMap) -> NodeRun);
    sql_command!(complete_map(command: CompleteMap) -> NodeRun);
    sql_command!(request_approval(command: RequestApproval) -> ApprovalGate);
    sql_command!(decide_approval(command: DecideApproval) -> ApprovalGate);
    sql_command!(expire_approval(command: ExpireApproval) -> ApprovalGate);
    sql_command!(resolve_terminal_node(command: ResolveTerminalNode) -> WorkflowRun);
    sql_command!(fail_contract(command: FailContract) -> WorkflowRun);
    sql_command!(cancel_run(command: CancelRun) -> CommandReceipt);
    sql_command!(expire_run_lifetime(command: ExpireRunLifetime) -> WorkflowRun);
    sql_command!(mark_corrupt_storage(command: MarkCorruptStorage) -> WorkflowRun);

    sql_read!(get_definition(definition_id: &Id) -> DefinitionRecord);
    sql_read!(get_revision(definition_id: &Id, revision_hash: &Digest) -> WorkflowRevision);
    sql_read!(get_run(run_id: &Id) -> WorkflowRunView);
    sql_read!(get_node(run_id: &Id, node_id: &NodeInstanceId) -> NodeRun);
    sql_read!(get_attempt(run_id: &Id, attempt_id: &Id) -> NodeAttempt);
    sql_read!(get_gate(run_id: &Id, gate_id: &Id) -> ApprovalGate);
    sql_read!(list_runs(page: PageRequest) -> Page<WorkflowRun>);
    sql_read!(list_nodes(run_id: &Id, page: PageRequest) -> Page<NodeRun>);
    sql_read!(
        list_events_after(run_id: &Id, page: EventPageRequest) -> Vec<WorkflowEvent>
    );
    sql_read!(scan_ready_nodes(page: PageRequest) -> Page<NodeRun>);
    sql_read!(scan_budget_waiters(page: PageRequest) -> Page<NodeRun>);
    sql_read!(scan_due_deadlines(page: PageRequest) -> Page<NodeAttempt>);
    sql_read!(scan_due_retries(page: PageRequest) -> Page<NodeRun>);
    sql_read!(scan_recovery_runs(page: PageRequest) -> Page<WorkflowRun>);
    sql_read!(scan_compatibility_rechecks(page: PageRequest) -> Page<WorkflowRun>);
    sql_read!(scan_due_gates(page: PageRequest) -> Page<ApprovalGate>);
    sql_read!(scan_due_run_lifetimes(page: PageRequest) -> Page<WorkflowRun>);
}
