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
use dagger_workflow_core::run::{NodeState, RunLimits, RunState, SkipReason};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::sqlite::{
    SqliteWorkflowStore, SCHEDULER_COMPATIBILITY_SCAN_SQL, SCHEDULER_DEADLINES_SCAN_SQL,
    SCHEDULER_GATES_SCAN_SQL, SCHEDULER_LIFETIMES_SCAN_SQL, SCHEDULER_NODES_SCAN_SQL,
    SCHEDULER_RECOVERY_SCAN_SQL, SCHEMA_VERSION,
};
use dagger_workflow_core::store::{
    CancelRun, ClaimNodeAttempt, ClaimNodeAttemptResult, CompleteAttempt, CompleteAttemptResult,
    CompleteMap, CompletionObjects, CreateDefinition, CreateRun, DecideApproval, ExpandMap,
    OrderedMapItem, PageRequest, PublishRevision, RequestApproval, ResolveTerminalNode,
    ResolvedActionSchemas, StartRun, StoreError, TimeoutAttempt, UpdateDefinitionMetadata,
    WorkflowStore,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::Row;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
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

#[tokio::test]
async fn publish_revision_reuses_existing_schema_artifact_refs() {
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = SqliteWorkflowStore::open_url("sqlite::memory:", clock, objects.clone())
        .await
        .unwrap();
    let fixture = seed_action_definition(&store, &objects, "reused-schema").await;
    let schema = objects
        .get(&fixture.scope, &fixture.evidence_digest)
        .await
        .unwrap()
        .reference;
    let definition_id = Id::new("definition").unwrap();
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: definition_id.clone(),
        name: "fixture revision two".to_owned(),
        description: "reuses the first revision schemas".to_owned(),
        run_input_schema_digest: schema.digest().clone(),
        run_output_schema_digest: schema.digest().clone(),
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
            &fixture.scope,
            &serde_jcs::to_vec(&definition).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let ranks = BTreeMap::from([
        (Id::new("action").unwrap(), TopologicalRank(0)),
        (Id::new("succeed").unwrap(), TopologicalRank(1)),
    ]);

    let revision = store
        .publish_revision(
            &fixture.scope,
            PublishRevision {
                definition_id,
                expected_definition_version: Version(2),
                canonical_definition: canonical.clone(),
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
                    definition,
                    topological_ranks: ranks,
                },
                principal: fixture.principal,
            },
        )
        .await
        .unwrap();

    assert_eq!(revision.revision_hash, *canonical.digest());
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

#[tokio::test]
async fn permanent_action_failure_skips_missing_output_dependent_in_sqlite() {
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = SqliteWorkflowStore::open_url("sqlite::memory:", clock, objects.clone())
        .await
        .unwrap();
    let fixture = seed_action_definition(&store, &objects, "permanent-skip").await;
    let run_id = Id::new("permanent-skip-run").unwrap();
    store
        .create_run(
            &fixture.scope,
            create_run_command(&fixture, run_id.as_str()),
        )
        .await
        .unwrap();
    let engine_claim = store
        .acquire_engine_claim(&fixture.scope, Id::new("permanent-skip-engine").unwrap())
        .await
        .unwrap();
    store
        .start_run(
            &fixture.scope,
            StartRun {
                permit: engine_claim.permit.clone(),
                run_id: run_id.clone(),
                compatibility_evidence: CompatibilityReport {
                    evidence_digest: fixture.evidence_digest.clone(),
                    incompatible_reference_locations: Vec::new(),
                    evidence: Vec::new(),
                },
            },
        )
        .await
        .unwrap();
    let action_id = Id::new("action").unwrap();
    let action = store
        .get_node(&fixture.scope, &run_id, &action_id)
        .await
        .unwrap();
    let completion_credential = match store
        .claim_node_attempt(
            &fixture.scope,
            ClaimNodeAttempt {
                permit: engine_claim.permit,
                run_id: run_id.clone(),
                node_id: action_id.clone(),
                expected_node_version: action.version,
                attempt_id: Id::new("permanent-skip-attempt").unwrap(),
                worker_id: Id::new("permanent-skip-worker").unwrap(),
                bound_input: fixture.input.clone(),
                binding_derivation_digest: fixture.evidence_digest.clone(),
            },
        )
        .await
        .unwrap()
    {
        ClaimNodeAttemptResult::Claimed {
            completion_credential,
            ..
        } => completion_credential,
        _ => panic!("expected claim"),
    };
    let result = store
        .complete_attempt(
            &fixture.scope,
            CompleteAttempt {
                completion_credential,
                run_id: run_id.clone(),
                node_id: action_id,
                attempt_id: Id::new("permanent-skip-attempt").unwrap(),
                submitted_outcome: ActionOutcome::permanent(
                    "fixture.permanent".to_owned(),
                    "expected permanent failure".to_owned(),
                    None,
                    CostUnits(1),
                )
                .unwrap(),
                objects: CompletionObjects {
                    output: None,
                    artifacts: Vec::new(),
                    diagnostics: None,
                },
            },
        )
        .await
        .unwrap();
    assert!(matches!(result, CompleteAttemptResult::TerminalRun(_)));
    assert_eq!(
        store
            .get_run(&fixture.scope, &run_id)
            .await
            .unwrap()
            .run
            .status,
        RunState::Failed
    );
    let succeed = store
        .get_node(&fixture.scope, &run_id, &Id::new("succeed").unwrap())
        .await
        .unwrap();
    assert_eq!(succeed.status, NodeState::Skipped);
    assert_eq!(succeed.skip_reason, Some(SkipReason::SkippedUpstreamFailed));
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
    let directory = TempDir::new().unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(directory.path().join("injected.sqlite"))
                .create_if_missing(true)
                .foreign_keys(false)
                .busy_timeout(Duration::from_millis(1)),
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
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(store.pool())
        .await
        .unwrap();
    let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(store.pool())
        .await
        .unwrap();
    let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(foreign_keys, 1);
    assert_eq!(busy_timeout, 5_000);
    assert_eq!(journal_mode, "wal");
    assert_eq!(version, SCHEMA_VERSION);
    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name LIKE 'dagger_workflow_%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(table_count, 36);
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
    assert_eq!(versions, vec![1, 2]);
}

#[tokio::test]
async fn version_one_rejects_missing_or_semantically_corrupt_schema_objects() {
    for corruption in [
        vec!["DROP TABLE dagger_workflow_invocation_rows"],
        vec!["DROP INDEX dagger_workflow_runs_lifetime_scan"],
        vec![
            "DROP TABLE dagger_workflow_invocation_rows",
            "CREATE TABLE dagger_workflow_invocation_rows (
                tenant_id TEXT NOT NULL, namespace TEXT NOT NULL, entity_id TEXT NOT NULL,
                sub_id TEXT NOT NULL, row_data TEXT NOT NULL,
                version INTEGER NOT NULL CHECK(version > 0)
            )",
        ],
        vec![
            "DROP INDEX dagger_workflow_runs_lifetime_scan",
            "CREATE INDEX dagger_workflow_runs_lifetime_scan
             ON dagger_workflow_runs(
                 namespace, tenant_id, status, lifetime_deadline_at_ms, run_id
             )",
        ],
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
        for statement in corruption {
            sqlx::query(statement).execute(store.pool()).await.unwrap();
        }
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
    for (name, statement, expected_index, expected_uses) in [
        (
            "ready nodes",
            SCHEDULER_NODES_SCAN_SQL,
            "dagger_workflow_nodes_scheduler_scan",
            1,
        ),
        (
            "budget waiters",
            SCHEDULER_NODES_SCAN_SQL,
            "dagger_workflow_nodes_scheduler_scan",
            1,
        ),
        (
            "deadlines",
            SCHEDULER_DEADLINES_SCAN_SQL,
            "dagger_workflow_attempts_deadline_scan",
            1,
        ),
        (
            "retries",
            SCHEDULER_NODES_SCAN_SQL,
            "dagger_workflow_nodes_scheduler_scan",
            1,
        ),
        (
            "recovery",
            SCHEDULER_RECOVERY_SCAN_SQL,
            "dagger_workflow_attempts_recovery_scan",
            1,
        ),
        (
            "gate expiry",
            SCHEDULER_GATES_SCAN_SQL,
            "dagger_workflow_gates_expiry_scan",
            1,
        ),
        (
            "lifetime",
            SCHEDULER_LIFETIMES_SCAN_SQL,
            "dagger_workflow_runs_lifetime_scan",
            3,
        ),
        (
            "compatibility",
            SCHEDULER_COMPATIBILITY_SCAN_SQL,
            "dagger_workflow_runs_compatibility_scan",
            3,
        ),
    ] {
        let explain = format!("EXPLAIN QUERY PLAN {statement}");
        let rows = match name {
            "ready nodes" | "budget waiters" | "retries" => sqlx::query(&explain)
                .bind("tenant")
                .bind("workflow")
                .bind("tenant")
                .bind("workflow")
                .bind("tenant")
                .bind("workflow")
                .bind(if name == "ready nodes" {
                    "Ready"
                } else if name == "budget waiters" {
                    "BudgetWaiting"
                } else {
                    "RetryWaiting"
                })
                .bind(100_i64)
                .bind(i64::from(name == "retries"))
                .bind(100_i64)
                .bind("")
                .bind("")
                .bind("")
                .bind(11_i64)
                .fetch_all(store.pool())
                .await
                .unwrap(),
            "deadlines" => sqlx::query(&explain)
                .bind("tenant")
                .bind("workflow")
                .bind("tenant")
                .bind("workflow")
                .bind(100_i64)
                .bind(100_i64)
                .bind(-100_i64)
                .bind(-100_i64)
                .bind("")
                .bind(-100_i64)
                .bind("")
                .bind("")
                .bind(-100_i64)
                .bind("")
                .bind("")
                .bind("")
                .bind(11_i64)
                .fetch_all(store.pool())
                .await
                .unwrap(),
            "recovery" => sqlx::query(&explain)
                .bind("tenant")
                .bind("workflow")
                .bind("tenant")
                .bind("workflow")
                .bind(2_i64)
                .bind(100_i64)
                .bind("")
                .bind(11_i64)
                .fetch_all(store.pool())
                .await
                .unwrap(),
            "gate expiry" => sqlx::query(&explain)
                .bind("tenant")
                .bind("workflow")
                .bind("tenant")
                .bind("workflow")
                .bind(100_i64)
                .bind(-100_i64)
                .bind(-100_i64)
                .bind("")
                .bind(-100_i64)
                .bind("")
                .bind("")
                .bind(11_i64)
                .fetch_all(store.pool())
                .await
                .unwrap(),
            "lifetime" | "compatibility" => sqlx::query(&explain)
                .bind("tenant")
                .bind("workflow")
                .bind("tenant")
                .bind("workflow")
                .bind(100_i64)
                .bind(-100_i64)
                .bind(-100_i64)
                .bind("")
                .bind(11_i64)
                .fetch_all(store.pool())
                .await
                .unwrap(),
            _ => unreachable!(),
        };
        let details: Vec<String> = rows.into_iter().map(|row| row.get("detail")).collect();
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
                .filter(|detail| detail.contains(expected_index))
                .count()
                >= expected_uses,
            "{name} scheduler scan did not use its scoped ordering index \
             {expected_index} {expected_uses} time(s):\n{}",
            details.join("\n")
        );
    }
}

