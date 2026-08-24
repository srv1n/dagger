//! Deterministic, no-network actions for the reference workflow fixtures.

use super::{
    digest_bytes, ActionContext, ActionDescriptor, ActionOutcome, InMemoryActionRegistry,
    WorkflowAction,
};
use crate::ids::CostUnits;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

/// Every action name required by the two reference workflows.
pub const FIXTURE_ACTION_NAMES: &[&str] = &[
    "legal.generate_initial_queries",
    "legal.search",
    "legal.summarize_evidence",
    "legal.generate_followup_queries",
    "legal.merge_second_round",
    "legal.synthesize_report",
    "legal.validate_citations",
    "intel.prepare_trigger",
    "intel.fetch_feed_alpha",
    "intel.fetch_feed_beta",
    "intel.fetch_feed_gamma",
    "intel.normalize_deduplicate",
    "intel.summarize_item",
    "intel.compile_report",
    "intel.publish",
];

/// A fixture registry plus observable idempotent-publication records.
#[derive(Clone, Debug)]
pub struct FixtureActions {
    registry: Arc<InMemoryActionRegistry>,
    published_idempotency_keys: Arc<Mutex<Vec<String>>>,
}

impl FixtureActions {
    /// Registers every reference fixture action with beta succeeding normally.
    pub fn new() -> Self {
        Self::with_beta_transient_failure(false)
    }

    /// Registers every fixture action, optionally making beta fail once retryably.
    pub fn with_beta_transient_failure(beta_fails_once: bool) -> Self {
        let registry = Arc::new(InMemoryActionRegistry::new());
        let published_idempotency_keys = Arc::new(Mutex::new(Vec::new()));
        let beta_failure_remaining = Arc::new(AtomicBool::new(beta_fails_once));
        for name in FIXTURE_ACTION_NAMES {
            registry
                .register(Arc::new(FixtureAction {
                    descriptor: descriptor(name),
                    beta_failure_remaining: beta_failure_remaining.clone(),
                    published_idempotency_keys: published_idempotency_keys.clone(),
                }))
                .expect("fixture action names are unique and non-empty");
        }
        Self {
            registry,
            published_idempotency_keys,
        }
    }

    /// Returns the registry as an action-registry implementation.
    pub fn registry(&self) -> Arc<InMemoryActionRegistry> {
        self.registry.clone()
    }

    /// Returns the logical-node keys accepted by the idempotent publisher.
    pub fn published_idempotency_keys(&self) -> Vec<String> {
        self.published_idempotency_keys
            .lock()
            .expect("fixture publisher lock is not poisoned")
            .clone()
    }
}

impl Default for FixtureActions {
    fn default() -> Self {
        Self::new()
    }
}

struct FixtureAction {
    descriptor: ActionDescriptor,
    beta_failure_remaining: Arc<AtomicBool>,
    published_idempotency_keys: Arc<Mutex<Vec<String>>>,
}

impl WorkflowAction for FixtureAction {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        canonical_bound_input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            let input = match serde_json::from_slice(canonical_bound_input) {
                Ok(input) => input,
                Err(_) => return permanent("fixture.invalid_input", "fixture input must be JSON"),
            };
            if context.cancellation_token.is_cancelled() {
                return retryable("fixture.cancelled", "fixture action observed cancellation");
            }
            fixture_outcome(
                &self.descriptor.name,
                input,
                canonical_bound_input,
                &context.idempotency_key,
                &self.beta_failure_remaining,
                &self.published_idempotency_keys,
            )
        })
    }
}

/// Returns the descriptor advertised by a named deterministic fixture action.
pub fn descriptor(name: &str) -> ActionDescriptor {
    ActionDescriptor {
        name: name.to_owned(),
        contract_version: "fixture-0.1".to_owned(),
        input_schema_digest: fixture_schema_digest(),
        output_schema_digest: fixture_schema_digest(),
        implementation_compatibility_digest: fixture_digest(name, "implementation"),
    }
}

/// Returns the single schema every deterministic fixture pin points at.
///
/// The schema is closed by construction (`additionalProperties: false` plus a declared
/// property set), so this cannot be `{}`. It is therefore the exact union of
/// the fields the three store-side validators actually see for the legal
/// research reference workflow:
///
/// - `legal_question` is the run input submitted at `create_run`;
/// - `query`, `sources` and `excerpts` are the `legal.search` output, which
///   is the action of both Map nodes and so is validated per child result;
/// - `artifact_ref_id`, `digest`, `size_bytes` and `media_type` are the
///   `report_artifact_ref` the Succeed node publishes as the run output.
///
/// Plain Action outputs are not schema-validated, so nothing else belongs here.
/// Keys stay in lexicographic order because the digest is over RFC 8785 bytes.
pub fn fixture_schema() -> Value {
    json!({
        "additionalProperties": false,
        "properties": {
            "artifact_ref_id": {"type": "string"},
            "digest": {"type": "string"},
            "excerpts": {"items": {"type": "string"}, "type": "array"},
            "legal_question": {"type": "string"},
            "media_type": {"type": "string"},
            "query": {"type": "string"},
            "size_bytes": {"type": "string"},
            "sources": {
                "items": {
                    "additionalProperties": false,
                    "properties": {
                        "excerpt": {"type": "string"},
                        "source_id": {"type": "string"}
                    },
                    "type": "object"
                },
                "type": "array"
            }
        },
        "type": "object"
    })
}

