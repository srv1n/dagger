# LLM Action Conventions

This memo defines v0.1 authoring conventions for `dagger-workflow-core` actions that call
LLM or other request/response providers. It does not extend the frozen workflow
contract or the public API. “MUST”, “SHOULD”, and “MUST NOT” are normative.

## 1. Complete request/response results only

An action MUST return exactly one complete `ActionOutcome`. It MUST NOT expose a
partial model response as `ActionOutcome::Success`.

The engine treats a node result as meaningful only after the complete JSON value can
be canonicalized, digest-verified, checked against the pinned output schema, written
to the object store, and durably accepted by `WorkflowStore::complete_attempt`.
Those are whole-result operations. A provider client MAY consume a streaming HTTP
response internally, but `WorkflowAction::invoke` MUST buffer and validate the final
result before returning it to the engine.

A post-v0.1 extension MAY add non-authoritative progress events. Such events MUST be
observability only: they MUST NOT be node results, binding sources, resumable stream
fragments, or evidence that an attempt completed.

## 2. Provider failure taxonomy

Actions MUST classify provider failures with the validated constructors
`ActionOutcome::retryable` and `ActionOutcome::permanent`. Error `code` values MUST
be namespaced persistence-safe identifiers; `message` values MUST contain no
credentials, response headers, prompts, or raw provider bodies.

| Provider condition | Required outcome | Example code |
|---|---|---|
| HTTP 429 | `ActionOutcome::Retryable` | `llm.rate_limited` |
| HTTP 5xx | `ActionOutcome::Retryable` | `llm.provider_unavailable` |
| Network failure | `ActionOutcome::Retryable` | `llm.network` |
| Provider/client timeout | `ActionOutcome::Retryable` | `llm.timeout` |
| HTTP 401 or 403 | `ActionOutcome::Permanent` | `llm.authentication` |
| Invalid request, including other provider 4xx responses | `ActionOutcome::Permanent` | `llm.invalid_request` |
| Provider content refusal | `ActionOutcome::Permanent` | `llm.content_refusal` |

An accepted `Retryable` result settles that attempt using its reported
`actual_cost_units`. If `attempt_number` is below the node retry policy's
`max_attempts`, the engine persists `next_eligible_at` and moves the node to
`RetryWaiting`; fixed or exponential backoff is computed from the database completion
time with no jitter (contract sections 5 and 14.2). The last allowed retryable attempt
ends the run as `RetriesExhausted`.

An accepted `Permanent` result also settles reported actual cost, then fails the node
and run immediately. It is never retried by the engine. Action-defined codes and
diagnostics are data; only the closed `Retryable` versus `Permanent` outcome category
controls retry behavior.

When provider cost is unknown after a timeout or ambiguous network failure, an action
SHOULD report `context.budget.declared_max_cost_units` as the conservative actual cost.
It MUST NOT invent a low “actual” value merely to preserve budget.

## 3. LLM output schema failures

An action MUST validate the decoded model output against the exact schema represented
by its `ActionDescriptor::output_schema_digest` before constructing `Success`. A
syntactically invalid response or a response that fails that pinned schema MUST return
`ActionOutcome::Retryable`, normally with code `llm.output_schema_mismatch`. This
retry remains bounded by the node's `max_attempts`. The rationale is specific to model
output: a repeated request can produce a conforming value because generation is
stochastic.

This convention is action-side. `WorkflowAction::invoke` receives canonical bound
input bytes, but `ActionContext` contains neither the output schema document nor a
schema-validation method. Implementations therefore must be constructed with a
validator for the exact pinned schema, or implement the same supported subset
locally, and keep it consistent with `ActionDescriptor::output_schema_digest`.

The frozen contract also requires authoritative validation of every submitted
`Success` at the engine/store completion boundary. That check is a safety net, not the
LLM retry mechanism: a schema-invalid `Success` is
`ActionOutputSchemaMismatch`/`ContractFailed`, not `Retryable`. At the time of this
memo, `ActionOutcome::validate` checks error fields and `DiagnosticsEnvelope` only,
and the checked-in memory and SQLite `complete_attempt` implementations do not
perform the required static-action output-schema check. Authors MUST still
prevalidate; see “API gaps observed”.

## 4. At-least-once execution and duplicate cost

