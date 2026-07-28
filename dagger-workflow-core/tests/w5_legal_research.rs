mod support;

use dagger_workflow_core::action::fixtures::FixtureActions;
use dagger_workflow_core::artifact::ObjectStore;
use dagger_workflow_core::engine::{EngineConfig, TestClock, WorkflowEngine};
use dagger_workflow_core::event::EventType;
use dagger_workflow_core::ids::{CostUnits, Id, Timestamp};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::run::{NodeState, RunLimits, RunState};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::store::{CreateRun, EventPageRequest, PageRequest, WorkflowStore};
use serde_json::json;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread;

use support::{principal, publish_legal_research_reference, PublishedReference};

fn block_on<T>(future: impl Future<Output = T>) -> T {
    struct ThreadWake(thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match Pin::new(&mut future).poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => thread::park(),
        }
    }
}

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}

fn scope() -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new("w5-tenant").unwrap(),
        namespace: ScopeAtom::new("legal-research").unwrap(),
    }
}

fn engine(
    store: Arc<InMemoryStore<TestClock>>,
    objects: Arc<InMemoryObjectStore<TestClock>>,
    fixtures: &FixtureActions,
    instance_id: &str,
) -> WorkflowEngine<
    InMemoryStore<TestClock>,
    InMemoryObjectStore<TestClock>,
    dagger_workflow_core::action::InMemoryActionRegistry,
> {
    WorkflowEngine::new(
        store,
        objects,
        fixtures.registry(),
        EngineConfig {
            instance_id: id(instance_id),
            max_concurrency: 3,
        },
    )
    .unwrap()
}

