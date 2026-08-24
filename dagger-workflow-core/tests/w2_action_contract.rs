use dagger_workflow_core::action::{
    check_compatibility, compatibility_evidence_digest, fixtures, invoke_registered_at,
    ActionContext, ActionDescriptor, ActionInvocation, ActionOutcome, ActionRegistry, BudgetHandle,
    CancellationSource, CanonicalBoundInput, CompatibilityMismatch, CompletionCredential,
    DiagnosticFact, DiagnosticScalar, DiagnosticsEnvelope, DiagnosticsValidationError,
    ExternalHandleAccess, InMemoryActionRegistry, InvocationError, WorkflowAction,
};
use dagger_workflow_core::artifact::{ArtifactKind, ArtifactRef, JsonRef};
use dagger_workflow_core::definition::ActionPin;
use dagger_workflow_core::ids::{CostUnits, Digest, Id, Timestamp};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use dagger_workflow_core::store::StoreError;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

fn scope() -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new("tenant-a").unwrap(),
        namespace: ScopeAtom::new("workflow-a").unwrap(),
    }
}

fn digest(label: &str) -> Digest {
    Digest::new(format!("sha256:{:x}", Sha256::digest(label.as_bytes()))).unwrap()
}

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}

fn descriptor() -> ActionDescriptor {
    ActionDescriptor {
        name: "test.action".to_owned(),
        contract_version: "v1".to_owned(),
        input_schema_digest: digest("input"),
        output_schema_digest: digest("output"),
        implementation_compatibility_digest: digest("implementation"),
    }
}

fn pin(descriptor: &ActionDescriptor) -> ActionPin {
    let artifact = ArtifactRef {
        scope: scope(),
        artifact_ref_id: id("schema"),
        digest: descriptor.input_schema_digest.clone(),
        size_bytes: 0,
        media_type: "application/json".to_owned(),
        kind: ArtifactKind::SchemaDocument,
        producer_run_id: None,
        producer_node_id: None,
        producer_attempt_id: None,
        ordinal: 0,
        created_at: Timestamp(0),
    };
    ActionPin {
        reference_location: "node".to_owned(),
        name: descriptor.name.clone(),
        contract_version: descriptor.contract_version.clone(),
        input_schema_digest: descriptor.input_schema_digest.clone(),
        output_schema_digest: descriptor.output_schema_digest.clone(),
        compatible_implementation_requirement: descriptor
            .implementation_compatibility_digest
            .clone(),
        input_schema_ref: JsonRef(artifact.clone()),
        output_schema_ref: JsonRef(artifact),
    }
}

fn context(
    deadline: i64,
    cancellation: Arc<dyn dagger_workflow_core::action::CancellationToken>,
) -> ActionContext {
    let scope = scope();
    let run = id("run");
    let node = id("node");
    ActionContext::new(
        scope,
        run,
        digest("revision"),
        node,
        id("attempt"),
        1,
        CompletionCredential::from_minted_bytes([7; 32]),
        Timestamp(deadline),
        cancellation,
        BudgetHandle {
            declared_max_cost_units: CostUnits(10),
        },
        Arc::new(|_| Box::pin(async { Err(StoreError::AttemptFenced) })),
        ExternalHandleAccess::unavailable(),
    )
}

fn invocation(descriptor: &ActionDescriptor, input: &CanonicalBoundInput) -> ActionInvocation {
    ActionInvocation {
        scope: scope(),
        run_id: id("run"),
        invocation_id: id("attempt"),
        node_instance_id: id("node"),
        attempt_id: id("attempt"),
        action_reference_location: "node".to_owned(),
        action_name: descriptor.name.clone(),
        contract_version: descriptor.contract_version.clone(),
        revision_hash: digest("revision"),
        input_schema_digest: descriptor.input_schema_digest.clone(),
        output_schema_digest: descriptor.output_schema_digest.clone(),
        compatible_implementation_requirement: descriptor
            .implementation_compatibility_digest
            .clone(),
        bound_input_ref: JsonRef(ArtifactRef {
            scope: scope(),
            artifact_ref_id: id("input"),
            digest: input.digest().clone(),
            size_bytes: input.bytes().len() as u64,
            media_type: "application/json".to_owned(),
            kind: ArtifactKind::ActionInvocationInput,
            producer_run_id: None,
            producer_node_id: None,
            producer_attempt_id: None,
            ordinal: 0,
            created_at: Timestamp(0),
        }),
        bound_input_digest: input.digest().clone(),
        bound_input_size_bytes: input.bytes().len() as u64,
        binding_derivation_digest: digest("bindings"),
        created_at: Timestamp(0),
    }
}

struct TestAction {
    descriptor: ActionDescriptor,
    called: Arc<AtomicBool>,
    cancel_source: Option<CancellationSource>,
}

