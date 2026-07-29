#![cfg(feature = "sqlite")]

use dagger_workflow_core::action::{ActionOutcome, CompatibilityReport};
use dagger_workflow_core::approval::{
    canonical_human_approval_result, ApprovalDecision, ApprovalExpiryPolicy,
    AuthenticatedPrincipal, DecisionAuthorizationPolicy,
};
use dagger_workflow_core::artifact::ObjectStore;
use dagger_workflow_core::definition::{
    ActionReference, ApprovalGateConfig, BackoffPolicy, Binding, BindingSource, MapBinding,
    MapBindingSource, NodeDefinition, PublishableDefinition, RetryPolicy, TimeoutPolicy,
    WorkflowDefinition,
};
use dagger_workflow_core::engine::TestClock;
use dagger_workflow_core::ids::{
    map_child_id, map_expansion_digest, CostUnits, Id, MapChildIdentity, Timestamp,
    TopologicalRank, Version,
};
use dagger_workflow_core::memory::InMemoryObjectStore;
use dagger_workflow_core::run::RunLimits;
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::sqlite::{SqliteWorkflowStore, SCHEMA_VERSION};
use dagger_workflow_core::store::{
    CancelRun, ClaimNodeAttempt, ClaimNodeAttemptResult, CompleteAttempt, CompleteAttemptResult,
    CompleteMap, CompletionObjects, CreateDefinition, CreateRun, DecideApproval, ExpandMap,
    OrderedMapItem, PageRequest, PublishRevision, RequestApproval, ResolvedActionSchemas, StartRun,
    StoreError, TimeoutAttempt, WorkflowStore,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::Row;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

fn scope(tenant: &str) -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new(tenant).unwrap(),
        namespace: ScopeAtom::new("workflow").unwrap(),
    }
}

#[derive(Clone)]
struct ActionFixture {
    scope: ExecutionScope,
    principal: AuthenticatedPrincipal,
    revision_hash: dagger_workflow_core::ids::Digest,
    input: dagger_workflow_core::artifact::VerifiedObjectRef,
    evidence_digest: dagger_workflow_core::ids::Digest,
}

async fn open_file_store(
    path: &Path,
    clock: Arc<TestClock>,
    objects: Arc<InMemoryObjectStore<TestClock>>,
) -> SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>> {
    SqliteWorkflowStore::open(path, clock, objects)
        .await
        .unwrap()
}