Provider calls are at-least-once. Actions MUST send
`ActionContext::idempotency_key` when a provider supports an idempotency key, but MUST
assume that LLM inference itself may be charged more than once.

The accepted crash window is:

1. `claim_node_attempt` atomically creates a started attempt and reserves the node's
   declared maximum.
2. The action sends the provider request and the provider charges it.
3. The process crashes before the fenced completion is durably accepted, or recovery
   wins the fence before the completion arrives.
4. Recovery marks the outcome unknown or the late completion stale, settles the full
   reservation, and a later attempt may call and pay the provider again.

Only the attempt currently named by `NodeRun::active_attempt_id` can affect workflow
state. Fencing therefore protects durable state, not provider spend.
`BudgetReservation::declared_max`, the claim-time `Reserve` ledger entry, and
completion/recovery `BudgetSettlement` bracket worst-case engine accounting for each
attempt. The action sees the same cap read-only as
`ActionContext::budget.declared_max_cost_units` and MUST translate it into provider
token/request/currency limits. Each retry reserves independently; the run budget must
be sized for the allowed worst case.

## 5. Secrets

Credentials MUST be injected when constructing the provider client or action and held
in private action/client state. `ActionContext` deliberately has no credential bag.
Credentials MUST NOT be placed in:

- workflow definitions or binding constants;
- run input or action output;
- artifacts;
- events;
- `DiagnosticsEnvelope`, error codes, or messages.

Actions MUST NOT log credentials, credential-bearing headers, raw
`CompletionCredential` bytes, or provider request objects containing secrets. The
workflow definition is canonical-hashed into an immutable revision, so embedding a
secret in an otherwise legal string would make it durable and content-addressed.
Closed schemas prevent dedicated credential fields; they cannot detect a secret
smuggled into an allowed string.

## 6. Provider telemetry

Every provider attempt SHOULD report the requested or returned model identifier,
input token count, output token count, and end-to-end provider latency. These values
belong in `DiagnosticsEnvelope::facts` as scalar `DiagnosticFact` entries, not in
free-form messages. A stable envelope shape is:

```json
{
  "summary": "LLM provider attempt",
  "facts": [
    {"name": "provider.model", "value": "model-version"},
    {"name": "provider.input_tokens", "value": 120},
    {"name": "provider.output_tokens", "value": 48},
    {"name": "provider.latency_ms", "value": 735}
  ],
  "related_artifact_refs": []
}
```

Facts MUST remain low-cardinality and MUST NOT contain prompts, completions, request
IDs that embed credentials, or headers. `DiagnosticsEnvelope::new` and
`DiagnosticsEnvelope::validate` enforce the closed shape: at most 65,536 serialized
bytes, a 2,000-byte summary, 512 unique facts, 2,000 bytes per string fact, and 32
related artifact refs. Fact names are bounded persistence-safe identifiers and
sensitive names such as `api_key`, `token`, and `credentials` are rejected. The
contract requires the 65,536-byte limit over canonical JSON at
`complete_attempt`; oversized diagnostics are a no-write
`DiagnosticsTooLarge` rejection. Large safe detail belongs in a scoped action
artifact referenced by the envelope.

## 7. Rate limiting

Fan-out definitions MUST use the Map node's `max_concurrency` to cap simultaneous
children. The store enforces that cap when `claim_node_attempt` counts the Map
parent's started child attempts; effective concurrency is also bounded by
`EngineConfig::max_concurrency`.

Provider-global rate or concurrency limits span runs and Maps, so they do not belong
in workflow topology. The host SHOULD inject one shared provider client into all
relevant action instances, and that client SHOULD acquire a semaphore or rate-limit
permit before sending a request. The permit wait MUST observe
`ActionContext::deadline` and `cancellation_token`. No engine change is required.

## 8. Worked action skeleton

This dependency-free mock demonstrates the public action surface. Application-local
types below are illustrative; no provider or schema API is implied. The local
`valid_pinned_output` is intentionally narrow because the crate does not expose its
supported-subset validator.

