#![cfg(feature = "sqlite")]
//! Acceptance tests for durable approval gates.
//!
//! The implemented contract is in `src/approval.rs`, `src/run.rs`, and the
//! `WorkflowStore` approval commands. The host rules are in
//! `docs/system/operations-and-limits.md`.
//!
//! Two deliberate choices, both mirroring `tests/w10_budgets.rs`.
//!
//! First, every fixture runs against both store implementations through the
//! `WorkflowStore` trait. `src/memory/mod.rs` and `src/sqlite/reducer.rs` are
//! textually near-identical for the approval commands, which means agreement
//! between them is worth nothing as evidence of correctness -- it only proves
//! the copy is faithful. The value of running both is catching the case where
//! one copy is later edited and the other is not.
//!
//! Second, the negative fixtures assert on the *state that did not change*,
//! not merely on the returned error. Unauthorized input cannot win or perturb
//! the race. An error return with a silently
//! bumped gate version would satisfy a naive assertion while breaking the
//! first-valid-decision-wins property that the whole workstream rests on. So
//! every rejection fixture captures the gate version, the run version, and the
//! event high-water mark before the call and requires all three unchanged.

use dagger_workflow_core::action::CompatibilityReport;
use dagger_workflow_core::approval::{
    canonical_expiry_approval_result, canonical_human_approval_result, ApprovalDecision,
    ApprovalExpiryPolicy, ApprovalGate, ApprovalResolutionSource, AuthenticatedPrincipal,
    DecisionAuthorizationPolicy,
};
use dagger_workflow_core::artifact::{ObjectStore, VerifiedObjectRef};
use dagger_workflow_core::definition::{
    ApprovalGateConfig, BindingSource, NodeDefinition, PublishableDefinition, WorkflowDefinition,
};
use dagger_workflow_core::engine::TestClock;
use dagger_workflow_core::ids::{
    CostUnits, Digest, Id, NodeInstanceId, Timestamp, TopologicalRank, Version,
};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::run::{
    GateState, NodeFailureKind, NodeState, RunFailureKind, RunLimits, RunState,
};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::sqlite::SqliteWorkflowStore;
use dagger_workflow_core::store::{
    CancelRun, CreateDefinition, CreateRun, DecideApproval, EnginePermit, EventPageRequest,
    ExpectedGateVersion, ExpireApproval, PublishRevision, RequestApproval, StartRun, StoreError,
    WorkflowStore,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::TempDir;

const SCHEMA: &[u8] = br#"{"additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"],"type":"object"}"#;
const RUN_INPUT: &[u8] = br#"{"value":1}"#;
const CLOCK_ORIGIN: Timestamp = Timestamp(1_000_000);
/// `expires_at` is computed from DB time at request.
/// Deliberately shorter than the 20s engine-claim lifetime so one claim
/// acquired at seed time is still live when the gate falls due.
const EXPIRES_AFTER_MS: u64 = 10_000;
const APPROVER: &str = "approver";
const ROLE: &str = "release-manager";

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}

fn node_id(value: &str) -> NodeInstanceId {
    Id::new(value).unwrap()
}

fn scope(tenant: &str) -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new(tenant).unwrap(),
        namespace: ScopeAtom::new("w9").unwrap(),
    }
}

fn limits() -> RunLimits {
    RunLimits {
        max_dynamic_node_instances: 8,
        max_total_attempts: 8,
        max_total_events: 1_000,
        max_inline_json_bytes_per_value: 100_000,
        max_artifacts_per_attempt: 8,
        max_aggregate_object_bytes_per_run: 1_000_000,
        max_run_lifetime_ms: 10_000_000,
    }
}

#[derive(Clone)]
struct Fixture {
    scope: ExecutionScope,
    /// The principal named in the gate's `allowed_principal_ids`.
    principal: AuthenticatedPrincipal,
    revision_hash: Digest,
    evidence_digest: Digest,
    input: VerifiedObjectRef,
    /// One claim per scope: `acquire_engine_claim` is exclusive, so fixtures
    /// that open several gates must share the permit rather than re-acquire.
    permit: EnginePermit,
}

/// A run driven all the way to a durable Pending gate.
struct Opened {
    run_id: Id,
    gate: ApprovalGate,
    run_version: Version,
}

/// Runs one fixture body against both store implementations.
///
/// The `advance` argument is not a convenience. The two stores keep time in
/// different places: the in-memory reducer reads the injected `TestClock`,
/// while the SQLite store's authoritative time is SQLite's own `now` plus a
/// durable `clock_offset_ms` column. Process clocks cannot control approval
/// expiry. A fixture that advanced only the `TestClock`
/// would silently prove nothing on the SQLite side.
macro_rules! both_stores {
    ($body:ident) => {{
        let clock = Arc::new(TestClock::new(CLOCK_ORIGIN));
        let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let store = InMemoryStore::new(clock.clone());
        {
            let ticker = clock.clone();
            $body(
                &store,
                &objects,
                move |ms| {
                    let ticker = ticker.clone();
                    async move {
                        ticker.advance_ms(ms).unwrap();
                    }
                },
                "memory",
            )
            .await;
        }

        let clock = Arc::new(TestClock::new(CLOCK_ORIGIN));
        let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
        let store =
            SqliteWorkflowStore::open_url("sqlite::memory:", clock.clone(), objects.clone())
                .await
                .unwrap();
        {
            let ticker = &store;
            $body(
                &store,
                &objects,
                move |ms| async move {
                    ticker.advance_database_clock_ms(ms).await.unwrap();
                },
                "sqlite",
            )
            .await;
        }
    }};
}