async fn seed_action_definition(
    store: &SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>>,
    objects: &Arc<InMemoryObjectStore<TestClock>>,
    tenant: &str,
) -> ActionFixture {
    let workflow_scope = scope(tenant);
    let schema = objects
        .put(
            &workflow_scope,
            br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#,
            "application/json",
        )
        .await
        .unwrap();
    let principal = AuthenticatedPrincipal::mint(
        workflow_scope.clone(),
        format!("{tenant}-host"),
        Vec::new(),
        schema.digest().clone(),
    )
    .unwrap();
    let definition_id = Id::new("definition").unwrap();
    store
        .create_definition(
            &workflow_scope,
            CreateDefinition {
                definition_id: definition_id.clone(),
                display_name: "fixture".to_owned(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: definition_id.clone(),
        name: "fixture".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.digest().clone(),
        run_output_schema_digest: schema.digest().clone(),
        entry_node_id: Id::new("action").unwrap(),
        nodes: vec![
            NodeDefinition::Action {
                id: Id::new("action").unwrap(),
                action: ActionReference {
                    name: "fixture.action".to_owned(),
                    contract_version: "1".to_owned(),
                    input_schema_digest: schema.digest().clone(),
                    output_schema_digest: schema.digest().clone(),
                    compatible_implementation_requirement: schema.digest().clone(),
                },
                bindings: vec![Binding {
                    target: "/value".to_owned(),
                    source: BindingSource::RunInput {
                        pointer: "/value".to_owned(),
                    },
                }],
                retry: RetryPolicy {
                    max_attempts: 2,
                    backoff: BackoffPolicy::Fixed { delay_ms: 0 },
                },
                timeout: TimeoutPolicy { timeout_ms: 60_000 },
                declared_max_cost_units: CostUnits(1),
                next: vec![Id::new("succeed").unwrap()],
            },
            NodeDefinition::Succeed {
                id: Id::new("succeed").unwrap(),
                output: BindingSource::NodeOutput {
                    node_id: Id::new("action").unwrap(),
                    pointer: String::new(),
                },
            },
        ],
    };
    let canonical = objects
        .put(
            &workflow_scope,
            &serde_jcs::to_vec(&definition).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let mut ranks = BTreeMap::new();
    ranks.insert(Id::new("action").unwrap(), TopologicalRank(0));
    ranks.insert(Id::new("succeed").unwrap(), TopologicalRank(1));
    let mut action_schemas = BTreeMap::new();
    action_schemas.insert(
        "action".to_owned(),
        ResolvedActionSchemas {
            input_schema: schema.clone(),
            output_schema: schema.clone(),
        },
    );
    store
        .publish_revision(
            &workflow_scope,
            PublishRevision {
                definition_id,
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema.clone(),
                resolved_action_schema_objects: action_schemas,
                parsed_revision: PublishableDefinition {
                    definition,
                    topological_ranks: ranks,
                },
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let input = objects
        .put(&workflow_scope, br#"{"value":1}"#, "application/json")
        .await
        .unwrap();
    ActionFixture {
        scope: workflow_scope,
        principal,
        revision_hash: canonical.digest().clone(),
        input,
        evidence_digest: schema.digest().clone(),
    }
}

async fn seed_approval_definition(
    store: &SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>>,
    objects: &Arc<InMemoryObjectStore<TestClock>>,
    tenant: &str,
) -> ActionFixture {
    let workflow_scope = scope(tenant);
    let schema = objects
        .put(
            &workflow_scope,
            br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#,
            "application/json",
        )
        .await
        .unwrap();
    let principal = AuthenticatedPrincipal::mint(
        workflow_scope.clone(),
        "approver".to_owned(),
        Vec::new(),
        schema.digest().clone(),
    )
    .unwrap();
    let definition_id = Id::new("definition").unwrap();
    store
        .create_definition(
            &workflow_scope,
            CreateDefinition {
                definition_id: definition_id.clone(),
                display_name: "approval fixture".to_owned(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: definition_id.clone(),
        name: "approval-fixture".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.digest().clone(),
        run_output_schema_digest: schema.digest().clone(),
        entry_node_id: Id::new("approval").unwrap(),
        nodes: vec![
            NodeDefinition::Approval {
                id: Id::new("approval").unwrap(),
                request: BindingSource::RunInput {
                    pointer: String::new(),
                },
                gate: ApprovalGateConfig {
                    expires_after_ms: 100_000,
                    on_expiry: ApprovalExpiryPolicy::Reject,
                    authorization: DecisionAuthorizationPolicy {
                        allowed_principal_ids: vec!["approver".to_owned()],
                        allowed_role_ids: Vec::new(),
                    },
                },
                next: vec![Id::new("succeed").unwrap()],
            },
            NodeDefinition::Succeed {
                id: Id::new("succeed").unwrap(),
                output: BindingSource::NodeOutput {
                    node_id: Id::new("approval").unwrap(),
                    pointer: String::new(),
                },
            },
        ],
    };
    let canonical = objects
        .put(
            &workflow_scope,
            &serde_jcs::to_vec(&definition).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let mut ranks = BTreeMap::new();
    ranks.insert(Id::new("approval").unwrap(), TopologicalRank(0));
    ranks.insert(Id::new("succeed").unwrap(), TopologicalRank(1));
    store
        .publish_revision(
            &workflow_scope,
            PublishRevision {
                definition_id,
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema.clone(),
                resolved_action_schema_objects: BTreeMap::new(),
                parsed_revision: PublishableDefinition {
                    definition,
                    topological_ranks: ranks,
                },
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let input = objects
        .put(&workflow_scope, br#"{"value":1}"#, "application/json")
        .await
        .unwrap();
    ActionFixture {
        scope: workflow_scope,
        principal,
        revision_hash: canonical.digest().clone(),
        input,
        evidence_digest: schema.digest().clone(),
    }
}

fn create_run_command(fixture: &ActionFixture, run_id: &str) -> CreateRun {
    CreateRun {
        run_id: Id::new(run_id).unwrap(),
        definition_id: Id::new("definition").unwrap(),
        revision_hash: fixture.revision_hash.clone(),
        input: fixture.input.clone(),
        budget_limit: CostUnits(10),
        limits: RunLimits {
            max_dynamic_node_instances: 10,
            max_total_attempts: 10,
            max_total_events: 1_000,
            max_inline_json_bytes_per_value: 10_000,
            max_artifacts_per_attempt: 10,
            max_aggregate_object_bytes_per_run: 100_000,
            max_run_lifetime_ms: 100_000,
        },
        principal: fixture.principal.clone(),
        idempotency_token: format!("create-{run_id}-token-long-enough"),
    }
}

async fn raw_scope_snapshot(
    pool: &sqlx::SqlitePool,
    workflow_scope: &ExecutionScope,
) -> BTreeMap<String, Vec<String>> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name LIKE 'dagger_workflow_%'
         ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let mut snapshot = BTreeMap::new();
    for table in tables {
        let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
        if !columns.iter().any(|column| column == "tenant_id")
            || !columns.iter().any(|column| column == "namespace")
        {
            continue;
        }
        let json_columns = columns
            .iter()
            .map(|column| format!("quote(\"{column}\")"))
            .collect::<Vec<_>>()
            .join(", ");
        let order = columns
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let statement = format!(
            "SELECT json_array({json_columns}) FROM \"{table}\"
             WHERE tenant_id = ? AND namespace = ? ORDER BY {order}"
        );
        let rows = sqlx::query_scalar(&statement)
            .bind(workflow_scope.tenant_id.as_str())
            .bind(workflow_scope.namespace.as_str())
            .fetch_all(pool)
            .await
            .unwrap();
        snapshot.insert(table, rows);
    }
    snapshot
}

async fn raw_physical_scope_snapshot(
    pool: &sqlx::SqlitePool,
    workflow_scope: &ExecutionScope,
) -> BTreeMap<String, Vec<String>> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name LIKE 'dagger_workflow_%'
         ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let mut snapshot = BTreeMap::new();
    for table in tables {
        let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
        if !columns.iter().any(|column| column == "tenant_id")
            || !columns.iter().any(|column| column == "namespace")
        {
            continue;
        }
        let json_columns = std::iter::once("rowid".to_owned())
            .chain(columns.iter().map(|column| format!("quote(\"{column}\")")))
            .collect::<Vec<_>>()
            .join(", ");
        let order = columns
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let statement = format!(
            "SELECT json_array({json_columns}) FROM \"{table}\"
             WHERE tenant_id = ? AND namespace = ? ORDER BY {order}"
        );
        let rows = sqlx::query_scalar(&statement)
            .bind(workflow_scope.tenant_id.as_str())
            .bind(workflow_scope.namespace.as_str())
            .fetch_all(pool)
            .await
            .unwrap();
        snapshot.insert(table, rows);
    }
    snapshot
}

#[tokio::test]
async fn pool_injection_preserves_host_tables_and_applies_schema() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(":memory:")
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::query("CREATE TABLE host_jobs(id INTEGER PRIMARY KEY, label TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO host_jobs(label) VALUES ('keep-me')")
        .execute(&pool)
        .await
        .unwrap();

    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let store = SqliteWorkflowStore::from_pool(
        pool.clone(),
        clock.clone(),
        Arc::new(InMemoryObjectStore::new(clock)),
    )
    .await
    .unwrap();
    let label: String = sqlx::query_scalar("SELECT label FROM host_jobs WHERE id = 1")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(label, "keep-me");
    let version: i64 =
        sqlx::query_scalar("SELECT MAX(version) FROM dagger_workflow_schema_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name LIKE 'dagger_workflow_%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(table_count, 34);
}

#[tokio::test]
async fn standalone_open_reopens_a_converged_migration() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("workflow.sqlite");
    let first_clock = Arc::new(TestClock::new(Timestamp(10)));
    let objects = Arc::new(InMemoryObjectStore::new(first_clock.clone()));
    let first = SqliteWorkflowStore::open(&path, first_clock, objects.clone())
        .await
        .unwrap();
    first.pool().close().await;
    let reopened_clock = Arc::new(TestClock::new(Timestamp(20)));
    let reopened = SqliteWorkflowStore::open(&path, reopened_clock, objects)
        .await
        .unwrap();
    let versions: Vec<i64> = sqlx::query_scalar(
        "SELECT version FROM dagger_workflow_schema_migrations ORDER BY version",
    )
    .fetch_all(reopened.pool())
    .await
    .unwrap();
    assert_eq!(versions, vec![1]);
}

#[tokio::test]
async fn version_one_rejects_any_missing_table_or_index() {
    for missing_object in [
        "DROP TABLE dagger_workflow_invocation_rows",
        "DROP INDEX dagger_workflow_runs_lifetime_scan",
    ] {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        let clock = Arc::new(TestClock::new(Timestamp(0)));
        let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let store = SqliteWorkflowStore::from_pool(pool.clone(), clock.clone(), objects.clone())
            .await
            .unwrap();
        sqlx::query(missing_object)
            .execute(store.pool())
            .await
            .unwrap();
        assert!(matches!(
            SqliteWorkflowStore::from_pool(pool, clock, objects).await,
            Err(StoreError::TransactionFailed)
        ));
    }
}

#[tokio::test]
async fn aborted_migration_transaction_reopens_and_converges() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("killed-migration.sqlite");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE dagger_workflow_schema_migrations(
            version INTEGER PRIMARY KEY, applied_at_ms INTEGER NOT NULL,
            clock_offset_ms INTEGER NOT NULL DEFAULT 0
        ) STRICT",
    )
    .execute(&pool)
    .await
    .unwrap();
    {
        let mut interrupted = pool.begin().await.unwrap();
        sqlx::query("CREATE TABLE dagger_workflow_partial_kill(id INTEGER PRIMARY KEY) STRICT")
            .execute(&mut *interrupted)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO dagger_workflow_schema_migrations(version, applied_at_ms) VALUES (1, 0)",
        )
        .execute(&mut *interrupted)
        .await
        .unwrap();
        // Transaction drop simulates a kill after DDL but before the migration commit.
    }
    pool.close().await;

    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let reopened = SqliteWorkflowStore::open(
        &path,
        clock.clone(),
        Arc::new(InMemoryObjectStore::new(clock)),
    )
    .await
    .unwrap();
    let version: i64 =
        sqlx::query_scalar("SELECT MAX(version) FROM dagger_workflow_schema_migrations")
            .fetch_one(reopened.pool())
            .await
            .unwrap();
    let partial: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table'
         AND name='dagger_workflow_partial_kill'",
    )
    .fetch_one(reopened.pool())
    .await
    .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(partial, 0);
}

#[tokio::test]
async fn interrupted_sql_transaction_leaves_no_partial_command_effect() {
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let store = SqliteWorkflowStore::open_url(
        "sqlite::memory:",
        clock.clone(),
        Arc::new(InMemoryObjectStore::new(clock)),
    )
    .await
    .unwrap();
    {
        let mut transaction = store.pool().begin().await.unwrap();
        sqlx::query(
            "INSERT INTO dagger_workflow_runs
             (tenant_id, namespace, run_id, definition_id, revision_hash, status,
              version, event_seq, aggregate_object_bytes, total_attempts,
              budget_limit, budget_reserved, budget_spent, lifetime_deadline_at_ms,
              created_at_ms, updated_at_ms)
             VALUES ('tenant', 'workflow', 'run', 'definition', 'sha256:x', 'Pending',
                     1, 0, 0, 0, 1, 0, 0, 100, 0, 0)",
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO dagger_workflow_events
             (tenant_id, namespace, run_id, event_seq, batch_id, batch_index, batch_count,
              event_type, actor_kind, actor_id, occurred_at_ms, payload_json)
             VALUES ('tenant', 'workflow', 'run', 1, 'batch', 0, 1,
                     'RunCreated', 'Host', 'host', 0, '{}')",
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        // Dropping an uncommitted transaction models connection loss/process death.
    }
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dagger_workflow_runs")
        .fetch_one(store.pool())
        .await
        .unwrap();
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dagger_workflow_events")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!((runs, events), (0, 0));
}

#[tokio::test]
async fn gate_4_tenant_command_changes_only_expected_rows_in_its_own_slice() {
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = SqliteWorkflowStore::open_url("sqlite::memory:", clock.clone(), objects.clone())
        .await
        .unwrap();
    let fixture_a = seed_action_definition(&store, &objects, "tenant-a").await;
    let fixture_b = seed_action_definition(&store, &objects, "tenant-b").await;
    store
        .create_run(&fixture_a.scope, create_run_command(&fixture_a, "run-a"))
        .await
        .unwrap();
    store
        .create_run(&fixture_b.scope, create_run_command(&fixture_b, "run-b"))
        .await
        .unwrap();
    let claim_a = store
        .acquire_engine_claim(&fixture_a.scope, Id::new("engine-a").unwrap())
        .await
        .unwrap();
    store
        .acquire_engine_claim(&fixture_b.scope, Id::new("engine-b").unwrap())
        .await
        .unwrap();

    let before_a = raw_physical_scope_snapshot(store.pool(), &fixture_a.scope).await;
    let before_b = raw_physical_scope_snapshot(store.pool(), &fixture_b.scope).await;

    store
        .start_run(
            &fixture_a.scope,
            StartRun {
                permit: claim_a.permit,
                run_id: Id::new("run-a").unwrap(),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: fixture_a.evidence_digest.clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .unwrap();

    let after_a = raw_physical_scope_snapshot(store.pool(), &fixture_a.scope).await;
    let after_b = raw_physical_scope_snapshot(store.pool(), &fixture_b.scope).await;
    assert_eq!(after_b, before_b, "tenant B changed byte-for-byte");

    let changed_a = before_a
        .iter()
        .filter_map(|(table, before)| {
            (after_a.get(table) != Some(before)).then_some(table.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        changed_a,
        vec![
            "dagger_workflow_event_batch_rows",
            "dagger_workflow_event_rows",
            "dagger_workflow_events",
            "dagger_workflow_run_rows",
            "dagger_workflow_runs",
            "dagger_workflow_scope_counters",
        ]
    );
    assert_eq!(
        before_a["dagger_workflow_node_run_rows"], after_a["dagger_workflow_node_run_rows"],
        "ordinary start_run physically rewrote untouched node authority rows"
    );
    assert_eq!(
        before_a["dagger_workflow_nodes"], after_a["dagger_workflow_nodes"],
        "ordinary start_run changed untouched projection rowids/versions"
    );
}

#[tokio::test]
async fn canonical_revision_and_root_input_schema_are_authoritative() {
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = SqliteWorkflowStore::open_url("sqlite::memory:", clock, objects.clone())
        .await
        .unwrap();
    let fixture = seed_action_definition(&store, &objects, "canonical-authority").await;
    let canonical = objects
        .get(&fixture.scope, &fixture.revision_hash)
        .await
        .unwrap();
    let mut mismatched_definition = dagger_workflow_core::definition::parse_json_definition(
        std::str::from_utf8(&canonical.bytes).unwrap(),
    )
    .unwrap();
    mismatched_definition.name = "caller-supplied-definition-b".to_owned();
    let ranks =
        dagger_workflow_core::definition::canonical_topological_ranks(&mismatched_definition)
            .unwrap();
    let schema = objects
        .get(&fixture.scope, &fixture.evidence_digest)
        .await
        .unwrap()
        .reference;
    let before = raw_scope_snapshot(store.pool(), &fixture.scope).await;
    let mismatch = store
        .publish_revision(
            &fixture.scope,
            PublishRevision {
                definition_id: Id::new("definition").unwrap(),
                expected_definition_version: Version(2),
                canonical_definition: canonical.reference,
                run_input_schema: schema.clone(),
                run_output_schema: schema.clone(),
                resolved_action_schema_objects: BTreeMap::from([(
                    "action".to_owned(),
                    ResolvedActionSchemas {
                        input_schema: schema.clone(),
                        output_schema: schema,
                    },
                )]),
                parsed_revision: PublishableDefinition {
                    definition: mismatched_definition,
                    topological_ranks: ranks,
                },
                principal: fixture.principal.clone(),
            },
        )
        .await;
    assert!(matches!(mismatch, Err(StoreError::RevisionInvalid { .. })));
    assert_eq!(
        raw_scope_snapshot(store.pool(), &fixture.scope).await,
        before,
        "canonical/typed mismatch mutated publication state"
    );

    let invalid_input = objects
        .put(&fixture.scope, br#"{"other":1}"#, "application/json")
        .await
        .unwrap();
    let mut command = create_run_command(&fixture, "schema-rejected-run");
    command.input = invalid_input;
    assert!(matches!(
        store.create_run(&fixture.scope, command).await,
        Err(StoreError::ContractValidation { .. })
    ));
    assert!(matches!(
        store
            .get_run(&fixture.scope, &Id::new("schema-rejected-run").unwrap())
            .await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn complete_map_rehydrates_pinned_output_schema_after_restart() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("map-schema-restart.sqlite");
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = open_file_store(&path, clock.clone(), objects.clone()).await;
    let workflow_scope = scope("map-schema-restart");
    let schema = objects
        .put(
            &workflow_scope,
            br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#,
            "application/json",
        )
        .await
        .unwrap();
    let principal = AuthenticatedPrincipal::mint(
        workflow_scope.clone(),
        "map-host".to_owned(),
        Vec::new(),
        schema.digest().clone(),
    )
    .unwrap();
    let definition_id = Id::new("map-definition").unwrap();
    store
        .create_definition(
            &workflow_scope,
            CreateDefinition {
                definition_id: definition_id.clone(),
                display_name: "map".to_owned(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: definition_id.clone(),
        name: "map".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.digest().clone(),
        run_output_schema_digest: schema.digest().clone(),
        entry_node_id: Id::new("map").unwrap(),
        nodes: vec![
            NodeDefinition::Map {
                id: Id::new("map").unwrap(),
                items: BindingSource::Constant {
                    value: serde_json::json!([{"value": 1}]),
                },
                max_items: 1,
                max_concurrency: 1,
                action: ActionReference {
                    name: "map.action".to_owned(),
                    contract_version: "1".to_owned(),
                    input_schema_digest: schema.digest().clone(),
                    output_schema_digest: schema.digest().clone(),
                    compatible_implementation_requirement: schema.digest().clone(),
                },
                bindings: vec![MapBinding {
                    target: "/value".to_owned(),
                    source: MapBindingSource::MapItem {
                        pointer: "/value".to_owned(),
                    },
                }],
                retry: RetryPolicy {
                    max_attempts: 1,
                    backoff: BackoffPolicy::Fixed { delay_ms: 0 },
                },
                timeout: TimeoutPolicy { timeout_ms: 60_000 },
                declared_max_cost_units: CostUnits(1),
                next: vec![Id::new("succeed").unwrap()],
            },
            NodeDefinition::Succeed {
                id: Id::new("succeed").unwrap(),
                output: BindingSource::NodeOutput {
                    node_id: Id::new("map").unwrap(),
                    pointer: String::new(),
                },
            },
        ],
    };
    let canonical = objects
        .put(
            &workflow_scope,
            &serde_jcs::to_vec(&definition).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    store
        .publish_revision(
            &workflow_scope,
            PublishRevision {
                definition_id: definition_id.clone(),
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema.clone(),
                resolved_action_schema_objects: BTreeMap::from([(
                    "map/map_action".to_owned(),
                    ResolvedActionSchemas {
                        input_schema: schema.clone(),
                        output_schema: schema.clone(),
                    },
                )]),
                parsed_revision: PublishableDefinition {
                    topological_ranks:
                        dagger_workflow_core::definition::canonical_topological_ranks(&definition)
                            .unwrap(),
                    definition,
                },
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let run_input = objects
        .put(&workflow_scope, br#"{"value":1}"#, "application/json")
        .await
        .unwrap();
    store
        .create_run(
            &workflow_scope,
            CreateRun {
                run_id: Id::new("map-run").unwrap(),
                definition_id,
                revision_hash: canonical.digest().clone(),
                input: run_input,
                budget_limit: CostUnits(10),
                limits: RunLimits {
                    max_dynamic_node_instances: 10,
                    max_total_attempts: 10,
                    max_total_events: 1_000,
                    max_inline_json_bytes_per_value: 10_000,
                    max_artifacts_per_attempt: 10,
                    max_aggregate_object_bytes_per_run: 100_000,
                    max_run_lifetime_ms: 100_000,
                },
                principal,
                idempotency_token: "map-create-token-long-enough".to_owned(),
            },
        )
        .await
        .unwrap();
    let claim = store
        .acquire_engine_claim(&workflow_scope, Id::new("map-engine").unwrap())
        .await
        .unwrap();
    store
        .start_run(
            &workflow_scope,
            StartRun {
                permit: claim.permit.clone(),
                run_id: Id::new("map-run").unwrap(),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: schema.digest().clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .unwrap();
    let item = objects
        .put(&workflow_scope, br#"{"value":1}"#, "application/json")
        .await
        .unwrap();
    let expansion_input = objects
        .put(&workflow_scope, br#"[{"value":1}]"#, "application/json")
        .await
        .unwrap();
    let run_id = Id::new("map-run").unwrap();
    let map_id = Id::new("map").unwrap();
    let child_id = map_child_id(&run_id, &map_id, 0, item.digest());
    let identities = vec![MapChildIdentity {
        item_index: 0,
        item_digest: item.digest().clone(),
        child_id: child_id.clone(),
    }];
    let map = store
        .get_node(&workflow_scope, &run_id, &map_id)
        .await
        .unwrap();
    store
        .expand_map(
            &workflow_scope,
            ExpandMap {
                permit: claim.permit.clone(),
                run_id: run_id.clone(),
                map_node_id: map_id.clone(),
                expected_node_version: map.version,
                input: expansion_input,
                ordered_items: vec![OrderedMapItem {
                    index: 0,
                    item_digest: item.digest().clone(),
                    child_id: child_id.clone(),
                }],
                expansion_digest: map_expansion_digest(&identities),
            },
        )
        .await
        .unwrap();
    let child = store
        .get_node(&workflow_scope, &run_id, &child_id)
        .await
        .unwrap();
    let credential = match store
        .claim_node_attempt(
            &workflow_scope,
            ClaimNodeAttempt {
                permit: claim.permit.clone(),
                run_id: run_id.clone(),
                node_id: child_id.clone(),
                expected_node_version: child.version,
                attempt_id: Id::new("map-attempt").unwrap(),
                worker_id: Id::new("map-worker").unwrap(),
                bound_input: item.clone(),
                binding_derivation_digest: schema.digest().clone(),
            },
        )
        .await
        .unwrap()
    {
        ClaimNodeAttemptResult::Claimed {
            completion_credential,
            ..
        } => completion_credential,
        _ => panic!("map child was not claimed"),
    };
    store
        .complete_attempt(
            &workflow_scope,
            CompleteAttempt {
                completion_credential: credential,
                run_id: run_id.clone(),
                node_id: child_id,
                attempt_id: Id::new("map-attempt").unwrap(),
                submitted_outcome: ActionOutcome::Success {
                    output: serde_json::json!({"value": 1}),
                    artifacts: Vec::new(),
                    actual_cost_units: CostUnits(1),
                    diagnostics: None,
                },
                objects: CompletionObjects {
                    output: Some(item),
                    artifacts: Vec::new(),
                    diagnostics: None,
                },
            },
        )
        .await
        .unwrap();
    let aggregate = objects
        .put(&workflow_scope, br#"[{"value":1}]"#, "application/json")
        .await
        .unwrap();
    store.pool().close().await;
    drop(store);
    let restarted = open_file_store(&path, clock, objects).await;
    let parent = restarted
        .get_node(&workflow_scope, &run_id, &map_id)
        .await
        .unwrap();
    let completed = restarted
        .complete_map(
            &workflow_scope,
            CompleteMap {
                permit: claim.permit,
                run_id,
                map_node_id: map_id,
                expected_node_version: parent.version,
                aggregate,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        completed.status,
        dagger_workflow_core::run::NodeState::Succeeded
    );
}

#[tokio::test]
async fn command_reads_authority_rows_not_projection_rows() {
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let store = SqliteWorkflowStore::open_url(
        "sqlite::memory:",
        clock.clone(),
        Arc::new(InMemoryObjectStore::new(clock)),
    )
    .await
    .unwrap();
    let workflow_scope = scope("authority-tenant");
    let acquired = store
        .acquire_engine_claim(&workflow_scope, Id::new("real-engine").unwrap())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE dagger_workflow_engine_claims SET instance_id = 'projection-poison'
         WHERE tenant_id = ? AND namespace = ? AND control_plane_id = 'scheduler'",
    )
    .bind(workflow_scope.tenant_id.as_str())
    .bind(workflow_scope.namespace.as_str())
    .execute(store.pool())
    .await
    .unwrap();

    let heartbeat = store
        .heartbeat_engine_claim(&workflow_scope, &acquired.permit)
        .await
        .unwrap();
    assert_eq!(heartbeat.instance_id.as_str(), "real-engine");
}

#[tokio::test]
async fn authority_row_version_overflow_is_a_cas_conflict_and_rolls_back() {
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let store = SqliteWorkflowStore::open_url(
        "sqlite::memory:",
        clock.clone(),
        Arc::new(InMemoryObjectStore::new(clock)),
    )
    .await
    .unwrap();
    let workflow_scope = scope("cas-tenant");
    let acquired = store
        .acquire_engine_claim(&workflow_scope, Id::new("cas-engine").unwrap())
        .await
        .unwrap();
    let projection_before: (i64, i64) = sqlx::query_as(
        "SELECT heartbeat_at_ms, version FROM dagger_workflow_engine_claims
         WHERE tenant_id = ? AND namespace = ? AND control_plane_id = 'scheduler'",
    )
    .bind(workflow_scope.tenant_id.as_str())
    .bind(workflow_scope.namespace.as_str())
    .fetch_one(store.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE dagger_workflow_engine_claim_rows SET version = 9223372036854775807
         WHERE tenant_id = ? AND namespace = ?
           AND entity_id = 'scheduler' AND sub_id = ''",
    )
    .bind(workflow_scope.tenant_id.as_str())
    .bind(workflow_scope.namespace.as_str())
    .execute(store.pool())
    .await
    .unwrap();

    assert_eq!(
        store
            .heartbeat_engine_claim(&workflow_scope, &acquired.permit)
            .await,
        Err(dagger_workflow_core::store::StoreError::CasConflict)
    );
    let projection_after: (i64, i64) = sqlx::query_as(
        "SELECT heartbeat_at_ms, version FROM dagger_workflow_engine_claims
         WHERE tenant_id = ? AND namespace = ? AND control_plane_id = 'scheduler'",
    )
    .bind(workflow_scope.tenant_id.as_str())
    .bind(workflow_scope.namespace.as_str())
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(projection_after, projection_before);
}

#[tokio::test]
async fn reopen_recovers_a_started_attempt_from_sql_state_only() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("attempt-recovery.sqlite");
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let workflow_scope = scope("recovery-tenant");
    let store = SqliteWorkflowStore::open(&path, clock.clone(), objects.clone())
        .await
        .unwrap();
    let schema = objects
        .put(
            &workflow_scope,
            br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#,
            "application/json",
        )
        .await
        .unwrap();
    let principal = AuthenticatedPrincipal::mint(
        workflow_scope.clone(),
        "recovery-host".to_owned(),
        Vec::new(),
        schema.digest().clone(),
    )
    .unwrap();
    store
        .create_definition(
            &workflow_scope,
            CreateDefinition {
                definition_id: Id::new("recovery-definition").unwrap(),
                display_name: "recovery".to_owned(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: Id::new("recovery-definition").unwrap(),
        name: "recovery".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.digest().clone(),
        run_output_schema_digest: schema.digest().clone(),
        entry_node_id: Id::new("action").unwrap(),
        nodes: vec![
            NodeDefinition::Action {
                id: Id::new("action").unwrap(),
                action: ActionReference {
                    name: "recovery.action".to_owned(),
                    contract_version: "1".to_owned(),
                    input_schema_digest: schema.digest().clone(),
                    output_schema_digest: schema.digest().clone(),
                    compatible_implementation_requirement: schema.digest().clone(),
                },
                bindings: vec![Binding {
                    target: "/value".to_owned(),
                    source: BindingSource::RunInput {
                        pointer: "/value".to_owned(),
                    },
                }],
                retry: RetryPolicy {
                    max_attempts: 2,
                    backoff: BackoffPolicy::Fixed { delay_ms: 0 },
                },
                timeout: TimeoutPolicy { timeout_ms: 60_000 },
                declared_max_cost_units: CostUnits(1),
                next: vec![Id::new("succeed").unwrap()],
            },
            NodeDefinition::Succeed {
                id: Id::new("succeed").unwrap(),
                output: BindingSource::NodeOutput {
                    node_id: Id::new("action").unwrap(),
                    pointer: String::new(),
                },
            },
        ],
    };
    let canonical = objects
        .put(
            &workflow_scope,
            &serde_jcs::to_vec(&definition).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let mut ranks = BTreeMap::new();
    ranks.insert(Id::new("action").unwrap(), TopologicalRank(0));
    ranks.insert(Id::new("succeed").unwrap(), TopologicalRank(1));
    let mut action_schemas = BTreeMap::new();
    action_schemas.insert(
        "action".to_owned(),
        ResolvedActionSchemas {
            input_schema: schema.clone(),
            output_schema: schema.clone(),
        },
    );
    store
        .publish_revision(
            &workflow_scope,
            PublishRevision {
                definition_id: Id::new("recovery-definition").unwrap(),
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema.clone(),
                resolved_action_schema_objects: action_schemas,
                parsed_revision: PublishableDefinition {
                    definition,
                    topological_ranks: ranks,
                },
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let input = objects
        .put(&workflow_scope, br#"{"value":1}"#, "application/json")
        .await
        .unwrap();
    store
        .create_run(
            &workflow_scope,
            CreateRun {
                run_id: Id::new("recovery-run").unwrap(),
                definition_id: Id::new("recovery-definition").unwrap(),
                revision_hash: canonical.digest().clone(),
                input: input.clone(),
                budget_limit: CostUnits(10),
                limits: dagger_workflow_core::run::RunLimits {
                    max_dynamic_node_instances: 10,
                    max_total_attempts: 10,
                    max_total_events: 1_000,
                    max_inline_json_bytes_per_value: 10_000,
                    max_artifacts_per_attempt: 10,
                    max_aggregate_object_bytes_per_run: 100_000,
                    max_run_lifetime_ms: 100_000,
                },
                principal,
                idempotency_token: "recovery-create-token".to_owned(),
            },
        )
        .await
        .unwrap();
    let durable_data: String = sqlx::query_scalar(
        "SELECT row_data FROM dagger_workflow_run_rows
         WHERE tenant_id = ? AND namespace = ? AND entity_id = ? AND sub_id = ''",
    )
    .bind(workflow_scope.tenant_id.as_str())
    .bind(workflow_scope.namespace.as_str())
    .bind("recovery-run")
    .fetch_one(store.pool())
    .await
    .unwrap();
    // The run input is a deliberately recognizable object payload. SQLite
    // persists its digest/reference only; the bytes remain in ObjectStore.
    assert!(!durable_data.contains(r#"{\"value\":1}"#));
    assert!(!durable_data.contains("verified_object_bytes"));
    let acquired = store
        .acquire_engine_claim(&workflow_scope, Id::new("engine-before-crash").unwrap())
        .await
        .unwrap();
    store
        .start_run(
            &workflow_scope,
            StartRun {
                permit: acquired.permit.clone(),
                run_id: Id::new("recovery-run").unwrap(),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: schema.digest().clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .unwrap();
    let node = store
        .get_node(
            &workflow_scope,
            &Id::new("recovery-run").unwrap(),
            &Id::new("action").unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .claim_node_attempt(
                &workflow_scope,
                ClaimNodeAttempt {
                    permit: acquired.permit,
                    run_id: Id::new("recovery-run").unwrap(),
                    node_id: Id::new("action").unwrap(),
                    expected_node_version: node.version,
                    attempt_id: Id::new("started-at-crash").unwrap(),
                    worker_id: Id::new("worker").unwrap(),
                    bound_input: input,
                    binding_derivation_digest: schema.digest().clone(),
                },
            )
            .await
            .unwrap(),
        ClaimNodeAttemptResult::Claimed { .. }
    ));

    store.pool().close().await;
    drop(store);

    let reopened = SqliteWorkflowStore::open(&path, clock.clone(), objects.clone())
        .await
        .unwrap();
    let started = reopened
        .get_attempt(
            &workflow_scope,
            &Id::new("recovery-run").unwrap(),
            &Id::new("started-at-crash").unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        started.status,
        dagger_workflow_core::run::AttemptState::Started
    );
    let persisted_status: String = sqlx::query_scalar(
        "SELECT status FROM dagger_workflow_attempts
         WHERE tenant_id = ? AND namespace = ? AND run_id = ? AND attempt_id = ?",
    )
    .bind(workflow_scope.tenant_id.as_str())
    .bind(workflow_scope.namespace.as_str())
    .bind("recovery-run")
    .bind("started-at-crash")
    .fetch_one(reopened.pool())
    .await
    .unwrap();
    assert_eq!(persisted_status, "Started");
    reopened.advance_database_clock_ms(20_000).await.unwrap();
    reopened
        .acquire_engine_claim(&workflow_scope, Id::new("engine-after-crash").unwrap())
        .await
        .unwrap();
    let recovery = reopened
        .scan_recovery_runs(
            &workflow_scope,
            PageRequest {
                cursor: None,
                page_size: 10,
            },
        )
        .await
        .unwrap();
    assert_eq!(recovery.items.len(), 1);
    assert_eq!(recovery.items[0].run_id.as_str(), "recovery-run");
}

#[tokio::test]
async fn phase_b_scheduler_keyset_pagination_is_deterministic_and_complete() {
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = SqliteWorkflowStore::open_url("sqlite::memory:", clock, objects.clone())
        .await
        .unwrap();
    let fixture = seed_action_definition(&store, &objects, "pagination-tenant").await;
    for index in 0..37 {
        let run_id = format!("paged-run-{index:03}");
        store
            .create_run(&fixture.scope, create_run_command(&fixture, &run_id))
            .await
            .unwrap();
    }

    async fn traverse(
        store: &SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>>,
        workflow_scope: &ExecutionScope,
        page_size: u16,
    ) -> Vec<String> {
        let mut cursor = None;
        let mut ids = Vec::new();
        loop {
            let page = store
                .scan_compatibility_rechecks(workflow_scope, PageRequest { cursor, page_size })
                .await
                .unwrap();
            ids.extend(
                page.items
                    .into_iter()
                    .map(|run| run.run_id.as_str().to_owned()),
            );
            cursor = page.next_cursor;
            if cursor.is_none() {
                return ids;
            }
        }
    }

    let first = traverse(&store, &fixture.scope, 7).await;
    let second = traverse(&store, &fixture.scope, 7).await;
    let unpaged = traverse(&store, &fixture.scope, 1000).await;
    assert_eq!(first, second);
    assert_eq!(first, unpaged);
    assert_eq!(first.len(), 37);
}

#[tokio::test]
async fn scheduler_multi_status_scans_are_index_ordered_without_temp_sorts() {
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let store = SqliteWorkflowStore::open_url(
        "sqlite::memory:",
        clock.clone(),
        Arc::new(InMemoryObjectStore::new(clock)),
    )
    .await
    .unwrap();
    for (name, ordered_column, expected_index) in [
        (
            "compatibility",
            "updated_at_ms",
            "dagger_workflow_runs_compatibility_scan",
        ),
        (
            "lifetime",
            "lifetime_deadline_at_ms",
            "dagger_workflow_runs_lifetime_scan",
        ),
    ] {
        let branches = ["Pending", "Running", "BlockedIncompatible"]
            .into_iter()
            .map(|status| {
                format!(
                    "SELECT rr.row_data, r.{ordered_column}, r.run_id
                     FROM dagger_workflow_runs AS r
                     JOIN dagger_workflow_run_rows AS rr
                       ON rr.tenant_id = r.tenant_id AND rr.namespace = r.namespace
                      AND rr.entity_id = r.run_id AND rr.sub_id = ''
                     WHERE r.tenant_id = 'tenant' AND r.namespace = 'workflow'
                       AND rr.tenant_id = 'tenant' AND rr.namespace = 'workflow'
                       AND r.status = '{status}' AND r.{ordered_column} <= 100
                       AND (r.{ordered_column} > -100
                         OR (r.{ordered_column} = -100 AND r.run_id > ''))"
                )
            })
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        let statement = format!(
            "EXPLAIN QUERY PLAN {branches}
             ORDER BY {ordered_column}, run_id LIMIT 11"
        );
        let details: Vec<String> = sqlx::query(&statement)
            .fetch_all(store.pool())
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get("detail"))
            .collect();
        assert!(
            details
                .iter()
                .all(|detail| !detail.contains("USE TEMP B-TREE FOR ORDER BY")),
            "{name} scheduler scan uses a temporary ORDER BY sort:\n{}",
            details.join("\n")
        );
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("MERGE (UNION ALL)"))
                && details
                    .iter()
                    .filter(|detail| detail.contains(expected_index))
                    .count()
                    == 3,
            "{name} scheduler scan did not merge three index-ordered status ranges:\n{}",
            details.join("\n")
        );
    }
}

#[cfg(feature = "conformance")]
#[tokio::test]
async fn gate_6_real_command_fault_windows_are_atomic_and_replay_safe() {
    use dagger_workflow_core::sqlite::SqliteCommitFault;

    let directory = TempDir::new().unwrap();
    let path = directory.path().join("commit-windows.sqlite");
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = open_file_store(&path, clock.clone(), objects.clone()).await;
    let fixture = seed_action_definition(&store, &objects, "fault-tenant").await;
    let before_fault = raw_scope_snapshot(store.pool(), &fixture.scope).await;

    store.inject_commit_fault_once(SqliteCommitFault::BeforeCommit);
    assert_eq!(
        store
            .create_run(
                &fixture.scope,
                create_run_command(&fixture, "pre-commit-run")
            )
            .await,
        Err(StoreError::TransactionFailed)
    );
    store.pool().close().await;
    drop(store);

    let reopened = open_file_store(&path, clock.clone(), objects.clone()).await;
    assert_eq!(
        reopened
            .get_run(&fixture.scope, &Id::new("pre-commit-run").unwrap())
            .await,
        Err(StoreError::NotFound)
    );
    assert_eq!(
        raw_scope_snapshot(reopened.pool(), &fixture.scope).await,
        before_fault
    );

    reopened.inject_commit_fault_once(SqliteCommitFault::AfterCommit);
    assert_eq!(
        reopened
            .create_run(
                &fixture.scope,
                create_run_command(&fixture, "post-commit-run")
            )
            .await,
        Err(StoreError::TransactionFailed)
    );
    reopened.pool().close().await;
    drop(reopened);

    let replayed = open_file_store(&path, clock, objects).await;
    let committed = replayed
        .get_run(&fixture.scope, &Id::new("post-commit-run").unwrap())
        .await
        .unwrap();
    assert_eq!(committed.run.run_id.as_str(), "post-commit-run");
    let first_replay = replayed
        .create_run(
            &fixture.scope,
            create_run_command(&fixture, "post-commit-run"),
        )
        .await
        .unwrap();
    let second_replay = replayed
        .create_run(
            &fixture.scope,
            create_run_command(&fixture, "post-commit-run"),
        )
        .await
        .unwrap();
    assert_eq!(first_replay, second_replay);
}

#[tokio::test]
async fn gate_7_two_connection_races_preserve_all_domain_fences() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("races.sqlite");
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let first = open_file_store(&path, clock.clone(), objects.clone()).await;
    let second = open_file_store(&path, clock.clone(), objects.clone()).await;

    let fixture = seed_action_definition(&first, &objects, "race-tenant").await;
    first
        .create_run(&fixture.scope, create_run_command(&fixture, "race-run"))
        .await
        .unwrap();
    let claim = first
        .acquire_engine_claim(&fixture.scope, Id::new("race-engine").unwrap())
        .await
        .unwrap();
    first
        .start_run(
            &fixture.scope,
            StartRun {
                permit: claim.permit.clone(),
                run_id: Id::new("race-run").unwrap(),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: fixture.evidence_digest.clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .unwrap();
    let node = first
        .get_node(
            &fixture.scope,
            &Id::new("race-run").unwrap(),
            &Id::new("action").unwrap(),
        )
        .await
        .unwrap();
    let claim_command = |attempt_id: &str| ClaimNodeAttempt {
        permit: claim.permit.clone(),
        run_id: Id::new("race-run").unwrap(),
        node_id: Id::new("action").unwrap(),
        expected_node_version: node.version,
        attempt_id: Id::new(attempt_id).unwrap(),
        worker_id: Id::new("worker").unwrap(),
        bound_input: fixture.input.clone(),
        binding_derivation_digest: fixture.evidence_digest.clone(),
    };
    let (left, right) = tokio::join!(
        first.claim_node_attempt(&fixture.scope, claim_command("attempt-left")),
        second.claim_node_attempt(&fixture.scope, claim_command("attempt-right"))
    );
    let (credential, attempt_id, loser) = match (left, right) {
        (
            Ok(ClaimNodeAttemptResult::Claimed {
                completion_credential,
                ..
            }),
            loser,
        ) => (completion_credential, "attempt-left", loser),
        (
            loser,
            Ok(ClaimNodeAttemptResult::Claimed {
                completion_credential,
                ..
            }),
        ) => (completion_credential, "attempt-right", loser),
        _ => panic!("exactly one attempt claim must win"),
    };
    assert!(matches!(loser, Err(StoreError::CasConflict)));
    let budget_after_reserve: (i64, i64) = sqlx::query_as(
        "SELECT budget_reserved, budget_spent FROM dagger_workflow_runs
         WHERE tenant_id = ? AND namespace = ? AND run_id = ?",
    )
    .bind(fixture.scope.tenant_id.as_str())
    .bind(fixture.scope.namespace.as_str())
    .bind("race-run")
    .fetch_one(first.pool())
    .await
    .unwrap();
    assert_eq!(budget_after_reserve, (1, 0));

    first.advance_database_clock_ms(120_000).await.unwrap();
    let timeout_claim = first
        .acquire_engine_claim(&fixture.scope, Id::new("timeout-engine").unwrap())
        .await
        .unwrap();
    let output = objects
        .put(&fixture.scope, br#"{"value":2}"#, "application/json")
        .await
        .unwrap();
    let complete = CompleteAttempt {
        completion_credential: credential,
        run_id: Id::new("race-run").unwrap(),
        node_id: Id::new("action").unwrap(),
        attempt_id: Id::new(attempt_id).unwrap(),
        submitted_outcome: ActionOutcome::Success {
            output: serde_json::json!({"value": 2}),
            artifacts: Vec::new(),
            actual_cost_units: CostUnits(1),
            diagnostics: None,
        },
        objects: CompletionObjects {
            output: Some(output),
            artifacts: Vec::new(),
            diagnostics: None,
        },
    };
    let timeout = TimeoutAttempt {
        permit: timeout_claim.permit,
        run_id: Id::new("race-run").unwrap(),
        node_id: Id::new("action").unwrap(),
        attempt_id: Id::new(attempt_id).unwrap(),
    };
    let (completion_result, timeout_result) = tokio::join!(
        first.complete_attempt(&fixture.scope, complete),
        second.timeout_attempt(&fixture.scope, timeout)
    );
    assert!(
        matches!(
            completion_result,
            Ok(CompleteAttemptResult::TimedOutAndStaleRecorded(_))
                | Ok(CompleteAttemptResult::StaleRecorded(_))
        ),
        "completion must observe the timeout fence"
    );
    assert!(
        timeout_result.is_ok() || matches!(timeout_result, Err(StoreError::AttemptFenced)),
        "unexpected timeout race result: {timeout_result:?}"
    );
    let ledger_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM dagger_workflow_budget_ledger
         WHERE tenant_id = ? AND namespace = ? AND run_id = ?",
    )
    .bind(fixture.scope.tenant_id.as_str())
    .bind(fixture.scope.namespace.as_str())
    .bind("race-run")
    .fetch_one(first.pool())
    .await
    .unwrap();
    assert_eq!(ledger_rows, 2, "one reserve and one settlement");
    let event_counts: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(DISTINCT event_seq) FROM dagger_workflow_events
         WHERE tenant_id = ? AND namespace = ? AND run_id = ?",
    )
    .bind(fixture.scope.tenant_id.as_str())
    .bind(fixture.scope.namespace.as_str())
    .bind("race-run")
    .fetch_one(first.pool())
    .await
    .unwrap();
    assert_eq!(event_counts.0, event_counts.1, "event allocation collided");

    let approval = seed_approval_definition(&first, &objects, "approval-race-tenant").await;
    first
        .create_run(
            &approval.scope,
            create_run_command(&approval, "approval-race-run"),
        )
        .await
        .unwrap();
    let approval_claim = first
        .acquire_engine_claim(&approval.scope, Id::new("approval-engine").unwrap())
        .await
        .unwrap();
    first
        .start_run(
            &approval.scope,
            StartRun {
                permit: approval_claim.permit.clone(),
                run_id: Id::new("approval-race-run").unwrap(),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: approval.evidence_digest.clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .unwrap();
    let approval_node = first
        .get_node(
            &approval.scope,
            &Id::new("approval-race-run").unwrap(),
            &Id::new("approval").unwrap(),
        )
        .await
        .unwrap();
    let request = objects
        .put(&approval.scope, br#"{"request":true}"#, "application/json")
        .await
        .unwrap();
    let gate = first
        .request_approval(
            &approval.scope,
            RequestApproval {
                permit: approval_claim.permit,
                run_id: Id::new("approval-race-run").unwrap(),
                node_id: Id::new("approval").unwrap(),
                expected_node_version: approval_node.version,
                gate_id: Id::new("approval-gate").unwrap(),
                request,
            },
        )
        .await
        .unwrap();
    let observed_run = first
        .get_run(&approval.scope, &Id::new("approval-race-run").unwrap())
        .await
        .unwrap()
        .run;
    let approval_output = objects
        .put(
            &approval.scope,
            &canonical_human_approval_result(None, &approval.principal),
            "application/json",
        )
        .await
        .unwrap();
    let approve = DecideApproval {
        run_id: Id::new("approval-race-run").unwrap(),
        gate_id: gate.gate_id.clone(),
        expected_run_version: observed_run.version,
        expected_gate_version: gate.version,
        decision: ApprovalDecision::Approve,
        decision_payload: None,
        approval_output: Some(approval_output),
        principal: approval.principal.clone(),
    };
    let reject = DecideApproval {
        run_id: Id::new("approval-race-run").unwrap(),
        gate_id: gate.gate_id,
        expected_run_version: observed_run.version,
        expected_gate_version: gate.version,
        decision: ApprovalDecision::Reject,
        decision_payload: None,
        approval_output: None,
        principal: approval.principal.clone(),
    };
    let (approve_result, reject_result) = tokio::join!(
        first.decide_approval(&approval.scope, approve),
        second.decide_approval(&approval.scope, reject)
    );
    assert!(
        (approve_result.is_ok()
            && matches!(reject_result, Err(StoreError::ApprovalAlreadyResolved)))
            || (reject_result.is_ok()
                && matches!(approve_result, Err(StoreError::IllegalTransition))),
        "approval race: approve={approve_result:?}, reject={reject_result:?}"
    );
}

#[tokio::test]
async fn gate_8_restart_replay_and_abandoned_attempt_recovery_bytes_are_deterministic() {
    use dagger_workflow_core::store::RecoverAbandonedAttemptsForRun;

    let directory = TempDir::new().unwrap();
    let path = directory.path().join("restart-replay.sqlite");
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = open_file_store(&path, clock.clone(), objects.clone()).await;
    let fixture = seed_action_definition(&store, &objects, "restart-tenant").await;

    let create_receipt = store
        .create_run(&fixture.scope, create_run_command(&fixture, "replay-run"))
        .await
        .unwrap();
    store.pool().close().await;
    drop(store);
    let reopened = open_file_store(&path, clock.clone(), objects.clone()).await;
    let create_replay = reopened
        .create_run(&fixture.scope, create_run_command(&fixture, "replay-run"))
        .await
        .unwrap();
    assert_eq!(create_replay, create_receipt);
    let observed = reopened
        .get_run(&fixture.scope, &Id::new("replay-run").unwrap())
        .await
        .unwrap()
        .run;
    let cancel = || CancelRun {
        run_id: Id::new("replay-run").unwrap(),
        expected_run_version: observed.version,
        expected_pending_gate_versions: Vec::new(),
        principal: fixture.principal.clone(),
        reason_code: "host.cancelled".to_owned(),
        idempotency_token: "cancel-replay-run-token-long-enough".to_owned(),
    };
    let cancel_receipt = reopened.cancel_run(&fixture.scope, cancel()).await.unwrap();
    reopened.pool().close().await;
    drop(reopened);
    let restarted = open_file_store(&path, clock.clone(), objects.clone()).await;
    let cancel_replay = restarted
        .cancel_run(&fixture.scope, cancel())
        .await
        .unwrap();
    assert_eq!(cancel_replay, cancel_receipt);

    restarted
        .create_run(
            &fixture.scope,
            create_run_command(&fixture, "recovery-batch-run"),
        )
        .await
        .unwrap();
    let first_engine = restarted
        .acquire_engine_claim(&fixture.scope, Id::new("engine-before-crash").unwrap())
        .await
        .unwrap();
    restarted
        .start_run(
            &fixture.scope,
            StartRun {
                permit: first_engine.permit.clone(),
                run_id: Id::new("recovery-batch-run").unwrap(),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: fixture.evidence_digest.clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .unwrap();
    let node = restarted
        .get_node(
            &fixture.scope,
            &Id::new("recovery-batch-run").unwrap(),
            &Id::new("action").unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        restarted
            .claim_node_attempt(
                &fixture.scope,
                ClaimNodeAttempt {
                    permit: first_engine.permit.clone(),
                    run_id: Id::new("recovery-batch-run").unwrap(),
                    node_id: Id::new("action").unwrap(),
                    expected_node_version: node.version,
                    attempt_id: Id::new("attempt-1").unwrap(),
                    worker_id: Id::new("worker").unwrap(),
                    bound_input: fixture.input.clone(),
                    binding_derivation_digest: fixture.evidence_digest.clone(),
                },
            )
            .await
            .unwrap(),
        ClaimNodeAttemptResult::Claimed { .. }
    ));
    restarted
        .create_run(
            &fixture.scope,
            create_run_command(&fixture, "recovery-batch-run-2"),
        )
        .await
        .unwrap();
    restarted
        .start_run(
            &fixture.scope,
            StartRun {
                permit: first_engine.permit.clone(),
                run_id: Id::new("recovery-batch-run-2").unwrap(),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: fixture.evidence_digest.clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .unwrap();
    let second_node = restarted
        .get_node(
            &fixture.scope,
            &Id::new("recovery-batch-run-2").unwrap(),
            &Id::new("action").unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        restarted
            .claim_node_attempt(
                &fixture.scope,
                ClaimNodeAttempt {
                    permit: first_engine.permit,
                    run_id: Id::new("recovery-batch-run-2").unwrap(),
                    node_id: Id::new("action").unwrap(),
                    expected_node_version: second_node.version,
                    attempt_id: Id::new("attempt-2").unwrap(),
                    worker_id: Id::new("worker").unwrap(),
                    bound_input: fixture.input.clone(),
                    binding_derivation_digest: fixture.evidence_digest.clone(),
                },
            )
            .await
            .unwrap(),
        ClaimNodeAttemptResult::Claimed { .. }
    ));

    restarted.pool().close().await;
    drop(restarted);
    let after_crash = open_file_store(&path, clock.clone(), objects.clone()).await;
    after_crash.advance_database_clock_ms(20_000).await.unwrap();
    let recovery_engine = after_crash
        .acquire_engine_claim(&fixture.scope, Id::new("engine-after-crash").unwrap())
        .await
        .unwrap();
    let recovered = after_crash
        .recover_abandoned_attempts_for_run(
            &fixture.scope,
            RecoverAbandonedAttemptsForRun {
                permit: recovery_engine.permit.clone(),
                run_id: Id::new("recovery-batch-run").unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(recovered[0].attempt_id.as_str(), "attempt-1");
    assert!(recovered.iter().all(|attempt| {
        attempt.status == dagger_workflow_core::run::AttemptState::UnknownOutcome
    }));
    let recovered_second = after_crash
        .recover_abandoned_attempts_for_run(
            &fixture.scope,
            RecoverAbandonedAttemptsForRun {
                permit: recovery_engine.permit.clone(),
                run_id: Id::new("recovery-batch-run-2").unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(recovered_second[0].attempt_id.as_str(), "attempt-2");
    let canonical_rows: Vec<String> = sqlx::query_scalar(
        "SELECT row_data FROM dagger_workflow_event_rows
         WHERE tenant_id = ? AND namespace = ?
           AND entity_id IN ('recovery-batch-run', 'recovery-batch-run-2')
         UNION ALL
         SELECT row_data FROM dagger_workflow_event_batch_rows
         WHERE tenant_id = ? AND namespace = ?
           AND entity_id IN ('recovery-batch-run', 'recovery-batch-run-2')
         ORDER BY row_data",
    )
    .bind(fixture.scope.tenant_id.as_str())
    .bind(fixture.scope.namespace.as_str())
    .bind(fixture.scope.tenant_id.as_str())
    .bind(fixture.scope.namespace.as_str())
    .fetch_all(after_crash.pool())
    .await
    .unwrap();
    assert!(canonical_rows.iter().all(|row| {
        let value: serde_json::Value = serde_json::from_str(row).unwrap();
        serde_jcs::to_string(&value).unwrap() == *row
    }));
    let recovered_snapshot = raw_scope_snapshot(after_crash.pool(), &fixture.scope).await;
    after_crash.pool().close().await;
    drop(after_crash);
    let replayed = open_file_store(&path, clock, objects).await;
    assert!(replayed
        .recover_abandoned_attempts_for_run(
            &fixture.scope,
            RecoverAbandonedAttemptsForRun {
                permit: recovery_engine.permit.clone(),
                run_id: Id::new("recovery-batch-run").unwrap(),
            },
        )
        .await
        .unwrap()
        .is_empty());
    assert!(replayed
        .recover_abandoned_attempts_for_run(
            &fixture.scope,
            RecoverAbandonedAttemptsForRun {
                permit: recovery_engine.permit,
                run_id: Id::new("recovery-batch-run-2").unwrap(),
            },
        )
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        raw_scope_snapshot(replayed.pool(), &fixture.scope).await,
        recovered_snapshot,
        "recovery replay changed persisted event or batch bytes"
    );
}

#[test]
fn gate_9_every_sqlite_domain_statement_is_statically_tenant_scoped() {
    fn top_level_sql(statement: &str) -> String {
        let mut result = String::with_capacity(statement.len());
        let mut depth = 0_u32;
        let mut quoted = false;
        for character in statement.chars() {
            match character {
                '\'' => {
                    quoted = !quoted;
                    if depth == 0 {
                        result.push(character);
                    }
                }
                '(' if !quoted => {
                    depth += 1;
                    result.push(' ');
                }
                ')' if !quoted => {
                    depth = depth.saturating_sub(1);
                    result.push(' ');
                }
                _ if depth == 0 => result.push(character),
                _ => result.push(' '),
            }
        }
        result.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn string_literals(source: &str) -> Vec<String> {
        let bytes = source.as_bytes();
        let mut literals = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'r' {
                let mut hashes = 0;
                while index + 1 + hashes < bytes.len() && bytes[index + 1 + hashes] == b'#' {
                    hashes += 1;
                }
                if index + 1 + hashes < bytes.len() && bytes[index + 1 + hashes] == b'"' {
                    let start = index + 2 + hashes;
                    let mut end = start;
                    while end < bytes.len() {
                        if bytes[end] == b'"'
                            && (0..hashes).all(|offset| {
                                end + 1 + offset < bytes.len() && bytes[end + 1 + offset] == b'#'
                            })
                        {
                            literals.push(source[start..end].to_owned());
                            index = end + 1 + hashes;
                            break;
                        }
                        end += 1;
                    }
                    continue;
                }
            }
            if bytes[index] == b'"' {
                let start = index + 1;
                let mut end = start;
                let mut escaped = false;
                while end < bytes.len() {
                    if bytes[end] == b'"' && !escaped {
                        literals.push(source[start..end].to_owned());
                        index = end + 1;
                        break;
                    }
                    escaped = bytes[end] == b'\\' && !escaped;
                    if bytes[end] != b'\\' {
                        escaped = false;
                    }
                    end += 1;
                }
                continue;
            }
            index += 1;
        }
        literals
    }

    let sqlite_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sqlite");
    let mut violations = Vec::new();
    for entry in std::fs::read_dir(sqlite_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        for statement in string_literals(&source) {
            let normalized = statement.split_whitespace().collect::<Vec<_>>().join(" ");
            let lower = normalized.to_ascii_lowercase();
            let is_sql = [
                "select ", "insert ", "update ", "delete ", "create ", "alter ",
            ]
            .iter()
            .any(|keyword| lower.starts_with(keyword));
            let touches_domain = lower.contains("dagger_workflow_") || lower.contains("{table}");
            if !is_sql || !touches_domain {
                continue;
            }
            let schema_ddl = lower.starts_with("create ") || lower.starts_with("alter ");
            let migration_bookkeeping = lower.contains("dagger_workflow_schema_migrations")
                && !lower
                    .replace("dagger_workflow_schema_migrations", "")
                    .contains("dagger_workflow_");
            if schema_ddl || migration_bookkeeping {
                continue;
            }
            let top_level = top_level_sql(&lower);
            let scoped = if lower.starts_with("insert ") {
                let target_columns = lower
                    .split_once(" values ")
                    .or_else(|| lower.split_once(" select "))
                    .map_or(lower.as_str(), |(prefix, _)| prefix);
                target_columns.contains("tenant_id") && target_columns.contains("namespace")
            } else if lower.starts_with("update ") || lower.starts_with("delete ") {
                top_level
                    .split_once(" where ")
                    .is_some_and(|(_, predicate)| {
                        predicate.contains("tenant_id") && predicate.contains("namespace")
                    })
            } else {
                top_level.contains("tenant_id") && top_level.contains("namespace")
            };
            if !scoped {
                violations.push(format!(
                    "{}: {}",
                    path.file_name().unwrap().to_string_lossy(),
                    normalized
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "tenant-free domain SQL is not allowlisted:\n{}",
        violations.join("\n")
    );
}

#[tokio::test]
async fn gate_10_identical_command_cost_is_non_linear_in_historical_runs() {
    async fn open_cost_store(
        path: &Path,
        clock: Arc<TestClock>,
        objects: Arc<InMemoryObjectStore<TestClock>>,
    ) -> SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        SqliteWorkflowStore::from_pool(pool, clock, objects)
            .await
            .unwrap()
    }

    async fn seed_completed_history(
        store: &SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>>,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        tenant: &str,
        count: usize,
    ) -> (
        ActionFixture,
        dagger_workflow_core::store::AcquiredEngineClaim,
    ) {
        let fixture = seed_action_definition(store, objects, tenant).await;
        sqlx::query(
            "WITH RECURSIVE history(index_value) AS (
                 SELECT 0 UNION ALL
                 SELECT index_value + 1 FROM history WHERE index_value + 1 < ?
             )
             INSERT INTO dagger_workflow_runs(
                 tenant_id, namespace, run_id, definition_id, revision_hash, status,
                 version, event_seq, aggregate_object_bytes, total_attempts,
                 budget_limit, budget_reserved, budget_spent, lifetime_deadline_at_ms,
                 created_at_ms, updated_at_ms
             )
             SELECT ?, ?, printf('history-%04d', index_value), 'historical-definition',
                    'sha256:historical', 'Succeeded', 1, 1, 0, 0, 0, 0, 0, 0, 0, 0
             FROM history",
        )
        .bind(count as i64)
        .bind(fixture.scope.tenant_id.as_str())
        .bind(fixture.scope.namespace.as_str())
        .execute(store.pool())
        .await
        .unwrap();
        sqlx::query(
            "WITH RECURSIVE history(index_value) AS (
                 SELECT 0 UNION ALL
                 SELECT index_value + 1 FROM history WHERE index_value + 1 < ?
             ), template(row_data) AS (
                 SELECT row_data FROM dagger_workflow_object_rows
                 WHERE tenant_id = ? AND namespace = ? LIMIT 1
             )
             INSERT INTO dagger_workflow_object_rows(
                 tenant_id, namespace, entity_id, sub_id, row_data, version
             )
             SELECT ?, ?, printf('sha256:%064x', index_value + 1000000), '',
                    json_set(template.row_data, '$.digest',
                             printf('sha256:%064x', index_value + 1000000)),
                    1
             FROM history CROSS JOIN template",
        )
        .bind(count as i64)
        .bind(fixture.scope.tenant_id.as_str())
        .bind(fixture.scope.namespace.as_str())
        .bind(fixture.scope.tenant_id.as_str())
        .bind(fixture.scope.namespace.as_str())
        .execute(store.pool())
        .await
        .unwrap();
        let claim = store
            .acquire_engine_claim(&fixture.scope, Id::new("cost-engine").unwrap())
            .await
            .unwrap();
        let completed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM dagger_workflow_runs
             WHERE tenant_id = ? AND namespace = ? AND status = 'Succeeded'",
        )
        .bind(fixture.scope.tenant_id.as_str())
        .bind(fixture.scope.namespace.as_str())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(completed, count as i64);
        (fixture, claim)
    }

    async fn measure_ordinary_start_vm_steps(
        store: &SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>>,
        fixture: &ActionFixture,
        claim: &dagger_workflow_core::store::AcquiredEngineClaim,
    ) -> u64 {
        store
            .create_run(&fixture.scope, create_run_command(fixture, "measured-run"))
            .await
            .unwrap();
        let steps = Arc::new(AtomicU64::new(0));
        let mut connection = store.pool().acquire().await.unwrap();
        connection
            .lock_handle()
            .await
            .unwrap()
            .set_progress_handler(1, {
                let steps = steps.clone();
                move || {
                    steps.fetch_add(1, Ordering::Relaxed);
                    true
                }
            });
        drop(connection);
        store
            .start_run(
                &fixture.scope,
                StartRun {
                    permit: claim.permit.clone(),
                    run_id: Id::new("measured-run").unwrap(),
                    compatibility_evidence: CompatibilityReport {
                        evidence_digest: fixture.evidence_digest.clone(),
                        incompatible_reference_locations: Vec::new(),
                        evidence: Vec::new(),
                    },
                },
            )
            .await
            .unwrap();
        steps.load(Ordering::Relaxed)
    }

    let small_directory = TempDir::new().unwrap();
    let large_directory = TempDir::new().unwrap();
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let small_objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let large_objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let small = open_cost_store(
        &small_directory.path().join("small.sqlite"),
        clock.clone(),
        small_objects.clone(),
    )
    .await;
    let large = open_cost_store(
        &large_directory.path().join("large.sqlite"),
        clock,
        large_objects.clone(),
    )
    .await;
    let (small_fixture, small_claim) =
        seed_completed_history(&small, &small_objects, "small-history", 100).await;
    let (large_fixture, large_claim) =
        seed_completed_history(&large, &large_objects, "large-history", 2_000).await;

    let small_steps = measure_ordinary_start_vm_steps(&small, &small_fixture, &small_claim).await;
    let large_steps = measure_ordinary_start_vm_steps(&large, &large_fixture, &large_claim).await;
    let bound = small_steps.saturating_mul(2).saturating_add(2_000);
    assert!(
        large_steps <= bound,
        "2,000-run ordinary start used {large_steps} SQLite VM steps and scaled with \
         completed history; 100-run ordinary start used {small_steps}, bound {bound}"
    );
}

#[cfg(feature = "conformance")]
#[test]
fn all_forty_conformance_fixtures_pass_unchanged() {
    struct Adapter {
        _directory: TempDir,
        clock: Arc<TestClock>,
        store: SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>>,
        objects: Arc<InMemoryObjectStore<TestClock>>,
    }

    impl Adapter {
        fn build() -> Self {
            std::thread::spawn(|| {
                let runtime = tokio::runtime::Runtime::new().unwrap();
                runtime.block_on(async {
                    let directory = TempDir::new().unwrap();
                    let clock = Arc::new(TestClock::new(Timestamp(0)));
                    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
                    let store = SqliteWorkflowStore::open(
                        directory.path().join("conformance.sqlite"),
                        clock.clone(),
                        objects.clone(),
                    )
                    .await
                    .unwrap();
                    Self {
                        _directory: directory,
                        store,
                        objects,
                        clock,
                    }
                })
            })
            .join()
            .unwrap()
        }
    }

    impl dagger_workflow_core::conformance::ConformanceAdapter for Adapter {
        type Store = SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>>;
        type Objects = InMemoryObjectStore<TestClock>;

        fn store(&self) -> &Self::Store {
            &self.store
        }

        fn objects(&self) -> &Self::Objects {
            self.objects.as_ref()
        }

        fn advance_clock_ms(&self, milliseconds: i64) {
            self.clock.advance_ms(milliseconds).unwrap();
            std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        tokio::runtime::Runtime::new()
                            .unwrap()
                            .block_on(self.store.advance_database_clock_ms(milliseconds))
                            .unwrap();
                    })
                    .join()
                    .unwrap();
            });
        }

        fn object_records(
            &self,
            scope: &ExecutionScope,
        ) -> Vec<dagger_workflow_core::artifact::ObjectRecord> {
            std::thread::scope(|thread_scope| {
                thread_scope
                    .spawn(|| {
                        tokio::runtime::Runtime::new()
                            .unwrap()
                            .block_on(self.store.object_records(scope))
                            .unwrap()
                    })
                    .join()
                    .unwrap()
            })
        }

        fn fresh(&self) -> Self {
            Self::build()
        }
    }

    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let adapter = Adapter::build();
                let results = dagger_workflow_core::conformance::run_conformance(
                    &adapter,
                    &scope("tenant-a"),
                    &scope("tenant-b"),
                )
                .await;
                assert_eq!(results.len(), 40);
                assert!(
                    results.iter().all(|result| result.passed()),
                    "{:?}",
                    results
                        .iter()
                        .filter(|result| !result.passed())
                        .collect::<Vec<_>>()
                );
            });
        })
        .unwrap()
        .join()
        .unwrap();
}
