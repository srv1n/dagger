//! SQLite workflow control plane.
//!
//! The database clock and every durable projection are scoped by tenant and
//! namespace. Object contents are intentionally never persisted here: only
//! verified object metadata and typed references belong to the control plane.

mod codec;
mod reducer;
mod schema;

use crate::action::ActionInvocation;
use crate::approval::ApprovalGate;
use crate::artifact::{ArtifactRef, ObjectRecord, ObjectStore};
use crate::budget::BudgetLedgerEntry;
use crate::definition::PublishableDefinition;
use crate::engine::Clock;
use crate::event::WorkflowEvent;
use crate::ids::{Digest, Id, NodeInstanceId, Timestamp};
use crate::revision::WorkflowRevision;
use crate::run::{EdgeFact, NodeAttempt, NodeRun, WorkflowRun, WorkflowRunView};
use crate::scope::ExecutionScope;
use crate::store::*;
use reducer::{
    finish_scan_page, scan_page_context, timestamp_cursor_key, CommandKindKey, ReducerState,
    ReducerStore, StoredClaim,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Sqlite, SqlitePool};
use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

pub use schema::SCHEMA_VERSION;

/// One-shot crash-window injection for the SQLite proof suite.
#[cfg(feature = "conformance")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqliteCommitFault {
    /// Abort the real command path after writes but before COMMIT.
    BeforeCommit,
    /// Report an abort immediately after COMMIT has durably succeeded.
    AfterCommit,
}

#[derive(Clone)]
struct DatabaseTimestamp(Timestamp);

impl Clock for DatabaseTimestamp {
    fn now(&self) -> Timestamp {
        self.0
    }
}

const DEFINITION_ROWS: &str = "dagger_workflow_definition_rows";
const REVISION_ROWS: &str = "dagger_workflow_revision_rows";
const ENGINE_CLAIM_ROWS: &str = "dagger_workflow_engine_claim_rows";
const RUN_ROWS: &str = "dagger_workflow_run_rows";
const NODE_ROWS: &str = "dagger_workflow_node_run_rows";
const EDGE_ROWS: &str = "dagger_workflow_edge_rows";
const ATTEMPT_ROWS: &str = "dagger_workflow_attempt_rows";
const APPROVAL_ROWS: &str = "dagger_workflow_approval_rows";
const INVOCATION_ROWS: &str = "dagger_workflow_invocation_rows";
const IDEMPOTENCY_ROWS: &str = "dagger_workflow_idempotency_rows";
const ARTIFACT_ROWS: &str = "dagger_workflow_artifact_rows";
const OBJECT_ROWS: &str = "dagger_workflow_object_rows";
const EVENT_ROWS: &str = "dagger_workflow_event_rows";
const EVENT_BATCH_ROWS: &str = "dagger_workflow_event_batch_rows";
const BUDGET_ROWS: &str = "dagger_workflow_budget_rows";
const STALE_ROWS: &str = "dagger_workflow_stale_observation_rows";
const COUNTER_ROWS: &str = "dagger_workflow_scope_counters";

const AUTHORITY_TABLES: &[&str] = &[
    DEFINITION_ROWS,
    REVISION_ROWS,
    ENGINE_CLAIM_ROWS,
    RUN_ROWS,
    NODE_ROWS,
    EDGE_ROWS,
    ATTEMPT_ROWS,
    APPROVAL_ROWS,
    INVOCATION_ROWS,
    IDEMPOTENCY_ROWS,
    ARTIFACT_ROWS,
    OBJECT_ROWS,
    EVENT_ROWS,
    EVENT_BATCH_ROWS,
    BUDGET_ROWS,
    STALE_ROWS,
    COUNTER_ROWS,
];

const RUN_SCOPED_AUTHORITY_TABLES: &[&str] = &[
    RUN_ROWS,
    NODE_ROWS,
    EDGE_ROWS,
    ATTEMPT_ROWS,
    APPROVAL_ROWS,
    INVOCATION_ROWS,
    EVENT_ROWS,
    EVENT_BATCH_ROWS,
    BUDGET_ROWS,
    STALE_ROWS,
];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AuthorityKey {
    table: &'static str,
    entity_id: String,
    sub_id: String,
}

impl AuthorityKey {
    fn new(table: &'static str, entity_id: String, sub_id: String) -> Self {
        Self {
            table,
            entity_id,
            sub_id,
        }
    }
}

struct LoadedAuthority {
    state: ReducerState,
    rows: BTreeMap<AuthorityKey, (String, i64)>,
}

#[derive(Deserialize, Serialize)]
struct RevisionAuthority {
    revision: WorkflowRevision,
    canonical_revision: PublishableDefinition,
}

#[derive(Deserialize, Serialize)]
struct EventBatchAuthority {
    run_id: Id,
    batch_id: Id,
    first_event_seq: u64,
    last_event_seq: u64,
    batch_count: u32,
}

#[derive(Deserialize, Serialize)]
struct ScopeAuthority {
    batch_counter: u64,
    object_store_nonce: Option<Vec<u8>>,
}

fn encode_row<T: Serialize>(value: &T) -> Result<String, StoreError> {
    serde_jcs::to_string(value).map_err(|_| StoreError::TransactionFailed)
}

fn decode_row<T: DeserializeOwned>(value: &str) -> Result<T, StoreError> {
    serde_json::from_str(value).map_err(|_| StoreError::CorruptControlPlane)
}

fn scoped<T>(
    scope: &ExecutionScope,
    row_scope: &ExecutionScope,
    value: T,
) -> Result<T, StoreError> {
    if row_scope == scope {
        Ok(value)
    } else {
        Err(StoreError::CorruptControlPlane)
    }
}

fn decode_authority_row(
    state: &mut ReducerState,
    scope: &ExecutionScope,
    key: &AuthorityKey,
    data: &str,
) -> Result<(), StoreError> {
    match key.table {
        DEFINITION_ROWS => {
            let row: DefinitionRecord = decode_row(data)?;
            scoped(scope, &row.scope, ())?;
            state
                .definitions
                .insert((scope.clone(), row.definition_id.clone()), row);
        }
        REVISION_ROWS => {
            let row: RevisionAuthority = decode_row(data)?;
            scoped(scope, &row.revision.scope, ())?;
            let map_key = (
                scope.clone(),
                row.revision.definition_id.clone(),
                row.revision.revision_hash.clone(),
            );
            state
                .parsed_revisions
                .insert(map_key.clone(), row.canonical_revision);
            state.revisions.insert(map_key, row.revision);
        }
        ENGINE_CLAIM_ROWS => {
            let row: StoredClaim = decode_row(data)?;
            scoped(scope, &row.claim.scope, ())?;
            state.claims.insert(scope.clone(), row);
        }
        RUN_ROWS => {
            let row: WorkflowRun = decode_row(data)?;
            scoped(scope, &row.scope, ())?;
            state.runs.insert((scope.clone(), row.run_id.clone()), row);
        }
        NODE_ROWS => {
            let row: NodeRun = decode_row(data)?;
            scoped(scope, &row.scope, ())?;
            state.nodes.insert(
                (
                    scope.clone(),
                    row.run_id.clone(),
                    row.node_instance_id.clone(),
                ),
                row,
            );
        }
        EDGE_ROWS => {
            let row: EdgeFact = decode_row(data)?;
            scoped(scope, &row.scope, ())?;
            state.edges.insert(
                (scope.clone(), row.run_id.clone(), row.edge_id.clone()),
                row,
            );
        }
        ATTEMPT_ROWS => {
            let row: NodeAttempt = decode_row(data)?;
            scoped(scope, &row.scope, ())?;
            state.attempts.insert(
                (scope.clone(), row.run_id.clone(), row.attempt_id.clone()),
                row,
            );
        }
        APPROVAL_ROWS => {
            let row: ApprovalGate = decode_row(data)?;
            scoped(scope, &row.scope, ())?;
            state.gates.insert(
                (scope.clone(), row.run_id.clone(), row.gate_id.clone()),
                row,
            );
        }
        INVOCATION_ROWS => {
            let row: ActionInvocation = decode_row(data)?;
            scoped(scope, &row.scope, ())?;
            state.invocations.insert(
                (scope.clone(), row.run_id.clone(), row.invocation_id.clone()),
                row,
            );
        }
        IDEMPOTENCY_ROWS => {
            let row: CommandReceipt = decode_row(data)?;
            scoped(scope, &row.scope, ())?;
            let kind = match row.command_kind {
                CommandKind::CreateRun => CommandKindKey::Create,
                CommandKind::CancelRun => CommandKindKey::Cancel,
            };
            state
                .receipts
                .insert((scope.clone(), kind, row.idempotency_token.clone()), row);
        }
        ARTIFACT_ROWS => {
            let row: ArtifactRef = decode_row(data)?;
            scoped(scope, &row.scope, ())?;
            state
                .artifact_refs
                .insert((scope.clone(), row.artifact_ref_id.clone()), row);
        }
        OBJECT_ROWS => {
            let row: ObjectRecord = decode_row(data)?;
            scoped(scope, &row.scope, ())?;
            state
                .object_records
                .insert((scope.clone(), row.digest.clone()), row);
        }
        EVENT_ROWS => {
            let row: WorkflowEvent = decode_row(data)?;
            scoped(scope, &row.scope, ())?;
            state
                .events
                .entry((scope.clone(), row.run_id.clone()))
                .or_default()
                .push(row);
        }
        EVENT_BATCH_ROWS => {
            let _: EventBatchAuthority = decode_row(data)?;
        }
        BUDGET_ROWS => {
            let row: BudgetLedgerEntry = decode_row(data)?;
            scoped(scope, &row.scope, ())?;
            state
                .ledger
                .entry((scope.clone(), row.run_id.clone()))
                .or_default()
                .push(row);
        }
        STALE_ROWS => {
            state.stale_observed.insert((
                scope.clone(),
                Id::new(key.entity_id.clone()).map_err(|_| StoreError::CorruptControlPlane)?,
                Id::new(key.sub_id.clone()).map_err(|_| StoreError::CorruptControlPlane)?,
            ));
        }
        COUNTER_ROWS => {
            let row: ScopeAuthority = decode_row(data)?;
            state.batch_counter = row.batch_counter;
            state.object_store_nonce = row.object_store_nonce;
        }
        _ => return Err(StoreError::CorruptControlPlane),
    }
    Ok(())
}