/// Publishes `approval -> succeed`, with a gate policy that names one
/// principal ID and one role ID. Both allowlist arms matter. The policy
/// authorizes on principal ID *or* role membership, so a fixture that only
/// exercised the principal arm could not tell an implemented role check from a
/// missing one.
async fn seed<S: WorkflowStore>(
    store: &S,
    objects: &Arc<InMemoryObjectStore<TestClock>>,
    tenant: &str,
    on_expiry: ApprovalExpiryPolicy,
) -> Fixture {
    let workflow_scope = scope(tenant);
    let schema = objects
        .put(&workflow_scope, SCHEMA, "application/json")
        .await
        .unwrap();
    let principal = principal_in(&workflow_scope, APPROVER, Vec::new(), schema.digest());
    let definition_id = id("definition");
    store
        .create_definition(
            &workflow_scope,
            CreateDefinition {
                definition_id: definition_id.clone(),
                display_name: "w9 approval fixture".to_owned(),
                description: String::new(),
                principal: principal.clone(),
            },
        )
        .await
        .unwrap();
    let definition = WorkflowDefinition {
        definition_format_version: "0.1".to_owned(),
        definition_id: definition_id.clone(),
        name: "w9-fixture".to_owned(),
        description: String::new(),
        run_input_schema_digest: schema.digest().clone(),
        run_output_schema_digest: schema.digest().clone(),
        entry_node_id: id("approval"),
        nodes: vec![
            NodeDefinition::Approval {
                id: id("approval"),
                request: BindingSource::RunInput {
                    pointer: String::new(),
                },
                gate: ApprovalGateConfig {
                    expires_after_ms: EXPIRES_AFTER_MS,
                    on_expiry,
                    authorization: DecisionAuthorizationPolicy {
                        allowed_principal_ids: vec![APPROVER.to_owned()],
                        allowed_role_ids: vec![ROLE.to_owned()],
                    },
                },
                next: vec![id("succeed")],
            },
            NodeDefinition::Succeed {
                id: id("succeed"),
                output: BindingSource::NodeOutput {
                    node_id: id("approval"),
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
    let ranks = BTreeMap::from([
        (id("approval"), TopologicalRank(0)),
        (id("succeed"), TopologicalRank(1)),
    ]);
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
        .put(&workflow_scope, RUN_INPUT, "application/json")
        .await
        .unwrap();
    let claim = store
        .acquire_engine_claim(&workflow_scope, id("w9-engine"))
        .await
        .unwrap();
    Fixture {
        scope: workflow_scope,
        principal,
        revision_hash: canonical.digest().clone(),
        evidence_digest: schema.digest().clone(),
        input,
        permit: claim.permit,
    }
}

fn principal_in(
    principal_scope: &ExecutionScope,
    principal_id: &str,
    roles: Vec<String>,
    context: &Digest,
) -> AuthenticatedPrincipal {
    AuthenticatedPrincipal::mint(
        principal_scope.clone(),
        principal_id.to_owned(),
        roles,
        context.clone(),
    )
    .unwrap()
}

/// create_run -> claim -> start_run -> request_approval.
async fn open_gate<S: WorkflowStore>(
    store: &S,
    objects: &Arc<InMemoryObjectStore<TestClock>>,
    fixture: &Fixture,
    run: &str,
) -> Opened {
    let run_id = id(run);
    store
        .create_run(
            &fixture.scope,
            CreateRun {
                run_id: run_id.clone(),
                definition_id: id("definition"),
                revision_hash: fixture.revision_hash.clone(),
                input: fixture.input.clone(),
                budget_limit: CostUnits(100),
                limits: limits(),
                principal: fixture.principal.clone(),
                idempotency_token: format!("create-{run}-token-long-enough-0123456789"),
            },
        )
        .await
        .unwrap();
    store
        .start_run(
            &fixture.scope,
            StartRun {
                permit: fixture.permit.clone(),
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
    let node = store
        .get_node(&fixture.scope, &run_id, &node_id("approval"))
        .await
        .unwrap();
    let request = objects
        .put(&fixture.scope, br#"{"request":true}"#, "application/json")
        .await
        .unwrap();
    let gate = store
        .request_approval(
            &fixture.scope,
            RequestApproval {
                permit: fixture.permit.clone(),
                run_id: run_id.clone(),
                node_id: node_id("approval"),
                expected_node_version: node.version,
                gate_id: id("w9-gate"),
                request,
            },
        )
        .await
        .unwrap();
    assert_eq!(gate.status, GateState::Pending);
    let run_version = store
        .get_run(&fixture.scope, &run_id)
        .await
        .unwrap()
        .run
        .version;
    Opened {
        run_id,
        gate,
        run_version,
    }
}

/// The (gate version, run version, last event seq) triple that a rejected
/// command must leave untouched. unauthorized or losing
/// input "cannot win or perturb the race".
async fn watermark<S: WorkflowStore>(
    store: &S,
    fixture: &Fixture,
    run_id: &Id,
    gate_id: &Id,
) -> (Version, Version, u64) {
    let gate = store
        .get_gate(&fixture.scope, run_id, gate_id)
        .await
        .unwrap();
    let run = store.get_run(&fixture.scope, run_id).await.unwrap().run;
    (
        gate.version,
        run.version,
        last_event_seq(store, fixture, run_id).await,
    )
}

async fn last_event_seq<S: WorkflowStore>(store: &S, fixture: &Fixture, run_id: &Id) -> u64 {
    let mut after = 0;
    loop {
        let page = store
            .list_events_after(
                &fixture.scope,
                run_id,
                EventPageRequest {
                    after_event_seq: after,
                    page_size: 500,
                    hard_response_byte_limit: 4_000_000,
                },
            )
            .await
            .unwrap();
        match page.last() {
            None => return after,
            Some(event) => after = event.event_seq,
        }
    }
}

fn approve(
    opened: &Opened,
    principal: &AuthenticatedPrincipal,
    output: VerifiedObjectRef,
) -> DecideApproval {
    DecideApproval {
        run_id: opened.run_id.clone(),
        gate_id: opened.gate.gate_id.clone(),
        expected_run_version: opened.run_version,
        expected_gate_version: opened.gate.version,
        decision: ApprovalDecision::Approve,
        decision_payload: None,
        approval_output: Some(output),
        principal: principal.clone(),
    }
}

/// The exact canonical human envelope for a no-payload approval by `principal`.
async fn canonical_output(
    objects: &Arc<InMemoryObjectStore<TestClock>>,
    fixture: &Fixture,
    principal: &AuthenticatedPrincipal,
) -> VerifiedObjectRef {
    objects
        .put(
            &fixture.scope,
            &canonical_human_approval_result(None, principal),
            "application/json",
        )
        .await
        .unwrap()
}

/// The durable state that an `ApprovalPayloadInvalid` refusal must leave behind.
///
/// `ContractValidationApplied` is an *applied*
/// error: unlike every other rejection in this file, it does not mean "nothing
/// happened", it means the transaction committed N67 (`WaitingApproval` ->
/// `ContractFailed`) together with the R08 run cascade and the G06 cancellation
/// of the still-Pending gate. Asserting only the error code would pass equally
/// against a store that rolled the whole transaction back, which is exactly the
/// bug this file previously could not see. So every site that observes the code
/// asserts the applied state, and any gate probed this way is terminal
/// afterwards and cannot be reused.
async fn assert_payload_invalid_applied<S: WorkflowStore>(
    store: &S,
    fixture: &Fixture,
    opened: &Opened,
    label: &str,
) {
    let node = store
        .get_node(&fixture.scope, &opened.run_id, &node_id("approval"))
        .await
        .unwrap();
    assert_eq!(
        node.status,
        NodeState::ContractFailed,
        "{label}: N67 did not apply to the approval node"
    );
    assert_eq!(
        node.failure_kind,
        Some(NodeFailureKind::ApprovalPayloadInvalid),
        "{label}"
    );
    assert!(
        node.result_ref.is_none(),
        "{label}: a refused envelope was committed as the node result"
    );
    let gate = store
        .get_gate(&fixture.scope, &opened.run_id, &opened.gate.gate_id)
        .await
        .unwrap();
    assert_eq!(
        gate.status,
        GateState::Cancelled,
        "{label}: G06 did not cancel the Pending gate"
    );
    assert!(
        gate.decision_fingerprint.is_none(),
        "{label}: a refused decision was fingerprinted"
    );
    let run = store
        .get_run(&fixture.scope, &opened.run_id)
        .await
        .unwrap()
        .run;
    assert_eq!(
        run.status,
        RunState::ContractFailed,
        "{label}: R08 did not apply to the run"
    );
    assert_eq!(
        run.failure_kind,
        Some(RunFailureKind::ApprovalPayloadInvalid),
        "{label}"
    );
}

// -------------------------------------------------------------------------
// Authorization. Highest priority: an unauthorized caller must not be able to
// move any CAS, because that is the only thing standing between a durable gate
// and an arbitrary host request.
// -------------------------------------------------------------------------

/// `decide_approval` first validates that the capability
/// was minted for the gate's `ExecutionScope`; "a capability minted for scope B
/// is structurally invalid in scope A". This fixture mints the capability with
/// the *same principal ID that the policy allows*, so passing the policy check
/// is not enough -- only an actual scope comparison rejects it. That is what
/// makes it a scope fixture rather than a second authorization fixture.
#[tokio::test]
async fn cross_scope_principal_cannot_perturb_the_gate() {
    async fn body<S: WorkflowStore, F: Fn(i64) -> Fut, Fut: std::future::Future<Output = ()>>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _advance: F,
        label: &str,
    ) {
        let fixture = seed(store, objects, "cross-scope", ApprovalExpiryPolicy::Reject).await;
        let opened = open_gate(store, objects, &fixture, "run").await;
        let before = watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await;

        // Same principal ID, same roles, same authentication context; only the
        // bound scope differs.
        let foreign = principal_in(
            &scope("other-tenant"),
            APPROVER,
            Vec::new(),
            fixture.principal.authentication_context_digest(),
        );
        let output = canonical_output(objects, &fixture, &foreign).await;
        let error = store
            .decide_approval(&fixture.scope, approve(&opened, &foreign, output))
            .await
            .unwrap_err();
        assert!(
            matches!(error, StoreError::ApprovalUnauthorized),
            "{label}: cross-scope capability must be structurally invalid, got {error:?}"
        );

        let after = watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await;
        assert_eq!(before, after, "{label}: rejected decision perturbed state");
        let gate = store
            .get_gate(&fixture.scope, &opened.run_id, &opened.gate.gate_id)
            .await
            .unwrap();
        assert_eq!(gate.status, GateState::Pending, "{label}");
        assert!(gate.deciding_principal.is_none(), "{label}");
        assert!(gate.decision_fingerprint.is_none(), "{label}");
    }
    both_stores!(body);
}

/// the immutable policy authorizes on principal ID or on
/// role membership. Three arms, so a check that collapsed to "always true" or
/// "always false" fails either the negative or the positive arm.
#[tokio::test]
async fn only_a_policy_satisfying_principal_can_decide() {
    async fn body<S: WorkflowStore, F: Fn(i64) -> Fut, Fut: std::future::Future<Output = ()>>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _advance: F,
        label: &str,
    ) {
        let fixture = seed(store, objects, "authz", ApprovalExpiryPolicy::Reject).await;
        let opened = open_gate(store, objects, &fixture, "run").await;
        let before = watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await;

        // Arm 1: correct scope, unlisted principal ID, no roles at all.
        let stranger = principal_in(
            &fixture.scope,
            "not-the-approver",
            Vec::new(),
            fixture.principal.authentication_context_digest(),
        );
        let output = canonical_output(objects, &fixture, &stranger).await;
        let error = store
            .decide_approval(&fixture.scope, approve(&opened, &stranger, output))
            .await
            .unwrap_err();
        assert!(
            matches!(error, StoreError::ApprovalUnauthorized),
            "{label}: unlisted principal decided the gate, got {error:?}"
        );

        // Arm 2: unlisted principal ID carrying a role that is not the allowed
        // one. This is the arm that catches a role check written as "the
        // principal has any role".
        let wrong_role = principal_in(
            &fixture.scope,
            "not-the-approver",
            vec!["auditor".to_owned()],
            fixture.principal.authentication_context_digest(),
        );
        let output = canonical_output(objects, &fixture, &wrong_role).await;
        let error = store
            .decide_approval(&fixture.scope, approve(&opened, &wrong_role, output))
            .await
            .unwrap_err();
        assert!(
            matches!(error, StoreError::ApprovalUnauthorized),
            "{label}: wrong role decided the gate, got {error:?}"
        );

        let after = watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await;
        assert_eq!(
            before, after,
            "{label}: unauthorized decisions perturbed gate/run/event state"
        );

        // Arm 3 (positive control): an unlisted principal ID whose role is in
        // the allowlist must succeed. Without this the two negatives above are
        // also satisfied by an implementation that rejects everything.
        let by_role = principal_in(
            &fixture.scope,
            "release-bot",
            vec![ROLE.to_owned()],
            fixture.principal.authentication_context_digest(),
        );
        let output = canonical_output(objects, &fixture, &by_role).await;
        let gate = store
            .decide_approval(&fixture.scope, approve(&opened, &by_role, output))
            .await
            .unwrap();
        assert_eq!(gate.status, GateState::Approved, "{label}");
        assert_eq!(
            gate.deciding_principal.as_deref(),
            Some("release-bot"),
            "{label}"
        );
    }
    both_stores!(body);
}

// -------------------------------------------------------------------------
// Idempotency and fail-closed conflict. "Exactly
// identical replay of G02/G03, determined by `decision_fingerprint`, returns
// the existing decision and emits no event. A different decision, payload,
// approval output, authenticated principal, or authentication-context digest
// returns `ApprovalAlreadyResolved`."
// -------------------------------------------------------------------------

#[tokio::test]
async fn identical_replay_is_a_no_op_and_any_difference_fails_closed() {
    async fn body<S: WorkflowStore, F: Fn(i64) -> Fut, Fut: std::future::Future<Output = ()>>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _advance: F,
        label: &str,
    ) {
        let fixture = seed(store, objects, "replay", ApprovalExpiryPolicy::Reject).await;
        let opened = open_gate(store, objects, &fixture, "run").await;
        let output = canonical_output(objects, &fixture, &fixture.principal).await;

        let first = store
            .decide_approval(
                &fixture.scope,
                approve(&opened, &fixture.principal, output.clone()),
            )
            .await
            .unwrap();
        assert_eq!(first.status, GateState::Approved, "{label}");
        let settled = watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await;

        // Byte-identical replay, including the now-stale observed versions: the
        // fingerprint short-circuit in section 3.5 runs before the version CAS,
        // so a retrying host does not need to re-read.
        let replay = store
            .decide_approval(
                &fixture.scope,
                approve(&opened, &fixture.principal, output.clone()),
            )
            .await
            .unwrap();
        assert_eq!(
            replay, first,
            "{label}: replay returned a different receipt"
        );
        assert_eq!(
            watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await,
            settled,
            "{label}: replay emitted a second event batch or bumped a version"
        );

        // Conflict 1: same principal ID and same decision, different
        // authentication-context digest. puts that digest
        // in the fingerprint precisely so a re-authenticated session cannot
        // pass as the committed decision.
        let other_context = principal_in(
            &fixture.scope,
            APPROVER,
            Vec::new(),
            output.digest(), // any digest that is not the seeded context
        );
        let other_output = canonical_output(objects, &fixture, &other_context).await;
        let error = store
            .decide_approval(
                &fixture.scope,
                approve(&opened, &other_context, other_output),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, StoreError::ApprovalAlreadyResolved),
            "{label}: changed authentication context was accepted as replay, got {error:?}"
        );

        // Conflict 2: the opposite decision from the same principal.
        let error = store
            .decide_approval(
                &fixture.scope,
                DecideApproval {
                    decision: ApprovalDecision::Reject,
                    approval_output: None,
                    ..approve(&opened, &fixture.principal, output)
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, StoreError::ApprovalAlreadyResolved),
            "{label}: conflicting decision was not fail-closed, got {error:?}"
        );

        assert_eq!(
            watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await,
            settled,
            "{label}: a conflicting decision perturbed the committed gate"
        );
    }
    both_stores!(body);
}

// -------------------------------------------------------------------------
// ApprovalResult byte exactness. the engine constructs
// the expected canonical bytes and `decide_approval` verifies byte-for-byte
// equality, media type, size, and digest before G02.
// -------------------------------------------------------------------------

/// The stored `result_ref` must be the exact engine-owned envelope, and the
/// envelope must be bound to the deciding principal and the decision payload.
/// The payload arm is the load-bearing one: `payload_ref` is derived from the
/// artifact the store registers, so an envelope built against any other ref
/// value cannot be constructed by the caller.
#[tokio::test]
async fn approved_output_is_the_exact_canonical_envelope() {
    async fn body<S: WorkflowStore, F: Fn(i64) -> Fut, Fut: std::future::Future<Output = ()>>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        _advance: F,
        label: &str,
    ) {
        let fixture = seed(store, objects, "envelope", ApprovalExpiryPolicy::Reject).await;

        // Arm 1: an envelope naming a different principal than the deciding
        // one. Byte-for-byte this is a valid ApprovalResult; only the binding
        // to the caller's identity rejects it.
        let opened = open_gate(store, objects, &fixture, "run-impostor").await;
        let impostor = principal_in(
            &fixture.scope,
            "release-bot",
            vec![ROLE.to_owned()],
            fixture.principal.authentication_context_digest(),
        );
        let mismatched = canonical_output(objects, &fixture, &impostor).await;
        let error = store
            .decide_approval(
                &fixture.scope,
                approve(&opened, &fixture.principal, mismatched),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&error, StoreError::ContractValidationApplied { code } if code == "ApprovalPayloadInvalid"),
            "{label}: envelope naming another principal was accepted, got {error:?}"
        );
        // N67 is an applied error, not a rollback: the refusal must durably
        // fail the node, cancel the gate, and fail the run.
        assert_payload_invalid_applied(store, &fixture, &opened, label).await;

        // Arm 2: a single altered byte inside an otherwise canonical envelope.
        let opened = open_gate(store, objects, &fixture, "run-altered").await;
        let mut altered_bytes = canonical_human_approval_result(None, &fixture.principal);
        let human = altered_bytes
            .windows(7)
            .position(|window| window == b"\"human\"")
            .expect("canonical envelope carries source=human");
        altered_bytes[human + 1] = b'H';
        let altered = objects
            .put(&fixture.scope, &altered_bytes, "application/json")
            .await
            .unwrap();
        let error = store
            .decide_approval(
                &fixture.scope,
                approve(&opened, &fixture.principal, altered),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&error, StoreError::ContractValidationApplied { code } if code == "ApprovalPayloadInvalid"),
            "{label}: altered envelope bytes were accepted, got {error:?}"
        );
        assert_payload_invalid_applied(store, &fixture, &opened, label).await;

        // Arm 3: a decision payload is supplied but the envelope carries
        // `payload_ref: null`. requires the human
        // envelope to use "the exact decision-payload ArtifactRef value or
        // null", so this must fail closed. It is the arm that proves the
        // envelope is bound to the *registered* payload ref rather than merely
        // being a well-formed ApprovalResult.
        let opened = open_gate(store, objects, &fixture, "run-payload").await;
        let payload = objects
            .put(&fixture.scope, br#"{"note":"ship it"}"#, "application/json")
            .await
            .unwrap();
        let null_ref = canonical_output(objects, &fixture, &fixture.principal).await;
        let error = store
            .decide_approval(
                &fixture.scope,
                DecideApproval {
                    decision_payload: Some(payload),
                    ..approve(&opened, &fixture.principal, null_ref)
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&error, StoreError::ContractValidationApplied { code } if code == "ApprovalPayloadInvalid"),
            "{label}: envelope omitting the decision payload ref was accepted, got {error:?}"
        );
        assert_payload_invalid_applied(store, &fixture, &opened, label).await;

        // Arm 4: the accepted path. The committed `result_ref` must be exactly
        // the canonical bytes, by digest and by size.
        let opened = open_gate(store, objects, &fixture, "run-good").await;
        let expected_bytes = canonical_human_approval_result(None, &fixture.principal);
        let expected = objects
            .put(&fixture.scope, &expected_bytes, "application/json")
            .await
            .unwrap();
        let gate = store
            .decide_approval(
                &fixture.scope,
                approve(&opened, &fixture.principal, expected.clone()),
            )
            .await
            .unwrap();
        assert_eq!(gate.status, GateState::Approved, "{label}");
        let node = store
            .get_node(&fixture.scope, &opened.run_id, &node_id("approval"))
            .await
            .unwrap();
        let result = node.result_ref.expect("approved node has a result");
        assert_eq!(
            result.0.digest,
            *expected.digest(),
            "{label}: committed result_ref is not the canonical envelope"
        );
        assert_eq!(
            result.0.size_bytes,
            expected_bytes.len() as u64,
            "{label}: committed envelope size differs from the canonical bytes"
        );
        // the approval-output digest participates in the
        // decision fingerprint, so an approve can never carry the null tag.
        assert!(gate.decision_fingerprint.is_some(), "{label}");
    }
    both_stores!(body);
}

// -------------------------------------------------------------------------
// Expiry. "Process wall clocks and monotonic clocks are
// never used for ownership, expiry, retry eligibility, attempt deadlines, or
// approval expiry."
// -------------------------------------------------------------------------

/// The expiry boundary asserted from both sides, `slack` milliseconds apart.
///
/// This one property cannot use `both_stores!`, because the two stores keep
/// time differently and the difference is exactly what the fixture measures.
/// The in-memory reducer reads a fixed `TestClock`, so the boundary can be
/// probed at one millisecond -- tight enough to catch a `>` written where
/// the rule is `DB-now >= expires_at`. The SQLite store's authoritative
/// time is real Unix time plus a durable offset column, so a one-millisecond
/// probe there would only measure how long the test itself took to run. It
/// gets a coarse margin and proves the weaker but still real claim that a gate
/// is not due before its deadline.
async fn expiry_boundary_body<
    S: WorkflowStore,
    F: Fn(i64) -> Fut,
    Fut: std::future::Future<Output = ()>,
>(
    store: &S,
    objects: &Arc<InMemoryObjectStore<TestClock>>,
    advance: F,
    label: &str,
    slack: i64,
) {
    let fixture = seed(store, objects, "expiry-clock", ApprovalExpiryPolicy::Reject).await;
    let opened = open_gate(store, objects, &fixture, "run").await;
    // `expires_at` is "computed from DB time". The
    // approval node's `updated_at` is stamped from the same in-transaction
    // `now`, so their difference is exactly the configured lifetime -- an
    // assertion that holds whichever clock the store reads, and that fails if
    // expiry were ever seeded from a host-supplied timestamp.
    let requested_at = store
        .get_node(&fixture.scope, &opened.run_id, &node_id("approval"))
        .await
        .unwrap()
        .updated_at;
    assert_eq!(
        opened.gate.expires_at.0 - requested_at.0,
        EXPIRES_AFTER_MS as i64,
        "{label}: expires_at was not computed from the in-transaction store clock"
    );
    let expire = || ExpireApproval {
        permit: fixture.permit.clone(),
        run_id: opened.run_id.clone(),
        gate_id: opened.gate.gate_id.clone(),
        approval_output: None,
    };

    // Before the deadline: not due, and nothing moves.
    advance(EXPIRES_AFTER_MS as i64 - slack).await;
    let before = watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await;
    let error = store
        .expire_approval(&fixture.scope, expire())
        .await
        .unwrap_err();
    assert!(
        matches!(error, StoreError::ExpiryNotDue { .. }),
        "{label}: gate expired early, got {error:?}"
    );
    assert_eq!(
        watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await,
        before,
        "{label}: a not-due expiry perturbed state"
    );

    // At or past the deadline: due. The rule is `DB-now >= expires_at`.
    advance(slack).await;
    let gate = store
        .expire_approval(&fixture.scope, expire())
        .await
        .unwrap();
    assert_eq!(gate.status, GateState::ExpiredRejected, "{label}");
    assert_eq!(
        gate.resolution_source,
        Some(ApprovalResolutionSource::Expiry),
        "{label}"
    );
}

#[tokio::test]
async fn expiry_is_due_only_at_the_exact_store_clock_boundary_in_memory() {
    let clock = Arc::new(TestClock::new(CLOCK_ORIGIN));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = InMemoryStore::new(clock.clone());
    let ticker = clock.clone();
    expiry_boundary_body(
        &store,
        &objects,
        move |ms| {
            let ticker = ticker.clone();
            async move {
                ticker.advance_ms(ms).unwrap();
            }
        },
        "memory",
        1,
    )
    .await;
}

#[tokio::test]
async fn expiry_is_not_due_before_the_database_deadline_in_sqlite() {
    let clock = Arc::new(TestClock::new(CLOCK_ORIGIN));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));
    let store = SqliteWorkflowStore::open_url("sqlite::memory:", clock.clone(), objects.clone())
        .await
        .unwrap();
    let ticker = &store;
    expiry_boundary_body(
        &store,
        &objects,
        move |ms| async move {
            ticker.advance_database_clock_ms(ms).await.unwrap();
        },
        "sqlite",
        2_000,
    )
    .await;
}