```rust
use dagger_workflow_core::action::{
    ActionContext, ActionDescriptor, ActionOutcome, DiagnosticFact, DiagnosticScalar,
    DiagnosticsEnvelope, WorkflowAction,
};
use dagger_workflow_core::ids::CostUnits;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone)]
enum TransportFailure {
    Network,
    Timeout,
}

#[derive(Clone)]
struct MockReply {
    status: u16,
    body: Vec<u8>,
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    actual_cost_units: CostUnits,
    content_refusal: bool,
}

struct MockHttpClient {
    next: Result<MockReply, TransportFailure>,
}

impl MockHttpClient {
    fn complete(
        &self,
        api_key: &str,
        _canonical_input: &[u8],
        _idempotency_key: &str,
        _declared_max: CostUnits,
    ) -> Result<MockReply, TransportFailure> {
        assert!(!api_key.is_empty()); // Used in an auth header by a real client.
        self.next.clone() // No network in this example.
    }
}

struct LlmAction {
    descriptor: ActionDescriptor,
    api_key: Arc<str>,           // Injected at construction, never in context.
    model: Arc<str>,             // Non-secret provider configuration.
    client: Arc<MockHttpClient>, // Shared clients can own a global semaphore.
}

impl LlmAction {
    fn new(
        descriptor: ActionDescriptor,
        api_key: Arc<str>,
        model: Arc<str>,
        client: Arc<MockHttpClient>,
    ) -> Self {
        Self {
            descriptor,
            api_key,
            model,
            client,
        }
    }
}

impl WorkflowAction for LlmAction {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        canonical_bound_input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        Box::pin(async move {
            if context.cancellation_token.is_cancelled() {
                return retryable(
                    "llm.cancelled",
                    "provider request cancelled",
                    CostUnits(0),
                    diagnostics(&self.model, 0, 0, 0),
                );
            }

            let started = Instant::now();
            let reply = self.client.complete(
                &self.api_key,
                canonical_bound_input,
                &context.idempotency_key,
                context.budget.declared_max_cost_units,
            );
            let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

            let reply = match reply {
                Ok(reply) => reply,
                Err(TransportFailure::Network) => {
                    return retryable(
                        "llm.network",
                        "provider network failure",
                        context.budget.declared_max_cost_units,
                        diagnostics(&self.model, 0, 0, latency_ms),
                    )
                }
                Err(TransportFailure::Timeout) => {
                    return retryable(
                        "llm.timeout",
                        "provider request timed out",
                        context.budget.declared_max_cost_units,
                        diagnostics(&self.model, 0, 0, latency_ms),
                    )
                }
            };

            let diag = diagnostics(
                &reply.model,
                reply.input_tokens,
                reply.output_tokens,
                latency_ms,
            );
            let cost = reply.actual_cost_units;

            if reply.status == 429 {
                return retryable("llm.rate_limited", "provider rate limit", cost, diag);
            }
            if reply.status >= 500 {
                return retryable(
                    "llm.provider_unavailable",
                    "provider server failure",
                    cost,
                    diag,
                );
            }
            if reply.status == 401 || reply.status == 403 {
                return permanent(
                    "llm.authentication",
                    "provider rejected credentials",
                    cost,
                    diag,
                );
            }
            if reply.content_refusal {
                return permanent(
                    "llm.content_refusal",
                    "provider refused requested content",
                    cost,
                    diag,
                );
            }
            if (400..500).contains(&reply.status) {
                return permanent(
                    "llm.invalid_request",
                    "provider rejected request",
                    cost,
                    diag,
                );
            }
            if !(200..300).contains(&reply.status) {
                return permanent(
                    "llm.invalid_response",
                    "unexpected provider status",
                    cost,
                    diag,
                );
            }

            let output: Value = match serde_json::from_slice(&reply.body) {
                Ok(value) => value,
                Err(_) => {
                    return retryable(
                        "llm.output_schema_mismatch",
                        "provider output was not JSON",
                        cost,
                        diag,
                    )
                }
            };
            if !valid_pinned_output(&output) {
                return retryable(
                    "llm.output_schema_mismatch",
                    "provider output failed the pinned schema",
                    cost,
                    diag,
                );
            }

            ActionOutcome::success(output, Vec::new(), cost, Some(diag))
                .expect("the action constructs persistence-safe outcomes")
        })
    }
}

// This action's pinned schema is:
// {"type":"object","properties":{"answer":{"type":"string"}},
//  "required":["answer"],"additionalProperties":false}
fn valid_pinned_output(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.len() == 1 && object.get("answer").is_some_and(Value::is_string)
}

fn diagnostics(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    latency_ms: u64,
) -> DiagnosticsEnvelope {
    let bounded_model = model.chars().take(256).collect::<String>();
    DiagnosticsEnvelope::new(
        Some("LLM provider attempt".to_owned()),
        vec![
            string_fact("provider.model", &bounded_model),
            number_fact("provider.input_tokens", input_tokens),
            number_fact("provider.output_tokens", output_tokens),
            number_fact("provider.latency_ms", latency_ms),
        ],
        Vec::new(),
    )
    .expect("bounded static diagnostics")
}

fn string_fact(name: &str, value: &str) -> DiagnosticFact {
    DiagnosticFact {
        name: name.to_owned(),
        value: DiagnosticScalar::String(value.to_owned()),
    }
}

fn number_fact(name: &str, value: u64) -> DiagnosticFact {
    DiagnosticFact {
        name: name.to_owned(),
        value: DiagnosticScalar::Number(value.into()),
    }
}

fn retryable(
    code: &str,
    message: &str,
    cost: CostUnits,
    diagnostics: DiagnosticsEnvelope,
) -> ActionOutcome {
    ActionOutcome::retryable(code.to_owned(), message.to_owned(), Some(diagnostics), cost)
        .expect("static retryable outcome is persistence-safe")
}

fn permanent(
    code: &str,
    message: &str,
    cost: CostUnits,
    diagnostics: DiagnosticsEnvelope,
) -> ActionOutcome {
    ActionOutcome::permanent(code.to_owned(), message.to_owned(), Some(diagnostics), cost)
        .expect("static permanent outcome is persistence-safe")
}

fn example_construction(descriptor: ActionDescriptor) -> LlmAction {
    let client = Arc::new(MockHttpClient {
        next: Ok(MockReply {
            status: 200,
            body: serde_json::to_vec(&json!({"answer": "complete"})).unwrap(),
            model: "mock-model-v1".to_owned(),
            input_tokens: 12,
            output_tokens: 3,
            actual_cost_units: CostUnits(2),
            content_refusal: false,
        }),
    });
    LlmAction::new(
        descriptor,
        Arc::<str>::from("injected-secret"),
        Arc::<str>::from("mock-model-v1"),
        client,
    )
}
```