fn encode_authority_rows(
    state: &ReducerState,
    scope: &ExecutionScope,
) -> Result<BTreeMap<AuthorityKey, String>, StoreError> {
    let mut rows = BTreeMap::new();
    let mut put = |table, entity_id: String, sub_id: String, data| {
        rows.insert(AuthorityKey::new(table, entity_id, sub_id), data);
    };
    for ((row_scope, id), row) in &state.definitions {
        if row_scope == scope {
            put(
                DEFINITION_ROWS,
                id.as_str().to_owned(),
                String::new(),
                encode_row(row)?,
            );
        }
    }
    for ((row_scope, definition_id, revision_hash), revision) in &state.revisions {
        if row_scope == scope {
            let canonical_revision = state
                .parsed_revisions
                .get(&(
                    row_scope.clone(),
                    definition_id.clone(),
                    revision_hash.clone(),
                ))
                .ok_or(StoreError::CorruptControlPlane)?
                .clone();
            put(
                REVISION_ROWS,
                definition_id.as_str().to_owned(),
                revision_hash.as_str().to_owned(),
                encode_row(&RevisionAuthority {
                    revision: revision.clone(),
                    canonical_revision,
                })?,
            );
        }
    }
    if let Some(row) = state.claims.get(scope) {
        put(
            ENGINE_CLAIM_ROWS,
            "scheduler".to_owned(),
            String::new(),
            encode_row(row)?,
        );
    }
    for ((row_scope, run_id), row) in &state.runs {
        if row_scope == scope {
            put(
                RUN_ROWS,
                run_id.as_str().to_owned(),
                String::new(),
                encode_row(row)?,
            );
        }
    }
    for ((row_scope, run_id, node_id), row) in &state.nodes {
        if row_scope == scope {
            put(
                NODE_ROWS,
                run_id.as_str().to_owned(),
                node_id.as_str().to_owned(),
                encode_row(row)?,
            );
        }
    }
    for ((row_scope, run_id, edge_id), row) in &state.edges {
        if row_scope == scope {
            put(
                EDGE_ROWS,
                run_id.as_str().to_owned(),
                edge_id.as_str().to_owned(),
                encode_row(row)?,
            );
        }
    }
    for ((row_scope, run_id, attempt_id), row) in &state.attempts {
        if row_scope == scope {
            put(
                ATTEMPT_ROWS,
                run_id.as_str().to_owned(),
                attempt_id.as_str().to_owned(),
                encode_row(row)?,
            );
        }
    }
    for ((row_scope, run_id, gate_id), row) in &state.gates {
        if row_scope == scope {
            put(
                APPROVAL_ROWS,
                run_id.as_str().to_owned(),
                gate_id.as_str().to_owned(),
                encode_row(row)?,
            );
        }
    }
    for ((row_scope, run_id, invocation_id), row) in &state.invocations {
        if row_scope == scope {
            put(
                INVOCATION_ROWS,
                run_id.as_str().to_owned(),
                invocation_id.as_str().to_owned(),
                encode_row(row)?,
            );
        }
    }
    for ((row_scope, kind, token), row) in &state.receipts {
        if row_scope == scope {
            put(
                IDEMPOTENCY_ROWS,
                match kind {
                    CommandKindKey::Create => "create-run",
                    CommandKindKey::Cancel => "cancel-run",
                }
                .to_owned(),
                token.clone(),
                encode_row(row)?,
            );
        }
    }
    for ((row_scope, id), row) in &state.artifact_refs {
        if row_scope == scope {
            put(
                ARTIFACT_ROWS,
                id.as_str().to_owned(),
                String::new(),
                encode_row(row)?,
            );
        }
    }
    for ((row_scope, digest), row) in &state.object_records {
        if row_scope == scope {
            put(
                OBJECT_ROWS,
                digest.as_str().to_owned(),
                String::new(),
                encode_row(row)?,
            );
        }
    }
    let mut batches = BTreeMap::<(Id, Id), EventBatchAuthority>::new();
    for ((row_scope, run_id), events) in &state.events {
        if row_scope != scope {
            continue;
        }
        for event in events {
            put(
                EVENT_ROWS,
                run_id.as_str().to_owned(),
                format!("{:020}", event.event_seq),
                encode_row(event)?,
            );
            batches
                .entry((run_id.clone(), event.batch_id.clone()))
                .and_modify(|batch| {
                    batch.first_event_seq = batch.first_event_seq.min(event.event_seq);
                    batch.last_event_seq = batch.last_event_seq.max(event.event_seq);
                })
                .or_insert(EventBatchAuthority {
                    run_id: run_id.clone(),
                    batch_id: event.batch_id.clone(),
                    first_event_seq: event.event_seq,
                    last_event_seq: event.event_seq,
                    batch_count: event.batch_count,
                });
        }
    }
    for ((run_id, batch_id), batch) in batches {
        put(
            EVENT_BATCH_ROWS,
            run_id.as_str().to_owned(),
            batch_id.as_str().to_owned(),
            encode_row(&batch)?,
        );
    }
    for ((row_scope, run_id), entries) in &state.ledger {
        if row_scope == scope {
            for entry in entries {
                put(
                    BUDGET_ROWS,
                    run_id.as_str().to_owned(),
                    format!("{:020}", entry.ledger_seq),
                    encode_row(entry)?,
                );
            }
        }
    }
    for (row_scope, run_id, attempt_id) in &state.stale_observed {
        if row_scope == scope {
            put(
                STALE_ROWS,
                run_id.as_str().to_owned(),
                attempt_id.as_str().to_owned(),
                "true".to_owned(),
            );
        }
    }
    put(
        COUNTER_ROWS,
        "event-batch".to_owned(),
        String::new(),
        encode_row(&ScopeAuthority {
            batch_counter: state.batch_counter,
            object_store_nonce: state.object_store_nonce.clone(),
        })?,
    );
    Ok(rows)
}