/// `on_expiry` selects the terminal behaviour,
/// and the approve arm must supply the canonical *expiry* envelope, which
/// differs from the human envelope in `source` and `principal`.
#[tokio::test]
async fn on_expiry_selects_the_terminal_behaviour() {
    async fn body<S: WorkflowStore, F: Fn(i64) -> Fut, Fut: std::future::Future<Output = ()>>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        advance: F,
        label: &str,
    ) {
        // Reject arm: node Failed, run Failed with ApprovalExpiredRejected.
        let rejecting = seed(
            store,
            objects,
            "expiry-reject",
            ApprovalExpiryPolicy::Reject,
        )
        .await;
        let opened = open_gate(store, objects, &rejecting, "run").await;
        advance(EXPIRES_AFTER_MS as i64).await;
        store
            .expire_approval(
                &rejecting.scope,
                ExpireApproval {
                    permit: rejecting.permit.clone(),
                    run_id: opened.run_id.clone(),
                    gate_id: opened.gate.gate_id.clone(),
                    approval_output: None,
                },
            )
            .await
            .unwrap();
        let node = store
            .get_node(&rejecting.scope, &opened.run_id, &node_id("approval"))
            .await
            .unwrap();
        assert_eq!(node.status, NodeState::Failed, "{label}");
        let run = store
            .get_run(&rejecting.scope, &opened.run_id)
            .await
            .unwrap()
            .run;
        assert_eq!(run.status, RunState::Failed, "{label}");
        assert_eq!(
            run.failure_kind,
            Some(RunFailureKind::ApprovalExpiredRejected),
            "{label}"
        );

        // Approve arm: node Succeeded carrying the canonical expiry envelope,
        // and the downstream Succeed node is reached, which is the frontier
        // half of N14.
        let approving = seed(
            store,
            objects,
            "expiry-approve",
            ApprovalExpiryPolicy::Approve,
        )
        .await;
        let opened = open_gate(store, objects, &approving, "run").await;
        // The wrong-envelope probe gets its own run and gate. The state rule
        // makes an invalid envelope terminal for the gate and the run, so it
        // cannot share a gate with the behaviour assertions below.
        let forged = open_gate(store, objects, &approving, "run-human-envelope").await;
        // A *human* envelope must not satisfy the expiry path.
        let human = canonical_output(objects, &approving, &approving.principal).await;
        let error = store
            .expire_approval(
                &approving.scope,
                ExpireApproval {
                    permit: approving.permit.clone(),
                    run_id: forged.run_id.clone(),
                    gate_id: forged.gate.gate_id.clone(),
                    approval_output: Some(human),
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&error, StoreError::ExpiryNotDue { .. }),
            "{label}: expiry ran before its deadline, got {error:?}"
        );
        advance(EXPIRES_AFTER_MS as i64).await;
        let human = canonical_output(objects, &approving, &approving.principal).await;
        let error = store
            .expire_approval(
                &approving.scope,
                ExpireApproval {
                    permit: approving.permit.clone(),
                    run_id: forged.run_id.clone(),
                    gate_id: forged.gate.gate_id.clone(),
                    approval_output: Some(human),
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&error, StoreError::ContractValidationApplied { code } if code == "ApprovalPayloadInvalid"),
            "{label}: human envelope satisfied the expiry path, got {error:?}"
        );
        assert_payload_invalid_applied(store, &approving, &forged, label).await;

        let expiry_bytes = canonical_expiry_approval_result();
        let expiry = objects
            .put(&approving.scope, &expiry_bytes, "application/json")
            .await
            .unwrap();
        let gate = store
            .expire_approval(
                &approving.scope,
                ExpireApproval {
                    permit: approving.permit.clone(),
                    run_id: opened.run_id.clone(),
                    gate_id: opened.gate.gate_id.clone(),
                    approval_output: Some(expiry.clone()),
                },
            )
            .await
            .unwrap();
        assert_eq!(gate.status, GateState::ExpiredApproved, "{label}");
        let node = store
            .get_node(&approving.scope, &opened.run_id, &node_id("approval"))
            .await
            .unwrap();
        assert_eq!(node.status, NodeState::Succeeded, "{label}");
        assert_eq!(
            node.result_ref.expect("expiry result").0.digest,
            *expiry.digest(),
            "{label}"
        );
        let downstream = store
            .get_node(&approving.scope, &opened.run_id, &node_id("succeed"))
            .await
            .unwrap();
        assert_eq!(
            downstream.status,
            NodeState::Ready,
            "{label}: N14 did not run the frontier reducer"
        );
    }
    both_stores!(body);
}