impl WorkflowAction for TestAction {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }
    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        bytes: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            self.called.store(true, Ordering::Release);
            if let Some(source) = &self.cancel_source {
                source.cancel();
            }
            if context.cancellation_token.is_cancelled() {
                return ActionOutcome::retryable(
                    "test.cancelled".to_owned(),
                    "cancelled".to_owned(),
                    None,
                    CostUnits(0),
                )
                .unwrap();
            }
            ActionOutcome::success(
                json!({"exact_bytes": bytes.to_vec()}),
                vec![],
                CostUnits(0),
                None,
            )
            .unwrap()
        })
    }
}

#[test]
fn registration_lookup_and_every_pin_dimension_are_exact() {
    let descriptor = descriptor();
    let registry = InMemoryActionRegistry::new();
    registry
        .register(Arc::new(TestAction {
            descriptor: descriptor.clone(),
            called: Arc::new(AtomicBool::new(false)),
            cancel_source: None,
        }))
        .unwrap();
    assert!(registry.resolve("test.action").is_some());
    assert!(check_compatibility(&pin(&descriptor), &descriptor).is_ok());

    let mut cases: Vec<(CompatibilityMismatch, fn(&mut ActionDescriptor))> = vec![
        (
            CompatibilityMismatch::Name,
            |value: &mut ActionDescriptor| value.name = "other.action".to_owned(),
        ),
        (
            CompatibilityMismatch::ContractVersion,
            |value: &mut ActionDescriptor| value.contract_version = "v2".to_owned(),
        ),
        (
            CompatibilityMismatch::InputSchemaDigest,
            |value: &mut ActionDescriptor| value.input_schema_digest = digest("other-input"),
        ),
        (
            CompatibilityMismatch::OutputSchemaDigest,
            |value: &mut ActionDescriptor| value.output_schema_digest = digest("other-output"),
        ),
        (
            CompatibilityMismatch::ImplementationCompatibilityDigest,
            |value: &mut ActionDescriptor| {
                value.implementation_compatibility_digest = digest("other-implementation")
            },
        ),
    ];
    for (expected, mutate) in cases.drain(..) {
        let mut changed = descriptor.clone();
        mutate(&mut changed);
        assert_eq!(
            check_compatibility(&pin(&descriptor), &changed),
            Err(expected)
        );
    }
    let report = registry.check_pins(&[pin(&descriptor)]);
    assert!(report.incompatible_reference_locations.is_empty());
    assert_eq!(report.evidence.len(), 1);
}