#[derive(Default)]
struct SqliteWriteLoadMetrics {
    latencies: Vec<Duration>,
    claims: u64,
    completions: u64,
}

async fn measure_sqlite_write<T>(
    metrics: &Arc<Mutex<SqliteWriteLoadMetrics>>,
    write: impl Future<Output = Result<T, StoreError>>,
) -> Result<T, StoreError> {
    let started = Instant::now();
    let result = write.await;
    metrics
        .lock()
        .expect("load metrics lock poisoned")
        .latencies
        .push(started.elapsed());
    result
}

fn load_compatibility(fixture: &ActionFixture) -> CompatibilityReport {
    CompatibilityReport {
        evidence_digest: fixture.evidence_digest.clone(),
        incompatible_reference_locations: Vec::new(),
        evidence: Vec::new(),
    }
}

#[derive(Clone, Copy)]
enum SqliteWriteLoadRole {
    Claim,
    Complete,
    Cancel,
    Heartbeat,
}

/// 30-second contention proof; run with `make sqlite-write-load`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long-running SQLite contention proof"]
async fn sqlite_write_path_load() {
    const WRITERS: usize = 16;
    const LOAD_DURATION: Duration = Duration::from_secs(30);

    let directory = TempDir::new().unwrap();
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let pool = SqlitePoolOptions::new()
        .max_connections(WRITERS as u32)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(directory.path().join("write-load.sqlite"))
                .create_if_missing(true)
                .foreign_keys(false)
                .busy_timeout(Duration::from_millis(1)),
        )
        .await
        .unwrap();
    let store = Arc::new(
        SqliteWorkflowStore::from_pool(pool, clock.clone(), objects.clone())
            .await
            .unwrap(),
    );
    let fixture = Arc::new(seed_action_definition(&store, &objects, "write-load").await);
    let permit = store
        .acquire_engine_claim(&fixture.scope, Id::new("write-load-engine").unwrap())
        .await
        .unwrap()
        .permit;
    let run_ids = (0..WRITERS)
        .map(|writer| Id::new(format!("write-load-run-{writer}")).unwrap())
        .collect::<Vec<_>>();
    for run_id in &run_ids {
        store
            .create_run(
                &fixture.scope,
                create_run_command(&fixture, run_id.as_str()),
            )
            .await
            .unwrap();
        store
            .start_run(
                &fixture.scope,
                StartRun {
                    permit: permit.clone(),
                    run_id: run_id.clone(),
                    compatibility_evidence: load_compatibility(&fixture),
                },
            )
            .await
            .unwrap();
    }
    let completion_output = objects
        .put(&fixture.scope, br#"{"value":2}"#, "application/json")
        .await
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(WRITERS));
    let metrics = Arc::new(Mutex::new(SqliteWriteLoadMetrics::default()));
    let started = Instant::now();
    let mut writers = Vec::with_capacity(WRITERS);
    for (writer, run_id) in run_ids.iter().cloned().enumerate() {
        let store = store.clone();
        let fixture = fixture.clone();
        let permit = permit.clone();
        let completion_output = completion_output.clone();
        let barrier = barrier.clone();
        let metrics = metrics.clone();
        writers.push(tokio::spawn(async move {
            let role = match writer % 4 {
                0 => SqliteWriteLoadRole::Claim,
                1 => SqliteWriteLoadRole::Complete,
                2 => SqliteWriteLoadRole::Cancel,
                _ => SqliteWriteLoadRole::Heartbeat,
            };
            barrier.wait().await;
            let deadline = Instant::now() + LOAD_DURATION;
            let mut claimed = None;
            let mut cancelled = None;
            while Instant::now() < deadline {
                match role {
                    SqliteWriteLoadRole::Claim if claimed.is_none() => {
                        let node = store
                            .get_node(&fixture.scope, &run_id, &Id::new("action").unwrap())
                            .await
                            .map_err(|error| error.to_string())?;
                        let result = measure_sqlite_write(
                            &metrics,
                            store.claim_node_attempt(
                                &fixture.scope,
                                ClaimNodeAttempt {
                                    permit: permit.clone(),
                                    run_id: run_id.clone(),
                                    node_id: Id::new("action").unwrap(),
                                    expected_node_version: node.version,
                                    attempt_id: Id::new(format!("write-load-attempt-{writer}"))
                                        .unwrap(),
                                    worker_id: Id::new(format!("write-load-worker-{writer}"))
                                        .unwrap(),
                                    bound_input: fixture.input.clone(),
                                    binding_derivation_digest: fixture.evidence_digest.clone(),
                                },
                            ),
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                        let ClaimNodeAttemptResult::Claimed {
                            completion_credential,
                            ..
                        } = result
                        else {
                            return Err("load claim did not claim".to_owned());
                        };
                        metrics.lock().expect("load metrics lock poisoned").claims += 1;
                        claimed = Some(completion_credential);
                    }
                    SqliteWriteLoadRole::Complete if claimed.is_none() => {
                        let node = store
                            .get_node(&fixture.scope, &run_id, &Id::new("action").unwrap())
                            .await
                            .map_err(|error| error.to_string())?;
                        let result = measure_sqlite_write(
                            &metrics,
                            store.claim_node_attempt(
                                &fixture.scope,
                                ClaimNodeAttempt {
                                    permit: permit.clone(),
                                    run_id: run_id.clone(),
                                    node_id: Id::new("action").unwrap(),
                                    expected_node_version: node.version,
                                    attempt_id: Id::new(format!("write-load-attempt-{writer}"))
                                        .unwrap(),
                                    worker_id: Id::new(format!("write-load-worker-{writer}"))
                                        .unwrap(),
                                    bound_input: fixture.input.clone(),
                                    binding_derivation_digest: fixture.evidence_digest.clone(),
                                },
                            ),
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                        let ClaimNodeAttemptResult::Claimed {
                            completion_credential,
                            ..
                        } = result
                        else {
                            return Err("load completion did not claim".to_owned());
                        };
                        metrics.lock().expect("load metrics lock poisoned").claims += 1;
                        claimed = Some(completion_credential);
                    }
                    SqliteWriteLoadRole::Complete => {
                        measure_sqlite_write(
                            &metrics,
                            store.complete_attempt(
                                &fixture.scope,
                                CompleteAttempt {
                                    completion_credential: claimed
                                        .as_ref()
                                        .expect("completion claim missing")
                                        .clone(),
                                    run_id: run_id.clone(),
                                    node_id: Id::new("action").unwrap(),
                                    attempt_id: Id::new(format!("write-load-attempt-{writer}"))
                                        .unwrap(),
                                    submitted_outcome: ActionOutcome::Success {
                                        output: serde_json::json!({"value": 2}),
                                        artifacts: Vec::new(),
                                        actual_cost_units: CostUnits(1),
                                        diagnostics: None,
                                    },
                                    objects: CompletionObjects {
                                        output: Some(completion_output.clone()),
                                        artifacts: Vec::new(),
                                        diagnostics: None,
                                    },
                                },
                            ),
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                        let mut metrics = metrics.lock().expect("load metrics lock poisoned");
                        if metrics.completions < (WRITERS / 4) as u64 {
                            metrics.completions += 1;
                        }
                    }
                    SqliteWriteLoadRole::Cancel if cancelled.is_none() => {
                        let observed = store
                            .get_run(&fixture.scope, &run_id)
                            .await
                            .map_err(|error| error.to_string())?;
                        cancelled = Some(observed.run.version);
                    }
                    SqliteWriteLoadRole::Cancel => {
                        measure_sqlite_write(
                            &metrics,
                            store.cancel_run(
                                &fixture.scope,
                                CancelRun {
                                    run_id: run_id.clone(),
                                    expected_run_version: cancelled
                                        .expect("cancel version missing"),
                                    expected_pending_gate_versions: Vec::new(),
                                    principal: fixture.principal.clone(),
                                    reason_code: "load-test".to_owned(),
                                    idempotency_token: format!("write-load-cancel-token-{writer}"),
                                },
                            ),
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    }
                    _ => {
                        measure_sqlite_write(
                            &metrics,
                            store.heartbeat_engine_claim(&fixture.scope, &permit),
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<(), String>(())
        }));
    }
    for writer in writers {
        writer.await.unwrap().unwrap();
    }

    let (claims, completions) = {
        let metrics = metrics.lock().expect("load metrics lock poisoned");
        (metrics.claims, metrics.completions)
    };
    assert_eq!(claims, 8, "four claim and four completion writers");
    assert_eq!(completions, 4, "each completion writer settles once");
    let ledger_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM dagger_workflow_budget_ledger
         WHERE tenant_id = ? AND namespace = ?",
    )
    .bind(fixture.scope.tenant_id.as_str())
    .bind(fixture.scope.namespace.as_str())
    .fetch_one(store.pool())
    .await
    .unwrap();
    let ledger_runs: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT run_id) FROM dagger_workflow_budget_ledger
         WHERE tenant_id = ? AND namespace = ?",
    )
    .bind(fixture.scope.tenant_id.as_str())
    .bind(fixture.scope.namespace.as_str())
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(ledger_rows as u64, claims + completions);
    assert_eq!(
        ledger_runs, 8,
        "ledger covers all claimed and completed runs"
    );
    let mut latencies = metrics
        .lock()
        .expect("load metrics lock poisoned")
        .latencies
        .clone();
    latencies.sort_unstable();
    let p99 = latencies[(latencies.len() * 99).div_ceil(100).saturating_sub(1)];
    eprintln!(
        "sqlite_write_path_load: writers={WRITERS} wall_ms={} writes={} p99_write_us={} ledger_rows={ledger_rows} unretried_transaction_failed=0",
        started.elapsed().as_millis(),
        latencies.len(),
        p99.as_micros(),
    );
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
                | Err(StoreError::AttemptFenced)
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
    // Word presence is not scoping. This checker resolves every table reference in a statement to
    // its alias, then requires that alias to be transitively equated to a bound tenant_id AND
    // namespace value inside the same statement. A scoped outer query with an unscoped join -- the
    // exact shape that once shipped in LIST_RUNS_SQL -- fails, because the joined alias is in no
    // bound equality component even though the words appear in the text.

    /// Lowercase and split into tokens with punctuation separated, so `x.tenant_id=?` and
    /// `x.tenant_id = ?` tokenize identically. Splitting `=` on its own also keeps `<=` and `>=`
    /// from being mistaken for equality: they tokenize as `<` `=`, and the equality scan only
    /// accepts `=` in the position directly after the column.
    fn tokenize(statement: &str) -> Vec<String> {
        let mut spaced = String::with_capacity(statement.len() * 2);
        for character in statement.to_ascii_lowercase().chars() {
            if matches!(character, '(' | ')' | ',' | '=' | '<' | '>' | '!') {
                spaced.push(' ');
                spaced.push(character);
                spaced.push(' ');
            } else {
                spaced.push(character);
            }
        }
        spaced.split_whitespace().map(str::to_owned).collect()
    }

    fn is_bound_value(token: &str) -> bool {
        token.starts_with('?')
            || token.starts_with(':')
            || token.starts_with('\'')
            || token.starts_with('{')
            || token.chars().all(|character| character.is_ascii_digit())
    }

    fn is_domain_table(token: &str) -> bool {
        (token.starts_with("dagger_workflow_") && token != "dagger_workflow_schema_migrations")
            || token.starts_with('{')
    }

    /// `(table, alias)` for every FROM / JOIN / UPDATE / INSERT INTO target in the branch. Alias is
    /// empty when the reference is unaliased, which is only legal in a single-table branch.
    fn table_references(tokens: &[String]) -> Vec<(String, String)> {
        const NOT_AN_ALIAS: [&str; 14] = [
            "where", "on", "join", "set", "values", "order", "limit", "group", "union", "left",
            "inner", "cross", "using", "select",
        ];
        let mut references = Vec::new();
        for index in 0..tokens.len() {
            if !matches!(tokens[index].as_str(), "from" | "join" | "into" | "update") {
                continue;
            }
            let Some(table) = tokens.get(index + 1) else {
                continue;
            };
            if !is_domain_table(table) {
                continue;
            }
            let alias = match tokens.get(index + 2).map(String::as_str) {
                Some("as") => tokens.get(index + 3).cloned().unwrap_or_default(),
                Some(next)
                    if !NOT_AN_ALIAS.contains(&next)
                        && next.chars().all(|character| {
                            character.is_ascii_alphanumeric() || character == '_'
                        }) =>
                {
                    next.to_owned()
                }
                _ => String::new(),
            };
            references.push((table.clone(), alias));
        }
        references
    }

    /// Aliases transitively equated to a bound value on `column`. `a.tenant_id = ?` binds `a`;
    /// `b.tenant_id = a.tenant_id` then carries that binding to `b`. Nothing else counts.
    fn bound_aliases(tokens: &[String], column: &str) -> std::collections::BTreeSet<String> {
        let suffix = format!(".{column}");
        let alias_of = |token: &str| -> Option<String> {
            if token == column {
                Some(String::new())
            } else {
                token
                    .strip_suffix(&suffix)
                    .map(|alias| alias.trim_start_matches('(').to_owned())
            }
        };
        let mut equivalences: Vec<(String, String)> = Vec::new();
        let mut bound: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for index in 0..tokens.len() {
            let Some(left) = alias_of(&tokens[index]) else {
                continue;
            };
            if tokens.get(index + 1).map(String::as_str) != Some("=") {
                continue;
            }
            let Some(right) = tokens.get(index + 2) else {
                continue;
            };
            if is_bound_value(right) {
                bound.insert(left);
            } else if let Some(other) = alias_of(right) {
                equivalences.push((left, other));
            }
        }
        // Closure over the equality edges; the statement is tiny, so iterate to a fixed point.
        loop {
            let before = bound.len();
            for (left, right) in &equivalences {
                if bound.contains(left) {
                    bound.insert(right.clone());
                }
                if bound.contains(right) {
                    bound.insert(left.clone());
                }
            }
            if bound.len() == before {
                break;
            }
        }
        bound
    }

    /// `None` when the statement is properly scoped, `Some(reason)` otherwise.
    fn scoping_violation(statement: &str) -> Option<String> {
        let tokens = tokenize(statement);
        if tokens.first().map(String::as_str) == Some("insert") {
            // INSERT has no predicate to scope: the scope must be written, so it must appear in the
            // target column list. INSERT ... SELECT would need the SELECT scoped too; the crate has
            // none, and this rejects one if it ever appears rather than passing it silently.
            if tokens.iter().any(|token| token == "select") {
                return Some("INSERT ... SELECT is not scope-checked by this gate".to_owned());
            }
            let columns: Vec<&String> = tokens
                .iter()
                .skip_while(|token| token.as_str() != "(")
                .take_while(|token| token.as_str() != ")")
                .collect();
            let missing: Vec<&str> = ["tenant_id", "namespace"]
                .into_iter()
                .filter(|column| !columns.iter().any(|token| token.as_str() == *column))
                .collect();
            return (!missing.is_empty()).then(|| {
                format!(
                    "insert target column list is missing {}",
                    missing.join(" and ")
                )
            });
        }
        // UNION branches are scoped independently; a scoped first branch must not vouch for an
        // unscoped second one.
        for branch in tokens.split(|token| token == "union") {
            let branch_tokens: Vec<String> = branch
                .iter()
                .filter(|token| token.as_str() != "all")
                .cloned()
                .collect();
            let references = table_references(&branch_tokens);
            if references.is_empty() {
                continue;
            }
            let tenant_bound = bound_aliases(&branch_tokens, "tenant_id");
            let namespace_bound = bound_aliases(&branch_tokens, "namespace");
            for (table, alias) in &references {
                if alias.is_empty() && references.len() > 1 {
                    return Some(format!(
                        "{table} is unaliased in a multi-table statement, so its scope predicate \
                         cannot be attributed"
                    ));
                }
                let missing: Vec<&str> = [
                    ("tenant_id", &tenant_bound),
                    ("namespace", &namespace_bound),
                ]
                .into_iter()
                .filter(|(_, bound)| !bound.contains(alias))
                .map(|(column, _)| column)
                .collect();
                if !missing.is_empty() {
                    return Some(format!(
                        "{table}{}{alias} is not bound on {}",
                        if alias.is_empty() { "" } else { " AS " },
                        missing.join(" and ")
                    ));
                }
            }
        }
        None
    }

    // RED FIXTURES. A gate with no proof that it can fail is how the previous word-presence scan
    // survived. Each of these must be rejected, and the last must be accepted so the checker is not
    // trivially rejecting everything.
    let red_fixtures: [(&str, &str); 5] = [
        (
            "scoped outer query with an unscoped join",
            "SELECT rr.row_data
               FROM dagger_workflow_runs AS r
               JOIN dagger_workflow_run_rows AS rr
                 ON rr.entity_id = r.run_id AND rr.sub_id = ''
              WHERE r.tenant_id = ? AND r.namespace = ? AND r.run_id > ?",
        ),
        (
            "missing namespace predicate",
            "SELECT row_data FROM dagger_workflow_run_rows
              WHERE tenant_id = ? AND entity_id = ? AND sub_id = ''",
        ),
        (
            "missing tenant_id entirely",
            "SELECT row_data FROM dagger_workflow_run_rows
              WHERE namespace = ? AND entity_id = ? AND sub_id = ''",
        ),
        (
            "unscoped second UNION branch",
            "SELECT rr.row_data FROM dagger_workflow_run_rows AS rr
              WHERE rr.tenant_id = ? AND rr.namespace = ?
             UNION ALL
             SELECT rr.row_data FROM dagger_workflow_run_rows AS rr
              WHERE rr.entity_id = ?",
        ),
        (
            "insert without the scope columns",
            "INSERT INTO dagger_workflow_runs (run_id, status, version) VALUES (?, ?, ?)",
        ),
    ];
    for (name, statement) in red_fixtures {
        assert!(
            scoping_violation(statement).is_some(),
            "red fixture '{name}' was accepted by the scoping checker: {statement}"
        );
    }
    // Green fixture: transitive binding through a join equality is legitimate scoping and must be
    // accepted, or the checker is just rejecting everything.
    assert_eq!(
        scoping_violation(
            "SELECT ar.row_data
               FROM dagger_workflow_artifact_refs AS a
               JOIN dagger_workflow_artifact_rows AS ar
                 ON ar.tenant_id = a.tenant_id AND ar.namespace = a.namespace
                AND ar.entity_id = a.artifact_ref_id
              WHERE a.tenant_id = ? AND a.namespace = ?"
        ),
        None
    );

    // Registry half: the scheduler scan statements the crate exports are checked by name, so a
    // constant that stops being discovered by the source scan is still covered.
    for (name, statement) in [
        ("SCHEDULER_NODES_SCAN_SQL", SCHEDULER_NODES_SCAN_SQL),
        ("SCHEDULER_DEADLINES_SCAN_SQL", SCHEDULER_DEADLINES_SCAN_SQL),
        ("SCHEDULER_RECOVERY_SCAN_SQL", SCHEDULER_RECOVERY_SCAN_SQL),
        (
            "SCHEDULER_COMPATIBILITY_SCAN_SQL",
            SCHEDULER_COMPATIBILITY_SCAN_SQL,
        ),
        ("SCHEDULER_GATES_SCAN_SQL", SCHEDULER_GATES_SCAN_SQL),
        ("SCHEDULER_LIFETIMES_SCAN_SQL", SCHEDULER_LIFETIMES_SCAN_SQL),
    ] {
        assert_eq!(scoping_violation(statement), None, "{name} is not scoped");
    }

    fn string_literals(source: &str) -> Vec<String> {
        let bytes = source.as_bytes();
        let mut literals = Vec::new();
        let mut index = 0;
        // An `r` only opens a raw string at a token boundary. Without this check an identifier
        // ending in `r` immediately before a quote is read as a raw-string opener, which makes the
        // scanner swallow the real closing quote and desynchronize for the rest of the file.
        let opens_raw_string = |index: usize| {
            bytes[index] == b'r'
                && (index == 0
                    || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_'))
        };
        while index < bytes.len() {
            if opens_raw_string(index) {
                let mut hashes = 0;
                while index + 1 + hashes < bytes.len() && bytes[index + 1 + hashes] == b'#' {
                    hashes += 1;
                }
                if index + 1 + hashes < bytes.len() && bytes[index + 1 + hashes] == b'"' {
                    let start = index + 2 + hashes;
                    let mut end = start;
                    let mut terminated = false;
                    while end < bytes.len() {
                        if bytes[end] == b'"'
                            && (0..hashes).all(|offset| {
                                end + 1 + offset < bytes.len() && bytes[end + 1 + offset] == b'#'
                            })
                        {
                            literals.push(source[start..end].to_owned());
                            index = end + 1 + hashes;
                            terminated = true;
                            break;
                        }
                        end += 1;
                    }
                    // An unterminated literal must still advance the cursor, or this loop spins
                    // forever. The source compiles, so reaching here means the scan desynchronized.
                    if !terminated {
                        index = bytes.len();
                    }
                    continue;
                }
            }
            if bytes[index] == b'"' {
                let start = index + 1;
                let mut end = start;
                let mut escaped = false;
                let mut terminated = false;
                while end < bytes.len() {
                    if bytes[end] == b'"' && !escaped {
                        literals.push(source[start..end].to_owned());
                        index = end + 1;
                        terminated = true;
                        break;
                    }
                    escaped = bytes[end] == b'\\' && !escaped;
                    end += 1;
                }
                if !terminated {
                    index = bytes.len();
                }
                continue;
            }
            index += 1;
        }
        literals
    }

    // The whole crate source is scanned, not just src/sqlite. Scoping is a property of
    // every domain statement wherever it is written, and a gate that only looks in one
    // module fails open the moment domain SQL lands outside it.
    fn rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_sources(&path, sources);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                sources.push(path);
            }
        }
    }

    let mut source_paths = Vec::new();
    rust_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut source_paths,
    );
    source_paths.sort();
    let mut violations = Vec::new();
    let mut checked = 0_usize;
    for path in source_paths {
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
            checked += 1;
            if let Some(reason) = scoping_violation(&normalized) {
                violations.push(format!(
                    "{}: {reason}\n    {normalized}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "domain SQL is not statically tenant-scoped:\n{}",
        violations.join("\n")
    );
    // A checker that discovers nothing passes vacuously. This floor makes a desynchronized literal
    // scan or a moved module fail the gate instead of silently checking zero statements.
    assert!(
        checked >= 30,
        "only {checked} domain statements were discovered; the literal scan is not finding the crate's SQL"
    );
}

#[tokio::test]
async fn gate_10_identical_command_cost_is_non_linear_in_historical_runs() {
    async fn open_cost_store(
        clock: Arc<TestClock>,
        objects: Arc<InMemoryObjectStore<TestClock>>,
    ) -> SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
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
        let claim = store
            .acquire_engine_claim(&fixture.scope, Id::new("cost-engine").unwrap())
            .await
            .unwrap();
        for index in 0..count {
            let run_id = format!("history-{index:04}");
            let attempt_id = format!("attempt-{index:04}");
            store
                .create_run(&fixture.scope, create_run_command(&fixture, &run_id))
                .await
                .unwrap();
            store
                .start_run(
                    &fixture.scope,
                    StartRun {
                        permit: claim.permit.clone(),
                        run_id: Id::new(&run_id).unwrap(),
                        compatibility_evidence: CompatibilityReport {
                            evidence_digest: fixture.evidence_digest.clone(),
                            incompatible_reference_locations: Vec::new(),
                            evidence: Vec::new(),
                        },
                    },
                )
                .await
                .unwrap();
            let action = store
                .get_node(
                    &fixture.scope,
                    &Id::new(&run_id).unwrap(),
                    &Id::new("action").unwrap(),
                )
                .await
                .unwrap();
            let credential = match store
                .claim_node_attempt(
                    &fixture.scope,
                    ClaimNodeAttempt {
                        permit: claim.permit.clone(),
                        run_id: Id::new(&run_id).unwrap(),
                        node_id: Id::new("action").unwrap(),
                        expected_node_version: action.version,
                        attempt_id: Id::new(&attempt_id).unwrap(),
                        worker_id: Id::new("history-worker").unwrap(),
                        bound_input: fixture.input.clone(),
                        binding_derivation_digest: fixture.evidence_digest.clone(),
                    },
                )
                .await
                .unwrap()
            {
                ClaimNodeAttemptResult::Claimed {
                    completion_credential,
                    ..
                } => completion_credential,
                _ => panic!("historical action was not claimed"),
            };
            store
                .complete_attempt(
                    &fixture.scope,
                    CompleteAttempt {
                        completion_credential: credential,
                        run_id: Id::new(&run_id).unwrap(),
                        node_id: Id::new("action").unwrap(),
                        attempt_id: Id::new(&attempt_id).unwrap(),
                        submitted_outcome: ActionOutcome::Success {
                            output: serde_json::json!({"value": 1}),
                            artifacts: Vec::new(),
                            actual_cost_units: CostUnits(1),
                            diagnostics: None,
                        },
                        objects: CompletionObjects {
                            output: Some(fixture.input.clone()),
                            artifacts: Vec::new(),
                            diagnostics: None,
                        },
                    },
                )
                .await
                .unwrap();
            let terminal = store
                .get_node(
                    &fixture.scope,
                    &Id::new(&run_id).unwrap(),
                    &Id::new("succeed").unwrap(),
                )
                .await
                .unwrap();
            store
                .resolve_terminal_node(
                    &fixture.scope,
                    ResolveTerminalNode {
                        permit: claim.permit.clone(),
                        run_id: Id::new(&run_id).unwrap(),
                        node_id: Id::new("succeed").unwrap(),
                        expected_node_version: terminal.version,
                        output: Some(fixture.input.clone()),
                    },
                )
                .await
                .unwrap();
            if index > 0 && index % 10 == 0 {
                store
                    .heartbeat_engine_claim(&fixture.scope, &claim.permit)
                    .await
                    .unwrap();
            }
        }
        let completed_projection: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM dagger_workflow_runs
             WHERE tenant_id = ? AND namespace = ? AND status = 'Succeeded'",
        )
        .bind(fixture.scope.tenant_id.as_str())
        .bind(fixture.scope.namespace.as_str())
        .fetch_one(store.pool())
        .await
        .unwrap();
        let completed_authority: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM dagger_workflow_run_rows
             WHERE tenant_id = ? AND namespace = ?
               AND entity_id LIKE 'history-%' AND json_extract(row_data, '$.status') = 'Succeeded'",
        )
        .bind(fixture.scope.tenant_id.as_str())
        .bind(fixture.scope.namespace.as_str())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(
            (completed_projection, completed_authority),
            (count as i64, count as i64)
        );
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

    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let small_objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let large_objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let small = open_cost_store(clock.clone(), small_objects.clone()).await;
    let large = open_cost_store(clock, large_objects.clone()).await;
    // Each history is measured immediately after it is seeded. Lease deadlines come from the
    // in-transaction database clock rather than TestClock, so seeding the large history before
    // measuring the small one expires the small claim in real time.
    let (small_fixture, small_claim) =
        seed_completed_history(&small, &small_objects, "small-history", 50).await;
    small
        .heartbeat_engine_claim(&small_fixture.scope, &small_claim.permit)
        .await
        .unwrap();
    let small_steps = measure_ordinary_start_vm_steps(&small, &small_fixture, &small_claim).await;

    let (large_fixture, large_claim) =
        seed_completed_history(&large, &large_objects, "large-history", 500).await;
    large
        .heartbeat_engine_claim(&large_fixture.scope, &large_claim.permit)
        .await
        .unwrap();
    let large_steps = measure_ordinary_start_vm_steps(&large, &large_fixture, &large_claim).await;
    let bound = small_steps.saturating_mul(2).saturating_add(2_000);
    assert!(
        large_steps <= bound,
        "500-run ordinary start used {large_steps} SQLite VM steps and scaled with \
         completed history; 50-run ordinary start used {small_steps}, bound {bound}"
    );
}

#[tokio::test]
async fn gate_11_definition_and_list_runs_cost_is_bounded_by_page_not_history() {
    async fn open_cost_store(
        clock: Arc<TestClock>,
        objects: Arc<InMemoryObjectStore<TestClock>>,
    ) -> SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        SqliteWorkflowStore::from_pool(pool, clock, objects)
            .await
            .unwrap()
    }

    async fn seed_history(
        store: &SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>>,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        tenant: &str,
        count: usize,
    ) -> ActionFixture {
        let fixture = seed_action_definition(store, objects, tenant).await;
        for index in 0..count {
            let run_id = format!("history-{index:04}");
            store
                .create_run(&fixture.scope, create_run_command(&fixture, &run_id))
                .await
                .unwrap();
        }
        fixture
    }

    async fn measure(
        store: &SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>>,
        name: &str,
        operation: impl std::future::Future<Output = Result<(), StoreError>>,
    ) -> u64 {
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
        operation
            .await
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        steps.load(Ordering::Relaxed)
    }

    /// The pre-bound shape of `list_runs`: the same scoped join, but without the keyset predicate
    /// and without LIMIT, so the whole history is materialized and paged in memory. This is the
    /// negative control for the bounded measurement -- without it, a `list_runs` number that is
    /// identical at both history sizes proves nothing, because a broken progress handler and a
    /// bounded query are indistinguishable.
    async fn measure_unbounded_list_runs(
        store: &SqliteWorkflowStore<TestClock, InMemoryObjectStore<TestClock>>,
        scope: &ExecutionScope,
    ) -> u64 {
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
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT rr.row_data
             FROM dagger_workflow_runs AS r
             JOIN dagger_workflow_run_rows AS rr
               ON rr.tenant_id = r.tenant_id AND rr.namespace = r.namespace
              AND rr.entity_id = r.run_id AND rr.sub_id = ''
             WHERE r.tenant_id = ? AND r.namespace = ?
               AND rr.tenant_id = ? AND rr.namespace = ?
             ORDER BY r.run_id",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .bind(scope.tenant_id.as_str())
        .bind(scope.namespace.as_str())
        .fetch_all(store.pool())
        .await
        .unwrap();
        assert!(!rows.is_empty());
        steps.load(Ordering::Relaxed)
    }

    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let small_objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let large_objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let small = open_cost_store(clock.clone(), small_objects.clone()).await;
    let large = open_cost_store(clock, large_objects.clone()).await;
    let small_fixture = seed_history(&small, &small_objects, "small-definition-history", 50).await;
    let large_fixture = seed_history(&large, &large_objects, "large-definition-history", 500).await;
    let small_definition_version = small
        .get_definition(&small_fixture.scope, &Id::new("definition").unwrap())
        .await
        .unwrap()
        .version;
    let large_definition_version = large
        .get_definition(&large_fixture.scope, &Id::new("definition").unwrap())
        .await
        .unwrap()
        .version;

    let small_definition_steps = measure(&small, "small definition update", async {
        small
            .update_definition_metadata(
                &small_fixture.scope,
                UpdateDefinitionMetadata {
                    definition_id: Id::new("definition").unwrap(),
                    expected_version: small_definition_version,
                    display_name: "updated".to_owned(),
                    description: String::new(),
                },
            )
            .await
            .map(|_| ())
    })
    .await;
    let large_definition_steps = measure(&large, "large definition update", async {
        large
            .update_definition_metadata(
                &large_fixture.scope,
                UpdateDefinitionMetadata {
                    definition_id: Id::new("definition").unwrap(),
                    expected_version: large_definition_version,
                    display_name: "updated".to_owned(),
                    description: String::new(),
                },
            )
            .await
            .map(|_| ())
    })
    .await;
    let small_list_steps = measure(&small, "small list_runs", async {
        small
            .list_runs(
                &small_fixture.scope,
                PageRequest {
                    cursor: None,
                    page_size: 10,
                },
            )
            .await
            .map(|_| ())
    })
    .await;
    let large_list_steps = measure(&large, "large list_runs", async {
        large
            .list_runs(
                &large_fixture.scope,
                PageRequest {
                    cursor: None,
                    page_size: 10,
                },
            )
            .await
            .map(|_| ())
    })
    .await;
    let small_unbounded_steps = measure_unbounded_list_runs(&small, &small_fixture.scope).await;
    let large_unbounded_steps = measure_unbounded_list_runs(&large, &large_fixture.scope).await;
    eprintln!(
        "gate_11 VM steps: definition update {small_definition_steps}/{large_definition_steps}; \
         list_runs {small_list_steps}/{large_list_steps}; \
         unbounded list_runs {small_unbounded_steps}/{large_unbounded_steps} (50/500 runs)"
    );
    // Red proof for the list_runs half: the same measurement apparatus, against the same seeded
    // histories, with only the page bound removed. If this does not scale, the bounded numbers
    // above are measuring nothing.
    assert!(
        large_unbounded_steps > small_unbounded_steps.saturating_mul(4),
        "removing the page bound did not make cost scale with history: unbounded list_runs used \
         {small_unbounded_steps} steps at 50 runs and {large_unbounded_steps} at 500, so the \
         bounded numbers {small_list_steps}/{large_list_steps} prove nothing"
    );
    for (name, small_steps, large_steps) in [
        (
            "definition update",
            small_definition_steps,
            large_definition_steps,
        ),
        ("list_runs", small_list_steps, large_list_steps),
    ] {
        let bound = small_steps.saturating_mul(2).saturating_add(2_000);
        assert!(
            large_steps <= bound,
            "{name}: 500-run cost {large_steps} scaled with unrelated history; \
             50-run cost {small_steps}, bound {bound}"
        );
    }
}

#[cfg(feature = "conformance")]
#[test]
fn all_conformance_fixtures_pass_unchanged() {
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
                assert_eq!(results.len(), dagger_workflow_core::conformance::CASE_COUNT);
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