// -------------------------------------------------------------------------
// First-valid-decision-wins. the loser of the Gate
// Pending CAS gets `ApprovalRaceLost` (expiry/cancellation) or
// `ApprovalAlreadyResolved` (a differing human decision); the winner is chosen
// by the store transaction, not by the caller.
// -------------------------------------------------------------------------

#[tokio::test]
async fn first_valid_resolution_wins_against_expiry_and_cancellation() {
    async fn body<S: WorkflowStore, F: Fn(i64) -> Fut, Fut: std::future::Future<Output = ()>>(
        store: &S,
        objects: &Arc<InMemoryObjectStore<TestClock>>,
        advance: F,
        label: &str,
    ) {
        // Human decision first, expiry arrives late and loses.
        let fixture = seed(store, objects, "race-human", ApprovalExpiryPolicy::Approve).await;
        let opened = open_gate(store, objects, &fixture, "run").await;
        let output = canonical_output(objects, &fixture, &fixture.principal).await;
        store
            .decide_approval(&fixture.scope, approve(&opened, &fixture.principal, output))
            .await
            .unwrap();
        let settled = watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await;
        advance(EXPIRES_AFTER_MS as i64).await;
        let expiry = objects
            .put(
                &fixture.scope,
                &canonical_expiry_approval_result(),
                "application/json",
            )
            .await
            .unwrap();
        let error = store
            .expire_approval(
                &fixture.scope,
                ExpireApproval {
                    permit: fixture.permit.clone(),
                    run_id: opened.run_id.clone(),
                    gate_id: opened.gate.gate_id.clone(),
                    approval_output: Some(expiry),
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, StoreError::ApprovalRaceLost),
            "{label}: late expiry did not lose the gate CAS, got {error:?}"
        );
        let gate = store
            .get_gate(&fixture.scope, &opened.run_id, &opened.gate.gate_id)
            .await
            .unwrap();
        assert_eq!(
            gate.status,
            GateState::Approved,
            "{label}: expiry overwrote a decision"
        );
        assert_eq!(
            gate.resolution_source,
            Some(ApprovalResolutionSource::Human),
            "{label}"
        );
        assert_eq!(
            watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await,
            settled,
            "{label}: the losing expiry still emitted events"
        );

        // Expiry first, human decision arrives late and loses.
        let fixture = seed(store, objects, "race-expiry", ApprovalExpiryPolicy::Approve).await;
        let opened = open_gate(store, objects, &fixture, "run").await;
        advance(EXPIRES_AFTER_MS as i64).await;
        let expiry = objects
            .put(
                &fixture.scope,
                &canonical_expiry_approval_result(),
                "application/json",
            )
            .await
            .unwrap();
        store
            .expire_approval(
                &fixture.scope,
                ExpireApproval {
                    permit: fixture.permit.clone(),
                    run_id: opened.run_id.clone(),
                    gate_id: opened.gate.gate_id.clone(),
                    approval_output: Some(expiry),
                },
            )
            .await
            .unwrap();
        let settled = watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await;
        let output = canonical_output(objects, &fixture, &fixture.principal).await;
        let error = store
            .decide_approval(&fixture.scope, approve(&opened, &fixture.principal, output))
            .await
            .unwrap_err();
        assert!(
            matches!(error, StoreError::ApprovalAlreadyResolved),
            "{label}: late human decision did not fail closed, got {error:?}"
        );
        let gate = store
            .get_gate(&fixture.scope, &opened.run_id, &opened.gate.gate_id)
            .await
            .unwrap();
        assert_eq!(gate.status, GateState::ExpiredApproved, "{label}");
        assert_eq!(
            watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await,
            settled,
            "{label}: the losing decision still emitted events"
        );

        // Cancellation first: G06 cancels the Pending gate in the same
        // transaction, and the later decision cannot revive it.
        let fixture = seed(store, objects, "race-cancel", ApprovalExpiryPolicy::Reject).await;
        let opened = open_gate(store, objects, &fixture, "run").await;
        store
            .cancel_run(
                &fixture.scope,
                CancelRun {
                    run_id: opened.run_id.clone(),
                    expected_run_version: opened.run_version,
                    expected_pending_gate_versions: vec![ExpectedGateVersion {
                        gate_id: opened.gate.gate_id.clone(),
                        version: opened.gate.version,
                    }],
                    principal: fixture.principal.clone(),
                    reason_code: "OperatorCancelled".to_owned(),
                    idempotency_token: "cancel-token-long-enough-0123456789".to_owned(),
                },
            )
            .await
            .unwrap();
        let gate = store
            .get_gate(&fixture.scope, &opened.run_id, &opened.gate.gate_id)
            .await
            .unwrap();
        assert_eq!(
            gate.status,
            GateState::Cancelled,
            "{label}: G06 did not fire"
        );
        assert_eq!(
            gate.resolution_source,
            Some(ApprovalResolutionSource::Cancellation),
            "{label}"
        );
        let settled = watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await;
        let output = canonical_output(objects, &fixture, &fixture.principal).await;
        let error = store
            .decide_approval(&fixture.scope, approve(&opened, &fixture.principal, output))
            .await
            .unwrap_err();
        assert!(
            !matches!(error, StoreError::NotFound),
            "{label}: cancelled gate disappeared instead of failing closed"
        );
        let gate = store
            .get_gate(&fixture.scope, &opened.run_id, &opened.gate.gate_id)
            .await
            .unwrap();
        assert_eq!(gate.status, GateState::Cancelled, "{label}");
        assert_eq!(
            watermark(store, &fixture, &opened.run_id, &opened.gate.gate_id).await,
            settled,
            "{label}: a decision against a cancelled gate perturbed state"
        );
    }
    both_stores!(body);
}