/// Returns the digest of [`fixture_schema`]'s canonical bytes.
pub fn fixture_schema_digest() -> crate::ids::Digest {
    digest_bytes(&serde_jcs::to_vec(&fixture_schema()).expect("fixture schema is canonical JSON"))
}

fn fixture_outcome(
    name: &str,
    input: Value,
    bytes: &[u8],
    idempotency_key: &str,
    beta_failure_remaining: &AtomicBool,
    published_idempotency_keys: &Mutex<Vec<String>>,
) -> ActionOutcome {
    if name == "intel.fetch_feed_beta" && beta_failure_remaining.swap(false, Ordering::AcqRel) {
        return retryable(
            "intel.feed_transient",
            "fixture beta feed transient failure",
        );
    }

    let input_digest = digest_bytes(bytes);
    let marker = &input_digest.0[7..19];
    let output = match name {
        "legal.generate_initial_queries" | "legal.generate_followup_queries" => {
            let requested = input
                .get("max_queries")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(3) as usize;
            let queries = (0..requested)
                .map(|index| Value::String(format!("fixture-query-{marker}-{index}")))
                .collect::<Vec<_>>();
            json!({ "queries": queries })
        }
        "legal.search" => json!({
            "query": input.get("query").cloned().unwrap_or(Value::Null),
            "sources": [{"source_id": format!("fixture-source-{marker}"), "excerpt": format!("evidence-{marker}")}],
            "excerpts": [format!("evidence-{marker}")]
        }),
        "legal.summarize_evidence" | "legal.merge_second_round" => json!({
            "findings": [format!("finding-{marker}")],
            "gaps": [format!("gap-{marker}")],
            "needs_second_round": input
                .get("question")
                .and_then(Value::as_str)
                .is_some_and(|question| question.contains("second-round"))
        }),
        "legal.synthesize_report" => json!({
            "draft_report": format!("draft-report-{marker}"),
            "citation_claims": [{"claim": format!("claim-{marker}"), "source_id": format!("fixture-source-{marker}")}]
        }),
        "legal.validate_citations" => json!({
            "report_artifact_ref": artifact_ref("fixture-legal-report", &input_digest)
        }),
        "intel.prepare_trigger" => json!({
            "trigger": input.get("trigger").cloned().unwrap_or(Value::Null)
        }),
        "intel.fetch_feed_alpha" | "intel.fetch_feed_beta" | "intel.fetch_feed_gamma" => {
            let feed = input
                .get("feed_name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            json!({"items": [
                {"id": format!("{feed}-{marker}-0"), "feed": feed, "title": format!("item-{marker}-0")},
                {"id": format!("{feed}-{marker}-1"), "feed": feed, "title": format!("item-{marker}-1")}
            ]})
        }
        "intel.normalize_deduplicate" => normalize_output(&input, marker),
        "intel.summarize_item" => json!({
            "item_index": input.get("item_index").cloned().unwrap_or(Value::Null),
            "summary": format!("item-summary-{marker}"),
            "item": input.get("item").cloned().unwrap_or(Value::Null)
        }),
        "intel.compile_report" => json!({
            "report": format!("intelligence-report-{marker}"),
            "approval_request": {"kind": "fixture-report", "digest": input_digest.0}
        }),
        "intel.publish" => {
            let mut keys = published_idempotency_keys
                .lock()
                .expect("fixture publisher lock is not poisoned");
            if !keys.iter().any(|key| key == idempotency_key) {
                keys.push(idempotency_key.to_owned());
            }
            json!({
                "published_artifact_ref": artifact_ref("fixture-intel-publication", &digest_bytes(idempotency_key.as_bytes()))
            })
        }
        _ => return permanent("fixture.unknown_action", "unknown fixture action"),
    };
    ActionOutcome::success(output, Vec::new(), CostUnits(0), None)
        .expect("fixture outcomes are persistence-safe")
}

fn normalize_output(input: &Value, marker: &str) -> Value {
    let mut serialized = BTreeSet::new();
    if let Some(feeds) = input.get("feeds").and_then(Value::as_object) {
        for feed in feeds.values() {
            if let Some(items) = feed.as_array() {
                for item in items {
                    serialized.insert(serde_json::to_string(item).expect("JSON values serialize"));
                }
            }
        }
    }
    let retained_items = serialized
        .into_iter()
        .map(|item| {
            serde_json::from_str::<Value>(&item).expect("previously serialized JSON parses")
        })
        .collect::<Vec<_>>();
    json!({
        "retained_items": retained_items,
        "stats": {"retained_count": retained_items.len(), "fixture_marker": marker}
    })
}

fn artifact_ref(kind: &str, digest: &crate::ids::Digest) -> Value {
    json!({
        "artifact_ref_id": format!("{kind}:{}", &digest.0[7..19]),
        "digest": digest.0,
        "size_bytes": "0",
        "media_type": "application/json"
    })
}

fn fixture_digest(name: &str, dimension: &str) -> crate::ids::Digest {
    digest_bytes(format!("dagger-workflow-fixture-v1:{name}:{dimension}").as_bytes())
}

fn retryable(code: &str, message: &str) -> ActionOutcome {
    ActionOutcome::retryable(code.to_owned(), message.to_owned(), None, CostUnits(0))
        .expect("fixture retryable outcome is persistence-safe")
}

fn permanent(code: &str, message: &str) -> ActionOutcome {
    ActionOutcome::permanent(code.to_owned(), message.to_owned(), None, CostUnits(0))
        .expect("fixture permanent outcome is persistence-safe")
}