/// SQLite-backed workflow control plane with no process-local domain state.
pub struct SqliteWorkflowStore<C, O> {
    pool: SqlitePool,
    /// The scoped durable object plane. SQLite retains only metadata and
    /// references. Proof validation is nonce-based, matching memory.
    objects: Arc<O>,
    /// Ephemeral reducer input only. It is neither serialized nor read after
    /// restart; object verification belongs exclusively to the engine.
    verified_bytes: Arc<Mutex<BTreeMap<(ExecutionScope, Digest), Vec<u8>>>>,
    #[cfg(feature = "conformance")]
    commit_fault: Arc<Mutex<Option<SqliteCommitFault>>>,
    marker: PhantomData<fn() -> C>,
}

impl<C, O> SqliteWorkflowStore<C, O>
where
    O: ObjectStore,
{
    /// Initializes the adapter on a caller-owned pool without disturbing host tables.
    pub async fn from_pool(
        pool: SqlitePool,
        _clock: Arc<C>,
        objects: Arc<O>,
    ) -> Result<Self, StoreError> {
        schema::migrate(&pool).await?;
        Ok(Self {
            pool,
            objects,
            verified_bytes: Arc::new(Mutex::new(BTreeMap::new())),
            #[cfg(feature = "conformance")]
            commit_fault: Arc::new(Mutex::new(None)),
            marker: PhantomData,
        })
    }

    /// Opens or creates a standalone WAL database and runs pending migrations.
    pub async fn open(
        path: impl AsRef<Path>,
        clock: Arc<C>,
        objects: Arc<O>,
    ) -> Result<Self, StoreError> {
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
        Self::from_pool(pool, clock, objects).await
    }

    /// Opens a SQLite URL, including a single-connection in-memory database.
    pub async fn open_url(url: &str, clock: Arc<C>, objects: Arc<O>) -> Result<Self, StoreError> {
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
        Self::from_pool(pool, clock, objects).await
    }

    /// Exposes the injected pool for host composition and diagnostics.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Arms one crash-window fault for the next successful domain command.
    #[cfg(feature = "conformance")]
    pub fn inject_commit_fault_once(&self, fault: SqliteCommitFault) {
        *self
            .commit_fault
            .lock()
            .expect("commit-fault lock poisoned") = Some(fault);
    }

    #[cfg(feature = "conformance")]
    fn take_commit_fault(&self) -> Option<SqliteCommitFault> {
        self.commit_fault
            .lock()
            .expect("commit-fault lock poisoned")
            .take()
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

    async fn begin_read(&self) -> Result<PoolConnection<Sqlite>, StoreError> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| StoreError::StorageUnavailable)?;
        sqlx::query("BEGIN")
            .execute(&mut *connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        Ok(connection)
    }

    async fn database_now(
        connection: &mut PoolConnection<Sqlite>,
    ) -> Result<Timestamp, StoreError> {
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
        Ok(Timestamp(now_ms))
    }

    async fn load_authority_table(
        connection: &mut PoolConnection<Sqlite>,
        scope: &ExecutionScope,
        table: &'static str,
        run_filter: Option<&Id>,
    ) -> Result<Vec<(String, String, String, i64)>, StoreError> {
        // `table` is selected only from AUTHORITY_TABLES below. The scope
        // predicate is deliberately part of the statement, not inferred.
        if RUN_SCOPED_AUTHORITY_TABLES.contains(&table) {
            if let Some(run_id) = run_filter {
                let statement = format!(
                    "SELECT entity_id, sub_id, row_data, version FROM {table}
                     WHERE tenant_id = ? AND namespace = ? AND entity_id = ?"
                );
                return sqlx::query_as(&statement)
                    .bind(scope.tenant_id.as_str())
                    .bind(scope.namespace.as_str())
                    .bind(run_id.as_str())
                    .fetch_all(&mut **connection)
                    .await
                    .map_err(|_| StoreError::StorageUnavailable);
            }
        }
        let statement = format!(
            "SELECT entity_id, sub_id, row_data, version FROM {table}
             WHERE tenant_id = ? AND namespace = ?"
        );
        sqlx::query_as(&statement)
            .bind(scope.tenant_id.as_str())
            .bind(scope.namespace.as_str())
            .fetch_all(&mut **connection)
            .await
            .map_err(|_| StoreError::StorageUnavailable)
    }

    async fn load_state(
        connection: &mut PoolConnection<Sqlite>,
        scope: &ExecutionScope,
        run_filter: Option<&Id>,
    ) -> Result<LoadedAuthority, StoreError> {
        let mut loaded = LoadedAuthority {
            state: ReducerState::default(),
            rows: BTreeMap::new(),
        };
        for table in AUTHORITY_TABLES {
            for (entity_id, sub_id, row_data, version) in
                Self::load_authority_table(connection, scope, table, run_filter).await?
            {
                let key = AuthorityKey::new(table, entity_id, sub_id);
                decode_authority_row(&mut loaded.state, scope, &key, &row_data)?;
                loaded.rows.insert(key, (row_data, version));
            }
        }
        for events in loaded.state.events.values_mut() {
            events.sort_by_key(|event| event.event_seq);
        }
        for entries in loaded.state.ledger.values_mut() {
            entries.sort_by_key(|entry| entry.ledger_seq);
        }
        for ((run_scope, run_id), run) in &loaded.state.runs {
            let parsed = loaded
                .state
                .parsed_revisions
                .get(&(
                    run_scope.clone(),
                    run.definition_id.clone(),
                    run.revision_hash.clone(),
                ))
                .ok_or(StoreError::CorruptControlPlane)?;
            loaded
                .state
                .run_definitions
                .insert((run_scope.clone(), run_id.clone()), parsed.clone());
        }
        Ok(loaded)
    }

    async fn load_engine_claim_state(
        connection: &mut PoolConnection<Sqlite>,
        scope: &ExecutionScope,
    ) -> Result<LoadedAuthority, StoreError> {
        let mut loaded = LoadedAuthority {
            state: ReducerState::default(),
            rows: BTreeMap::new(),
        };
        for table in [ENGINE_CLAIM_ROWS, COUNTER_ROWS] {
            for (entity_id, sub_id, row_data, version) in
                Self::load_authority_table(connection, scope, table, None).await?
            {
                let key = AuthorityKey::new(table, entity_id, sub_id);
                decode_authority_row(&mut loaded.state, scope, &key, &row_data)?;
                loaded.rows.insert(key, (row_data, version));
            }
        }
        Ok(loaded)
    }

    async fn persist_authority(
        connection: &mut PoolConnection<Sqlite>,
        scope: &ExecutionScope,
        before: &LoadedAuthority,
        state: &ReducerState,
    ) -> Result<(), StoreError> {
        let after = encode_authority_rows(state, scope)?;
        let mut keys = before
            .rows
            .keys()
            .chain(after.keys())
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        for key in keys {
            match (before.rows.get(&key), after.get(&key)) {
                (None, Some(data)) => {
                    let statement = format!(
                        "INSERT INTO {} (tenant_id, namespace, entity_id, sub_id, row_data, version)
                         VALUES (?, ?, ?, ?, ?, 1)",
                        key.table
                    );
                    sqlx::query(&statement)
                        .bind(scope.tenant_id.as_str())
                        .bind(scope.namespace.as_str())
                        .bind(&key.entity_id)
                        .bind(&key.sub_id)
                        .bind(data)
                        .execute(&mut **connection)
                        .await
                        .map_err(|_| StoreError::CasConflict)?;
                }
                (Some((old_data, version)), Some(data)) if old_data != data => {
                    let statement = format!(
                        "UPDATE {} SET row_data = ?, version = version + 1
                         WHERE tenant_id = ? AND namespace = ? AND entity_id = ? AND sub_id = ?
                           AND version = ? AND version < 9223372036854775807",
                        key.table
                    );
                    let changed = sqlx::query(&statement)
                        .bind(data)
                        .bind(scope.tenant_id.as_str())
                        .bind(scope.namespace.as_str())
                        .bind(&key.entity_id)
                        .bind(&key.sub_id)
                        .bind(version)
                        .execute(&mut **connection)
                        .await
                        .map_err(|_| StoreError::TransactionFailed)?;
                    if changed.rows_affected() != 1 {
                        return Err(StoreError::CasConflict);
                    }
                }
                (Some((_, version)), None) => {
                    let statement = format!(
                        "DELETE FROM {} WHERE tenant_id = ? AND namespace = ?
                         AND entity_id = ? AND sub_id = ? AND version = ?",
                        key.table
                    );
                    let changed = sqlx::query(&statement)
                        .bind(scope.tenant_id.as_str())
                        .bind(scope.namespace.as_str())
                        .bind(&key.entity_id)
                        .bind(&key.sub_id)
                        .bind(version)
                        .execute(&mut **connection)
                        .await
                        .map_err(|_| StoreError::TransactionFailed)?;
                    if changed.rows_affected() != 1 {
                        return Err(StoreError::CasConflict);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn sql_integer(value: u64) -> Result<i64, StoreError> {
        i64::try_from(value).map_err(|_| StoreError::ArithmeticOverflow)
    }

    async fn persist_projections(
        connection: &mut PoolConnection<Sqlite>,
        scope: &ExecutionScope,
        before: &LoadedAuthority,
        state: &ReducerState,
    ) -> Result<(), StoreError> {
        let after = encode_authority_rows(state, scope)?;
        let changed = before
            .rows
            .keys()
            .chain(after.keys())
            .filter(|key| before.rows.get(*key).map(|(data, _)| data) != after.get(*key))
            .cloned()
            .collect::<BTreeSet<_>>();
        let touched = |table: &'static str, entity_id: &str, sub_id: &str| {
            changed.contains(&AuthorityKey::new(
                table,
                entity_id.to_owned(),
                sub_id.to_owned(),
            ))
        };
        for key in &changed {
            let (table, entity_column, sub_column) = match key.table {
                DEFINITION_ROWS => ("dagger_workflow_definitions", "definition_id", None),
                REVISION_ROWS => {
                    for table in [
                        "dagger_workflow_revision_nodes",
                        "dagger_workflow_revisions",
                    ] {
                        let statement = format!(
                            "DELETE FROM {table} WHERE tenant_id = ? AND namespace = ?
                             AND definition_id = ? AND revision_hash = ?"
                        );
                        sqlx::query(&statement)
                            .bind(scope.tenant_id.as_str())
                            .bind(scope.namespace.as_str())
                            .bind(&key.entity_id)
                            .bind(&key.sub_id)
                            .execute(&mut **connection)
                            .await
                            .map_err(|_| StoreError::TransactionFailed)?;
                    }
                    continue;
                }
                ENGINE_CLAIM_ROWS => ("dagger_workflow_engine_claims", "control_plane_id", None),
                RUN_ROWS => ("dagger_workflow_runs", "run_id", None),
                NODE_ROWS => ("dagger_workflow_nodes", "run_id", Some("node_id")),
                EDGE_ROWS => ("dagger_workflow_edges", "run_id", Some("edge_id")),
                ATTEMPT_ROWS => ("dagger_workflow_attempts", "run_id", Some("attempt_id")),
                APPROVAL_ROWS => ("dagger_workflow_approval_gates", "run_id", Some("gate_id")),
                INVOCATION_ROWS => (
                    "dagger_workflow_action_invocations",
                    "run_id",
                    Some("invocation_id"),
                ),
                IDEMPOTENCY_ROWS => (
                    "dagger_workflow_command_receipts",
                    "command_kind",
                    Some("idempotency_token"),
                ),
                ARTIFACT_ROWS => ("dagger_workflow_artifact_refs", "artifact_ref_id", None),
                OBJECT_ROWS => ("dagger_workflow_objects", "digest", None),
                EVENT_ROWS => ("dagger_workflow_events", "run_id", Some("event_seq")),
                BUDGET_ROWS => (
                    "dagger_workflow_budget_ledger",
                    "run_id",
                    Some("ledger_seq"),
                ),
                _ => continue,
            };
            let statement = if let Some(sub_column) = sub_column {
                format!(
                    "DELETE FROM {table} WHERE tenant_id = ? AND namespace = ?
                     AND {entity_column} = ? AND {sub_column} = ?"
                )
            } else {
                format!(
                    "DELETE FROM {table} WHERE tenant_id = ? AND namespace = ?
                     AND {entity_column} = ?"
                )
            };
            let mut query = sqlx::query(&statement)
                .bind(scope.tenant_id.as_str())
                .bind(scope.namespace.as_str())
                .bind(match key.table {
                    IDEMPOTENCY_ROWS if key.entity_id == "create-run" => "CreateRun",
                    IDEMPOTENCY_ROWS if key.entity_id == "cancel-run" => "CancelRun",
                    _ => key.entity_id.as_str(),
                });
            if sub_column.is_some() {
                if matches!(key.table, EVENT_ROWS | BUDGET_ROWS) {
                    query = query.bind(
                        key.sub_id
                            .parse::<i64>()
                            .map_err(|_| StoreError::CorruptControlPlane)?,
                    );
                } else {
                    query = query.bind(&key.sub_id);
                }
            }
            query
                .execute(&mut **connection)
                .await
                .map_err(|_| StoreError::TransactionFailed)?;
        }

        for definition in state.definitions.values() {
            if !touched(DEFINITION_ROWS, definition.definition_id.as_str(), "") {
                continue;
            }
            sqlx::query(
                "INSERT INTO dagger_workflow_definitions
                 (tenant_id, namespace, definition_id, display_name, description,
                  created_at_ms, created_by, latest_revision_hash, version)
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
            if !touched(
                REVISION_ROWS,
                revision.definition_id.as_str(),
                revision.revision_hash.as_str(),
            ) {
                continue;
            }
            sqlx::query(
                "INSERT INTO dagger_workflow_revisions
                 (tenant_id, namespace, definition_id, revision_hash,
                  canonical_definition_digest, run_input_schema_digest,
                  run_output_schema_digest, created_at_ms, created_by)
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
                     (tenant_id, namespace, definition_id, revision_hash, node_id,
                      node_kind, topological_rank)
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
            if !touched(ENGINE_CLAIM_ROWS, "scheduler", "") {
                continue;
            }
            sqlx::query(
                "INSERT INTO dagger_workflow_engine_claims
                 (tenant_id, namespace, control_plane_id, instance_id, generation,
                  session_token_digest, released_token_digest, claimed_at_ms,
                  heartbeat_at_ms, expires_at_ms, version)
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
            if !touched(RUN_ROWS, run.run_id.as_str(), "") {
                continue;
            }
            sqlx::query(
                "INSERT INTO dagger_workflow_runs
                 (tenant_id, namespace, run_id, definition_id, revision_hash, status,
                  version, event_seq, aggregate_object_bytes, total_attempts, budget_limit,
                  budget_reserved, budget_spent, lifetime_deadline_at_ms, created_at_ms,
                  updated_at_ms)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
            .bind(run.updated_at.0)
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
            let run_key =
                AuthorityKey::new(RUN_ROWS, run.run_id.as_str().to_owned(), String::new());
            if !before.rows.contains_key(&run_key) {
                sqlx::query(
                    "INSERT INTO dagger_workflow_run_limits
                 (tenant_id, namespace, run_id, max_dynamic_node_instances,
                  max_total_attempts, max_total_events, max_inline_json_bytes_per_value,
                  max_artifacts_per_attempt, max_aggregate_object_bytes_per_run,
                  max_run_lifetime_ms)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        }
        for node in state.nodes.values() {
            if !touched(
                NODE_ROWS,
                node.run_id.as_str(),
                node.node_instance_id.as_str(),
            ) {
                continue;
            }
            sqlx::query(
                "INSERT INTO dagger_workflow_nodes
                 (tenant_id, namespace, run_id, node_id, node_kind, status,
                  topological_rank, map_item_index, attempt_count, active_attempt_id,
                  next_eligible_at_ms, version, updated_at_ms)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
            .bind(node.updated_at.0)
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        }
        for edge in state.edges.values() {
            if !touched(EDGE_ROWS, edge.run_id.as_str(), edge.edge_id.as_str()) {
                continue;
            }
            sqlx::query(
                "INSERT INTO dagger_workflow_edges
                 (tenant_id, namespace, run_id, edge_id, from_node_id, to_node_id,
                  status, version)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(edge.scope.tenant_id.as_str())
            .bind(edge.scope.namespace.as_str())
            .bind(edge.run_id.as_str())
            .bind(edge.edge_id.as_str())
            .bind(edge.from_node_id.as_str())
            .bind(edge.to_node_id.as_str())
            .bind(format!("{:?}", edge.state))
            .bind(Self::sql_integer(edge.version.0)?)
            .execute(&mut **connection)
            .await
            .map_err(|_| StoreError::TransactionFailed)?;
        }
        for attempt in state.attempts.values() {
            if !touched(
                ATTEMPT_ROWS,
                attempt.run_id.as_str(),
                attempt.attempt_id.as_str(),
            ) {
                continue;
            }
            sqlx::query(
                "INSERT INTO dagger_workflow_attempts
                 (tenant_id, namespace, run_id, attempt_id, node_id, attempt_number,
                  status, engine_generation, completion_credential_digest,
                  reservation_units, started_at_ms, deadline_at_ms, finished_at_ms,
                  output_digest, diagnostics_digest, version)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
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
            if !touched(
                INVOCATION_ROWS,
                invocation.run_id.as_str(),
                invocation.invocation_id.as_str(),
            ) {
                continue;
            }
            sqlx::query(
                "INSERT INTO dagger_workflow_action_invocations
                 (tenant_id, namespace, run_id, invocation_id, attempt_id, node_id,
                  input_digest, action_semantic_digest, idempotency_key)
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
            if !touched(APPROVAL_ROWS, gate.run_id.as_str(), gate.gate_id.as_str()) {
                continue;
            }
            sqlx::query(
                "INSERT INTO dagger_workflow_approval_gates
                 (tenant_id, namespace, run_id, gate_id, node_id, status,
                  request_digest, decision_payload_digest, output_digest,
                  expires_at_ms, version)
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
                if !touched(
                    BUDGET_ROWS,
                    entry.run_id.as_str(),
                    &format!("{:020}", entry.ledger_seq),
                ) {
                    continue;
                }
                let reserved_delta = i64::try_from(entry.reserved_delta)
                    .map_err(|_| StoreError::ArithmeticOverflow)?;
                sqlx::query(
                    "INSERT INTO dagger_workflow_budget_ledger
                     (tenant_id, namespace, run_id, ledger_seq, attempt_id, node_id,
                      kind, reserved_delta, consumed_delta, reservation_amount,
                      reason, created_at_ms)
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
                if !touched(
                    EVENT_ROWS,
                    event.run_id.as_str(),
                    &format!("{:020}", event.event_seq),
                ) {
                    continue;
                }
                sqlx::query(
                    "INSERT INTO dagger_workflow_events
                     (tenant_id, namespace, run_id, event_seq, batch_id, batch_index,
                      batch_count, event_type, actor_kind, actor_id, occurred_at_ms,
                      payload_json)
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
            let command_kind = match receipt.command_kind {
                CommandKind::CreateRun => "create-run",
                CommandKind::CancelRun => "cancel-run",
            };
            if !touched(IDEMPOTENCY_ROWS, command_kind, &receipt.idempotency_token) {
                continue;
            }
            sqlx::query(
                "INSERT INTO dagger_workflow_command_receipts
                 (tenant_id, namespace, command_kind, idempotency_token,
                  request_fingerprint, run_id, outcome_json, batch_id, committed_at_ms)
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
            if !touched(OBJECT_ROWS, record.digest.as_str(), "") {
                continue;
            }
            sqlx::query(
                "INSERT INTO dagger_workflow_objects
                 (tenant_id, namespace, digest, size_bytes, object_key, created_at_ms)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
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
            if !touched(ARTIFACT_ROWS, reference.artifact_ref_id.as_str(), "") {
                continue;
            }
            sqlx::query(
                "INSERT INTO dagger_workflow_artifact_refs
                 (tenant_id, namespace, artifact_ref_id, digest, size_bytes,
                  media_type, kind, producer_run_id, producer_node_id,
                  producer_attempt_id, ordinal, created_at_ms)
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

    async fn persist_engine_claim_projection(
        connection: &mut PoolConnection<Sqlite>,
        scope: &ExecutionScope,
        state: &ReducerState,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "DELETE FROM dagger_workflow_engine_claims
             WHERE tenant_id = ? AND namespace = ? AND control_plane_id = 'scheduler'",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .execute(&mut **connection)
        .await
        .map_err(|_| StoreError::TransactionFailed)?;
        if let Some(stored) = state.claims.get(scope) {
            let claim = &stored.claim;
            sqlx::query(
                "INSERT INTO dagger_workflow_engine_claims
                 (tenant_id, namespace, control_plane_id, instance_id, generation,
                  session_token_digest, released_token_digest, claimed_at_ms,
                  heartbeat_at_ms, expires_at_ms, version)
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

    async fn hydrate_revision_schemas(
        &self,
        scope: &ExecutionScope,
        definition_id: &Id,
        revision_hash: &Digest,
        include_action_schemas: bool,
    ) -> Result<(), StoreError> {
        let mut connection = self.begin_read().await?;
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT row_data FROM dagger_workflow_revision_rows
             WHERE tenant_id = ? AND namespace = ? AND entity_id = ? AND sub_id = ?",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(definition_id.as_str())
        .bind(revision_hash.as_str())
        .fetch_optional(&mut *connection)
        .await
        .map_err(|_| StoreError::StorageUnavailable)?;
        Self::commit(connection).await?;
        let (data,) = row.ok_or(StoreError::NotFound)?;
        let authority: RevisionAuthority = decode_row(&data)?;
        let mut digests = vec![authority.revision.run_input_schema_ref.0.digest];
        if include_action_schemas {
            digests.extend(authority.revision.action_pins.into_iter().flat_map(|pin| {
                [
                    pin.input_schema_ref.0.digest,
                    pin.output_schema_ref.0.digest,
                ]
            }));
        }
        digests.sort();
        digests.dedup();
        for digest in digests {
            let object = self
                .objects
                .get(scope, &digest)
                .await
                // The frozen StoreError taxonomy cannot transport FailedReadProof.
                // CorruptControlPlane is the closest no-write command failure; the
                // engine can repeat ObjectStore::get to receive the minted proof
                // and submit mark_corrupt_storage.
                .map_err(|_| StoreError::CorruptControlPlane)?;
            self.verified_bytes
                .lock()
                .expect("verified-byte cache lock poisoned")
                .insert((scope.clone(), digest), object.bytes);
        }
        Ok(())
    }

    async fn hydrate_run_action_schemas(
        &self,
        scope: &ExecutionScope,
        run_id: &Id,
    ) -> Result<(), StoreError> {
        let mut connection = self.begin_read().await?;
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT row_data FROM dagger_workflow_run_rows
             WHERE tenant_id = ? AND namespace = ? AND entity_id = ? AND sub_id = ''",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(run_id.as_str())
        .fetch_optional(&mut *connection)
        .await
        .map_err(|_| StoreError::StorageUnavailable)?;
        Self::commit(connection).await?;
        let (data,) = row.ok_or(StoreError::NotFound)?;
        let run: WorkflowRun = decode_row(&data)?;
        self.hydrate_revision_schemas(scope, &run.definition_id, &run.revision_hash, true)
            .await
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
        let mut connection = self.begin_read().await?;
        let now = Self::database_now(&mut connection).await?;
        let mut state = Self::load_state(&mut connection, scope, None).await?.state;
        self.restore_verified_bytes(&mut state, scope);
        Self::commit(connection).await?;
        Ok(ReducerStore::from_state(
            Arc::new(DatabaseTimestamp(now)),
            state,
        ))
    }

    fn timestamp_from_cursor(value: &str) -> Result<i64, StoreError> {
        let encoded = u64::from_str_radix(value, 16).map_err(|_| StoreError::InvalidField)?;
        Ok((encoded ^ (1_u64 << 63)) as i64)
    }

    async fn scan_nodes_sql(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
        query: &'static str,
        status: &'static str,
        due_retry: bool,
    ) -> Result<Page<NodeRun>, StoreError> {
        let mut connection = self.begin_read().await?;
        let now = Self::database_now(&mut connection).await?;
        let (limit, cutoff, last) = scan_page_context(&page, scope, query, now)?;
        let (last_run_id, last_node_id) = if last.is_empty() {
            ("", "")
        } else if last.len() == 2 {
            (last[0].as_str(), last[1].as_str())
        } else {
            return Err(StoreError::InvalidField);
        };
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT nr.row_data
             FROM dagger_workflow_nodes AS n
             JOIN dagger_workflow_runs AS r
               ON r.tenant_id = n.tenant_id AND r.namespace = n.namespace
              AND r.run_id = n.run_id
             JOIN dagger_workflow_node_run_rows AS nr
               ON nr.tenant_id = n.tenant_id AND nr.namespace = n.namespace
              AND nr.entity_id = n.run_id AND nr.sub_id = n.node_id
             WHERE n.tenant_id = ? AND n.namespace = ?
               AND r.tenant_id = ? AND r.namespace = ?
               AND nr.tenant_id = ? AND nr.namespace = ?
               AND r.status = 'Running' AND n.status = ?
               AND n.updated_at_ms <= ?
               AND (? = 0 OR n.next_eligible_at_ms <= ?)
               AND (n.run_id > ? OR (n.run_id = ? AND n.node_id > ?))
             ORDER BY n.run_id, n.node_id
             LIMIT ?",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(status)
        .bind(cutoff.0)
        .bind(i64::from(due_retry))
        .bind(cutoff.0)
        .bind(last_run_id)
        .bind(last_run_id)
        .bind(last_node_id)
        .bind(i64::try_from(limit + 1).map_err(|_| StoreError::ArithmeticOverflow)?)
        .fetch_all(&mut *connection)
        .await
        .map_err(|_| StoreError::StorageUnavailable)?;
        Self::commit(connection).await?;
        let mut items = Vec::with_capacity(rows.len());
        for (data,) in rows {
            let node: NodeRun = decode_row(&data)?;
            scoped(scope, &node.scope, ())?;
            items.push(node);
        }
        finish_scan_page(items, limit, scope, query, cutoff, |node| {
            vec![
                node.run_id.as_str().to_owned(),
                node.node_instance_id.as_str().to_owned(),
            ]
        })
    }

    async fn scan_due_deadlines_sql(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeAttempt>, StoreError> {
        let mut connection = self.begin_read().await?;
        let now = Self::database_now(&mut connection).await?;
        let query = "due_deadlines";
        let (limit, cutoff, last) = scan_page_context(&page, scope, query, now)?;
        let (last_deadline, last_run_id, last_node_id, last_attempt_id) = if last.is_empty() {
            (i64::MIN, "", "", "")
        } else if last.len() == 4 {
            (
                Self::timestamp_from_cursor(&last[0])?,
                last[1].as_str(),
                last[2].as_str(),
                last[3].as_str(),
            )
        } else {
            return Err(StoreError::InvalidField);
        };
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT ar.row_data
             FROM dagger_workflow_attempts AS a
             JOIN dagger_workflow_attempt_rows AS ar
               ON ar.tenant_id = a.tenant_id AND ar.namespace = a.namespace
              AND ar.entity_id = a.run_id AND ar.sub_id = a.attempt_id
             WHERE a.tenant_id = ? AND a.namespace = ?
               AND ar.tenant_id = ? AND ar.namespace = ?
               AND a.status = 'Started' AND a.started_at_ms <= ?
               AND a.deadline_at_ms <= ?
               AND (a.deadline_at_ms > ?
                 OR (a.deadline_at_ms = ? AND a.run_id > ?)
                 OR (a.deadline_at_ms = ? AND a.run_id = ? AND a.node_id > ?)
                 OR (a.deadline_at_ms = ? AND a.run_id = ? AND a.node_id = ?
                     AND a.attempt_id > ?))
             ORDER BY a.deadline_at_ms, a.run_id, a.node_id, a.attempt_id
             LIMIT ?",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(cutoff.0)
        .bind(cutoff.0)
        .bind(last_deadline)
        .bind(last_deadline)
        .bind(last_run_id)
        .bind(last_deadline)
        .bind(last_run_id)
        .bind(last_node_id)
        .bind(last_deadline)
        .bind(last_run_id)
        .bind(last_node_id)
        .bind(last_attempt_id)
        .bind(i64::try_from(limit + 1).map_err(|_| StoreError::ArithmeticOverflow)?)
        .fetch_all(&mut *connection)
        .await
        .map_err(|_| StoreError::StorageUnavailable)?;
        Self::commit(connection).await?;
        let mut items = Vec::with_capacity(rows.len());
        for (data,) in rows {
            let attempt: NodeAttempt = decode_row(&data)?;
            scoped(scope, &attempt.scope, ())?;
            items.push(attempt);
        }
        finish_scan_page(items, limit, scope, query, cutoff, |attempt| {
            vec![
                timestamp_cursor_key(attempt.deadline_at),
                attempt.run_id.as_str().to_owned(),
                attempt.node_instance_id.as_str().to_owned(),
                attempt.attempt_id.as_str().to_owned(),
            ]
        })
    }

    async fn scan_recovery_runs_sql(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError> {
        let mut connection = self.begin_read().await?;
        let now = Self::database_now(&mut connection).await?;
        let query = "recovery_runs";
        let (limit, cutoff, last) = scan_page_context(&page, scope, query, now)?;
        let generation = sqlx::query_scalar::<_, String>(
            "SELECT row_data FROM dagger_workflow_engine_claim_rows
             WHERE tenant_id = ? AND namespace = ?
               AND entity_id = 'scheduler' AND sub_id = ''",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .fetch_optional(&mut *connection)
        .await
        .map_err(|_| StoreError::StorageUnavailable)?
        .map(|row| decode_row::<StoredClaim>(&row).map(|claim| claim.claim.generation))
        .transpose()?
        .unwrap_or(0);
        let last_run_id = last.first().map_or("", String::as_str);
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT DISTINCT rr.entity_id, rr.row_data
             FROM dagger_workflow_attempts AS a
             JOIN dagger_workflow_run_rows AS rr
               ON rr.tenant_id = a.tenant_id AND rr.namespace = a.namespace
              AND rr.entity_id = a.run_id AND rr.sub_id = ''
             WHERE a.tenant_id = ? AND a.namespace = ?
               AND rr.tenant_id = ? AND rr.namespace = ?
               AND a.status = 'Started' AND a.engine_generation < ?
               AND a.started_at_ms <= ? AND rr.entity_id > ?
             ORDER BY rr.entity_id
             LIMIT ?",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(Self::sql_integer(generation)?)
        .bind(cutoff.0)
        .bind(last_run_id)
        .bind(i64::try_from(limit + 1).map_err(|_| StoreError::ArithmeticOverflow)?)
        .fetch_all(&mut *connection)
        .await
        .map_err(|_| StoreError::StorageUnavailable)?;
        Self::commit(connection).await?;
        let mut items = Vec::with_capacity(rows.len());
        for (_, data) in rows {
            let run: WorkflowRun = decode_row(&data)?;
            scoped(scope, &run.scope, ())?;
            items.push(run);
        }
        finish_scan_page(items, limit, scope, query, cutoff, |run| {
            vec![run.run_id.as_str().to_owned()]
        })
    }

    async fn scan_compatibility_rechecks_sql(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError> {
        let mut connection = self.begin_read().await?;
        let now = Self::database_now(&mut connection).await?;
        let query = "compatibility_rechecks";
        let (limit, cutoff, last) = scan_page_context(&page, scope, query, now)?;
        let (last_updated, last_run_id) = if last.is_empty() {
            (i64::MIN, "")
        } else if last.len() == 2 {
            (Self::timestamp_from_cursor(&last[0])?, last[1].as_str())
        } else {
            return Err(StoreError::InvalidField);
        };
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT rr.row_data
             FROM dagger_workflow_runs AS r
             JOIN dagger_workflow_run_rows AS rr
               ON rr.tenant_id = r.tenant_id AND rr.namespace = r.namespace
              AND rr.entity_id = r.run_id AND rr.sub_id = ''
             WHERE r.tenant_id = ? AND r.namespace = ?
               AND rr.tenant_id = ? AND rr.namespace = ?
               AND r.status IN ('Pending', 'Running', 'BlockedIncompatible')
               AND r.updated_at_ms <= ?
               AND (r.updated_at_ms > ? OR (r.updated_at_ms = ? AND r.run_id > ?))
             ORDER BY r.updated_at_ms, r.run_id
             LIMIT ?",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(cutoff.0)
        .bind(last_updated)
        .bind(last_updated)
        .bind(last_run_id)
        .bind(i64::try_from(limit + 1).map_err(|_| StoreError::ArithmeticOverflow)?)
        .fetch_all(&mut *connection)
        .await
        .map_err(|_| StoreError::StorageUnavailable)?;
        Self::commit(connection).await?;
        let mut items = Vec::with_capacity(rows.len());
        for (data,) in rows {
            let run: WorkflowRun = decode_row(&data)?;
            scoped(scope, &run.scope, ())?;
            items.push(run);
        }
        finish_scan_page(items, limit, scope, query, cutoff, |run| {
            vec![
                timestamp_cursor_key(run.updated_at),
                run.run_id.as_str().to_owned(),
            ]
        })
    }

    async fn scan_due_gates_sql(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<ApprovalGate>, StoreError> {
        let mut connection = self.begin_read().await?;
        let now = Self::database_now(&mut connection).await?;
        let query = "due_gates";
        let (limit, cutoff, last) = scan_page_context(&page, scope, query, now)?;
        let (last_expiry, last_run_id, last_gate_id) = if last.is_empty() {
            (i64::MIN, "", "")
        } else if last.len() == 3 {
            (
                Self::timestamp_from_cursor(&last[0])?,
                last[1].as_str(),
                last[2].as_str(),
            )
        } else {
            return Err(StoreError::InvalidField);
        };
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT ar.row_data
             FROM dagger_workflow_approval_gates AS g
             JOIN dagger_workflow_approval_rows AS ar
               ON ar.tenant_id = g.tenant_id AND ar.namespace = g.namespace
              AND ar.entity_id = g.run_id AND ar.sub_id = g.gate_id
             WHERE g.tenant_id = ? AND g.namespace = ?
               AND ar.tenant_id = ? AND ar.namespace = ?
               AND g.status = 'Pending' AND g.expires_at_ms <= ?
               AND (g.expires_at_ms > ?
                 OR (g.expires_at_ms = ? AND g.run_id > ?)
                 OR (g.expires_at_ms = ? AND g.run_id = ? AND g.gate_id > ?))
             ORDER BY g.expires_at_ms, g.run_id, g.gate_id
             LIMIT ?",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(cutoff.0)
        .bind(last_expiry)
        .bind(last_expiry)
        .bind(last_run_id)
        .bind(last_expiry)
        .bind(last_run_id)
        .bind(last_gate_id)
        .bind(i64::try_from(limit + 1).map_err(|_| StoreError::ArithmeticOverflow)?)
        .fetch_all(&mut *connection)
        .await
        .map_err(|_| StoreError::StorageUnavailable)?;
        Self::commit(connection).await?;
        let mut items = Vec::with_capacity(rows.len());
        for (data,) in rows {
            let gate: ApprovalGate = decode_row(&data)?;
            scoped(scope, &gate.scope, ())?;
            items.push(gate);
        }
        finish_scan_page(items, limit, scope, query, cutoff, |gate| {
            vec![
                timestamp_cursor_key(gate.expires_at),
                gate.run_id.as_str().to_owned(),
                gate.gate_id.as_str().to_owned(),
            ]
        })
    }

    async fn scan_due_run_lifetimes_sql(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError> {
        let mut connection = self.begin_read().await?;
        let now = Self::database_now(&mut connection).await?;
        let query = "due_run_lifetimes";
        let (limit, cutoff, last) = scan_page_context(&page, scope, query, now)?;
        let (last_deadline, last_run_id) = if last.is_empty() {
            (i64::MIN, "")
        } else if last.len() == 2 {
            (Self::timestamp_from_cursor(&last[0])?, last[1].as_str())
        } else {
            return Err(StoreError::InvalidField);
        };
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT rr.row_data
             FROM dagger_workflow_runs AS r
             JOIN dagger_workflow_run_rows AS rr
               ON rr.tenant_id = r.tenant_id AND rr.namespace = r.namespace
              AND rr.entity_id = r.run_id AND rr.sub_id = ''
             WHERE r.tenant_id = ? AND r.namespace = ?
               AND rr.tenant_id = ? AND rr.namespace = ?
               AND r.status NOT IN ('Succeeded', 'Failed', 'Cancelled', 'CorruptStorage')
               AND r.lifetime_deadline_at_ms <= ?
               AND (r.lifetime_deadline_at_ms > ?
                 OR (r.lifetime_deadline_at_ms = ? AND r.run_id > ?))
             ORDER BY r.lifetime_deadline_at_ms, r.run_id
             LIMIT ?",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(cutoff.0)
        .bind(last_deadline)
        .bind(last_deadline)
        .bind(last_run_id)
        .bind(i64::try_from(limit + 1).map_err(|_| StoreError::ArithmeticOverflow)?)
        .fetch_all(&mut *connection)
        .await
        .map_err(|_| StoreError::StorageUnavailable)?;
        Self::commit(connection).await?;
        let mut items = Vec::with_capacity(rows.len());
        for (data,) in rows {
            let run: WorkflowRun = decode_row(&data)?;
            scoped(scope, &run.scope, ())?;
            items.push(run);
        }
        finish_scan_page(items, limit, scope, query, cutoff, |run| {
            vec![
                timestamp_cursor_key(run.lifetime_deadline_at),
                run.run_id.as_str().to_owned(),
            ]
        })
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
    ($name:ident ( $($arg:ident : $arg_type:ty),* $(,)? ) -> $output:ty; $run_filter:expr $(; |$store:ident, $workflow_scope:ident| $hydrate:expr)?) => {
        async fn $name(
            &self,
            scope: &ExecutionScope,
            $($arg: $arg_type),*
        ) -> Result<$output, StoreError> {
            $(
                let $store = self;
                let $workflow_scope = scope;
                $hydrate;
            )?
            let mut connection = self.begin_immediate().await?;
            let now = match Self::database_now(&mut connection).await {
                Ok(value) => value,
                Err(error) => {
                    Self::rollback(&mut connection).await;
                    return Err(error);
                }
            };
            let run_filter: Option<Id> = $run_filter;
            let loaded = match Self::load_state(&mut connection, scope, run_filter.as_ref()).await {
                Ok(value) => value,
                Err(error) => {
                    Self::rollback(&mut connection).await;
                    return Err(error);
                }
            };
            let mut state = loaded.state.clone();
            self.restore_verified_bytes(&mut state, scope);
            let reducer = ReducerStore::from_state(Arc::new(DatabaseTimestamp(now)), state);
            match reducer.$name(scope, $($arg),*).await {
                Ok(value) => {
                    let state = reducer.snapshot();
                    self.remember_verified_bytes(&state);
                    if let Err(error) =
                        Self::persist_projections(
                            &mut connection,
                            scope,
                            &loaded,
                            &state,
                        )
                        .await
                    {
                        Self::rollback(&mut connection).await;
                        return Err(error);
                    }
                    if let Err(error) = Self::persist_authority(
                        &mut connection,
                        scope,
                        &loaded,
                        &state,
                    )
                    .await {
                        Self::rollback(&mut connection).await;
                        return Err(error);
                    }
                    #[cfg(feature = "conformance")]
                    let commit_fault = self.take_commit_fault();
                    #[cfg(feature = "conformance")]
                    if commit_fault == Some(SqliteCommitFault::BeforeCommit) {
                        Self::rollback(&mut connection).await;
                        return Err(StoreError::TransactionFailed);
                    }
                    Self::commit(connection).await?;
                    #[cfg(feature = "conformance")]
                    if commit_fault == Some(SqliteCommitFault::AfterCommit) {
                        return Err(StoreError::TransactionFailed);
                    }
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

macro_rules! sql_engine_command {
    ($name:ident ( $($arg:ident : $arg_type:ty),* $(,)? ) -> $output:ty) => {
        async fn $name(
            &self,
            scope: &ExecutionScope,
            $($arg: $arg_type),*
        ) -> Result<$output, StoreError> {
            let mut connection = self.begin_immediate().await?;
            let now = match Self::database_now(&mut connection).await {
                Ok(value) => value,
                Err(error) => {
                    Self::rollback(&mut connection).await;
                    return Err(error);
                }
            };
            let loaded = match Self::load_engine_claim_state(&mut connection, scope).await {
                Ok(value) => value,
                Err(error) => {
                    Self::rollback(&mut connection).await;
                    return Err(error);
                }
            };
            let reducer = ReducerStore::from_state(
                Arc::new(DatabaseTimestamp(now)),
                loaded.state.clone(),
            );
            match reducer.$name(scope, $($arg),*).await {
                Ok(value) => {
                    let state = reducer.snapshot();
                    if let Err(error) =
                        Self::persist_engine_claim_projection(&mut connection, scope, &state).await
                    {
                        Self::rollback(&mut connection).await;
                        return Err(error);
                    }
                    if let Err(error) =
                        Self::persist_authority(&mut connection, scope, &loaded, &state).await
                    {
                        Self::rollback(&mut connection).await;
                        return Err(error);
                    }
                    #[cfg(feature = "conformance")]
                    let commit_fault = self.take_commit_fault();
                    #[cfg(feature = "conformance")]
                    if commit_fault == Some(SqliteCommitFault::BeforeCommit) {
                        Self::rollback(&mut connection).await;
                        return Err(StoreError::TransactionFailed);
                    }
                    Self::commit(connection).await?;
                    #[cfg(feature = "conformance")]
                    if commit_fault == Some(SqliteCommitFault::AfterCommit) {
                        return Err(StoreError::TransactionFailed);
                    }
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
    ($name:ident ( $($arg:ident : $arg_type:ty),* $(,)? ) -> $output:ty; $run_filter:expr) => {
        async fn $name(
            &self,
            scope: &ExecutionScope,
            $($arg: $arg_type),*
        ) -> Result<$output, StoreError> {
            let mut connection = self.begin_read().await?;
            let now = Self::database_now(&mut connection).await?;
            let run_filter: Option<Id> = $run_filter;
            let mut state =
                Self::load_state(&mut connection, scope, run_filter.as_ref()).await?.state;
            self.restore_verified_bytes(&mut state, scope);
            let result = ReducerStore::from_state(Arc::new(DatabaseTimestamp(now)), state)
                .$name(scope, $($arg),*)
                .await;
            match result {
                Ok(value) => {
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

impl<C: Send + Sync, O: ObjectStore> WorkflowStore for SqliteWorkflowStore<C, O> {
    sql_command!(create_definition(command: CreateDefinition) -> DefinitionRecord; None);
    sql_command!(
        update_definition_metadata(command: UpdateDefinitionMetadata) -> DefinitionRecord;
        None
    );
    sql_command!(publish_revision(command: PublishRevision) -> WorkflowRevision; None);
    sql_engine_command!(acquire_engine_claim(instance_id: Id) -> AcquiredEngineClaim);
    sql_engine_command!(heartbeat_engine_claim(permit: &EnginePermit) -> EngineClaim);
    sql_engine_command!(release_engine_claim(permit: &EnginePermit) -> ());
    sql_command!(
        create_run(command: CreateRun) -> CommandReceipt;
        Some(command.run_id.clone());
        |store, workflow_scope| store.hydrate_revision_schemas(
            workflow_scope,
            &command.definition_id,
            &command.revision_hash,
            false,
        ).await?
    );
    sql_command!(start_run(command: StartRun) -> WorkflowRun; Some(command.run_id.clone()));
    sql_command!(
        suspend_incompatible(command: SuspendIncompatible) -> WorkflowRun;
        Some(command.run_id.clone())
    );
    sql_command!(
        resume_compatible(command: ResumeCompatible) -> WorkflowRun;
        Some(command.run_id.clone())
    );
    sql_command!(
        claim_node_attempt(command: ClaimNodeAttempt) -> ClaimNodeAttemptResult;
        Some(command.run_id.clone())
    );
    sql_command!(
        complete_attempt(command: CompleteAttempt) -> CompleteAttemptResult;
        Some(command.run_id.clone())
    );
    sql_command!(timeout_attempt(command: TimeoutAttempt) -> NodeAttempt; Some(command.run_id.clone()));
    sql_command!(
        recover_abandoned_attempts_for_run(command: RecoverAbandonedAttemptsForRun)
            -> Vec<NodeAttempt>;
        Some(command.run_id.clone())
    );
    sql_command!(
        release_retry(command: ReleaseRetry) -> NodeRun;
        Some(command.run_id.clone())
    );
    sql_command!(
        record_choice(command: RecordChoice) -> NodeRun;
        Some(command.run_id.clone())
    );
    sql_command!(
        expand_map(command: ExpandMap) -> NodeRun;
        Some(command.run_id.clone())
    );
    sql_command!(
        complete_map(command: CompleteMap) -> NodeRun;
        Some(command.run_id.clone());
        |store, workflow_scope| store.hydrate_run_action_schemas(
            workflow_scope,
            &command.run_id,
        ).await?
    );
    sql_command!(
        request_approval(command: RequestApproval) -> ApprovalGate;
        Some(command.run_id.clone())
    );
    sql_command!(
        decide_approval(command: DecideApproval) -> ApprovalGate;
        Some(command.run_id.clone())
    );
    sql_command!(
        expire_approval(command: ExpireApproval) -> ApprovalGate;
        Some(command.run_id.clone())
    );
    sql_command!(
        resolve_terminal_node(command: ResolveTerminalNode) -> WorkflowRun;
        Some(command.run_id.clone())
    );
    sql_command!(
        fail_contract(command: FailContract) -> WorkflowRun;
        Some(command.run_id.clone())
    );
    sql_command!(
        cancel_run(command: CancelRun) -> CommandReceipt;
        Some(command.run_id.clone())
    );
    sql_command!(
        expire_run_lifetime(command: ExpireRunLifetime) -> WorkflowRun;
        Some(command.run_id.clone())
    );
    sql_command!(
        mark_corrupt_storage(command: MarkCorruptStorage) -> WorkflowRun;
        Some(command.run_id.clone())
    );

    sql_read!(get_definition(definition_id: &Id) -> DefinitionRecord; None);
    sql_read!(
        get_revision(definition_id: &Id, revision_hash: &Digest) -> WorkflowRevision;
        None
    );
    sql_read!(get_run(run_id: &Id) -> WorkflowRunView; Some((*run_id).clone()));
    sql_read!(get_node(run_id: &Id, node_id: &NodeInstanceId) -> NodeRun; Some((*run_id).clone()));
    sql_read!(get_attempt(run_id: &Id, attempt_id: &Id) -> NodeAttempt; Some((*run_id).clone()));
    sql_read!(get_gate(run_id: &Id, gate_id: &Id) -> ApprovalGate; Some((*run_id).clone()));
    sql_read!(list_runs(page: PageRequest) -> Page<WorkflowRun>; None);
    sql_read!(list_nodes(run_id: &Id, page: PageRequest) -> Page<NodeRun>; Some((*run_id).clone()));
    sql_read!(
        list_events_after(run_id: &Id, page: EventPageRequest) -> Vec<WorkflowEvent>;
        Some((*run_id).clone())
    );
    async fn scan_ready_nodes(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError> {
        self.scan_nodes_sql(scope, page, "ready", "Ready", false)
            .await
    }

    async fn scan_budget_waiters(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError> {
        self.scan_nodes_sql(scope, page, "budget_waiters", "BudgetWaiting", false)
            .await
    }

    async fn scan_due_deadlines(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeAttempt>, StoreError> {
        self.scan_due_deadlines_sql(scope, page).await
    }

    async fn scan_due_retries(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<NodeRun>, StoreError> {
        self.scan_nodes_sql(scope, page, "due_retries", "RetryWaiting", true)
            .await
    }
    async fn scan_recovery_runs(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError> {
        self.scan_recovery_runs_sql(scope, page).await
    }

    async fn scan_compatibility_rechecks(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError> {
        self.scan_compatibility_rechecks_sql(scope, page).await
    }

    async fn scan_due_gates(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<ApprovalGate>, StoreError> {
        self.scan_due_gates_sql(scope, page).await
    }

    async fn scan_due_run_lifetimes(
        &self,
        scope: &ExecutionScope,
        page: PageRequest,
    ) -> Result<Page<WorkflowRun>, StoreError> {
        self.scan_due_run_lifetimes_sql(scope, page).await
    }
}