// -------------------------------------------------------------------------
// Restart. "The Pending gate, request ref, expiry, and
// node WaitingApproval state survive. Recovery neither approves nor recreates
// it."
// -------------------------------------------------------------------------

/// SQLite-only by necessity: the in-memory store has no reopen seam, so this
/// property cannot be proven for it through the public API.
#[tokio::test]
async fn restart_preserves_the_pending_gate_and_resumes_only_downstream() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("w9-approvals.sqlite");
    let clock = Arc::new(TestClock::new(CLOCK_ORIGIN));
    let objects = Arc::new(InMemoryObjectStore::new(clock.clone()));

    let fixture;
    let opened;
    {
        let store = SqliteWorkflowStore::open(&path, clock.clone(), objects.clone())
            .await
            .unwrap();
        fixture = seed(&store, &objects, "restart", ApprovalExpiryPolicy::Reject).await;
        opened = open_gate(&store, &objects, &fixture, "run").await;
    }

    // Fresh store instance over the same database file.
    let store = SqliteWorkflowStore::open(&path, clock.clone(), objects.clone())
        .await
        .unwrap();
    let gate = store
        .get_gate(&fixture.scope, &opened.run_id, &opened.gate.gate_id)
        .await
        .unwrap();
    // Every immutable field, not just the status. The relational projection in
    // src/sqlite/schema.rs stores only a subset of the gate columns, so a
    // hydration path that read the projection instead of the row blob would
    // silently drop the authorization policy and on_expiry -- which would make
    // an unauthorized principal authorized after a restart.
    assert_eq!(
        gate, opened.gate,
        "restart mutated or truncated the gate row"
    );
    assert_eq!(gate.status, GateState::Pending);
    assert_eq!(
        gate.authorization_policy.allowed_principal_ids,
        vec![APPROVER.to_owned()]
    );
    assert_eq!(
        gate.authorization_policy.allowed_role_ids,
        vec![ROLE.to_owned()]
    );
    assert_eq!(gate.on_expiry, ApprovalExpiryPolicy::Reject);
    assert_eq!(gate.expires_at, opened.gate.expires_at);

    let node = store
        .get_node(&fixture.scope, &opened.run_id, &node_id("approval"))
        .await
        .unwrap();
    assert_eq!(node.status, NodeState::WaitingApproval);
    let downstream = store
        .get_node(&fixture.scope, &opened.run_id, &node_id("succeed"))
        .await
        .unwrap();
    assert_eq!(
        downstream.status,
        NodeState::Pending,
        "restart advanced the downstream frontier without a decision"
    );

    // The surviving gate is still decidable with the versions observed before
    // the restart, and only then does the frontier move.
    let output = canonical_output(&objects, &fixture, &fixture.principal).await;
    let gate = store
        .decide_approval(&fixture.scope, approve(&opened, &fixture.principal, output))
        .await
        .unwrap();
    assert_eq!(gate.status, GateState::Approved);
    let downstream = store
        .get_node(&fixture.scope, &opened.run_id, &node_id("succeed"))
        .await
        .unwrap();
    assert_eq!(downstream.status, NodeState::Ready);
}
