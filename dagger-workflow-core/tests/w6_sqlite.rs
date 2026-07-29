#![cfg(feature = "sqlite")]

use dagger_workflow_core::action::CompatibilityReport;
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::ObjectStore;
use dagger_workflow_core::definition::{
    ActionReference, BackoffPolicy, Binding, BindingSource, NodeDefinition, PublishableDefinition,
    RetryPolicy, TimeoutPolicy, WorkflowDefinition,
};
use dagger_workflow_core::engine::TestClock;
use dagger_workflow_core::ids::{CostUnits, Id, Timestamp, TopologicalRank, Version};
use dagger_workflow_core::memory::InMemoryObjectStore;
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::sqlite::{SqliteWorkflowStore, SCHEMA_VERSION};
use dagger_workflow_core::store::{
    ClaimNodeAttempt, ClaimNodeAttemptResult, CreateDefinition, CreateRun, PageRequest,
    PublishRevision, ResolvedActionSchemas, StartRun, WorkflowStore,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::TempDir;

fn scope(tenant: &str) -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new(tenant).unwrap(),
        namespace: ScopeAtom::new("workflow").unwrap(),
    }
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

    let store =
        SqliteWorkflowStore::from_pool(pool.clone(), Arc::new(TestClock::new(Timestamp(0))))
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
    assert_eq!(table_count, 18);
}

#[tokio::test]
async fn standalone_open_reopens_a_converged_migration() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("workflow.sqlite");
    let first = SqliteWorkflowStore::open(&path, Arc::new(TestClock::new(Timestamp(10))))
        .await
        .unwrap();
    first.pool().close().await;
    let reopened = SqliteWorkflowStore::open(&path, Arc::new(TestClock::new(Timestamp(20))))
        .await
        .unwrap();
    let versions: Vec<i64> = sqlx::query_scalar(
        "SELECT version FROM dagger_workflow_schema_migrations ORDER BY version",
    )
    .fetch_all(reopened.pool())
    .await
    .unwrap();
    assert_eq!(versions, (1..=SCHEMA_VERSION).collect::<Vec<_>>());
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
            version INTEGER PRIMARY KEY, applied_at_ms INTEGER NOT NULL
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

    let reopened = SqliteWorkflowStore::open(&path, Arc::new(TestClock::new(Timestamp(0))))
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
    let store =
        SqliteWorkflowStore::open_url("sqlite::memory:", Arc::new(TestClock::new(Timestamp(0))))
            .await
            .unwrap();
    {
        let mut transaction = store.pool().begin().await.unwrap();
        sqlx::query(
            "INSERT INTO dagger_workflow_runs
             (tenant_id, namespace, run_id, definition_id, revision_hash, status,
              version, event_seq, aggregate_object_bytes, total_attempts,
              budget_limit, budget_reserved, budget_spent, lifetime_deadline_at_ms, created_at_ms)
             VALUES ('tenant', 'workflow', 'run', 'definition', 'sha256:x', 'Pending',
                     1, 0, 0, 0, 1, 0, 0, 100, 0)",
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
async fn tenant_a_command_does_not_rewrite_tenant_b_rows() {
    let store =
        SqliteWorkflowStore::open_url("sqlite::memory:", Arc::new(TestClock::new(Timestamp(0))))
            .await
            .unwrap();
    let scope_a = scope("tenant-a");
    let scope_b = scope("tenant-b");
    let claim_a = store
        .acquire_engine_claim(&scope_a, Id::new("engine-a").unwrap())
        .await
        .unwrap();
    store
        .acquire_engine_claim(&scope_b, Id::new("engine-b").unwrap())
        .await
        .unwrap();

    let before: (String, i64) = sqlx::query_as(
        "SELECT reducer_data, version FROM dagger_workflow_adapter_state
         WHERE tenant_id = ? AND namespace = ?",
    )
    .bind(scope_b.tenant_id.as_str())
    .bind(scope_b.namespace.as_str())
    .fetch_one(store.pool())
    .await
    .unwrap();
    let before_claim: (String, i64, i64) = sqlx::query_as(
        "SELECT instance_id, generation, version FROM dagger_workflow_engine_claims
         WHERE tenant_id = ? AND namespace = ? AND control_plane_id = 'scheduler'",
    )
    .bind(scope_b.tenant_id.as_str())
    .bind(scope_b.namespace.as_str())
    .fetch_one(store.pool())
    .await
    .unwrap();

    store
        .heartbeat_engine_claim(&scope_a, &claim_a.permit)
        .await
        .unwrap();

    let after: (String, i64) = sqlx::query_as(
        "SELECT reducer_data, version FROM dagger_workflow_adapter_state
         WHERE tenant_id = ? AND namespace = ?",
    )
    .bind(scope_b.tenant_id.as_str())
    .bind(scope_b.namespace.as_str())
    .fetch_one(store.pool())
    .await
    .unwrap();
    let after_claim: (String, i64, i64) = sqlx::query_as(
        "SELECT instance_id, generation, version FROM dagger_workflow_engine_claims
         WHERE tenant_id = ? AND namespace = ? AND control_plane_id = 'scheduler'",
    )
    .bind(scope_b.tenant_id.as_str())
    .bind(scope_b.namespace.as_str())
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!((after, after_claim), (before, before_claim));
}

#[tokio::test]
async fn reopen_recovers_a_started_attempt_from_sql_state_only() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("attempt-recovery.sqlite");
    let clock = Arc::new(TestClock::new(Timestamp(0)));
    let objects = InMemoryObjectStore::new(clock.clone());
    let workflow_scope = scope("recovery-tenant");
    let store = SqliteWorkflowStore::open(&path, clock.clone())
        .await
        .unwrap();
    let schema = objects
        .put(&workflow_scope, b"{}", "application/json")
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
                    target: String::new(),
                    source: BindingSource::RunInput {
                        pointer: String::new(),
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
        "SELECT reducer_data FROM dagger_workflow_adapter_state
         WHERE tenant_id = ? AND namespace = ?",
    )
    .bind(workflow_scope.tenant_id.as_str())
    .bind(workflow_scope.namespace.as_str())
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

    let reopened = SqliteWorkflowStore::open(&path, Arc::new(TestClock::new(Timestamp(0))))
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

#[cfg(feature = "conformance")]
#[test]
fn all_forty_conformance_fixtures_pass_unchanged() {
    struct Adapter {
        _directory: TempDir,
        clock: Arc<TestClock>,
        store: SqliteWorkflowStore<TestClock>,
        objects: InMemoryObjectStore<TestClock>,
    }

    impl Adapter {
        fn build() -> Self {
            std::thread::spawn(|| {
                let runtime = tokio::runtime::Runtime::new().unwrap();
                runtime.block_on(async {
                    let directory = TempDir::new().unwrap();
                    let clock = Arc::new(TestClock::new(Timestamp(0)));
                    let store = SqliteWorkflowStore::open(
                        directory.path().join("conformance.sqlite"),
                        clock.clone(),
                    )
                    .await
                    .unwrap();
                    Self {
                        _directory: directory,
                        store,
                        objects: InMemoryObjectStore::new(clock.clone()),
                        clock,
                    }
                })
            })
            .join()
            .unwrap()
        }
    }

    impl dagger_workflow_core::conformance::ConformanceAdapter for Adapter {
        type Store = SqliteWorkflowStore<TestClock>;
        type Objects = InMemoryObjectStore<TestClock>;

        fn store(&self) -> &Self::Store {
            &self.store
        }

        fn objects(&self) -> &Self::Objects {
            &self.objects
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