#[test]
fn compatibility_evidence_uses_known_rfc8785_bytes() {
    let value = serde_json::json!({"z": [3, 2], "a": {"b": true, "a": null}});
    let canonical = serde_jcs::to_vec(&value).unwrap();
    assert_eq!(canonical, br#"{"a":{"a":null,"b":true},"z":[3,2]}"#);
    assert_eq!(
        compatibility_evidence_digest(&value).as_str(),
        "sha256:d570148de3398d16b58b6267438ad1656f50352cfd66da2432a3c5f3a4b7a8c7"
    );
}

#[test]
fn expired_deadline_prevents_action_delivery() {
    let descriptor = descriptor();
    let called = Arc::new(AtomicBool::new(false));
    let registry = InMemoryActionRegistry::new();
    registry
        .register(Arc::new(TestAction {
            descriptor: descriptor.clone(),
            called: called.clone(),
            cancel_source: None,
        }))
        .unwrap();
    let input = CanonicalBoundInput::from_canonical_bytes(br#"{"x":1}"#.to_vec());
    let error = block_on(invoke_registered_at(
        &registry,
        &invocation(&descriptor, &input),
        context(10, CancellationSource::new().token()),
        &input,
        Timestamp(10),
    ))
    .unwrap_err();
    assert!(matches!(error, InvocationError::DeadlineExpired { .. }));
    assert!(!called.load(Ordering::Acquire));
}

#[test]
fn cancellation_is_observed_during_invocation() {
    let descriptor = descriptor();
    let source = CancellationSource::new();
    let registry = InMemoryActionRegistry::new();
    registry
        .register(Arc::new(TestAction {
            descriptor: descriptor.clone(),
            called: Arc::new(AtomicBool::new(false)),
            cancel_source: Some(source.clone()),
        }))
        .unwrap();
    let input = CanonicalBoundInput::from_canonical_bytes(br#"{"x":1}"#.to_vec());
    let outcome = block_on(invoke_registered_at(
        &registry,
        &invocation(&descriptor, &input),
        context(100, source.token()),
        &input,
        Timestamp(1),
    ))
    .unwrap();
    assert!(matches!(outcome, ActionOutcome::Retryable { code, .. } if code == "test.cancelled"));
}

#[test]
fn invocation_delivers_the_exact_bound_input_bytes() {
    let descriptor = descriptor();
    let registry = InMemoryActionRegistry::new();
    registry
        .register(Arc::new(TestAction {
            descriptor: descriptor.clone(),
            called: Arc::new(AtomicBool::new(false)),
            cancel_source: None,
        }))
        .unwrap();
    let input = CanonicalBoundInput::from_canonical_bytes(b"{\"z\":1,\"a\":[true,false]}".to_vec());
    let outcome = block_on(invoke_registered_at(
        &registry,
        &invocation(&descriptor, &input),
        context(100, CancellationSource::new().token()),
        &input,
        Timestamp(1),
    ))
    .unwrap();
    assert!(
        matches!(outcome, ActionOutcome::Success { output, .. } if output == json!({"exact_bytes": input.bytes().to_vec()}))
    );
}

#[test]
fn invocation_rejects_bound_ref_digest_and_size_contradictions() {
    let descriptor = descriptor();
    let called = Arc::new(AtomicBool::new(false));
    let registry = InMemoryActionRegistry::new();
    registry
        .register(Arc::new(TestAction {
            descriptor: descriptor.clone(),
            called: called.clone(),
            cancel_source: None,
        }))
        .unwrap();
    let input = CanonicalBoundInput::from_canonical_bytes(br#"{"x":1}"#.to_vec());
    let mut contradictory_digest = invocation(&descriptor, &input);
    contradictory_digest.bound_input_ref.0.digest = digest("other-object");
    assert!(matches!(
        block_on(invoke_registered_at(
            &registry,
            &contradictory_digest,
            context(100, CancellationSource::new().token()),
            &input,
            Timestamp(1)
        )),
        Err(InvocationError::BoundInputDigestMismatch { .. })
    ));
    let mut contradictory_size = invocation(&descriptor, &input);
    contradictory_size.bound_input_ref.0.size_bytes += 1;
    assert!(matches!(
        block_on(invoke_registered_at(
            &registry,
            &contradictory_size,
            context(100, CancellationSource::new().token()),
            &input,
            Timestamp(1)
        )),
        Err(InvocationError::BoundInputSizeMismatch { .. })
    ));
    assert!(!called.load(Ordering::Acquire));
}

#[test]
fn oversized_diagnostics_are_closed_rejections() {
    let facts = (0..34)
        .map(|index| DiagnosticFact {
            name: format!("fact-{index}"),
            value: DiagnosticScalar::String("x".repeat(2_000)),
        })
        .collect();
    let error = DiagnosticsEnvelope::new(None, facts, vec![]).unwrap_err();
    assert!(
        matches!(error, DiagnosticsValidationError::TooLarge { limit_bytes: 65_536, observed_bytes } if observed_bytes > 65_536)
    );
}

#[test]
fn outcome_error_taxonomy_remains_distinct() {
    let retryable = ActionOutcome::retryable(
        "test.retry".to_owned(),
        "try again".to_owned(),
        None,
        CostUnits(1),
    )
    .unwrap();
    let permanent = ActionOutcome::permanent(
        "test.permanent".to_owned(),
        "stop".to_owned(),
        None,
        CostUnits(2),
    )
    .unwrap();
    assert!(
        matches!(retryable, ActionOutcome::Retryable { code, actual_cost_units, .. } if code == "test.retry" && actual_cost_units == CostUnits(1))
    );
    assert!(
        matches!(permanent, ActionOutcome::Permanent { code, actual_cost_units, .. } if code == "test.permanent" && actual_cost_units == CostUnits(2))
    );
}

#[test]
fn idempotency_key_is_stable_across_attempt_ids() {
    let source = CancellationSource::new();
    let first = ActionContext::new(
        scope(),
        id("run"),
        digest("revision"),
        id("node"),
        id("attempt-1"),
        1,
        CompletionCredential::from_minted_bytes([1; 32]),
        Timestamp(100),
        source.token(),
        BudgetHandle {
            declared_max_cost_units: CostUnits(1),
        },
        Arc::new(|_| Box::pin(async { Err(StoreError::AttemptFenced) })),
        ExternalHandleAccess::unavailable(),
    );
    let second = ActionContext::new(
        scope(),
        id("run"),
        digest("revision"),
        id("node"),
        id("attempt-2"),
        2,
        CompletionCredential::from_minted_bytes([2; 32]),
        Timestamp(100),
        source.token(),
        BudgetHandle {
            declared_max_cost_units: CostUnits(1),
        },
        Arc::new(|_| Box::pin(async { Err(StoreError::AttemptFenced) })),
        ExternalHandleAccess::unavailable(),
    );
    assert_eq!(first.idempotency_key, second.idempotency_key);
}

#[test]
fn every_fixture_returns_identical_output_for_identical_input() {
    let fixtures = fixtures::FixtureActions::new();
    for (name, value) in fixture_inputs() {
        let descriptor = fixtures::descriptor(name);
        let input = CanonicalBoundInput::from_canonical_bytes(value.to_string().into_bytes());
        let first = block_on(invoke_registered_at(
            fixtures.registry().as_ref(),
            &invocation(&descriptor, &input),
            context(100, CancellationSource::new().token()),
            &input,
            Timestamp(1),
        ))
        .unwrap();
        let second = block_on(invoke_registered_at(
            fixtures.registry().as_ref(),
            &invocation(&descriptor, &input),
            context(100, CancellationSource::new().token()),
            &input,
            Timestamp(1),
        ))
        .unwrap();
        assert_eq!(first, second, "fixture {name} was nondeterministic");
    }
}

#[test]
fn beta_fails_once_and_publisher_records_the_logical_node_key() {
    let fixtures = fixtures::FixtureActions::with_beta_transient_failure(true);
    let beta = fixtures::descriptor("intel.fetch_feed_beta");
    let beta_input = CanonicalBoundInput::from_canonical_bytes(
        json!({"feed_name":"beta","trigger":{}})
            .to_string()
            .into_bytes(),
    );
    let first = block_on(invoke_registered_at(
        fixtures.registry().as_ref(),
        &invocation(&beta, &beta_input),
        context(100, CancellationSource::new().token()),
        &beta_input,
        Timestamp(1),
    ))
    .unwrap();
    let second = block_on(invoke_registered_at(
        fixtures.registry().as_ref(),
        &invocation(&beta, &beta_input),
        context(100, CancellationSource::new().token()),
        &beta_input,
        Timestamp(1),
    ))
    .unwrap();
    assert!(
        matches!(first, ActionOutcome::Retryable { code, .. } if code == "intel.feed_transient")
    );
    assert!(matches!(second, ActionOutcome::Success { .. }));

    let publisher = fixtures::descriptor("intel.publish");
    let publish_input = CanonicalBoundInput::from_canonical_bytes(
        json!({"approval":{},"channel":"fixture","report":"r"})
            .to_string()
            .into_bytes(),
    );
    let publish_context = context(100, CancellationSource::new().token());
    let expected_key = publish_context.idempotency_key.clone();
    block_on(invoke_registered_at(
        fixtures.registry().as_ref(),
        &invocation(&publisher, &publish_input),
        publish_context,
        &publish_input,
        Timestamp(1),
    ))
    .unwrap();
    assert_eq!(fixtures.published_idempotency_keys(), vec![expected_key]);
}

fn fixture_inputs() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "legal.generate_initial_queries",
            json!({"question":"q","max_queries":3}),
        ),
        (
            "legal.search",
            json!({"question":"q","query":"q1","round":1}),
        ),
        (
            "legal.summarize_evidence",
            json!({"question":"q","evidence":[]}),
        ),
        (
            "legal.generate_followup_queries",
            json!({"question":"q","gaps":[],"max_queries":3}),
        ),
        (
            "legal.merge_second_round",
            json!({"question":"q","initial_summary":{},"second_round_evidence":[]}),
        ),
        (
            "legal.synthesize_report",
            json!({"question":"q","initial_summary":{},"second_round_binding_status":"none"}),
        ),
        (
            "legal.validate_citations",
            json!({"question":"q","draft_report":"d","citation_claims":[]}),
        ),
        ("intel.prepare_trigger", json!({"trigger":{}})),
        (
            "intel.fetch_feed_alpha",
            json!({"feed_name":"alpha","trigger":{}}),
        ),
        (
            "intel.fetch_feed_beta",
            json!({"feed_name":"beta","trigger":{}}),
        ),
        (
            "intel.fetch_feed_gamma",
            json!({"feed_name":"gamma","trigger":{}}),
        ),
        (
            "intel.normalize_deduplicate",
            json!({"feeds":{"alpha":[],"beta":[],"gamma":[]},"trigger":{}}),
        ),
        (
            "intel.summarize_item",
            json!({"item":{},"item_index":0,"trigger":{}}),
        ),
        (
            "intel.compile_report",
            json!({"deduplication_stats":{},"summaries":[],"trigger":{}}),
        ),
        (
            "intel.publish",
            json!({"approval":{},"channel":"fixture","report":"r"}),
        ),
    ]
}

fn block_on<F: Future>(future: F) -> F::Output {
    fn raw_waker() -> RawWaker {
        fn clone(_: *const ()) -> RawWaker {
            raw_waker()
        }
        fn wake(_: *const ()) {}
        fn wake_by_ref(_: *const ()) {}
        fn drop(_: *const ()) {}
        RawWaker::new(
            std::ptr::null(),
            &RawWakerVTable::new(clone, wake, wake_by_ref, drop),
        )
    }
    let waker = unsafe { Waker::from_raw(raw_waker()) };
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