The workflow definition, not the action, supplies
`declared_max_cost_units`; `claim_node_attempt` reserves it before `invoke`. The
action reads the cap through `context.budget`, passes it to the provider client, and
reports trusted actual cost in every `ActionOutcome`. A real client MUST enforce that
cap and MUST NOT return an actual cost above it; doing so causes
`ActionCostProtocolViolation` and `ContractFailed`.

## API gaps observed

1. **No action-side schema facility.** `ActionContext` exposes no pinned schema
   document or validator, `ActionDescriptor` exposes only
   `output_schema_digest`, and the crate's supported-subset value validators are
   private. Correctly turning stochastic schema failures into `Retryable` therefore
   requires duplicated validation logic or a separately injected validator.
2. **The authoritative static-action output check is missing in the checked-in
   adapters.** The contract requires output-schema validation before accepting
   `Success`, but `ActionOutcome::validate`, the engine's `completion_objects`, and
   the current memory/SQLite static-action completion paths do not perform it. Map
   aggregate children are checked separately. This is a contract implementation gap,
   not permission for actions to skip validation.
3. **No unknown-cost action outcome.** `Retryable` and `Permanent` always require
   `actual_cost_units`. Provider timeouts and ambiguous network failures often cannot
   supply trusted usage, so actions must conservatively report the full declared
   maximum or risk under-accounting.
4. **No progress-event surface.** `WorkflowAction::invoke` can only return its final
   future output. Non-authoritative progress events require a post-v0.1 capability
   that cannot be confused with completion intake.
5. **No shared throttling primitive in the engine API.** Provider-global throttling
   must be implemented by an injected shared client. This is workable, but deadline
   conversion and cancellation-aware semaphore acquisition are entirely
   host/runtime-specific.
6. **Diagnostics byte-count mismatch.** The contract defines the 65,536-byte limit
   over canonical JSON. `DiagnosticsEnvelope::validate` currently measures
   `serde_json::to_vec(self)`, while the engine later persists
   `serde_jcs::to_vec(diagnostics)`. These encodings should use one authoritative
   measurement.