async fn create_run(
    store: &InMemoryStore<TestClock>,
    objects: &InMemoryObjectStore<TestClock>,
    execution_scope: &ExecutionScope,
    reference: &PublishedReference,
    run_id: &str,
    question: &str,
) {
    let input = objects
        .put(
            execution_scope,
            &serde_jcs::to_vec(&json!({ "legal_question": question })).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    store
        .create_run(
            execution_scope,
            CreateRun {
                run_id: id(run_id),
                definition_id: reference.definition.definition_id.clone(),
                revision_hash: reference.revision_hash.clone(),
                input,
                budget_limit: CostUnits(500),
                limits: RunLimits {
                    max_dynamic_node_instances: 20,
                    max_total_attempts: 40,
                    max_total_events: 2_000,
                    max_inline_json_bytes_per_value: 100_000,
                    max_artifacts_per_attempt: 10,
                    max_aggregate_object_bytes_per_run: 1_000_000,
                    max_run_lifetime_ms: 100_000,
                },
                principal: principal(execution_scope, "w5-runner"),
                idempotency_token: format!("w5-create-token-{run_id}"),
            },
        )
        .await
        .unwrap();
}

async fn events(
    store: &InMemoryStore<TestClock>,
    execution_scope: &ExecutionScope,
    run_id: &str,
) -> Vec<dagger_workflow_core::event::WorkflowEvent> {
    store
        .list_events_after(
            execution_scope,
            &id(run_id),
            EventPageRequest {
                after_event_seq: 0,
                page_size: 1_000,
                hard_response_byte_limit: 4_000_000,
            },
        )
        .await
        .unwrap()
}

async fn attempt_counts(
    store: &InMemoryStore<TestClock>,
    execution_scope: &ExecutionScope,
    run_id: &str,
) -> BTreeMap<Id, u32> {
    store
        .list_nodes(
            execution_scope,
            &id(run_id),
            PageRequest {
                cursor: None,
                page_size: 100,
            },
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .filter(|node| node.status == NodeState::Succeeded && node.attempt_count > 0)
        .map(|node| (node.node_instance_id, node.attempt_count))
        .collect()
}

#[test]
fn repinned_legal_research_completes_both_choice_paths_and_recovers_without_replay() {
    block_on(async {
        let execution_scope = scope();
        let clock = Arc::new(TestClock::new(Timestamp(10_000)));
        let store = Arc::new(InMemoryStore::new(clock.clone()));
        let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let fixtures = FixtureActions::new();
        let reference =
            publish_legal_research_reference(&store, &objects, &execution_scope, &fixtures).await;

        // The default Choice edge must skip the second-round branch and reconverge at synthesis.
        create_run(
            &store,
            &objects,
            &execution_scope,
            &reference,
            "default-branch",
            "single-pass question",
        )
        .await;
        let default_engine = engine(
            store.clone(),
            objects.clone(),
            &fixtures,
            "w5-default-engine",
        );
        default_engine
            .acquire_scope(&execution_scope)
            .await
            .unwrap();
        default_engine
            .start(&execution_scope, &id("default-branch"))
            .await
            .unwrap();
        default_engine
            .run_until_idle(&execution_scope, 20)
            .await
            .unwrap();
        let default_run = store
            .get_run(&execution_scope, &id("default-branch"))
            .await
            .unwrap()
            .run;
        assert_eq!(default_run.status, RunState::Succeeded);
        assert!(default_run.output_ref.is_some());
        for node_id in [
            "generate_followup_queries",
            "search_followup_queries",
            "merge_second_round",
        ] {
            assert_eq!(
                store
                    .get_node(&execution_scope, &id("default-branch"), &id(node_id))
                    .await
                    .unwrap()
                    .status,
                NodeState::Skipped
            );
        }
        let default_events = events(&store, &execution_scope, "default-branch").await;
        assert!(default_events
            .iter()
            .any(|event| event.event_type == EventType::ChoiceSelected));
        assert!(default_events
            .iter()
            .any(|event| event.event_type == EventType::NodeSkipped));
        assert!(default_events
            .iter()
            .any(|event| event.event_type == EventType::RunSucceeded));
        assert_eq!(
            default_events
                .iter()
                .filter(|event| event.event_type == EventType::BudgetReserved)
                .count(),
            default_events
                .iter()
                .filter(|event| event.event_type == EventType::BudgetSettled)
                .count()
        );
        assert_eq!(default_run.budget_consumed, CostUnits(0));
        assert_eq!(default_run.budget_reserved, CostUnits(0));
        default_engine
            .release_scope(&execution_scope)
            .await
            .unwrap();

        // The selected edge executes the second round. Stop the first scheduler after durable
        // first-round work, expire its claim as if it died, then prove recovery does not replay it.
        create_run(
            &store,
            &objects,
            &execution_scope,
            &reference,
            "second-round",
            "needs-second-round question",
        )
        .await;
        let dead_engine = engine(store.clone(), objects.clone(), &fixtures, "w5-dead-engine");
        dead_engine.acquire_scope(&execution_scope).await.unwrap();
        dead_engine
            .start(&execution_scope, &id("second-round"))
            .await
            .unwrap();
        dead_engine
            .run_until_idle(&execution_scope, 4)
            .await
            .unwrap();
        let before_recovery = attempt_counts(&store, &execution_scope, "second-round").await;
        assert!(before_recovery.contains_key(&id("generate_initial_queries")));
        assert!(before_recovery.contains_key(&id("summarize_initial_evidence")));
        clock.advance_ms(20_001).unwrap();

        let recovery_engine = engine(
            store.clone(),
            objects.clone(),
            &fixtures,
            "w5-recovery-engine",
        );
        recovery_engine
            .acquire_scope(&execution_scope)
            .await
            .unwrap();
        assert_eq!(
            attempt_counts(&store, &execution_scope, "second-round").await,
            before_recovery,
            "claim takeover/recovery must not rerun completed nodes"
        );
        recovery_engine
            .run_until_idle(&execution_scope, 20)
            .await
            .unwrap();
        let recovered_run = store
            .get_run(&execution_scope, &id("second-round"))
            .await
            .unwrap()
            .run;
        assert_eq!(recovered_run.status, RunState::Succeeded);
        assert!(recovered_run.output_ref.is_some());
        for node_id in [
            "generate_followup_queries",
            "search_followup_queries",
            "merge_second_round",
            "synthesize_report",
            "validate_citations",
        ] {
            assert_eq!(
                store
                    .get_node(&execution_scope, &id("second-round"), &id(node_id))
                    .await
                    .unwrap()
                    .status,
                NodeState::Succeeded
            );
        }
        let second_events = events(&store, &execution_scope, "second-round").await;
        assert!(second_events
            .iter()
            .any(|event| event.event_type == EventType::ChoiceSelected));
        assert!(second_events
            .iter()
            .any(|event| event.event_type == EventType::MapExpanded));
        assert!(second_events
            .iter()
            .any(|event| event.event_type == EventType::RunSucceeded));
        assert_eq!(
            second_events
                .iter()
                .filter(|event| event.event_type == EventType::BudgetReserved)
                .count(),
            second_events
                .iter()
                .filter(|event| event.event_type == EventType::BudgetSettled)
                .count()
        );
        assert_eq!(recovered_run.budget_consumed, CostUnits(0));
        assert_eq!(recovered_run.budget_reserved, CostUnits(0));
    });
}
