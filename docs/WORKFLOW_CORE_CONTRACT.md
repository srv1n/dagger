# dagger-workflow-core Contract

Status: W0 complete and frozen after final document-only pass
Contract version: 0.1
Source: `WORKFLOW_CORE_PLAN.md`, revision 2, 2026-07-28

This document is normative for v0.1. “Must” and “must not” are requirements. The SQLite adapter may use a different physical layout, but observable behavior, transaction boundaries, scope confinement, and state transitions must match this contract. All durable control-plane tables use the `dagger_workflow_*` namespace. Legacy runtimes are not normative.

The durability tier is Tier 2: one live scheduler process per scoped control plane, durable restart recovery, and attempt-level fencing. Distributed leases and multi-scheduler execution are not v0.1 behavior.

## 1. Entity model

### 1.1 Common types and write rules

| Type | Definition |
|---|---|
| `ExecutionScope` | `{ tenant_id: ScopeAtom, namespace: ScopeAtom }`; both atoms are non-empty UTF-8 strings, 1–128 bytes, matching `^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$`. |
| `ScopedId<T>` | Logical key `(tenant_id, namespace, id)`. An unscoped ID is never accepted by a store API. |
| `Id` | Opaque, case-sensitive, non-empty UTF-8 string, at most 128 bytes. Generated IDs should be UUIDv7 text; callers must not infer ordering from them. |
| `Digest` | `sha256:` followed by exactly 64 lowercase hexadecimal characters. |
| `Timestamp` | UTC Unix epoch milliseconds as signed `i64`; all durable comparisons use the database clock. |
| `CostUnits` | `u64`. Arithmetic is checked; overflow is a transaction failure. |
| `JsonRef` | An `ArtifactRef` whose media type is `application/json` and whose bytes are canonical JSON. |
| `Version` | `u64` CAS counter, initialized to 1 and incremented once by every transaction that mutates the row. |
| `NodeInstanceId` | Definition node ID for static nodes; the Map child hash for synthetic children. |
| `EnginePermit` | Opaque `{ instance_id, generation, session_token }`; the store mints a fresh 256-bit session token on every acquisition, persists only its digest, and checks it on every scheduler-authored command. |
| `CompletionCredential` | Opaque per-attempt 256-bit capability minted at A01; only its SHA-256 digest is persisted. It authorizes result intake independently of the scheduler claim. |
| `AuthenticatedPrincipal` | Opaque host-authentication capability `{ scope: ExecutionScope, principal_id, role_ids, authentication_context_digest }`, minted for exactly that scope; callers cannot construct it from an arbitrary string. |
| `FailedReadProof` | Opaque object-store capability for a failed read of an already-committed ref, containing scope, requested digest, closed error class `Missing` or `DigestInvalid`, observed digest when any, store-instance nonce, proof nonce, and checked-at diagnostic time. |
| `ApprovalResult` | Fixed engine-owned canonical JSON envelope emitted as a successful Approval NodeRun `result_ref`; its exact schema and construction rules are in section 3.5. A definition or action cannot replace or extend this schema. |
| `DiagnosticsEnvelope` | Closed canonical JSON `{ "summary": String|null, "facts": [{"name": String, "value": scalar}], "related_artifact_refs": [canonical ArtifactRef] }`; all three fields are required, unknown fields are rejected, `summary` is at most 2000 bytes, `facts` has at most 512 entries with unique names matching `^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$`, scalar string values are at most 2000 bytes, and refs number at most 32. Fact names matching case-insensitive `password`, `passwd`, `secret`, `secret_key`, `token`, `access_token`, `refresh_token`, `api_key`, `apikey`, `credential`, `credentials`, or `private_key` are rejected. Total canonical size is at most 65,536 bytes. |

“Immutable” means the field can never change after its creating transaction. “CAS” means the named command must compare the entity version and any stated field preconditions in its `WHERE` clause; a mismatch commits nothing. Terminal `NodeAttempt` fields are immutable. Events and budget entries are append-only. A command may insert an immutable row and mutate related CAS rows in one transaction.

Every timestamp written by a store command is obtained inside that transaction from the database clock. Host- or worker-supplied timestamps may be retained only as untrusted diagnostic payload fields.

Persistence safety is a mechanically enforced format boundary, not semantic secret detection:

- definition structural fields are exactly the closed section 14 schema, unknown fields are rejected, canonical definition bytes are limited to 4 MiB, and every free-text field has its declared byte limit;
- event payload fields are exactly the section 15 catalogue and each canonical payload is at most 64 KiB;
- domain errors use only the closed section 5.5 variants; `path` is at most 1024 bytes, `message` at most 2000 bytes, and `valid_alternatives` at most 64 entries of at most 128 bytes each;
- no definition, event, error, diagnostic, permit, or receipt format has a field for raw credentials, access tokens, private keys, or credential-bearing headers; unknown fields are rejected;
- definition constant values and allowed human-readable strings remain opaque host-authored data. The host is responsible for semantic secret hygiene and must use runtime credential handles rather than embedding secret bytes. The crate does not claim to recognize secrets by content.

### 1.2 WorkflowDefinition

Logical key: `(scope, definition_id)`.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope` | `ExecutionScope` | Immutable | `create_definition` |
| `definition_id` | `Id` | Immutable | `create_definition` |
| `display_name` | UTF-8 string, 1–200 bytes | Mutable with `version` CAS | `create_definition`, `update_definition_metadata` |
| `description` | UTF-8 string, 0–4000 bytes | Mutable with `version` CAS | `create_definition`, `update_definition_metadata` |
| `created_at` | `Timestamp` | Immutable | `create_definition` |
| `created_by` | persistence-safe principal ID | Immutable | `create_definition` |
| `latest_revision_hash` | `Option<Digest>` | Mutable with `version` CAS | `publish_revision` |
| `version` | `Version` | Mutable with CAS | metadata update, `publish_revision` |

Deleting definitions or revisions is not a v0.1 command. Publication never changes an existing revision.

### 1.3 WorkflowRevision

Logical key: `(scope, definition_id, revision_hash)`. The row and its action-pin rows are immutable.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope` | `ExecutionScope` | Immutable | `publish_revision` |
| `definition_id` | `Id` | Immutable | `publish_revision` |
| `revision_hash` | `Digest` | Immutable; SHA-256 of canonical definition bytes | `publish_revision` |
| `definition_format_version` | exact string `"0.1"` | Immutable | `publish_revision` |
| `canonical_definition_ref` | `JsonRef` | Immutable | `publish_revision` after verified object put |
| `run_input_schema_ref` | `JsonRef` | Immutable; bytes are the validated supported-subset schema | `publish_revision` after verified object put |
| `run_output_schema_ref` | `JsonRef` | Immutable; bytes are the validated supported-subset schema | `publish_revision` after verified object put |
| `run_input_schema_digest` | `Digest` | Immutable; must equal definition field and schema object bytes | `publish_revision` |
| `run_output_schema_digest` | `Digest` | Immutable; must equal definition field and schema object bytes | `publish_revision` |
| `entry_node_id` | `Id` | Immutable | `publish_revision` |
| `node_count` | `u32` | Immutable | `publish_revision` |
| `node_topological_ranks` | ordered map `node_id -> u32` | Immutable; canonical Kahn ranks from section 1.5 | `publish_revision` |
| `action_pins` | ordered set of `ActionPin` keyed by reference location | Immutable | `publish_revision` |
| `published_at` | `Timestamp` | Immutable | `publish_revision` |
| `published_by` | persistence-safe principal ID | Immutable | `publish_revision` |

`ActionPin` is:

```text
{
  reference_location: node_id or "node_id/map_action",
  name: string,
  contract_version: string,
  input_schema_digest: Digest,
  output_schema_digest: Digest,
  compatible_implementation_requirement: Digest
}
```

All five executable pin fields are required for Action nodes and Map child actions. The immutable action-pin row additionally stores `input_schema_ref` and `output_schema_ref` to durable supported-subset SchemaDocument ArtifactRefs whose digests equal the two pin fields; the refs are resolution metadata, not extra compatibility dimensions.

### 1.4 WorkflowRun

Logical key: `(scope, run_id)`.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope` | `ExecutionScope` | Immutable | `create_run` |
| `run_id` | `Id` | Immutable | `create_run` |
| `definition_id` | `Id` | Immutable | `create_run` |
| `revision_hash` | `Digest` | Immutable; no live migration | `create_run` |
| `input_ref` | `JsonRef` | Immutable | `create_run` after verified put |
| `create_request_fingerprint` | `Digest` | Immutable; canonical fingerprint of all creation inputs | `create_run` |
| `status` | `RunState` | Mutable with CAS only by transitions in section 3 | Runtime commands named in section 3 |
| `failure_kind` | `Option<RunFailureKind>` | Set exactly once when entering `Failed` or `ContractFailed`; otherwise null | The command causing that terminal transition |
| `failure_diagnostics_ref` | `Option<JsonRef>` | Set with `failure_kind`; thereafter immutable | The command causing failure after verified put |
| `output_ref` | `Option<JsonRef>` | Set exactly once on `Succeeded`; may only be invalidated by `mark_corrupt_storage` | `resolve_terminal_node` |
| `budget_limit` | `CostUnits` | Immutable | `create_run` |
| `budget_consumed` | `CostUnits` | Mutable with CAS; monotonic | Attempt claim/settlement commands |
| `budget_reserved` | `CostUnits` | Mutable with CAS | Attempt claim/settlement commands |
| `dynamic_node_count` | `u64` | Mutable with CAS; monotonic | `expand_map` |
| `total_attempt_count` | `u64` | Mutable with CAS; monotonic | `claim_node_attempt` |
| `aggregate_object_bytes` | `u64` | Mutable with CAS; sum of charged run-data ArtifactRef sizes, charging repeated refs separately | Every runtime command registering charged run data |
| `limits` | `RunLimits` | Immutable | `create_run` |
| `lifetime_deadline_at` | `Timestamp` | Immutable; `created_at + limits.max_run_lifetime_ms` using checked arithmetic | `create_run` |
| `frontier_epoch` | `u64` | Mutable with CAS; incremented by every frontier change | Runtime commands |
| `last_event_seq` | `u64` | Mutable with CAS; starts at 0 | Every event-producing run command |
| `created_at` | `Timestamp` | Immutable | `create_run` |
| `updated_at` | `Timestamp` | Mutable with every run-row CAS | Every command mutating the run |
| `started_at` | `Option<Timestamp>` | Set once on first transition to `Running` | `start_run`, `resume_compatible` only if never started |
| `finished_at` | `Option<Timestamp>` | Set on terminal outcome; replaced only by later integrity override time | Terminal runtime command, `mark_corrupt_storage` |
| `blocked_incompatibilities_ref` | `Option<JsonRef>` | Set on suspension, cleared on compatible resume | `suspend_incompatible`, `resume_compatible` |
| `blocked_incompatibility_fingerprint` | `Option<Digest>` | Canonical exact-replay fingerprint set with suspension and cleared on compatible resume | `suspend_incompatible`, `resume_compatible` |
| `corrupt_bad_artifact_ref_id` | `Option<Id>` | Set exactly once on entry to `CorruptStorage`; otherwise null | `mark_corrupt_storage` |
| `corrupt_owner_node_id` | `Option<NodeInstanceId>` | Set with corruption when the bad ref is node-owned | `mark_corrupt_storage` |
| `corrupt_error_class` | `Option<Missing\|DigestInvalid>` | Set exactly once with corruption | `mark_corrupt_storage` |
| `corrupt_proof_fingerprint` | `Option<Digest>` | SHA-256 of the validated opaque proof envelope; set exactly once | `mark_corrupt_storage` |
| `version` | `Version` | Mutable with CAS | Every command mutating the run |

`RunLimits` uses `u64` fields and is immutable. Omitted host values receive these v0.1 defaults:

| Limit | Default | Absolute accepted maximum | Enforcement |
|---|---:|---:|---|
| `max_dynamic_node_instances` | 20,000 | 100,000 | Before Map expansion |
| `max_total_attempts` | 100,000 | 1,000,000 | Before A01 |
| `max_total_events` | 1,000,000 | 10,000,000 | Before every event batch |
| `max_inline_json_bytes_per_value` | 262,144 (256 KiB) | 16,777,216 (16 MiB) | Before binding, invocation, event-inline value, or output commit |
| `max_artifacts_per_attempt` | 32 | 1,024 | Before accepted action completion |
| `max_aggregate_object_bytes_per_run` | 1,073,741,824 (1 GiB) | 68,719,476,736 (64 GiB) | Before every charged run-data ArtifactRef registration |
| `max_run_lifetime_ms` | 2,592,000,000 (30 days) | 31,536,000,000 (365 days) | Database-clock cancellation deadline |

Charged run data includes run input, invocation inputs, Choice/Map inputs, action outputs/artifacts, approval request/decision/output, and Map/Succeed outputs. Each ArtifactRef use charges its size even when content deduplicates. Revision/schema refs, pre-existing literal refs, compatibility evidence, and persistence-safe diagnostics do not count. Compatibility evidence and each diagnostics value instead have a mandatory fixed maximum of 65,536 canonical JSON bytes, so suspension/terminal accounting cannot be blocked by exhausted data capacity. Oversized compatibility evidence makes `suspend_incompatible` return `EvidenceInvalid` with no transition or event. Oversized diagnostics make `complete_attempt` or `fail_contract`, the only commands accepting a diagnostics ref, return `DiagnosticsTooLarge{limit_bytes:65536,observed_bytes}` with no transition, ledger mutation, or event; the same completion may be resubmitted without diagnostics while its normal fencing/deadline preconditions still hold. This makes enforcement deterministic without consulting global deduplication state.

Closed `RunFailureKind`:

```text
ActionPermanent
ExplicitFailNode
MapChildFailed
ApprovalRejected
ApprovalExpiredRejected
RunDynamicNodeLimitExceeded
RunAttemptLimitExceeded
InlineJsonLimitExceeded
ArtifactsPerAttemptLimitExceeded
AggregateObjectLimitExceeded
RunOutputSchemaMismatch
BindingSourceUnavailable
BindingPointerMissing
BindingTypeMismatch
ActionOutputSchemaMismatch
ChoiceInputInvalid
MapInputInvalid
MapBoundExceeded
ApprovalPayloadInvalid
ActionCostProtocolViolation
```

No free-form failure category may affect control flow. Action-defined error codes and diagnostics are data, not outcome categories.

### 1.5 NodeRun

Logical key: `(scope, run_id, node_instance_id)`. All static NodeRuns are created with the run. Map child NodeRuns are created by `expand_map`.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope`, `run_id` | scope and `Id` | Immutable | `create_run`, `expand_map` |
| `node_instance_id` | `NodeInstanceId` | Immutable | creating command |
| `definition_node_id` | `Id` | Immutable; Map children name their parent’s definition node | creating command |
| `kind` | `Action`, `Map`, `Choice`, `Approval`, `Succeed`, `Fail` | Immutable; synthetic Map children are `Action` | creating command |
| `parent_map_instance_id` | `Option<NodeInstanceId>` | Immutable | `expand_map` |
| `map_item_index` | `Option<u32>` | Immutable | `expand_map` |
| `map_item_digest` | `Option<Digest>` | Immutable | `expand_map` |
| `topological_rank` | `u32` | Immutable; static nodes use the canonical definition rank, and Map children inherit their parent Map node's rank | `create_run`, `expand_map` |
| `status` | `NodeState` | Mutable with CAS only by section 3 | Runtime commands |
| `blocked_from_status` | `Option<Pending\|Ready\|RetryWaiting\|BudgetWaiting>` | Set with `BlockedIncompatible`, cleared on resume | compatibility commands |
| `active_attempt_id` | `Option<Id>` | Mutable with CAS; names the only attempt allowed to complete | `claim_node_attempt`; completion, timeout, recovery, cancellation clear it |
| `attempt_count` | `u32` | Mutable with CAS; incremented atomically at every successful claim | `claim_node_attempt` |
| `next_eligible_at` | `Option<Timestamp>` | Set when retry is scheduled, cleared when eligible | completion/timeout/recovery, `release_retry` |
| `budget_wait_amount` | `Option<CostUnits>` | Set while `BudgetWaiting`, cleared on claim/terminalization | `claim_node_attempt` |
| `result_ref` | `Option<JsonRef>` | Set exactly once on success | completion, Choice, Map, Approval, Succeed commands |
| `failure_kind` | `Option<NodeFailureKind>` | Set exactly once on entry to `Failed` or `ContractFailed`; otherwise null | failing runtime command |
| `failure_diagnostics_ref` | `Option<JsonRef>` | Set with failure | failing runtime command after verified put |
| `incoming_total` | `u32` | Immutable for static nodes; Map children use 0 | creating command |
| `incoming_satisfied` | `u32` | Mutable with CAS, derived from edge facts | frontier reducer inside runtime commands |
| `incoming_skipped` | `u32` | Mutable with CAS, derived from edge facts | frontier reducer inside runtime commands |
| `choice_input_ref` | `Option<JsonRef>` | Null until the single committed Choice decision, then immutable | `record_choice` |
| `choice_selected_case` | `Option<ChoiceSelection>` | Set with `choice_input_ref`, then immutable | `record_choice` |
| `map_input_ref` | `Option<JsonRef>` | Set once at expansion | `expand_map` |
| `map_expansion_digest` | `Option<Digest>` | Set once; digest of ordered `(index,item_digest,child_id)` tuples | `expand_map` |
| `map_child_count` | `Option<u32>` | Set once at expansion | `expand_map` |
| `approval_gate_id` | `Option<Id>` | Set once | `request_approval` |
| `created_at`, `updated_at` | `Timestamp` | created immutable; updated with CAS | creating/mutating commands |
| `version` | `Version` | Mutable with CAS | Every command mutating the node |

Closed `NodeFailureKind` is the same set as `RunFailureKind`. `ChoiceSelection` is either `{ case_index: u32, edge_id: Id }` or `{ default: true, edge_id: Id }`; it is closed. Invalid definitions never create a run; `publish_revision` returns the closed structured validation error described in section 5.

Canonical definition-node ranks are assigned at publication with Kahn's algorithm over the control graph. Compute every node's indegree, place all zero-indegree node IDs in a lexical min-priority queue, then repeatedly remove the lexical-minimum ID, assign the next consecutive `u32` rank starting at 0, decrement its outgoing targets in lexical edge-ID order, and enqueue any target whose indegree becomes zero. Failure to rank every node is the existing cycle publication error. `create_run` copies the resulting rank to every static NodeRun; `expand_map` copies the parent Map's rank to each synthetic child. The persisted rank, not row order or a recomputed implementation-specific index, is the recovery-order key in sections 3.4 and 4.

### 1.6 EdgeFact

This supporting control-plane entity makes frontier reconstruction declarative. Logical key: `(scope, run_id, edge_id)`.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope`, `run_id`, `edge_id` | scoped IDs | Immutable | `create_run` |
| `from_node_id`, `to_node_id` | `NodeInstanceId` | Immutable | `create_run` |
| `choice_case_index` | `Option<u32>` | Immutable | `create_run` |
| `state` | `Dormant`, `Satisfied`, `Skipped` | One-way CAS from `Dormant` to a terminal value | Frontier reducer in runtime commands |
| `resolved_at` | `Option<Timestamp>` | Set with terminal state | Frontier reducer |
| `version` | `Version` | Mutable once | Frontier reducer |

There is no command that directly edits an edge. `Satisfied` means its source completed successfully on an active path. `Skipped` means the path was not selected or its source node was itself skipped.

`edge_id` is `"edge_" + lowercase_hex(SHA-256(length_prefixed("dagger-edge-v1"), revision_hash bytes, from node ID, edge label, to node ID))`. Edge label is `next/<array-index>`, `case/<case-index>`, or `default`; array order is therefore revision meaning.

### 1.7 NodeAttempt

Logical key: `(scope, run_id, attempt_id)`. A row is inserted in `Started`. One CAS may make it terminal. Once terminal, the row is immutable forever.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope`, `run_id`, `attempt_id` | scoped IDs | Immutable | `claim_node_attempt` |
| `node_instance_id` | `NodeInstanceId` | Immutable | `claim_node_attempt` |
| `attempt_number` | `u32`, starting at 1 | Immutable | `claim_node_attempt` |
| `worker_id` | `Id` | Immutable | `claim_node_attempt` |
| `engine_instance_id`, `engine_generation` | `Id`, `u64` | Immutable | `claim_node_attempt` |
| `completion_credential_digest` | `Digest` | Immutable; the raw capability is returned exactly once by `claim_node_attempt` for delivery in `ActionContext`, but is never persisted durably, logged, or included in any event payload | `claim_node_attempt` |
| `invocation_id` | `Id` | Immutable; foreign key to ActionInvocation | `claim_node_attempt` |
| `idempotency_key` | string | Immutable; identical across retries of the node | `claim_node_attempt` |
| `status` | `AttemptState` | One CAS from `Started` to a closed terminal state | attempt terminalization commands |
| `declared_max_cost` | `CostUnits` | Immutable | `claim_node_attempt` |
| `reserved_cost` | `CostUnits` | Immutable; equals declared maximum | `claim_node_attempt` |
| `settled_cost` | `Option<CostUnits>` | Set with terminal status | attempt terminalization command |
| `deadline_at` | `Timestamp` | Immutable | `claim_node_attempt` |
| `started_at` | `Timestamp` | Immutable | `claim_node_attempt` |
| `finished_at` | `Option<Timestamp>` | Set with terminal status | attempt terminalization command |
| `output_ref` | `Option<JsonRef>` | Set with `Succeeded`, then immutable | `complete_attempt` |
| `artifact_refs` | ordered list of `ArtifactRef` keys | Set with terminal completion, then immutable | `complete_attempt` |
| `error_class` | `Option<Retryable\|Permanent\|Contract>` | Set with corresponding terminal outcome | `complete_attempt` |
| `error_code` | `Option<String>` | Persistence-safe, namespaced action code; data only | `complete_attempt` |
| `diagnostics_ref` | `Option<JsonRef>` | Set at terminalization after verified put | terminalization command |

`UnknownOutcome`, every timeout, cancellation without a trusted actual cost, and stale live attempts settle at the full reservation. Every inserted attempt consumes one retry-ceiling slot even if invocation had not begun before a crash.

### 1.8 ActionInvocation

Logical key: `(scope, run_id, invocation_id)`; `invocation_id = attempt_id` in v0.1. The row is immutable and inserted atomically with A01.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope`, `run_id`, `invocation_id` | scoped IDs | Immutable | `claim_node_attempt` |
| `node_instance_id`, `attempt_id` | IDs | Immutable | `claim_node_attempt` |
| `action_reference_location` | string | Immutable | `claim_node_attempt` |
| `action_name`, `contract_version` | strings | Immutable; copied from revision pin | `claim_node_attempt` |
| `revision_hash` | `Digest` | Immutable; copied from the owning run | `claim_node_attempt` |
| `input_schema_digest`, `output_schema_digest` | `Digest` | Immutable; copied from revision pin | `claim_node_attempt` |
| `compatible_implementation_requirement` | `Digest` | Immutable semantic-compatibility pin | `claim_node_attempt` |
| `bound_input_ref` | `JsonRef` | Immutable | `claim_node_attempt` after verified put |
| `bound_input_digest` | `Digest` | Immutable; SHA-256 of the exact canonical bytes delivered | `claim_node_attempt` |
| `bound_input_size_bytes` | `u64` | Immutable; within run inline-value limit | `claim_node_attempt` |
| `binding_derivation_digest` | `Digest` | Immutable; digest of ordered target/source/value-digest derivation records | `claim_node_attempt` |
| `created_at` | database `Timestamp` | Immutable | `claim_node_attempt` |

The action receives exactly `bound_input_ref` bytes after read verification. It never receives a newly reconstructed object that merely compares equal.

### 1.9 ApprovalGate

Logical key: `(scope, run_id, gate_id)`.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope`, `run_id`, `gate_id`, `node_instance_id` | scoped IDs | Immutable | `request_approval` |
| `request_ref` | `JsonRef` | Immutable | `request_approval` after verified put |
| `status` | `GateState` | One CAS from `Pending` to a terminal state | decision, expiry, cancellation commands |
| `expires_at` | `Timestamp` | Immutable | `request_approval`, computed from DB time |
| `on_expiry` | `Approve` or `Reject` | Immutable | `request_approval` |
| `authorization_policy` | `DecisionAuthorizationPolicy` | Immutable | `request_approval`, copied from revision |
| `decision_payload_ref` | `Option<JsonRef>` | Set by a human decision, then immutable | `decide_approval` |
| `deciding_principal` | `Option<String>` | Set by a human decision, then immutable | `decide_approval` |
| `resolution_source` | `Option<Human\|Expiry\|Cancellation>` | Set with terminal status | resolving command |
| `decided_at` | `Option<Timestamp>` | Set with terminal status | resolving command |
| `decision_fingerprint` | `Option<Digest>` | Versioned length-prefixed SHA-256 of decision, decision-payload digest/null tag, approval-output digest/null tag, principal ID, and authentication-context digest; supports exact duplicate detection | `decide_approval` |
| `version` | `Version` | Mutable once | resolving command |

Closed `GateState`: `Pending`, `Approved`, `Rejected`, `ExpiredApproved`, `ExpiredRejected`, `Cancelled`.

`DecisionAuthorizationPolicy` is `{ allowed_principal_ids: ordered unique list<String>, allowed_role_ids: ordered unique list<String> }`; at least one list is non-empty. A human decision is authorized when its authenticated principal ID is allowed or at least one authenticated role is allowed.

### 1.10 ArtifactRef and ObjectRecord

An `ObjectRecord` describes reusable content; an `ArtifactRef` describes one typed use of that content. This avoids conflating equal bytes produced by different attempts or used for different roles. Content-address deduplication remains scope-local.

`ObjectRecord` logical key: `(scope, digest)`.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope`, `digest` | scoped digest | Immutable | any registering command after verified put |
| `size_bytes` | `u64` | Immutable | same |
| `object_key` | store-private scoped key | Immutable | object store, registered by domain command |
| `created_at` | `Timestamp` | Immutable | first registering command |

If candidate publication or re-registration encounters the same scoped digest, size and durable object bytes must match; a different size or bytes fails `ArtifactMetadataConflict`. `FailedReadProof` is not a candidate-publication disposition and is reserved for failed reads of already-committed refs.

`ArtifactRef` logical key: `(scope, artifact_ref_id)`. “Registering command” below means exactly one of `publish_revision`, `suspend_incompatible`, `create_run`, `claim_node_attempt`, `complete_attempt`, `record_choice`, `expand_map`, `complete_map`, `request_approval`, `decide_approval`, `expire_approval`, `resolve_terminal_node`, or `fail_contract`.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope`, `artifact_ref_id` | scoped `Id` | Immutable | registering command |
| `digest` | `Digest`; foreign key to scoped ObjectRecord | Immutable | registering command |
| `size_bytes` | `u64`, equal to ObjectRecord | Immutable | registering command |
| `media_type` | normalized MIME string | Immutable | registering command |
| `kind` | `RunInput`, `SchemaDocument`, `Definition`, `NodeOutput`, `ActionInvocationInput`, `ActionArtifact`, `Diagnostics`, `CompatibilityEvidence`, `ChoiceInput`, `MapInput`, `MapAggregate`, `ApprovalRequest`, `ApprovalDecisionPayload` | Immutable | registering command |
| `producer_run_id`, `producer_node_id`, `producer_attempt_id` | nullable correlation IDs | Immutable | registering command |
| `ordinal` | `u32`; distinguishes ordered artifacts from one producer | Immutable | registering command |
| `created_at` | `Timestamp` | Immutable | registering command |

`artifact_ref_id` is deterministic from length-prefixed domain `"dagger-artifact-ref-v1"`, scope atoms, digest, kind, producer correlation tuple, and ordinal. Exact re-registration is idempotent; a conflicting tuple is `ArtifactMetadataConflict`. Equal bytes may therefore have several typed refs without duplicating the object.

### 1.11 CommandReceipt

Logical key: `(scope, command_kind, idempotency_token)`. v0.1 requires receipts for `create_run` and `cancel_run`; adapters may use the same entity for other commands.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope`, `command_kind`, `idempotency_token` | scoped key, closed command name, opaque 128-bit-or-stronger token | Immutable | `create_run`, `cancel_run` |
| `request_fingerprint` | `Digest` of canonical scope-bound request | Immutable | creating command |
| `run_id` | `Id` | Immutable | creating command |
| `outcome` | closed `CommandReceiptOutcome` below | Immutable | creating command |
| `batch_id` | `Id` | Immutable; event batch that committed the outcome | creating command |
| `committed_at` | database `Timestamp` | Immutable | creating command |

An exact replay returns `outcome` without re-executing state logic. A token with a different request fingerprint returns `IdempotencyConflict`. Failed transactions create no receipt.

`CommandReceiptOutcome` is exactly one of:

```text
CreateRunCommitted {
  run_id, status: Pending, run_version,
  batch_id, first_event_seq, last_event_seq
}
CancelRunCommitted {
  run_id, prior_status: Pending | Running | BlockedIncompatible,
  status: Cancelled, run_version,
  batch_id, first_event_seq, last_event_seq
}
```

IDs have type `Id`, versions and event sequences are `u64`, and the event interval is inclusive. No command-specific response may be added without a contract-version change.

### 1.12 WorkflowEvent

Logical key: `(scope, run_id, event_seq)`. Events are immutable and never deleted in v0.1.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope`, `run_id` | scoped run key | Immutable | event-producing command |
| `event_seq` | `u64` | Immutable; allocated as `last_event_seq + 1` inside the same transaction | event-producing command |
| `event_type` | closed `EventType` from section 15 | Immutable | event-producing command |
| `transition_id` | section 3 row ID | Immutable | event-producing command |
| `batch_id` | store-minted `Id` unique for the complete scoped run lifetime | Immutable; shared by every event in one transaction; database uniqueness is `(scope, run_id, batch_id)` | event-producing command |
| `batch_index` | `u32` | Immutable; zero-based within the batch | event-producing command |
| `batch_count` | `u32` | Immutable; identical for the batch | event-producing command |
| `occurred_at` | database `Timestamp` | Immutable | event-producing command |
| `actor_kind` | `Engine`, `ActionCompletion`, `Host`, `Recovery`, `Clock` | Immutable | event-producing command |
| `actor_id` | persistence-safe identifier | Immutable | event-producing command |
| `node_instance_id`, `attempt_id`, `gate_id` | nullable correlation IDs | Immutable | event-producing command |
| `payload` | event-specific small JSON object | Immutable | event-producing command |

When one transaction performs several transition rows, the store builds and writes the event batch using the single normative ordering algorithm in section 15.1. This entity section does not define a second or abbreviated ordering.

### 1.13 ExecutionScope

`ExecutionScope` is an immutable value entity embedded in every durable key, foreign key, unique constraint, index prefix, command parameter, object key, event cursor, and engine claim. There is no global default scope and no API that converts an unscoped ID to a scoped ID. Scope is copied from the command, never inferred from a found row.

### 1.14 EngineInstanceClaim

Logical key: `(scope, control_plane_id)`; `control_plane_id` is fixed as `"scheduler"` for v0.1.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope`, `control_plane_id` | scoped singleton key | Immutable | `acquire_engine_claim` |
| `instance_id` | `Id` | Mutable only on expired takeover CAS | `acquire_engine_claim` |
| `generation` | `u64`, starting at 1 | Incremented on every expired takeover; never reset | `acquire_engine_claim` |
| `session_token_digest` | `Digest` | Replaced on each acquisition/takeover; raw token returned once | `acquire_engine_claim` |
| `claimed_at` | `Timestamp` | Replaced on takeover | `acquire_engine_claim` |
| `heartbeat_at` | `Timestamp` | Mutable with owner/generation CAS | acquire/heartbeat |
| `expires_at` | `Timestamp` | Mutable with owner/generation CAS | acquire/heartbeat |
| `version` | `Version` | Mutable with CAS | acquire/heartbeat |

Release is deliberately not required for liveness. `release_engine_claim` may set `expires_at` to database-now using owner/generation/session-token CAS as a graceful optimization; it does not delete the row or reset generation.

### 1.15 BudgetLedgerEntry

Logical key: `(scope, run_id, ledger_seq)`. Entries are immutable. `ledger_seq` is independent of event sequence but allocated in the same transaction as its corresponding event.

| Field | Type | Mutability | Writer |
|---|---|---|---|
| `scope`, `run_id`, `ledger_seq` | scoped key and `u64` | Immutable | `claim_node_attempt`, `complete_attempt`, `timeout_attempt`, `recover_abandoned_attempts_for_run`, `cancel_run`, or a fail-fast terminal reducer |
| `attempt_id`, `node_instance_id` | IDs | Immutable | same commands |
| `kind` | `Reserve`, `SettleActual`, `SettleFullUnknown` | Immutable | same commands |
| `reserved_delta` | signed `i128` constrained to exact `u64` magnitude | Immutable | same commands |
| `consumed_delta` | `CostUnits` | Immutable | same commands |
| `reservation_amount` | `CostUnits` | Immutable | same commands |
| `reason` | `Started`, `Succeeded`, `Retryable`, `Permanent`, `Contract`, `TimedOut`, `UnknownOutcome`, `Cancelled`, `Stale` | Immutable | same commands |
| `created_at` | database `Timestamp` | Immutable | same commands |

For `Reserve`, `reserved_delta = +reservation_amount` and `consumed_delta = 0`. For settlement, `reserved_delta = -reservation_amount`; `consumed_delta` is actual cost or the full reservation. Raw reserve or settle operations are not public store commands.

## 2. Closed outcome vocabulary

### 2.1 Run states

| State | Class | Meaning | Entry event |
|---|---|---|---|
| `Pending` | Active | Durable run exists but compatibility has not yet been accepted. | `RunCreated` |
| `Running` | Active | Scheduling and clock transitions are allowed. | `RunStarted` or `RunResumedCompatible` |
| `BlockedIncompatible` | Suspended, recoverable | One or more pinned action implementations are absent or incompatible. No node may be claimed. | `RunBlockedIncompatible` |
| `Succeeded` | Terminal, subject only to integrity override | All active paths completed and at least one active Succeed node completed. | `RunSucceeded` |
| `Failed` | Terminal, subject only to integrity override | A permanent action, explicit Fail node, Map fail-fast child, or rejected approval failed the run. | `RunFailed` |
| `ContractFailed` | Terminal, subject only to integrity override | A closed contract-validation failure occurred at runtime. | `RunContractFailed` |
| `RetriesExhausted` | Terminal, subject only to integrity override | A retryable, timed-out, or crash-unknown attempt consumed the last allowed attempt. | `RunRetriesExhausted` |
| `BudgetExhausted` | Terminal, subject only to integrity override | A declared maximum exceeds `budget_limit - budget_consumed` and therefore cannot fit even after all active reservations settle to zero. | `RunBudgetExhausted` |
| `Cancelled` | Terminal, subject only to integrity override | Host cancellation, run-lifetime expiry, or event-cap enforcement committed. | `RunCancelled` |
| `CorruptStorage` | Terminal and absorbing | A committed object reference was missing or digest-invalid when read. | `RunCorruptStorage` |

The only legal transition out of a terminal run state is the integrity override to `CorruptStorage`. `BlockedIncompatible` is not terminal. A host may explicitly cancel it. Recheck occurs on every engine acquisition and through `resume_compatible`; a matching registry snapshot transitions it to `Running`.

### 2.2 Node states

| State | Class | Meaning | Entry event |
|---|---|---|---|
| `Pending` | Inactive | Incoming edge facts are not all terminal. | `NodeCreatedPending` |
| `Ready` | Frontier | All incoming facts are terminal, at least one is satisfied, and the node is eligible except for action retry timing. Entry and Map-child nodes start here. | `NodeCreatedReady`, `MapChildCreated`, `NodeBecameReady`, or `NodeRetryEligible` |
| `Running` | Active | An Action attempt is active. | `NodeAttemptClaimed` |
| `RetryWaiting` | Suspended by clock | A retry is due at persisted `next_eligible_at`. | `NodeRetryScheduled` |
| `BudgetWaiting` | Suspended by accounting | The declared maximum can fit after live reservations settle, but not now. No attempt exists for this wait. | `NodeBudgetWaiting` |
| `WaitingApproval` | Suspended by gate | A durable pending ApprovalGate exists. | `ApprovalRequested` |
| `WaitingChildren` | Active | A non-empty Map expansion is durable and children are incomplete. | `MapExpanded` |
| `BlockedIncompatible` | Suspended, recoverable | This node’s pinned action is unavailable; prior state is retained in `blocked_from_status`. | `NodeBlockedIncompatible` |
| `Succeeded` | Terminal, subject only to integrity override | Node semantics completed successfully. | `NodeSucceeded`, `ChoiceSelected`, `MapZeroItemsSucceeded`, `MapSucceeded`, `ApprovalApproved`, `ApprovalExpiredApproved`, or `SucceedNodeReached` |
| `Failed` | Terminal, subject only to integrity override | Closed permanent node failure. | `NodeFailed`, `ApprovalRejected`, `ApprovalExpiredRejected`, `FailNodeReached`, or `MapFailedFast` |
| `ContractFailed` | Terminal, subject only to integrity override | Runtime input/output/config contract failure. | `NodeContractFailed` or `MapContractFailed` |
| `RetriesExhausted` | Terminal, subject only to integrity override | The node consumed its maximum attempt count. | `NodeRetriesExhausted` or `MapRetriesExhausted` |
| `BudgetExhausted` | Terminal, subject only to integrity override | The declared maximum is permanently infeasible because it exceeds `budget_limit - budget_consumed`. | `NodeBudgetExhausted` or `MapBudgetExhausted` |
| `Cancelled` | Terminal, subject only to integrity override | A run-terminalization cascade cancelled this node. | `NodeCancelled` |
| `CorruptStorage` | Terminal and absorbing | A required committed object was missing or invalid. | `NodeCorruptStorage` |
| `Skipped` | Terminal | No incoming active path reaches this node. | `NodeSkipped` |

### 2.3 Attempt states

| State | Class | Meaning | Entry event |
|---|---|---|---|
| `Started` | Live | Reservation and retry slot are committed; invocation may or may not yet have begun. | `AttemptStarted` |
| `Succeeded` | Terminal immutable | Accepted success matched `active_attempt_id`. | `AttemptSucceeded` |
| `RetryableFailed` | Terminal immutable | Accepted structured retryable action error. | `AttemptRetryableFailed` |
| `PermanentFailed` | Terminal immutable | Accepted structured permanent action error. | `AttemptPermanentFailed` |
| `ContractFailed` | Terminal immutable | Action output/cost violated its pinned contract. | `AttemptContractFailed` |
| `TimedOut` | Terminal immutable | Database deadline became due before accepted completion. | `AttemptTimedOut` |
| `UnknownOutcome` | Terminal immutable | Recovery found an attempt left `Started` by a dead engine generation. It consumes a retry slot and full reservation. | `AttemptOutcomeUnknown` |
| `Cancelled` | Terminal immutable | A run-terminalization cascade terminalized a live attempt. | `AttemptCancelled` |
| `Stale` | Terminal immutable | A still-`Started` attempt submitted completion while `NodeRun.active_attempt_id` named another attempt. | `AttemptMarkedStale` |

If late completion targets any already-terminal attempt, its state is not rewritten. The only durable effect is `StaleCompletionObserved`.

### 2.4 Gate and edge states

Gate states are exactly `Pending`, `Approved`, `Rejected`, `ExpiredApproved`, `ExpiredRejected`, and `Cancelled`; entry events have the corresponding names in section 15. Edge states are exactly `Dormant`, `Satisfied`, and `Skipped`; their terminal entry events are `EdgeSatisfied` and `EdgeSkipped`.

## 3. Complete state-transition table

### 3.1 Reading the table

Each row is one legal durable state transition. `∅` means row creation. Source-to-same-source rows are explicitly identified observation transitions; they append an event but do not rewrite immutable state. Any transition not listed is illegal.

Every command CASes the `WorkflowRun.version` it read inside its transaction. Commands that define a user-visible race (`cancel_run`, `decide_approval`) additionally require the caller’s observed run/gate versions. Long-running action completions do not require an obsolete claim-time run version; their decisive CAS is the attempt state plus `NodeRun.active_attempt_id`. Engine actors additionally require a live matching `EnginePermit`. “Node CAS” means `(version, status, active_attempt_id where relevant)`. “Gate CAS” means `(version, status)`. A CAS failure commits no partial row, ledger entry, frontier fact, or event.

“Initiating section 5 terminal command” in cascade rows means the exact atomic command whose transition-list cell cross-references that row; it is never a separate public reducer. Its actor remains the initiating actor: host API, authenticated action completion, engine scheduler, recovery, or clock as named by that command.

The frontier reducer runs inside the initiating command’s transaction:

1. A successful active node changes each normal outgoing `Dormant` edge to `Satisfied`.
2. An unselected Choice edge changes to `Skipped`.
3. In lexical node-ID order, when all a `Pending` node’s incoming edges are terminal:
   - if `incoming_satisfied > 0`, it becomes `Ready`;
   - otherwise it becomes `Skipped`, and its outgoing `Dormant` edges become `Skipped`.
4. Steps 2–3 repeat to a fixed point in the same transaction.
5. A Map child has no static edge facts. Its success contributes only to its parent Map’s completion test.

This fixed point is finite because published graphs are acyclic. Frontier changes increment `frontier_epoch` once per command, not once per affected row. Each affected row still gets its own event.

### 3.2 Run transitions

| ID | Source | Target | Actor and command | CAS precondition | Resulting event | Side effects |
|---|---|---|---|---|---|---|
| R01 | `∅` | `Pending` | Host API, `create_run` | Scoped run absent; revision/root schemas verify; input/schema/limits validate; idempotency fingerprint absent or exact replay | `RunCreated` | Insert receipt, immutable limits/lifetime/fingerprint, run, static graph/frontier, budget/event counters, `aggregate_object_bytes=input.size_bytes`, and one creation batch. |
| R02 | `Pending` | `Running` | Engine scheduler, `start_run` | Run version/status; live session permit; availability evidence covers every exact action pin | `RunStarted` | Set `started_at`; scheduling may begin. No frontier/budget change. |
| R03 | `Pending` | `BlockedIncompatible` | Engine scheduler, `suspend_incompatible` | Run version/status; live session permit; evidence names at least one unavailable exact semantic digest | `RunBlockedIncompatible` | Store incompatibility ref and exact request fingerprint; affected Pending/Ready nodes follow N29/N30. No budget change. |
| R04 | `Running` | `BlockedIncompatible` | Recovery or engine scheduler, `suspend_incompatible` | Run version/status; live permit; all abandoned attempts have first been terminalized; no `Started` attempt remains; evidence names an unavailable exact semantic digest | `RunBlockedIncompatible` | Store incompatibility ref and exact request fingerprint; affected nodes follow N29–N31/N61. Stop new claims; preserve frontier, retry time, and budget waits. |
| R05 | `BlockedIncompatible` | `Running` | Host-triggered engine API or engine-startup recheck, `resume_compatible` | Run version/status; live permit; fresh registry evidence proves every exact pinned semantic digest is available | `RunResumedCompatible` | Clear incompatibility ref and suspension fingerprint; restore blocked nodes by N32–N34/N62. No override or substitute digest is accepted. |
| R06 | `Running` | `Succeeded` | Engine scheduler, `resolve_terminal_node` | Run version/status; a Succeed node just completed, no active nonterminal path remains, and every maximal active path ended at Succeed | `RunSucceeded` | Persist final output ref; frontier fixed point already complete; no reserved budget may remain. |
| R07 | `Running` | `Failed` | Action completion, engine scheduler, host API, or clock via `complete_attempt`, `resolve_terminal_node`, `decide_approval`, or `expire_approval` | Run version/status; corresponding node failure transition succeeds | `RunFailed` | Set closed failure kind/diagnostics; cancel all remaining nonterminal nodes and live attempts in the same transaction; settle live reservations full; cancel pending gates. |
| R08 | `Running` | `ContractFailed` | Engine scheduler, authenticated action completion, host approval, or clock via `fail_contract`, `complete_attempt`, `record_choice`, `expand_map`, `complete_map`, `request_approval`, `decide_approval`, `expire_approval`, `resolve_terminal_node`, or `claim_node_attempt` | Run version/status; a closed contract or run-limit failure and affected node are supplied | `RunContractFailed` | Set closed failure kind/diagnostics; fail-fast cancellation and full settlement as R07. |
| R09 | `Running` | `RetriesExhausted` | Action completion, clock, or recovery via `complete_attempt`, `timeout_attempt`, or `recover_abandoned_attempts_for_run` | Run version/status; attempt number equals retry policy `max_attempts` and outcome is retryable, timeout, or unknown | `RunRetriesExhausted` | Affected node follows N24–N26; Map parent mirrors exhaustion; cancel remaining work/gates; settle all reservations. |
| R10 | `Running` | `BudgetExhausted` | Engine scheduler, `claim_node_attempt` | Run version/status; node Ready or BudgetWaiting; `declared_max > budget_limit - budget_consumed`; no attempt inserted | `RunBudgetExhausted` | Node follows N27/N60; append terminal refusal event but no ledger reservation; cancel remaining work/gates and settle live reservations full. |
| R11 | `Pending` | `Cancelled` | Host API `cancel_run`, database clock `expire_run_lifetime`, or internal `event_capacity_guard` | Run version/source CAS; host cancellation also checks caller-observed version; clock requires DB-now at lifetime deadline | `RunCancelled` | Cancel nonterminal nodes/gates. System reason is `RunLifetimeExceeded` or `RunEventLimitExceeded` when applicable. |
| R12 | `Running` | `Cancelled` | Host API `cancel_run`, database clock `expire_run_lifetime`, or internal `event_capacity_guard` | Run version/source CAS; host gate race checks observed versions; clock requires DB-now at lifetime deadline | `RunCancelled` | Cancel all nonterminal nodes/live attempts/pending gates; signal tokens after commit; live reservations settle full. |
| R13 | `BlockedIncompatible` | `Cancelled` | Host API `cancel_run`, database clock `expire_run_lifetime`, or internal `event_capacity_guard` | Run version/source CAS; host call checks observed version; clock requires DB-now at lifetime deadline | `RunCancelled` | Cancel BlockedIncompatible, Pending, Ready, RetryWaiting, BudgetWaiting, WaitingApproval, and WaitingChildren nodes and Pending gates; no live attempt exists by R04 invariant. |
| R14 | `Pending` | `CorruptStorage` | Recovery or host read path, `mark_corrupt_storage` | Run version/status; object-store-minted FailedReadProof matches committed scoped ref/digest/store nonce | `RunCorruptStorage` | Record proof fingerprint/bad ref; cancel nonterminal nodes. Never invoke an action to repair it. |
| R15 | `Running` | `CorruptStorage` | Engine, recovery, or host read path, `mark_corrupt_storage` | Same as R14 | `RunCorruptStorage` | Cancel work/gates; settle live reservations full; signal tokens after commit. |
| R16 | `BlockedIncompatible` | `CorruptStorage` | Recovery or host read path, `mark_corrupt_storage` | Same as R14 | `RunCorruptStorage` | Mark affected node if any; cancel suspended work. |
| R17 | `Succeeded` | `CorruptStorage` | Host read path or recovery audit, `mark_corrupt_storage` | Same as R14, including run output or referenced node output | `RunCorruptStorage` | Integrity override; preserve prior status in event payload, invalidate usable output. |
| R18 | `Failed` | `CorruptStorage` | Host read path or recovery audit, `mark_corrupt_storage` | Same as R14 | `RunCorruptStorage` | Integrity override; preserve prior outcome in event payload. |
| R19 | `ContractFailed` | `CorruptStorage` | Host read path or recovery audit, `mark_corrupt_storage` | Same as R14 | `RunCorruptStorage` | Same as R18. |
| R20 | `RetriesExhausted` | `CorruptStorage` | Host read path or recovery audit, `mark_corrupt_storage` | Same as R14 | `RunCorruptStorage` | Same as R18. |
| R21 | `BudgetExhausted` | `CorruptStorage` | Host read path or recovery audit, `mark_corrupt_storage` | Same as R14 | `RunCorruptStorage` | Same as R18. |
| R22 | `Cancelled` | `CorruptStorage` | Host read path or recovery audit, `mark_corrupt_storage` | Same as R14 | `RunCorruptStorage` | Same as R18. |

### 3.3 Node transitions

| ID | Source | Target | Actor and command | CAS precondition | Resulting event | Side effects |
|---|---|---|---|---|---|---|
| N01 | `∅` | `Pending` | Host API, `create_run` | R01; node is not the entry node | `NodeCreatedPending` | Insert static node with its canonical persisted `topological_rank`; no budget change. |
| N02 | `∅` | `Ready` | Host API, `create_run` | R01; node is the unique entry node | `NodeCreatedReady` | Insert on frontier with its canonical persisted `topological_rank`; increment initial frontier epoch. |
| N02M | `∅` | `Ready` | Engine scheduler, `expand_map` | N06; synthetic child ID absent, required hash matches, and run dynamic-node limit admits the whole set | `MapChildCreated` | Insert one child per ordered item inheriting the parent Map's `topological_rank`; no budget is reserved until N05/N58. |
| N03 | `Pending` | `Ready` | Host API, action completion, engine scheduler, recovery, or clock; frontier reducer inside the initiating section 5 runtime command | Node version/status; all incoming terminal and at least one satisfied | `NodeBecameReady` | Add to frontier; no budget change. |
| N04 | `RetryWaiting` | `Ready` | Clock, `release_retry` | Node version/status; DB-now `>= next_eligible_at`; run Running | `NodeRetryEligible` | Clear `next_eligible_at`; add to frontier. |
| N05 | `Ready` | `Running` | Engine scheduler, `claim_node_attempt` | Node version/status; no active attempt; run attempt limit not reached; canonical bound input verified; retry count below maximum; Map slot available; budget reservation succeeds | `NodeAttemptClaimed` | Persist ActionInvocation, set active attempt, increment node/run attempt counts, remove from frontier; A01 and budget reserve occur atomically. |
| N58 | `BudgetWaiting` | `Running` | Engine scheduler, `claim_node_attempt` | Node version/status and wait amount; no active attempt; run attempt limit not reached; canonical bound input verified; `available >= declared_max`; Map slot available | `NodeAttemptClaimed` | Clear budget wait, persist ActionInvocation, set active attempt, increment counts; A01 and budget reserve occur atomically. |
| N06 | `Ready` | `WaitingChildren` | Engine scheduler, `expand_map` | Node version/status; expansion fields null; bound array length is 1..=`max_items` | `MapExpanded` | Persist input/expansion digest; insert exact child set as Ready; parent waits; no budget reservation yet. |
| N07 | `Ready` | `Succeeded` | Engine scheduler, `expand_map` | Same as N06; bound array length is 0 | `MapZeroItemsSucceeded` | Persist verified empty-array aggregate; expansion digest/child count 0; run frontier reducer. |
| N08 | `WaitingChildren` | `Succeeded` | Engine scheduler, `complete_map` | Node version/status; all children Succeeded; aggregate verified/ordered and schema/inline/cumulative-byte limits pass | `MapSucceeded` | Store aggregate, increment aggregate-byte counter, satisfy outgoing edges, and reduce frontier. |
| N09 | `Ready` | `Succeeded` | Engine scheduler, `record_choice` | Node version/status; decision fields null; verified input digest; selected first matching case or the required default | `ChoiceSelected` | Persist input/selection; exactly selected edge E01, every other outgoing edge E02; fixed-point skip/readiness propagation. |
| N11 | `Ready` | `WaitingApproval` | Engine scheduler, `request_approval` | Node version/status; gate absent; request verified/within limit; revision gate authorization policy valid | `ApprovalRequested` | Insert G01 including authorization policy; remove node from frontier; no budget change. |
| N12 | `WaitingApproval` | `Succeeded` | Host API, `decide_approval` | Node version/status; G02 succeeds; caller-observed run/gate versions; supplied output bytes exactly equal the canonical human `ApprovalResult` envelope | `ApprovalApproved` | Store the ApprovalResult as `result_ref`; satisfy outgoing edges and run the frontier reducer. |
| N13 | `WaitingApproval` | `Failed` | Host API, `decide_approval` | Node version/status; G03 succeeds | `ApprovalRejected` | R07 with `ApprovalRejected`; cancel remaining work. |
| N14 | `WaitingApproval` | `Succeeded` | Clock, `expire_approval` | Node version/status; G04 succeeds at DB-now `>= expires_at`; supplied output bytes exactly equal the canonical expiry `ApprovalResult` envelope | `ApprovalExpiredApproved` | Store the ApprovalResult as `result_ref`, satisfy outgoing edges, and run the frontier reducer. |
| N15 | `WaitingApproval` | `Failed` | Clock, `expire_approval` | Node version/status; G05 succeeds at DB-now `>= expires_at` | `ApprovalExpiredRejected` | R07 with `ApprovalExpiredRejected`. |
| N16 | `Ready` | `Succeeded` | Engine scheduler, `resolve_terminal_node` | Node version/status; it is the definition’s unique Succeed; verified output matches pinned root output schema/value limit | `SucceedNodeReached` | No outgoing edge; R06 in the same transaction only when no other active path remains. |
| N17 | `Ready` | `Failed` | Engine scheduler, `resolve_terminal_node` | Node version/status; kind Fail | `FailNodeReached` | R07 with `ExplicitFailNode`. |
| N18 | `Running` | `Succeeded` | Credential-authenticated action completion, `complete_attempt` | Node active attempt equals Started attempt; run Running; DB-now `< deadline`; output/artifact/value limits and cost validate | `NodeSucceeded` | A02; settle actual; store output/artifacts; clear active; static node satisfies edges; Map child makes parent eligible for later aggregation. |
| N19 | `Running` | `RetryWaiting` | Credential-authenticated action completion, `complete_attempt` | Active Started attempt; run Running; DB-now `< deadline`; retryable result; attempt number `< max_attempts` | `NodeRetryScheduled` | A03; settle actual; clear active; persist DB-clock-derived `next_eligible_at`; no frontier edge changes. |
| N20 | `Running` | `Failed` | Credential-authenticated action completion, `complete_attempt` | Active Started attempt; run Running; DB-now `< deadline`; permanent result | `NodeFailed` | A04; settle actual; R07; if child, N42 before R07. |
| N21 | `Running` | `ContractFailed` | Credential-authenticated action completion, `complete_attempt` | Active Started attempt; run Running; DB-now `< deadline`; output/schema/value/artifact or cost protocol violation | `NodeContractFailed` | A05; charge valid reported actual, otherwise full reservation; R08; if child, N43. |
| N22 | `Running` | `RetryWaiting` | Clock via `timeout_attempt`, or action completion arriving due via `complete_attempt` | Active Started attempt; DB-now `>= deadline_at`; attempt number `< max_attempts` | `NodeRetryScheduled` | A06; clear active; settle full reservation; persist next eligibility; signal cancellation after commit. A due submitted completion also appends A14. |
| N23 | `Running` | `RetryWaiting` | Recovery, `recover_abandoned_attempts_for_run` | The command’s complete old-generation attempt set includes this active Started attempt; attempt number `< max_attempts` | `NodeRetryScheduled` | After every abandoned attempt has A07/full settlement, clear active and persist next eligibility. |
| N24 | `Running` | `RetriesExhausted` | Credential-authenticated action completion, `complete_attempt` | A03 condition and attempt number `= max_attempts` | `NodeRetriesExhausted` | A03; settle; R09; Map parent N44 if child. |
| N25 | `Running` | `RetriesExhausted` | Clock or due action completion via `timeout_attempt` or `complete_attempt` | A06 condition and attempt number `= max_attempts` | `NodeRetriesExhausted` | A06; full settle; R09; Map parent N44 if child. A due submitted completion also appends A14. |
| N26 | `Running` | `RetriesExhausted` | Recovery, `recover_abandoned_attempts_for_run` | Complete abandoned set contains this attempt and attempt number `= max_attempts` | `NodeRetriesExhausted` | After all A07/full settlements, deterministic exhaustion reduction applies R09; Map parent N44 if child. |
| N27 | `Ready` | `BudgetExhausted` | Engine scheduler, `claim_node_attempt` | Node CAS; `declared_max > budget_limit - budget_consumed` | `NodeBudgetExhausted` | No attempt/reservation; R10; Map parent N45 if child. |
| N59 | `Ready` | `BudgetWaiting` | Engine scheduler, `claim_node_attempt` | Node CAS; `available < declared_max <= budget_limit - budget_consumed`; shortage is solely active reservations | `NodeBudgetWaiting` | Persist wait amount; remove from Ready scan; run remains Running; no attempt, reservation, or ledger entry. |
| N60 | `BudgetWaiting` | `BudgetExhausted` | Engine scheduler, `claim_node_attempt` | Node CAS; settlement increased consumed such that `declared_max > budget_limit - budget_consumed` | `NodeBudgetExhausted` | Clear wait amount; no attempt/reservation; R10; Map parent N45 if child. |
| N28 | `Pending` | `Skipped` | Engine scheduler via `record_choice`, frontier reducer inside that command | Node version/status; all incoming terminal and zero satisfied | `NodeSkipped` | Remove any dormant reachability; E02 on every outgoing edge; recursively reduce. |
| N29 | `Pending` | `BlockedIncompatible` | Engine/recovery, `suspend_incompatible` | Node version/status; no registry implementation advertises its exact pinned semantic digest | `NodeBlockedIncompatible` | Save `blocked_from_status=Pending`; run R03/R04. |
| N30 | `Ready` | `BlockedIncompatible` | Engine/recovery, `suspend_incompatible` | Same availability failure, source Ready | `NodeBlockedIncompatible` | Save Ready; remove from schedulable frontier without changing edge facts. |
| N31 | `RetryWaiting` | `BlockedIncompatible` | Engine/recovery, `suspend_incompatible` | Same availability failure, source RetryWaiting | `NodeBlockedIncompatible` | Save RetryWaiting and preserve `next_eligible_at`. |
| N61 | `BudgetWaiting` | `BlockedIncompatible` | Engine/recovery, `suspend_incompatible` | Node version/status BudgetWaiting; its action pin’s exact semantic digest is unavailable | `NodeBlockedIncompatible` | Save BudgetWaiting and preserve wait amount. |
| N32 | `BlockedIncompatible` | `Pending` | Engine/recovery, `resume_compatible` | Node version/status; saved Pending; exact pinned semantic digest is available | `NodeResumedCompatible` | Clear saved source; R05. |
| N33 | `BlockedIncompatible` | `Ready` | Engine/recovery, `resume_compatible` | Same availability proof, saved Ready | `NodeResumedCompatible` | Clear saved source; restore frontier; R05. |
| N34 | `BlockedIncompatible` | `RetryWaiting` | Engine/recovery, `resume_compatible` | Same availability proof, saved RetryWaiting | `NodeResumedCompatible` | Clear saved source; preserve eligibility timestamp; R05 then N04 if due. |
| N62 | `BlockedIncompatible` | `BudgetWaiting` | Engine/recovery, `resume_compatible` | Node version/status; saved source BudgetWaiting; exact pinned semantic digest is available | `NodeResumedCompatible` | Clear saved source; preserve wait amount; R05. |
| N35 | `Pending` | `Cancelled` | Host API, action completion, engine scheduler, recovery, or clock; cancellation/fail-fast reducer inside the initiating section 5 terminal command | Node version/status Pending; run enters terminal fail/cancel | `NodeCancelled` | Outgoing dormant edges remain irrelevant; no budget. |
| N36 | `Ready` | `Cancelled` | Host API, action completion, engine scheduler, recovery, or clock; cancellation/fail-fast reducer inside the initiating section 5 terminal command | Node version/status Ready; run terminalization CAS succeeds | `NodeCancelled` | Remove from frontier. |
| N37 | `Running` | `Cancelled` | Host API, action completion, engine scheduler, recovery, or clock; cancellation/fail-fast reducer inside the initiating section 5 terminal command | Node active Started attempt and run terminalization wins CAS | `NodeCancelled` | A08, clear active, settle full unless trusted cost supplied; signal token after commit. |
| N38 | `RetryWaiting` | `Cancelled` | Host API, action completion, engine scheduler, recovery, or clock; cancellation/fail-fast reducer inside the initiating section 5 terminal command | Node version/status RetryWaiting; run terminalization CAS succeeds | `NodeCancelled` | Clear next eligibility. |
| N63 | `BudgetWaiting` | `Cancelled` | Host API, action completion, engine scheduler, recovery, or clock; cancellation/fail-fast reducer inside the initiating section 5 terminal command | Node version/status BudgetWaiting; run terminalization CAS succeeds | `NodeCancelled` | Clear budget wait amount. |
| N39 | `WaitingApproval` | `Cancelled` | Host API, action completion, engine scheduler, recovery, or clock; cancellation/fail-fast reducer inside the initiating section 5 terminal command | Node version/status WaitingApproval; gate Pending; run terminalization CAS succeeds | `NodeCancelled` | G06 in same transaction. |
| N40 | `WaitingChildren` | `Cancelled` | Host API, action completion, engine scheduler, recovery, or clock; cancellation/fail-fast reducer inside the initiating section 5 terminal command | Node version/status WaitingChildren; run terminalization CAS succeeds | `NodeCancelled` | Cancel every nonterminal child by N35–N38/N63. |
| N41 | `BlockedIncompatible` | `Cancelled` | Host API, recovery, or clock via `cancel_run`, `expire_run_lifetime`, internal `event_capacity_guard`, or `mark_corrupt_storage`; cancellation reducer inside that command | Node version/status BlockedIncompatible; run terminalization CAS succeeds | `NodeCancelled` | Clear saved source. |
| N42 | `WaitingChildren` | `Failed` | Credential-authenticated action completion via `complete_attempt`, Map fail-fast reducer inside that command | Parent CAS; a child enters Failed | `MapFailedFast` | Cancel sibling children; R07 with `MapChildFailed`. |
| N43 | `WaitingChildren` | `ContractFailed` | Action completion or engine scheduler via `complete_attempt`, `claim_node_attempt`, or `fail_contract`; Map fail-fast reducer inside that command | Parent CAS; a child enters ContractFailed | `MapContractFailed` | Cancel siblings; R08 with child contract kind. |
| N44 | `WaitingChildren` | `RetriesExhausted` | Completion, clock, or recovery via `complete_attempt`, `timeout_attempt`, or `recover_abandoned_attempts_for_run`, Map fail-fast reducer inside that command | Parent CAS; a child enters RetriesExhausted | `MapRetriesExhausted` | Cancel siblings; R09. |
| N45 | `WaitingChildren` | `BudgetExhausted` | Engine scheduler via `claim_node_attempt`, Map fail-fast reducer inside that command | Parent CAS; a child enters BudgetExhausted | `MapBudgetExhausted` | Cancel siblings; R10. |
| N46 | `Ready` | `ContractFailed` | Engine scheduler through `fail_contract`, `record_choice`, `expand_map`, `request_approval`, `resolve_terminal_node`, or `claim_node_attempt` | Node CAS; binding/schema/value limit, Choice input, Map/run dynamic-node bound, approval payload, final output, or total-attempt limit fails | `NodeContractFailed` | R08; no attempt/reservation. |
| N64 | `BudgetWaiting` | `ContractFailed` | Engine scheduler, `claim_node_attempt` | Node CAS; run total-attempt limit became exhausted while waiting | `NodeContractFailed` | Clear wait amount; R08 with `RunAttemptLimitExceeded`; no attempt/reservation. |
| N65 | `WaitingChildren` | `ContractFailed` | Engine scheduler, `complete_map` | Parent CAS; aggregate would exceed run aggregate-byte limit or inline output/schema contract | `MapContractFailed` | Do not register aggregate ref; R08 with `AggregateObjectLimitExceeded` or output contract kind; cancel children. |
| N66 | `Running` | `Cancelled` | Recovery, `recover_abandoned_attempts_for_run` deterministic exhaustion cascade | Node version/status; its active attempt was already made UnknownOutcome/full-settled in this transaction; another candidate won primary exhaustion | `NodeCancelled` | Clear active attempt; no second settlement or attempt transition; R09 cancellation cascade. |
| N67 | `WaitingApproval` | `ContractFailed` | Authenticated host decision or clock expiry via `decide_approval` or `expire_approval` | Node/gate CAS; proposed approval payload/output exceeds inline or aggregate object-byte limit, or supplied approval output is not the exact canonical ApprovalResult envelope | `NodeContractFailed` | Do not resolve as approve/reject; cancel Pending gate by G06; R08 with `InlineJsonLimitExceeded`, `AggregateObjectLimitExceeded`, or `ApprovalPayloadInvalid`. |
| N47 | `Ready` | `CorruptStorage` | Engine/recovery, `mark_corrupt_storage` | Node CAS; object required to bind/evaluate is committed but unreadable/invalid | `NodeCorruptStorage` | Corresponding R14, R15, or R16; no invocation. |
| N48 | `RetryWaiting` | `CorruptStorage` | Recovery/read path, `mark_corrupt_storage` | Node CAS and bad committed diagnostics/input ref | `NodeCorruptStorage` | Corresponding run integrity transition. |
| N49 | `WaitingApproval` | `CorruptStorage` | Recovery/read path, `mark_corrupt_storage` | Node and gate refs identify bad committed object | `NodeCorruptStorage` | Gate is cancelled if Pending; corresponding run integrity transition. |
| N50 | `WaitingChildren` | `CorruptStorage` | Recovery/read path, `mark_corrupt_storage` | Node’s Map input/child output/aggregate ref is bad | `NodeCorruptStorage` | Cancel children; corresponding run integrity transition. |
| N51 | `BlockedIncompatible` | `CorruptStorage` | Recovery/read path, `mark_corrupt_storage` | Bad committed node-owned ref | `NodeCorruptStorage` | Corresponding R16. |
| N52 | `Succeeded` | `CorruptStorage` | Host/recovery read path, `mark_corrupt_storage` | Result/artifact ref is bad | `NodeCorruptStorage` | Corresponding R15 or R17–R22 as applicable; attempt remains immutable. |
| N53 | `Failed` | `CorruptStorage` | Host/recovery read path, `mark_corrupt_storage` | Diagnostics ref is bad | `NodeCorruptStorage` | Corresponding run integrity override. |
| N54 | `ContractFailed` | `CorruptStorage` | Host/recovery read path, `mark_corrupt_storage` | Diagnostics ref is bad | `NodeCorruptStorage` | Corresponding run integrity override. |
| N55 | `RetriesExhausted` | `CorruptStorage` | Host/recovery read path, `mark_corrupt_storage` | Diagnostics ref is bad | `NodeCorruptStorage` | Corresponding run integrity override. |
| N56 | `BudgetExhausted` | `CorruptStorage` | Host/recovery read path, `mark_corrupt_storage` | Referenced diagnostics ref is bad | `NodeCorruptStorage` | Corresponding run integrity override. |
| N57 | `Cancelled` | `CorruptStorage` | Host/recovery read path, `mark_corrupt_storage` | Referenced diagnostics ref is bad | `NodeCorruptStorage` | Corresponding run integrity override. |

`Skipped` owns no payload reference and cannot become `CorruptStorage`. A `Running` action receives already-verified bindings; object access performed independently by host action code is an action result, not a control-plane read. `CorruptStorage` is absorbing.

### 3.4 Attempt transitions and late-completion observations

| ID | Source | Target | Actor and command | CAS precondition | Resulting event | Side effects |
|---|---|---|---|---|---|---|
| A01 | `∅` | `Started` | Engine scheduler, `claim_node_attempt` | Attempt absent; N05/N58, ActionInvocation, run-limit checks, and budget reservation CAS succeed | `AttemptStarted` | Mint completion credential; persist only its digest; insert invocation/reservation ledger and `BudgetReserved`; invocation is permitted only after commit. |
| A02 | `Started` | `Succeeded` | Authenticated action completion, `complete_attempt` | Completion credential digest matches; attempt Started; node active ID matches; run Running; DB-now `< deadline_at` | `AttemptSucceeded` | Store verified result refs; settle actual; N18. No EnginePermit is checked. |
| A03 | `Started` | `RetryableFailed` | Authenticated action completion, `complete_attempt` | Same completion credential, fencing, run-state, and deadline preconditions | `AttemptRetryableFailed` | Store safe diagnostics; settle actual; N19 or N24. |
| A04 | `Started` | `PermanentFailed` | Authenticated action completion, `complete_attempt` | Same completion credential, fencing, run-state, and deadline preconditions | `AttemptPermanentFailed` | Store diagnostics; settle actual; N20. |
| A05 | `Started` | `ContractFailed` | Authenticated action completion, `complete_attempt` | Same completion credential/fencing preconditions; pinned output, value/artifact limit, or cost contract failed | `AttemptContractFailed` | Settle valid actual or full reservation; N21. |
| A06 | `Started` | `TimedOut` | Clock via `timeout_attempt`, or credential-authenticated due completion via `complete_attempt` | Attempt/node fencing; run Running; DB-now `>= deadline_at` | `AttemptTimedOut` | Settle full reservation; N22/N25. Due completion also causes A14 in the same batch. |
| A07 | `Started` | `UnknownOutcome` | Recovery, `recover_abandoned_attempts_for_run` | Attempt is in the complete deterministic set owned by generations older than the live claim and node still names it active | `AttemptOutcomeUnknown` | Terminalize every selected attempt and full-settle every reservation before deriving N23/N26/Map/run outcomes. |
| A08 | `Started` | `Cancelled` | Host API, action completion, engine scheduler, recovery, or clock; cancellation/fail-fast reducer inside the initiating section 5 terminal command | Attempt/node fencing and run terminalization CAS | `AttemptCancelled` | Full settlement unless trusted cost was atomically supplied; clear active. |
| A09 | `Started` | `Stale` | Credential-authenticated action completion, `complete_attempt` | Credential matches; attempt Started but node active ID differs; run is not BlockedIncompatible | `AttemptMarkedStale` | Never change node output/status; settle any still-open reservation full; append stale event. |
| A10 | `Succeeded` | `Succeeded` | Credential-authenticated action completion, `complete_attempt` late-result intake | Credential matches; immutable attempt Succeeded; no prior stale-completion observation for this attempt | `StaleCompletionObserved` | Event only; do not rewrite attempt, node, output, frontier, or budget. Allowed while the run is BlockedIncompatible. |
| A11 | `RetryableFailed` | `RetryableFailed` | Credential-authenticated action completion, `complete_attempt` late-result intake | Same, attempt RetryableFailed | `StaleCompletionObserved` | Same as A10. |
| A12 | `PermanentFailed` | `PermanentFailed` | Credential-authenticated action completion, `complete_attempt` late-result intake | Same, attempt PermanentFailed | `StaleCompletionObserved` | Same as A10. |
| A13 | `ContractFailed` | `ContractFailed` | Credential-authenticated action completion, `complete_attempt` late-result intake | Same, attempt ContractFailed | `StaleCompletionObserved` | Same as A10. |
| A14 | `TimedOut` | `TimedOut` | Credential-authenticated action completion, `complete_attempt` late-result intake | Same, attempt TimedOut | `StaleCompletionObserved` | Same as A10; specifically cannot turn timeout into success. |
| A15 | `UnknownOutcome` | `UnknownOutcome` | Credential-authenticated action completion, `complete_attempt` late-result intake | Same, attempt UnknownOutcome | `StaleCompletionObserved` | Same as A10. |
| A16 | `Cancelled` | `Cancelled` | Credential-authenticated action completion, `complete_attempt` late-result intake | Same, attempt Cancelled | `StaleCompletionObserved` | Same as A10. |
| A17 | `Stale` | `Stale` | Credential-authenticated action completion, `complete_attempt` late-result intake | Same, attempt Stale | `StaleCompletionObserved` | Same as A10. |

For A10–A17, the completion payload is not persisted and its claimed cost is ignored because the reservation was already settled. The event records only payload digest, arrival database time, submitted outcome category, and correlation IDs. A unique `(scope, run_id, attempt_id, StaleCompletionObserved)` constraint makes later submissions an idempotent no-event result; this bounds late-observation events to one per attempt.

When a due completion emits A06 and A14 in the same transaction, the terminalizing A06 event precedes the A14 observation event, following the section 15.1 same-subject tie-break rule.

For recovery, the complete abandoned set is frozen inside the transaction. After all A07/settlements: if no attempt reached `max_attempts`, every corresponding node takes N23. If one or more reached it, the first candidate by persisted `(topological_rank, map_item_index_or_minus_one, node_instance_id, attempt_number, attempt_id)` takes N26 and any parent N44, then R09; every other recovered Running node takes N66 during the deterministic cancellation cascade. Static nodes use `map_item_index_or_minus_one = -1`; Map children use their persisted non-negative item index. Thus all attempt accounting is complete before one primary exhaustion is chosen, and row iteration order or rank recomputation is irrelevant.

### 3.5 ApprovalGate transitions

| ID | Source | Target | Actor and command | CAS precondition | Resulting event | Side effects |
|---|---|---|---|---|---|---|
| G01 | `∅` | `Pending` | Engine scheduler, `request_approval` | Gate absent; N11 CAS succeeds | `ApprovalGateCreated` | Persist request ref, DB-clock expiry, policy. |
| G02 | `Pending` | `Approved` | Host API, `decide_approval` | AuthenticatedPrincipal scope matches gate scope before the principal satisfies gate policy; gate/run/node versions match caller observation; decision is Approve; approval output exactly matches the canonical human ApprovalResult | `ApprovalGateApproved` | Persist authenticated principal ID/payload/fingerprint including approval-output digest; N12. |
| G03 | `Pending` | `Rejected` | Host API, `decide_approval` | AuthenticatedPrincipal scope matches gate scope before the principal satisfies gate policy; versions match; decision is Reject | `ApprovalGateRejected` | Persist authenticated principal ID and decision; N13. |
| G04 | `Pending` | `ExpiredApproved` | Clock, `expire_approval` | Gate Pending; DB-now `>= expires_at`; policy Approve; approval output exactly matches the canonical expiry ApprovalResult | `ApprovalGateExpiredApproved` | N14. |
| G05 | `Pending` | `ExpiredRejected` | Clock, `expire_approval` | Gate Pending; DB-now `>= expires_at`; policy Reject | `ApprovalGateExpiredRejected` | N15. |
| G06 | `Pending` | `Cancelled` | Host API, action completion, engine scheduler, recovery, or clock; cancellation/fail-fast reducer inside the initiating section 5 terminal command | Gate Pending and run terminalization wins its CAS | `ApprovalGateCancelled` | N39 and R07–R13/R15/R16 as initiating outcome requires. |

`decide_approval` first validates that the AuthenticatedPrincipal capability was minted for the gate's `ExecutionScope`; a capability minted for scope B is structurally invalid in scope A. Only after that scope check does it evaluate the immutable authorization policy, and both checks precede any decision CAS. Unauthorized input returns `ApprovalUnauthorized` and cannot win or perturb the race. Exactly identical replay of G02/G03, determined by `decision_fingerprint`, returns the existing decision and emits no event. A different decision, payload, approval output, authenticated principal, or authentication-context digest returns `ApprovalAlreadyResolved`; expiry/cancellation that loses the Gate Pending CAS returns `ApprovalRaceLost`. A cancellation request uses its observed run/gate versions, so only one concurrent resolution commits. A caller may issue a new cancellation against a later observed run version, but that is a new operation rather than the losing race being silently accepted.

`gate_id` is deterministic: `approval_` plus lowercase SHA-256 hex of length-prefixed domain `"dagger-approval-v1"`, run ID, and node-instance ID.

`ApprovalResult` has exactly this engine-owned schema: an object with `additionalProperties: false`; required fields `decision`, `source`, `payload_ref`, and `principal`; `decision` is exactly `"approve"`; `source` is exactly `"human"` or `"expiry"`; `payload_ref` is either null or the exact canonical ArtifactRef value from section 8.1; and `principal` is either a 1–256-byte UTF-8 principal ID string or null. The canonical human envelope must use `source="human"`, the deciding AuthenticatedPrincipal's exact principal ID, and the exact decision-payload ArtifactRef value or null. The canonical expiry envelope must use `source="expiry"`, `payload_ref=null`, and `principal=null`. The engine constructs the expected canonical bytes and `decide_approval`/`expire_approval` verify byte-for-byte equality, media type, size, and digest of the supplied canonical `VerifiedObjectRef` before G02/G04. Missing, extra, noncanonical, or mismatched values apply N67/R08 with `ApprovalPayloadInvalid`; a valid envelope is committed as the Approval NodeRun `result_ref`. Rejection has no successful node output.

For human decisions:

```text
decision_fingerprint = "sha256:" + lowercase_hex(SHA-256(
  LP("dagger-approval-decision-v1") ||
  LP(scope.tenant_id) ||
  LP(scope.namespace) ||
  LP(run_id) ||
  LP(gate_id) ||
  LP(decision_tag) ||
  LP(decision_payload_digest_or_null_tag) ||
  LP(approval_output_digest_or_null_tag) ||
  LP(authenticated_principal_id) ||
  LP(authentication_context_digest)
))
```

`LP(x)` is the section 7.1 length-prefix encoding. Approve includes the verified ApprovalResult digest; Reject uses the literal null tag `"none"`. The fingerprint therefore cannot treat a changed output envelope as an exact replay.

### 3.6 EdgeFact transitions

| ID | Source | Target | Actor and command | CAS precondition | Resulting event | Side effects |
|---|---|---|---|---|---|---|
| E01 | `Dormant` | `Satisfied` | Action completion, engine scheduler, host API, or clock via `complete_attempt`, `record_choice`, `expand_map`, `complete_map`, `decide_approval`, or `expire_approval`; frontier reducer inside that command | Edge version/state; source node just succeeded and edge is selected/normal | `EdgeSatisfied` | Increment target `incoming_satisfied`; may cause N03. |
| E02 | `Dormant` | `Skipped` | Engine scheduler via `record_choice`, skip reducer inside that command | Edge version/state; branch unselected or source N28 | `EdgeSkipped` | Increment target `incoming_skipped`; may cause N03 or N28. |

No edge can change between `Satisfied` and `Skipped`, and no terminal node can be made Ready again.

## 4. Crash matrix

### 4.1 Universal atomicity rule

SQLite transactions are the only control-plane commit boundary. Immediately before a transaction commits, recovery sees all source states, the old run version/event sequence/frontier, and no entries or events from that command. Immediately after it commits, recovery sees all target states, ledger entries, frontier facts, and a contiguous event suffix. SQLite rollback eliminates partially written rows even if the process dies during the commit call.

Object puts are separate and always precede the SQLite transaction. Thus “before commit” below includes a possible verified but unreferenced object. Such an object is an orphan, not evidence that the state transition happened.

Recovery after acquiring a newer engine generation performs this order:

1. verify the live claim and use database time;
2. scan recovery candidate runs in deterministic run-ID order;
3. for one run, select the complete set of lower-generation Started attempts in one transaction, ordered by persisted `(topological_rank, map_item_index_or_minus_one, node_instance_id, attempt_number, attempt_id)`;
4. apply A07 and full settlement to every selected attempt before changing any corresponding NodeRun;
5. derive all N23/N26/Map effects; if several exhaust, the first tuple in that order is the primary R09 correlation, independent of scan/iteration order;
6. validate every pinned action reference, applying R04 and N29–N31/N61 only after no Started attempt remains;
7. verify referenced objects as read; apply proof-backed corruption transitions rather than rerun;
8. scan due retries, budget waiters, gates, lifetimes, and the persisted frontier.

### 4.2 Coverage index

Every section 3 transition appears in at least one crash class below. A transition with several legal initiating commands can appear in several classes; cascaded transitions share the initiating transaction’s boundary.

| Crash class | Transition IDs covered |
|---|---|
| C01 run creation | R01, N01, N02 and the create CommandReceipt |
| C02 start | R02 |
| C03 incompatible suspension | R03, R04, N29, N30, N31, N61 |
| C04 compatible resume | R05, N32, N33, N34, N62 |
| C05 successful attempt claim | N05, N58, A01 and ActionInvocation |
| C06W temporary budget wait | N59 |
| C06E permanent budget exhaustion | R10, N27, N60, N45 and cancellation cascades N35–N40/N63/G06 |
| C07 accepted action success | A02, N18 and any E01/N03 caused by it |
| C08 accepted action error | A03, A04, A05, N19, N20, N21, N24, N42, N43, N44, R07, R08, R09 and their fail-fast cancellation cascades |
| C09 timeout | A06, N22, N25, N44, R09 and cascades |
| C10 crash-unknown recovery | all A07 for one run, then N23/N26/N66, N44, R09 and cascades |
| C11 stale/late completion | A09–A17 |
| C12 retry eligibility | N04 |
| C13 Choice | N09, E01, E02, N03, N28 |
| C14 Map expansion | N02M, N06, N07 and frontier effects |
| C15 Map aggregation | N08, E01, N03 and downstream frontier effects |
| C16 approval request | N11, G01 |
| C17 human approval decision | G02, G03, N12, N13, E01, N03, R07 and cascades |
| C18 approval expiry | G04, G05, N14, N15, E01, N03, R07 and cascades |
| C19 Succeed/Fail terminal node | N16, N17, R06, R07 and cascades |
| C20 contract/run-limit failure | N46, N64, N65, N67, R08 and cascades |
| C21 cancellation/lifetime/event-cap guard | R11, R12, R13, N35–N41, N63, A08, G06 and cancel CommandReceipt when host-authored |
| C22 corrupt object detection | R14–R22, N47–N57 |
| C23 frontier fixed point | E01, E02, N03, N28 when caused by any command not otherwise named; it is never a standalone transaction |

N45 appears in C06E only when the refused node is a Map child. N42–N44 and cancellation transitions may be side effects of C08–C10; their commit boundary is still the initiating transaction. R06 is reached only through the unique Succeed node in C19.

### 4.3 Per-class before/after behavior

| Class | Durable state if process dies immediately before SQLite commit | Recovery before-case | Durable state if process dies immediately after SQLite commit | Recovery after-case |
|---|---|---|---|---|
| C01 | No run, nodes, edges, receipt, or run event. Verified input/schema-related objects may be orphans. | Retry with the same token/fingerprint. No object alone implies a run. | Run is Pending; static graph with canonical persisted topological ranks, creation batch, create fingerprint, charged input-byte counter, and CommandReceipt outcome are all durable. | Exact replay returns the stored outcome/batch without re-execution; conflicting fingerprint fails. |
| C02 | Run remains Pending. | Recheck all action pins and retry start. | Run is Running with `started_at` and `RunStarted`. | Schedule only persisted Ready nodes. Never emit a second RunStarted. |
| C03 | Run and affected nodes retain source states; no incompatibility ref/fingerprint/event. | First complete C10 for every abandoned attempt, then recompute availability of exact semantic digests. | Run is BlockedIncompatible; the incompatibility ref and exact request fingerprint are durable; affected nodes remember Pending/Ready/RetryWaiting/BudgetWaiting source; no Started attempt exists. | `suspend_incompatible` checks an exact fingerprint replay before the blocked fence and returns this committed state with no write/event; a non-replay reaches the fence. Otherwise only compatible resume, cancellation-class operations, corruption handling, and terminal-attempt observation intake are accepted. |
| C04 | Run remains BlockedIncompatible; blocked nodes/ref remain. | Recheck availability of the exact pinned digests; there is no override path. | Run is Running; nodes restored; incompatibility ref cleared. | Reconstruct scans; due retry and budget-wait work is handled from durable fields. |
| C05 | Node stays Ready or BudgetWaiting; no invocation, attempt, count increment, reservation, completion credential, or events exist. A verified bound-input object may be orphaned. | Rebind deterministically and retry claim. | ActionInvocation exact bytes/digest/derivation, Started attempt, completion-credential digest, counters, reservation, active fence, and event batch are durable. Invocation may not have happened. | On restart C10 treats it UnknownOutcome/full-charge. A completion racing takeover uses its credential and either commits first or becomes an observation after recovery. |
| C06W | Node remains Ready; no wait/event exists. | Retry admission from current ledger. | Node is BudgetWaiting with declared amount; run stays Running; no attempt/reservation exists. | Deterministic budget-wait scan retries claim after settlements. If capacity became permanently impossible, C06E applies. |
| C06E | Node remains Ready/BudgetWaiting and run Running; no terminal refusal event/receipt exists. | Re-evaluate `declared_max > limit-consumed`; reservation-only shortage must use C06W instead. | Node/Map parent/run are BudgetExhausted, remaining work cancelled, no reservation created for refused attempt. | Terminal; immutable budget limit prevents later revival. |
| C07 | Attempt remains Started and node Running. Verified output/artifact/diagnostic objects may be orphans; no settlement or frontier change exists. | On restart use C10, full-charge the original reservation, and retry with the same idempotency key if allowed. Never infer success from the orphan. | Attempt and node are Succeeded, actual cost settled, refs and frontier/event changes committed. | Do not rerun. Verify objects when read; if invalid, use C22. If a Map child made the set complete, C15 is still a distinct later command. |
| C08 | Attempt remains Started/node Running, or Map/run remain in their old states. Any verified diagnostics object may be orphaned. A malformed or over-65,536-byte diagnostics ref is rejected before this boundary with `DiagnosticsInvalid` or `DiagnosticsTooLarge` and no transition/event. | C10 after restart; the crash-unknown result, not the uncommitted action error, governs retry accounting. | Attempt terminal error, node retry/failure/exhaustion, budget settlement, Map fail-fast, run outcome, cancellations, and events are all committed together. | Honor persisted `next_eligible_at`, or leave terminal. Do not apply error edges; none exist. |
| C09 | Attempt remains Started and deadline remains due. | Clock command retries immediately using DB time. A completion submitted at or after the deadline takes the same timeout path. | Attempt TimedOut; full reservation settled; node RetryWaiting or RetriesExhausted; cancellation token may not yet have been signalled. A due submitted completion also has A14 observation. | State is authoritative. Signal is best-effort after commit. A later result follows A14 and cannot overwrite timeout. |
| C10 | Every old-generation attempt in the run remains Started with reservations open; none is partially recovered. | Retry the whole run batch. A valid credential-authenticated completion may win its attempt CAS before recovery begins. | Every abandoned attempt is UnknownOutcome and full-settled; only then are all node retry/exhaustion/Map/run outcomes selected by the persisted topological-rank key. | Honor persisted backoffs or terminal exhaustion. Late results follow A15. No recovery iteration order or graph traversal can change the chosen primary failure. |
| C11 | For A09, attempt remains Started. For A10–A17, no observation event exists and terminal attempt is unchanged. | Retry credential-authenticated intake. | A09 is Stale/full-settled; A10–A17 append one observation without rewriting state. | No scheduling change. Later submissions for that attempt are idempotent no-event results. |
| C12 | Node remains RetryWaiting with persisted timestamp. | Clock scan retries once DB-now is due. | Node is Ready, timestamp cleared, frontier/event durable. | Scheduler may claim. It must not emit another eligibility event. |
| C13 | Choice remains Ready with decision fields null. A verified canonical Choice input object may be orphaned. | Rebind to canonical input and retry deterministic first-match-or-required-default evaluation. | Input ref/digest and exactly one case/default selection are durable; one edge is Satisfied, all alternatives Skipped, and the fixed point committed. | Never reevaluate; reconstruct from edge facts and selection. |
| C14 | Map remains Ready with expansion fields null. The empty aggregate object for a zero-item Map may be an orphan. Inserts made inside an interrupted SQLite transaction are rolled back. | Re-read the same pinned input digest and recompute all child IDs. Retry expansion. | For non-empty input, parent WaitingChildren and the complete exact child set are durable. For zero input, parent Succeeded with verified `[]` aggregate. No partial committed child set exists. | Validate `map_expansion_digest`; insert nothing if it matches. Any re-expansion request with a different digest fails. Schedule at most `max_concurrency` children. |
| C15 | Parent remains WaitingChildren even though all children are Succeeded. Verified aggregate object may be orphaned. | Rebuild ordered aggregate from durable child refs, producing the same digest, then retry. | Parent Succeeded, aggregate ref and downstream frontier facts committed. | Do not reaggregate for control flow; verify aggregate on read. |
| C16 | Approval node remains Ready; no gate exists. Verified request object may be orphaned. | Rebind the same request and retry with deterministic gate ID. | Node WaitingApproval and one Pending gate with DB-clock expiry are durable. | Do not recreate or notify as a new logical gate. Host notification delivery may be retried using gate ID. Schedule expiry from durable `expires_at`. |
| C17 | Gate remains Pending and node WaitingApproval; decision/output objects may be orphaned. | Reauthenticate and reauthorize principal, reconstruct and byte-validate the fixed ApprovalResult, then retry observed-version CAS; unauthorized input cannot affect the race. | Authorized human decision, exact ApprovalResult `result_ref`, decision fingerprint including its digest, node/frontier/run effects, and event batch are committed. | Exact fingerprint replay is a no-op success. A changed decision payload, ApprovalResult digest, principal, or authentication context fails closed. |
| C18 | Gate remains Pending. | Clock retries if DB-now remains due; a human/cancel command can still win first. | Gate has ExpiredApproved/ExpiredRejected and corresponding node/run effects. | Human arrival loses with `ApprovalAlreadyResolved`. No gate recreation. |
| C19 | Succeed/Fail node remains Ready; a final output object may be orphaned. | Retry terminal-node resolution. | Node and possible run outcome/cancellations are fully committed. | Never resolve the node twice or rerun completed paths. |
| C20 | Node remains Ready/BudgetWaiting/WaitingChildren and run Running; diagnostics or oversized-output object may be orphaned. | Deterministically re-evaluate the same contract/run-limit guard. | Node/Map and run ContractFailed with cancellations and batch events durable. | No invocation or retry. |
| C21 | Run and children/attempts/gates retain prior states; host cancel has no receipt; no settlement/event exists. | Host retries same fingerprint, or clock/guard re-evaluates its database predicate. | Run/nodes/gates/attempts cancelled; settlements and one batch committed; host cancel receipt stores outcome. | Exact host replay returns receipt outcome. Tokens remain best-effort; late completion uses credentials and stale observation. |
| C22 | No corruption transition exists; bad committed ref remains; supplied failed-read proof is unconsumed. | Object store may mint a fresh proof; no caller assertion is accepted. | Proof-validated node/run CorruptStorage, bad ArtifactRef ID, owner node when any, error class, proof fingerprint, cancellation, settlements, and batch are durable. | Absorbing; same proof/report is idempotent. Never rerun or return fallback bytes. |
| C23 | All edge/node counters and event totals remain at prior fixed point because enclosing transaction did not commit; no batch ID is consumed. | Re-run command; reducer and batch ordering recompute deterministically and mint a fresh ID. | Edge facts, counters, Ready/Skipped states, run-lifetime-unique `batch_id/index/count`, frontier epoch, and event-cap accounting commit together under the `(scope,run_id,batch_id)` uniqueness constraint. | Consumers reconstruct one atomic command from the batch; scheduler reconstructs from state. |

### 4.4 Required adversarial crash cases

**Object committed, SQLite uncommitted.** For every command accepting a new object ref, a death after durable atomic no-replace publication/digest verification but before SQLite commit leaves a scope-local orphan. Recovery does not scan objects to infer control state. The original command either retries and reuses the same content digest or the orphan remains unreachable until deferred GC.

**Mid-Map expansion.** `expand_map` inserts the parent expansion marker and all children in one SQLite transaction. A kill during child insertion rolls the transaction back. Retry recomputes each ID as specified in section 10 and converges to the identical complete set. A matching already-committed expansion is an idempotent success; a different expansion digest is `IdempotencyConflict`.

**Restart at a pending approval.** The Pending gate, request ref, expiry, and node WaitingApproval state survive. Recovery neither approves nor recreates it. The database clock determines whether C18 is due. Host decision, expiry, and observed-version cancellation race on the same Gate/Run CAS.

**Crash-unknown attempt.** Any A01 that lacks a committed terminal transition when its engine generation dies becomes A07. It consumes its already-incremented attempt number and full reservation even if user code probably never ran. A subsequent attempt uses the same scope-bound logical-node idempotency key and a new attempt ID.

## 5. Store command API

### 5.1 Boundary and transaction contract

The control-plane trait exposes domain commands and scoped read models. It does not expose `insert_attempt`, `set_status`, `reserve_budget`, `settle_budget`, `advance_frontier`, `put_event`, or raw CRUD. The in-memory and SQLite adapters implement identical semantics.

Trait methods are asynchronous. Signatures carry worker IDs and opaque permits without exposing SQLite or assuming a process-local caller, but v0.1 still permits only one live scheduler generation and defines no distributed worker lease.

Every command:

- takes `ExecutionScope` explicitly as its first logical parameter;
- resolves all nested IDs under that same scope;
- uses one SQLite transaction for all control-plane writes;
- obtains time from the database inside that transaction;
- checks closed state/CAS preconditions before writing;
- allocates all per-run event sequences inside the transaction;
- returns only after commit, or returns an error with no partial control-plane effects;
- accepts only `VerifiedObjectRef` values minted by the scoped object store after durable put and digest verification;
- checks a live `EnginePermit` for engine-authored commands.

`VerifiedObjectRef` is an opaque capability containing scope, digest, size, media type, and object key. It cannot be constructed from a digest string through the public API.

Every command registering charged run data atomically checks/increments `WorkflowRun.aggregate_object_bytes`; no execution-data command bypasses it. A verified object rejected by the limit remains an orphan, never a committed run ref.

Commands accepting `CompatibilityEvidence` or `Diagnostics` refs enforce the fixed 65,536-byte canonical JSON cap before any state/CAS/event work. Diagnostics must also validate the closed `DiagnosticsEnvelope`. The closed no-write results are `EvidenceInvalid`, `DiagnosticsInvalid{path,code}`, and `DiagnosticsTooLarge{limit_bytes:65536,observed_bytes}`; none is converted into a workflow transition.

### 5.2 Definition and engine commands

These commands do not perform run state transitions.

| Command | Parameters and qualification | CAS/preconditions | Single-transaction commit | Errors |
|---|---|---|---|---|
| `create_definition` | `scope, definition_id, display_name, description, principal` | Scoped ID absent | WorkflowDefinition at version 1 | `AlreadyExists`, `InvalidField`, store errors |
| `update_definition_metadata` | `scope, definition_id, expected_version, display_name, description` | Scoped definition/version | Metadata and version | `NotFound`, `CasConflict`, `InvalidField` |
| `publish_revision` | `scope, definition_id, expected_definition_version, canonical_definition: VerifiedObjectRef, run_input_schema: VerifiedObjectRef, run_output_schema: VerifiedObjectRef, resolved_action_schema_objects: map<reference_location,(VerifiedObjectRef,VerifiedObjectRef)>, parsed_revision, principal` | Definition/version; canonical document `definition_id` exactly equals publication target; all digests recompute; every root/action schema ref matches its digest and supported subset; definition/semantic rules and canonical Kahn ranking pass | Immutable revision/root schema refs/action pins with schema refs/ArtifactRefs, canonical node ranks, latest hash, definition version | `RevisionDefinitionIdMismatch`, `RevisionInvalid{path,code,message,valid_alternatives}`, `SchemaSubsetUnsupported`, `DigestMismatch`, `ArtifactMetadataConflict`, `CasConflict`, `NotFound` |
| `acquire_engine_claim` | `scope, instance_id` | Insert if absent; takeover only when `expires_at <= DB-now`; any live row fails even when instance ID text matches; DB-now must not precede persisted claim time | Claim with new store-minted session-token digest; expired takeover increments generation; raw token returned once | `EngineAlreadyLive{owner,expires_at}`, `EntropyUnavailable`, `ClockUnavailable`, `ClockNonMonotonic`, store errors |
| `heartbeat_engine_claim` | `scope, permit` | Exact owner/generation/session-token digest and `expires_at > DB-now`; DB-now `>= heartbeat_at` | `heartbeat_at`, new `expires_at`, claim version | `EngineClaimLost`, `EngineClaimExpired`, `ClockNonMonotonic` |
| `release_engine_claim` | `scope, permit` | First release: exact live instance/generation/session-token digest. Replay: the same instance/generation/session-token digest on the same already-expired claim, without the live-permit check | First release sets `expires_at = DB-now` and increments claim version; matching replay is an idempotent success with no write | `EngineClaimLost` for a different generation or session token; never mutates a claim owned by another session token |

### 5.3 Runtime commands

Parameters named `expected_*_version` are caller-visible CAS inputs. Other row versions are read and CASed internally within the transaction.

| Command | Parameters, including scope | CAS and validation | Everything committed in its one transaction | Transition rows | Command-specific errors/result |
|---|---|---|---|---|---|
| `create_run` | `scope, run_id, definition_id, revision_hash, input: VerifiedObjectRef, budget_limit, limits: RunLimits/defaults, principal, idempotency_token` | Scoped revision/root input schema refs exist and verify; input matches pinned schema/subset and inline/aggregate limits; limits valid; run absent | Input ArtifactRef, run/limits/fingerprint with `aggregate_object_bytes=input.size_bytes`, static graph/frontier with persisted canonical topological ranks, one run-lifetime-unique event batch, CommandReceipt outcome | R01, N01, N02 | Exact token/fingerprint replay returns stored outcome/batch; differing request `IdempotencyConflict`; `ContractValidation`, `RunLimitsInvalid`, `NotFound` |
| `start_run` | `scope, permit, run_id, compatibility_evidence` | Run Pending; evidence freshly covers all pins | Running state/start time/event | R02 | `IncompatiblePins` instructs caller to invoke suspension; `IllegalTransition`, `EngineClaimLost` |
| `suspend_incompatible` | `scope, permit, run_id, incompatibilities: VerifiedObjectRef, evidence` | After scope/live-permit validation, compute the canonical fingerprint over the complete request and compare it to `blocked_incompatibility_fingerprint` before source-state/fence validation. Exact replay returns immediately. Only a non-replay requires Run Pending/Running, evidence proving an exact semantic digest unavailable, and batch recovery leaving no Started attempts | Non-replay only: run suspension and fingerprint, affected saved states including BudgetWaiting, incompatibility ArtifactRef, events. Exact replay writes nothing | R03/R04, N29–N31/N61 for non-replay; no row for replay | Exact replay returns the committed BlockedIncompatible state and no event; a different request against BlockedIncompatible reaches the fence and returns `RunBlockedIncompatible`; `ActiveAttemptExists`, `EvidenceInvalid` |
| `resume_compatible` | `scope, permit, run_id, availability_evidence` | Run Blocked; registry freshly proves every exact pinned semantic digest available; evidence cannot replace digest | Run Running, restored nodes including BudgetWaiting, cleared ref, event batch | R05, N32–N34/N62 | `StillIncompatible{pins}`, `CompatibilityOverrideForbidden`, `IllegalTransition` |
| `claim_node_attempt` | `scope, permit, run_id, node_id, expected_node_version, attempt_id, worker_id, bound_input: VerifiedObjectRef, binding_derivation_digest` | Run Running; node Ready/BudgetWaiting Action; exact action pin available; bound input validates and fits; run/node attempt limits; Map slot; budget predicates | Success: ActionInvocation, completion credential digest, Node Running/count/active, Started attempt, run attempt count, reserve ledger/summary, event batch. Reservation-only shortage: BudgetWaiting only. Permanent budget/limit failure: terminal batch, no attempt | N05/N58/A01; N59; N27/N60/R10/N45; N46/N64/R08 and cascades | `Claimed{ActionInvocation, completion_credential}`, `BudgetWaitingApplied`, `MapConcurrencyLimited`, `BudgetExhaustedApplied`, or `RunLimitApplied`; `CasConflict`, `AttemptIdConflict`, `EngineClaimLost` |
| `complete_attempt` | `scope, completion_credential, run_id, node_id, attempt_id, submitted_outcome, verified output/artifact/diagnostic refs` | Credential digest matches; a supplied diagnostics ref validates `DiagnosticsEnvelope` and is at most 65,536 canonical bytes; see A02–A17; accepted state change requires run Running, active fence, pre-deadline; limits/output/cost validate. No EnginePermit | Accepted: immutable attempt/node outcome, refs, budget, Map/frontier/run effects, run-lifetime-unique event batch. Due: timeout/full settlement plus A14. Terminal attempt: at most one A10–A17 observation, including while Blocked. Blocked plus Started is rejected | A02–A06, A09–A17; N18–N25; N42–N44; R07–R09; cascades | `Applied`, `RetryScheduled`, `TerminalRun`, `TimedOutAndStaleRecorded`, `StaleRecorded`, or idempotent `AlreadyObserved`; `DiagnosticsInvalid`/`DiagnosticsTooLarge` are pre-transition no-write rejections; `InvalidCompletionCredential`, `RunBlockedIncompatible` for a non-observation |
| `timeout_attempt` | `scope, permit, run_id, node_id, attempt_id` | Run Running; active Started attempt; DB deadline due | TimedOut attempt, full settlement, node retry/exhaustion, Map/run effects, event batch | A06, N22/N25, N44/R09, cascades | `DeadlineNotDue{database_now,deadline}`, `AttemptFenced`, `IllegalTransition` |
| `recover_abandoned_attempts_for_run` | `scope, permit, run_id` | Run Running; select the complete set of Started attempts from lower generations inside one transaction and order by persisted `(topological_rank,map_item_index_or_minus_one,node_instance_id,attempt_number,attempt_id)` | All A07/full settlements first; then either all N23, or primary N26/N44/R09 plus N66/cancellation for others; one ordered run-lifetime-unique batch | A07, N23/N26/N66, N44/R09, cascades | Empty set idempotent; `CurrentGenerationAttemptPresent`, `EngineClaimLost` |
| `release_retry` | `scope, permit, run_id, node_id, expected_node_version` | Run Running; node RetryWaiting; DB time due | Node Ready, cleared timestamp, frontier/event | N04 | `RetryNotDue{database_now,next_eligible_at}`, `CasConflict` |
| `record_choice` | `scope, permit, run_id, node_id, expected_node_version, input: VerifiedObjectRef, evaluated_selector_digest, selection` | Run Running; Ready Choice; decision null; input valid; supplied selection is deterministic first match or required default | ArtifactRef, pinned decision/input, selected/skipped edges, fixed point, event batch | N09, E01/E02, N03/N28 | Matching replay returns decision; differing input/selection `IdempotencyConflict`; invalid proof applies N46/R08 |
| `expand_map` | `scope, permit, run_id, map_node_id, expected_node_version, input: VerifiedObjectRef, ordered_items, expansion_digest` | Run Running; Ready Map; array/index/digests valid; per-Map and run dynamic-node limits pass; expansion null/identical | Input ref, expansion marker, exact full child set/count and batch; zero aggregate path | N06/N07/N02M, E01/N03; limit failure N46/R08 | Matching expansion idempotent; changed digest `IdempotencyConflict`; `RunLimitApplied`, `ContractValidationApplied` |
| `complete_map` | `scope, permit, run_id, map_node_id, expected_node_version, aggregate: VerifiedObjectRef` | Run Running; parent WaitingChildren; exact children Succeeded; ordering/schema/inline and cumulative aggregate-byte limits pass | Aggregate ref/byte counter, parent success, frontier/batch; limit failure commits contract outcome without registering ref | N08/E01/N03, or N65/R08 | Matching replay succeeds; `ChildrenIncomplete`, `AggregateMismatch`, `RunLimitApplied`, `CasConflict` |
| `request_approval` | `scope, permit, run_id, node_id, expected_node_version, gate_id, request: VerifiedObjectRef` | Run Running; Ready Approval; request valid/fits; gate absent; revision policy has at least one authorized principal/role | Request ref, gate Pending with policy/DB expiry, node WaitingApproval, batch | N11, G01 | Exact replay returns gate; mismatch `IdempotencyConflict`; validation failure N46/R08 |
| `decide_approval` | `scope, run_id, gate_id, expected_run_version, expected_gate_version, decision, nullable decision_payload, approval_output when approving, principal: AuthenticatedPrincipal` | Run Running; principal capability scope matches gate scope before authorization-policy evaluation; authenticated principal satisfies policy; gate/node Pending/Waiting; versions; proposed refs fit run limits; approve output exactly equals the engine-constructed canonical human ApprovalResult; decision fingerprint includes its digest | Decision/output refs/counter, authenticated principal, gate/node/run/frontier and run-lifetime-unique batch; invalid envelope or limit breach contract-fails without registering refs | G02/G03, N12/N13, E01/N03, R07; or N67/G06/R08 | Structurally invalid principal scope or `ApprovalUnauthorized` before CAS; exact fingerprint no-op; `ApprovalAlreadyResolved`; `ApprovalRaceLost`; `ContractValidationApplied{ApprovalPayloadInvalid}`; `RunLimitApplied` |
| `expire_approval` | `scope, permit, run_id, gate_id, approval_output when policy Approve` | Run Running; gate Pending; DB time due; approve output exactly equals the engine-constructed canonical expiry ApprovalResult and is within limits | Output ref/counter when approved, expired gate/node/frontier/run-lifetime-unique batch; invalid envelope or limit breach contract-fails | G04/G05, N14/N15, E01/N03, R07; or N67/G06/R08 | `ExpiryNotDue`, `ApprovalRaceLost`, `ContractValidationApplied{ApprovalPayloadInvalid}`, `RunLimitApplied` |
| `resolve_terminal_node` | `scope, permit, run_id, node_id, expected_node_version, output: VerifiedObjectRef for Succeed` | Run Running; Ready Succeed/Fail; unique Succeed output validates pinned root output schema/limit; output absent for Fail | ArtifactRef, node terminal, run success if quiescent or fail-fast outcome, batch | N16/N17, R06/R07, cascades; invalid output N46/R08 | Matching replay succeeds; `ContractValidationApplied`, `IllegalNodeKind`, `CasConflict` |
| `fail_contract` | `scope, permit, run_id, node_id, expected_node_version, closed_failure_kind, diagnostics: VerifiedObjectRef` | Diagnostics validates `DiagnosticsEnvelope` and is at most 65,536 canonical bytes; run Running; node Ready and failure kind legal | ArtifactRef, node/run ContractFailed, cancellations/settlements/gates/batch | N46, R08, cascades | `ContractValidationApplied`; `DiagnosticsInvalid`/`DiagnosticsTooLarge` are pre-transition no-write rejections; invalid kind `InvalidField` |
| `cancel_run` | `scope, run_id, expected_run_version, expected_pending_gate_versions, principal: AuthenticatedPrincipal, reason_code, idempotency_token` | Run Pending/Running/Blocked; fingerprint and observed versions/gate set match | Cancellation/cascades/settlements, one batch, and CommandReceipt with durable outcome | R11–R13, N35–N41/N63, A08, G06 | Exact token/fingerprint returns stored outcome even after terminalization; conflict `IdempotencyConflict`; stale race `CancellationRaceLost` |
| `expire_run_lifetime` | `scope, permit, run_id` | DB-now `>= lifetime_deadline_at`; run Pending/Running/Blocked | Cancellation-class terminal batch with system reason `RunLifetimeExceeded` | R11–R13, N35–N41/N63, A08, G06 | `LifetimeNotDue`; terminal replay returns current state |
| `event_capacity_guard` | Internal branch of every scoped event-producing command; not independently host-callable | Proposed batch would violate the section 5.3 reserve inequality; reserved cancellation batch fits | Original command does not apply; cancellation-class batch with system reason `RunEventLimitExceeded` | R11–R13, N35–N41/N63, A08, G06 | Returns `RunLimitApplied{RunEventLimitExceeded}` as the initiating command outcome |
| `mark_corrupt_storage` | `scope, run_id, bad_ref, proof: FailedReadProof, nullable owner_node_id` | Opaque proof validates store nonce/scope/requested digest/error class; ref already committed | Node/run corruption, cancellations/full settlements, batch; proof fingerprint retained for idempotency | R14–R22, N47–N57 | `InvalidFailedReadProof`, `NotFound`; matching proof replay returns state |

There is no separate `advance_frontier` or Map child insertion command. There is no budget mutation command. `complete_attempt` does not accept a requested target status; it derives the closed transitions from `ActionOutcome`, retry policy, attempt number, and pinned contract.

Create/cancel request fingerprints are SHA-256 over canonical JSON with an operation domain and scope digest. Create includes `run_id, definition_id, revision_hash, input digest, budget_limit, every resolved RunLimits field, principal ID`; cancel includes `run_id, expected_run_version, complete sorted gate-version set, authenticated principal ID, reason_code`. The idempotency token itself is not part of the fingerprint. Receipt lookup occurs before current-state validation, so a committed cancel replay still returns its original outcome after the run is terminal; conflicting fingerprints never inherit that result.

The suspension request fingerprint is `"sha256:" + lowercase_hex(SHA-256(LP("dagger-suspend-request-v1") || LP(scope.tenant_id) || LP(scope.namespace) || LP(run_id) || LP(incompatibilities.artifact_ref_id) || LP(incompatibilities.digest) || LP(incompatibilities.size_bytes_as_u64_be) || LP(evidence_digest)))`. `LP` is the section 7.1 encoding. The raw EnginePermit is authorization, not request identity, and is excluded. Comparing every listed field makes replay exact rather than merely “same evidence.”

**Blocked command fence.** While a run is `BlockedIncompatible`, the only state-changing commands accepted are `resume_compatible`, `cancel_run`/cancellation-class lifetime or event-cap enforcement, and `mark_corrupt_storage`. A credential-authenticated `complete_attempt` may append A10–A17 only when the attempt is already terminal; this is audit intake, not an execution state change. `suspend_incompatible` is the one replay-order exception: after scope/live-permit validation it computes the complete request fingerprint and performs exact committed-replay detection before this fence. A match returns the committed BlockedIncompatible state with no write; only a non-match proceeds to fence evaluation and returns `RunBlockedIncompatible`. `start_run`, every non-replay `suspend_incompatible`, `claim_node_attempt`, `timeout_attempt`, `recover_abandoned_attempts_for_run`, `release_retry`, `record_choice`, `expand_map`, `complete_map`, `request_approval`, `decide_approval`, `expire_approval`, `resolve_terminal_node`, and `fail_contract` return that distinct error with no write. Fence validation precedes event-cap preflight, so a forbidden command cannot turn itself into a capacity cancellation. A Started attempt in a blocked run is an invariant violation and completion cannot repair it; R04 requires batch recovery first.

**Event-cap preflight.** Before an event-producing transaction, the store computes its exact batch count and preserves:

```text
terminal_reserve =
    1                                      // RunCancelled
  + count(nonterminal NodeRuns)            // NodeCancelled
  + 2 * count(Started attempts)            // AttemptCancelled + BudgetSettled
  + count(Pending gates)                    // ApprovalGateCancelled

late_completion_reserve =
    count(attempts without StaleCompletionObserved)

integrity_reserve = 2                       // one NodeCorruptStorage + RunCorruptStorage
```

A normal batch may commit only when its post-state still satisfies:

```text
last_event_seq + proposed_batch_count
  + terminal_reserve
  + late_completion_reserve
  + integrity_reserve
<= limits.max_total_events
```

Claims include their new attempt in the post-state reserves. If the check would fail, the proposed command does not apply; the store instead uses the already-reserved cancellation batch R11/R12/R13 with system reason `RunEventLimitExceeded`. Run creation rejects limits too small for its creation batch plus reserves. One observation per attempt and one integrity override keep these reserves finite.

### 5.4 Read API

Point/list methods are scoped projections: `get_definition`, `get_revision`, `get_run`, `get_node`, `get_attempt`, `get_gate`, `list_runs`, `list_nodes`, and `list_events_after`. Every key/cursor contains and is checked against `ExecutionScope`. Reads never mutate state. A failed verified object read returns an opaque `FailedReadProof`; callers invoke `mark_corrupt_storage` before returning and never assert failure themselves.

`get_run` includes a non-durable `RunOperationalView`, computed from one control-plane read snapshot. It is null unless durable run status is Running. Its fields are:

```text
{
  phase: Executing | AwaitingBudget | AwaitingApproval | RetryDelay | WaitingChildren | Mixed,
  counts: {
    ready, running_attempts, budget_waiting,
    pending_approvals, retry_waiting, maps_waiting_children
  },
  next_due_at: Timestamp | null
}
```

Category `Executing` is nonzero only when Ready or Running work exists. `AwaitingBudget`, `AwaitingApproval`, `RetryDelay`, and `WaitingChildren` correspond respectively to BudgetWaiting, pending-approval, RetryWaiting, and WaitingChildren counts. If exactly one category is nonzero, that category is the phase; therefore a run whose only nonterminal nodes are BudgetWaiting is `AwaitingBudget`, never `Executing`. If two or more categories are nonzero, phase is Mixed. `next_due_at` is the minimum persisted attempt deadline, retry time, gate expiry, and run lifetime deadline. The view is operational convenience and never a transition precondition.

A conforming Running run has at least one nonzero category. A zero-category snapshot is an invariant violation returned as `CorruptControlPlane`, not a sixth operational phase and not an inferred success.

Scheduler scans use keyset pagination, never offset pagination. Each first page captures `cutoff = database_now`; subsequent opaque authenticated cursors bind `{scope, query_kind, filter_digest, cutoff, last_order_key}`. Rows must have their relevant `created_at/updated_at <= cutoff`; later changes are found by a new scan. Page size is 1–1000, default 100. Every returned row is rechecked by its command CAS.

| Scan | Predicate at captured cutoff | Ascending order key |
|---|---|---|
| `scan_ready_nodes` | Run Running; node Ready; `updated_at <= cutoff` | `(run_id, node_instance_id)` |
| `scan_budget_waiters` | Run Running; node BudgetWaiting; updated before cutoff | `(run_id, node_instance_id)` |
| `scan_due_deadlines` | Started active attempt; `deadline_at <= cutoff` | `(deadline_at, run_id, node_instance_id, attempt_id)` |
| `scan_due_retries` | Run Running; RetryWaiting; `next_eligible_at <= cutoff` | `(next_eligible_at, run_id, node_instance_id)` |
| `scan_recovery_runs` | At least one Started attempt from a lower generation | `(run_id)`; returns each run once, and batch recovery orders its attempts internally |
| `scan_compatibility_rechecks` | Every nonterminal run in Pending, Running, or BlockedIncompatible with `updated_at <= cutoff` | `(updated_at, run_id)` |
| `scan_due_gates` | Run Running; gate Pending; `expires_at <= cutoff` | `(expires_at, run_id, gate_id)` |
| `scan_due_run_lifetimes` | Run Pending/Running/Blocked; `lifetime_deadline_at <= cutoff` | `(lifetime_deadline_at, run_id)` |

Event pagination orders strictly by `(run_id, event_seq)` for a single run and returns complete batches by default: if a requested page boundary falls inside a batch, the adapter extends that page through `batch_index = batch_count - 1`, subject to an explicit hard response-byte limit that returns `BatchTooLarge` rather than splitting silently.

### 5.5 Error taxonomy

The store error set is closed at the domain boundary:

| Error | Meaning |
|---|---|
| `NotFound` | No entity exists in the supplied scope. It is also returned for an ID that exists only in another scope. |
| `AlreadyExists` | Scoped immutable ID already exists without an idempotent match. |
| `IdempotencyConflict` | Same deterministic ID/token was reused with different immutable inputs. |
| `CasConflict` | Expected mutable row version/status changed. |
| `IllegalTransition` | Requested source/target pair is absent from section 3. |
| `InvalidField` | Parameter is malformed before domain evaluation. |
| `ContractValidation{kind,path,message,valid_alternatives}` | Validation failed before a run existed, so no run transition can be recorded. |
| `ContractValidationApplied` | Runtime validation atomically produced ContractFailed. |
| `RunBlockedIncompatible` | Command is forbidden by the Blocked command fence; no write occurred. |
| `RunLimitsInvalid` / `RunLimitApplied` | Creation limits are inconsistent, or a durable run limit terminalized/cancelled the run. |
| `EngineAlreadyLive` | A non-expired claim exists, even if caller repeats the same instance ID text. |
| `EngineClaimLost` / `EngineClaimExpired` | Permit no longer owns a live generation. |
| `IncompatiblePins` / `StillIncompatible` / `EvidenceInvalid` | Exact semantic compatibility evidence cannot start/resume. |
| `CompatibilityOverrideForbidden` | Resume proposed a substitute/mismatched digest; mismatch is never overridable. |
| `InvalidCompletionCredential` | Result intake did not prove possession of the per-attempt capability. |
| `DiagnosticsInvalid{path,code}` | A diagnostics object violated the closed `DiagnosticsEnvelope`; no transition, ledger mutation, or event occurred. |
| `DiagnosticsTooLarge{limit_bytes,observed_bytes}` | `complete_attempt` or `fail_contract` received diagnostics whose canonical JSON exceeds the mandatory 65,536-byte limit; no transition, ledger mutation, or event occurred. |
| `AttemptIdConflict` / `AttemptFenced` / `CurrentGenerationAttemptPresent` | Attempt identity or recovery-set precondition failed. |
| `MapConcurrencyLimited` | Transient admission limit; no state change. |
| `DeadlineNotDue` / `RetryNotDue` / `ExpiryNotDue` / `LifetimeNotDue` | Database clock condition is false. |
| `ApprovalUnauthorized` | Authenticated principal does not satisfy the immutable gate policy; no CAS/event. |
| `ApprovalAlreadyResolved` / `ApprovalRaceLost` / `CancellationRaceLost` | First-valid-decision CAS was lost. |
| `RunAlreadyTerminal` / `ChildrenIncomplete` / `AggregateMismatch` | Domain precondition failed. |
| `ObjectNotVerified` / `DigestMismatch` / `ArtifactMetadataConflict` | A proposed new reference is not safely committable. |
| `InvalidFailedReadProof` | Corruption command lacked an object-store-minted proof matching scope/digest/store nonce. |
| `RevisionDefinitionIdMismatch` / `SchemaSubsetUnsupported` | Publication target identity or schema document is invalid. |
| `BatchTooLarge` | A consumer response limit cannot contain one complete atomic event batch. |
| `ClockNonMonotonic` | Database time is earlier than the persisted claim clock; claim acquisition/heartbeat fails closed with no write until time catches up or the database clock is repaired. |
| `CorruptControlPlane` | A read-only derived view found an impossible durable-state combination. No workflow outcome is inferred and no row is changed. |
| `ArithmeticOverflow` | Checked input could not fit `u64`, or a persisted budget invariant is already broken. The transaction rolls back; under valid rows this is unreachable and indicates a non-conforming/corrupt adapter, not an additional workflow outcome. |
| `StorageUnavailable` / `TransactionFailed` | Infrastructure failure; no uncommitted state may be treated as durable. |

Cross-scope probing never returns `ScopeMismatch` to untrusted host callers; it returns `NotFound`. Internal `ScopeMismatch` in `mark_corrupt_storage` is a programming error and commits nothing.

## 6. Singleton engine claim

The heartbeat interval is 5 seconds and the expiry interval is 20 seconds. Both are v0.1 wire-level constants, not per-process configuration. A heartbeat sets:

```text
heartbeat_at = database_now
expires_at   = database_now + 20 seconds
```

SQLite time is read inside the claim transaction. Process wall clocks and monotonic clocks are never used for ownership, expiry, retry eligibility, attempt deadlines, or approval expiry.

Operational clock assumption: the database clock for a scoped control plane must not move backward and must continue advancing. Claim acquisition and heartbeat compare DB-now with the persisted `heartbeat_at`/`claimed_at`; if DB-now is earlier, they return `ClockNonMonotonic` and commit nothing. No takeover, deadline, retry, gate expiry, or lifetime transition advances DB-now to compensate: every due predicate uses the regressed value unchanged, so future-due work is delayed while work already due under that value remains eligible. Operators must repair or advance a regressed/stalled database clock; fail-closed ownership preserves single-owner safety but suspends the fixed wall-time liveness guarantee.

Acquisition uses an atomic insert-or-conditional-update:

1. absent row: use store CSPRNG to mint a 256-bit session token, insert its digest with generation 1, and return the raw token once;
2. any live row where `expires_at > database_now`: fail closed with `EngineAlreadyLive`, even when the caller repeats the same `instance_id`;
3. any expired row: CAS its version, mint a new session token, replace owner/token digest, and set `generation = old_generation + 1`.

Heartbeat is the only live-claim renewal path and requires the raw session token. Every scheduler-authored state command checks `(instance_id, generation, SHA-256(session_token), expires_at > database_now)` inside its transaction. Instance IDs are labels, not credentials. A paused old engine therefore cannot write after takeover. It stops scheduling immediately on heartbeat failure. Completion intake instead uses its per-attempt CompletionCredential, so a result from an old worker can race safely with batch recovery and be recorded after takeover. Host approval/cancellation commands do not require the engine claim but retain scoped/authenticated CAS.

Proof of eventual unlock under the operational clock assumption: a crashed process cannot update `expires_at`; once database time reaches that persisted timestamp, the expiry predicate becomes true and a new acquisition can atomically increment generation. With a normally advancing real-time database clock this is at most 20 seconds plus acquisition scheduling/SQLite lock delay. If the clock regresses or stalls, ownership remains fail-closed and the bound is weakened to “after DB-now again reaches `expires_at`”; no false fixed-time claim applies. No process-held mutex or deletion is required.

Claim crash boundary: death before acquisition commit leaves no new owner/session. Death after commit but before the raw session token reaches the caller leaves an intentionally unusable live claim; repeating `instance_id` cannot recover it, but database expiry makes takeover possible under the conditional bound above. Heartbeat/release death before commit leaves the previous expiry; death after commit leaves the new expiry/release. Under a non-regressing, advancing database clock, none can create a permanent lock.

## 7. Action invocation contract

### 7.1 ActionContext

The engine passes:

```text
ActionContext {
  scope: ExecutionScope,
  run_id: Id,
  revision_hash: Digest,
  node_instance_id: NodeInstanceId,
  attempt_id: Id,
  attempt_number: u32,
  idempotency_key: String,
  completion_credential: opaque CompletionCredential,
  deadline: Timestamp,
  cancellation_token: cooperative token,
  budget: BudgetHandle { declared_max_cost_units: u64 }
}
```

For every field byte string `x`, define:

```text
LP(x) = u64_be(byte_length(x)) || x
```

No delimiter or unescaped text concatenation participates in key derivation. For a static node:

```text
idempotency_hash = SHA-256(
  LP(UTF8("dagger-idem-v1")) ||
  LP(UTF8(scope.tenant_id)) ||
  LP(UTF8(scope.namespace)) ||
  LP(UTF8(run_id)) ||
  LP(UTF8(node_instance_id))
)

idempotency_key = "dwf-idem-v1:" || lowercase_hex(idempotency_hash)
```

For a Map child the common tuple is followed by the complete child identity:

```text
idempotency_hash = SHA-256(
  LP(UTF8("dagger-idem-v1")) ||
  LP(UTF8(scope.tenant_id)) ||
  LP(UTF8(scope.namespace)) ||
  LP(UTF8(run_id)) ||
  LP(UTF8(child_node_instance_id)) ||
  LP(UTF8("map-child")) ||
  LP(UTF8(map_parent_node_instance_id)) ||
  LP(u32_be(map_item_index)) ||
  LP(UTF8(map_item_digest))
)

idempotency_key = "dwf-idem-v1:" || lowercase_hex(idempotency_hash)
```

The versioned domain and length-prefixed complete tuple prevent delimiter ambiguity and make equal run/node IDs in two scopes produce different hashes without exposing raw scope atoms. The Map suffix binds parent, index, and item digest in addition to the synthetic child NodeInstanceId. The key is identical across retries of one scoped logical node and different across scopes, runs, and Map children. `deadline` is the persisted database timestamp from A01. The cancellation token is cooperative and advisory. The completion credential authorizes only this attempt’s result intake; it is not an EnginePermit and must not be logged or forwarded to external side-effect targets.

The engine invokes the action with this context plus the immutable ActionInvocation’s exact canonical bound-input bytes. The digest is checked immediately before delivery. The budget handle exposes the declared maximum and allows actual-cost reporting but cannot mutate the ledger. Ledger accounting alone cannot stop a provider from overspending: each action/provider adapter must translate the declared maximum into enforceable provider limits such as token, request, or monetary caps and abort provider work at that cap.

### 7.2 ActionOutcome

The result is exactly one of:

```text
Success {
  output: typed JSON,
  artifacts: ordered list<ArtifactOutput {
    media_type: String,
    object: VerifiedObjectRef
  }>,
  actual_cost_units: u64,
  diagnostics: DiagnosticsEnvelope | null
}

Retryable {
  code: namespaced string,
  message: persistence-safe string,
  diagnostics: DiagnosticsEnvelope | null,
  actual_cost_units: u64
}

Permanent {
  code: namespaced string,
  message: persistence-safe string,
  diagnostics: DiagnosticsEnvelope | null,
  actual_cost_units: u64
}
```

The engine validates Success output against the pinned output schema before completion. Any `actual_cost_units > declared_max_cost_units` is `ActionCostProtocolViolation`: charge the full reservation and enter ContractFailed. Diagnostics must validate the closed `DiagnosticsEnvelope`; malformed envelopes return `DiagnosticsInvalid{path,code}` and envelopes over 65,536 canonical bytes return `DiagnosticsTooLarge{limit_bytes:65536,observed_bytes}`. Both are mandatory pre-transition, no-write rejections by `complete_attempt`; no attempt, node, ledger, or event changes, and the caller may resubmit without diagnostics while normal fencing/deadline preconditions still hold. Large diagnostic detail belongs in a scoped ActionArtifact referenced from the envelope. The host retains the section 1.1 responsibility not to place semantic secrets in allowed strings or artifact content.

An action that exits because of its cancellation token may return Retryable or Permanent only if that classification is semantically true. If the engine has already committed TimedOut/Cancelled/UnknownOutcome, the result is stale regardless.

### 7.3 Guarantee

Action invocation is at-least-once for unfinished logical nodes. A completed fenced node is never rerun. Crashes can cause reinvocation with the same scope-bound logical-node idempotency key. Only the attempt currently named by `NodeRun.active_attempt_id` can affect internal state. A credential-authenticated old-worker result can commit before batch recovery or becomes Stale/a single stale observation after recovery; scheduler generation is irrelevant to result intake. This provides exact durable bookkeeping, not exactly-once external side effects. Side-effecting actions must use the idempotency key with the external system.

## 8. Dataflow binding spec

### 8.1 Input construction

Action and Map-child input bindings are an ordered array of explicit target-field assignments. The engine starts from an empty JSON object and applies bindings in lexical target JSON-pointer order. Each target is an RFC 6901 pointer to one leaf in the pinned action input schema. A target occurs exactly once. Parent/child target overlap such as `/a` and `/a/b` is invalid.

Binding sources are closed:

| Source | Form | Result |
|---|---|---|
| Literal constant | `{ "kind": "constant", "value": <JSON> }` | The exact canonical JSON value. |
| Run input | `{ "kind": "run_input", "pointer": <JSON pointer> }` | Value selected from the immutable run-input object. Empty pointer selects the root. |
| Upstream output | `{ "kind": "node_output", "node_id": <static ID>, "pointer": <JSON pointer> }` | Value selected from the named successful upstream output. |
| Artifact reference | `{ "kind": "artifact_ref", "source": <artifact locator> }` | A typed `ArtifactRef` value, not bytes. Locator is an exact pre-existing scoped ArtifactRef identity, a run-input pointer, or a successful upstream-output pointer. |

Map child bindings additionally admit `{ "kind": "map_item", "pointer": ... }` and `{ "kind": "map_index" }`; these are defined in section 10 and are legal only inside a Map node’s child binding list.

The canonical bound ArtifactRef value is `{ "artifact_ref_id": <Id>, "digest": <Digest>, "size_bytes": <u64 decimal string>, "media_type": <string> }`. Scope comes only from `ActionContext.scope` and is not caller-overridable inside the value. The store resolves `artifact_ref_id` and digest together under that scope before claim.

A literal artifact locator contains all of `{ artifact_ref_id, digest, media_type }`. At publication, the store resolves that exact ArtifactRef and its ObjectRecord under the definition scope, verifies identity/digest/media type and bytes, and records the reference as revision-owned constant metadata. A bare digest never creates or guesses an ArtifactRef.

There is no implicit object merge, inheritance, environment lookup, “current scope” lookup, positional matching, string interpolation, or type coercion. Numeric strings remain strings. Artifact bytes are not substituted for a ref. Missing non-required action fields are omitted, not set to null. A literal null must be explicit.

### 8.2 ActionInvocation derivation

Before claim, the engine:

1. verifies every source object and resolves each source pointer;
2. canonicalizes each resolved value and records its value digest;
3. applies assignments in lexical target-pointer order to the empty schema-shaped object;
4. validates the finished value against the pinned supported-subset input schema;
5. canonicalizes the finished object using the section 13 rules;
6. rejects it if its byte length exceeds `max_inline_json_bytes_per_value`;
7. durably puts/verifies those exact bytes;
8. computes `binding_derivation_digest` over ordered `{ target, canonical source descriptor, resolved_value_digest }` tuples;
9. passes the verified ref/digest/derivation to `claim_node_attempt`, which commits ActionInvocation with A01.

The bytes in ActionInvocation are the sole action input. Recovery/retry may recompute them for verification, but a committed invocation is never rewritten and the action never receives a separately serialized reconstruction.

### 8.3 Static validation

Publication rejects:

- a missing binding for any required input-schema leaf;
- duplicate/overlapping targets or a target absent from the pinned input schema;
- a source node that does not exist, is not a strict ancestor, or cannot dominate the consumer on every path that can make the consumer Ready;
- a statically detectable source that may be Skipped or failed while the consumer can still activate;
- a JSON pointer invalid under RFC 6901 or statically absent from a known schema;
- a source schema type not assignable to the target schema type without coercion;
- an artifact binding whose target does not accept the canonical ArtifactRef schema;
- a literal artifact identity/digest/media tuple that does not resolve to one verified ArtifactRef/ObjectRecord in the definition scope at publication;
- use of Map intrinsics outside Map child bindings;
- a binding dependency that would introduce a cycle even when no control edge does.

The dominance rule deliberately rejects every post-reconvergence binding to a branch-specific output. A post-reconvergence consumer may bind only to run input or upstream nodes that dominate it and therefore exist on every active path. v0.1 has no phi/branch-value merge. Work needing a branch output must remain on that branch before reconvergence.

Pointer validation decodes RFC 6901 tokens exactly once. Source pointers may traverse a declared object property or an array item using a canonical non-negative decimal index with no leading zero except `0`; an array index is statically guaranteed only when `minItems > index`. Target pointers may traverse declared object properties only; an array is assigned as one leaf value, never assembled element-by-element. A pointer that names no possible schema location is rejected; a possible but non-required source location is checked at runtime.

Schema type assignability is conservative:

- source type set must be a subset of target type set; `integer` is assignable to `number`, never the reverse;
- source `const`/`enum` values must all validate against the target; otherwise enum sets must be a subset;
- source numeric interval must be within the target interval;
- source string length interval must be within the target interval; a target pattern is satisfied statically only by an identical source pattern or by finite source const/enum values that validate;
- arrays require assignable item schemas, source `minItems >= target minItems`, and source `maxItems <= target maxItems` when target has a maximum;
- objects require every target-required property, recursively assignable shared properties, and no source property outside a target with `additionalProperties: false`;
- no inference is attempted across regex implication, unions, conditional schemas, defaults, or coercion, because those features are outside the supported subset.

### 8.4 Runtime failures

Dynamic JSON content can still violate a valid schema. Before A01, the engine resolves and verifies every binding:

| Runtime condition | Closed transition |
|---|---|
| Named upstream is `Skipped`, `Failed`, `ContractFailed`, `RetriesExhausted`, `BudgetExhausted`, `Cancelled`, or `CorruptStorage` | N46/R08 with `BindingSourceUnavailable`, except corrupt ref uses N47/R15 |
| Pointer does not exist | N46/R08 with `BindingPointerMissing` |
| Value fails target schema/type | N46/R08 with `BindingTypeMismatch` |
| Artifact locator is not a valid scoped ArtifactRef | N46/R08 with `BindingTypeMismatch` |
| Referenced object is missing or digest-invalid | N47/R15 through `mark_corrupt_storage` |

No binding failure is retryable in v0.1, because revisions and upstream results are immutable.

## 9. Choice contract

A Choice node has one input binding, one RFC 6901 selector pointer, an ordered non-empty case list, and one required default target.

1. The bound Choice input is canonicalized, durably put, and digest-verified.
2. The selector resolves exactly once for the committed decision. The engine checks cases in array order.
3. The selector pointer must exist and resolve to a JSON scalar. Missing or non-scalar selection commits N46/R08 with `ChoiceInputInvalid`. `equals` compares exact scalar type and value. `in` tests exact membership in a non-empty unique list of JSON scalar values. There is no numeric/string coercion, truthiness, relational operation, regular expression, Boolean composition, function call, or variable lookup.
4. First matching case wins. If none matches, the required default wins.
5. `record_choice` atomically persists `choice_input_ref`, selector-value digest, selected case/default, node outcome, edge facts, skip propagation, and events. Only one decision can commit. A crash before commit may repeat the deterministic computation against the same digest; a crash after commit never reevaluates it.
6. Exactly one outgoing edge becomes Satisfied. Every other outgoing edge becomes Skipped. Publication rejects duplicate Choice target IDs, so “one edge” and “one branch” are the same in v0.1.
7. Skipped propagation uses the section 3 fixed point. A reconverged node becomes Ready only when every incoming edge is Satisfied or Skipped and at least one is Satisfied. A node with all incoming edges Skipped becomes Skipped and propagates Skipped.

Publication rejects a missing default. A definition that wants fail-closed no-match behavior points its default edge to an explicit Fail node, keeping failure visible in topology and the normal N17/R07 transition.

The pinned input digest prevents evaluation against a changed view after restart. Choice cannot read mutable host state; any such value must first be captured by an Action output.

## 10. Map contract

### 10.1 Shape and identity

In v0.1 a Map is a bounded fan-out of one pinned Action, not an arbitrary subworkflow. Its `items` binding must resolve to a JSON array. Each item is canonicalized independently:

```text
item_digest = SHA-256(canonical_item_json)
child_id = "mapchild_" + hex(SHA-256(
  length_prefixed("dagger-map-child-v1") ||
  length_prefixed(workflow_run_id) ||
  length_prefixed(map_node_instance_id) ||
  u32_be(item_index) ||
  digest_bytes(item_digest)
))
```

Scope is part of the database key but not the hash formula. Including `workflow_run_id` makes IDs differ across separate runs. The item index makes equal duplicate items distinct.

The expansion digest is SHA-256 of the domain separator plus the ordered list of `(u32_be(index), item_digest bytes, child_id bytes)`. It is persisted before children can be claimed.

### 10.2 Binding and duplicates

Map child action bindings use the normal sources plus:

- `map_item`: selects the whole item or an RFC 6901 pointer within it;
- `map_index`: yields the zero-based `u32` index as a JSON integer.

Duplicate items are preserved, not deduplicated. They receive different child IDs and external idempotency keys because the section 7.1 versioned hash includes the child ID, parent ID, index, and item digest; each independently reserves/charges budget.

### 10.3 Bounds and scheduling

Both `max_items` and `max_concurrency` are required positive bounded integers in the published definition. `max_concurrency <= max_items`. The immutable run dynamic-node ceiling also applies to the sum of all Map children across all Map nodes. The engine-wide concurrency bound applies; effective Map concurrency is the minimum. The store enforces per-Map concurrency in `claim_node_attempt` by counting that parent’s Started child attempts inside the transaction.

If runtime array length exceeds `max_items`, the Map and run enter ContractFailed with `MapBoundExceeded`; truncation is forbidden.

### 10.4 Zero, completion, aggregation, and failure

- Zero items: expansion persists child count 0 and a verified canonical `[]` aggregate; the parent succeeds in N07 without claiming an attempt or reserving budget.
- Non-zero parent completion: all and only the persisted children must be Succeeded. The engine builds a canonical JSON array of child output values in ascending `item_index`, puts/verifies it, and `complete_map` commits N08.
- Completion does not depend on finish order. Missing index, duplicate index, child ID mismatch, output schema mismatch, inline-value overflow, or cumulative aggregate-byte overflow prevents aggregation and applies the closed contract/run-limit outcome.
- Child failure policy is fail-fast. A child PermanentFailed, ContractFailed, RetriesExhausted, BudgetExhausted, Cancelled by external run cancellation, or CorruptStorage terminates the Map/run through the matching closed outcome. There is no tolerance threshold, partial aggregate, catch edge, or error edge.
- On fail-fast or run cancellation, pending/ready/retry siblings become Cancelled, active sibling attempts become Cancelled and full-settled unless a trusted cost was atomically available, and cooperative tokens are signalled after commit. Already-successful children remain immutable.

### 10.5 Budget and idempotent expansion

Expansion reserves no aggregate budget. Each child claim atomically reserves that child action’s declared maximum, and each retry is a new independent reservation and charge. A child whose shortage is solely sibling reservations enters BudgetWaiting; it is not failed. Concurrent children therefore cannot oversubscribe the run.

`expand_map` is one transaction containing the parent marker and entire child set. Re-expansion with the same input and expansion digest is an idempotent read of that set. Re-expansion with any changed length/item/index/digest/child ID is `IdempotencyConflict`. Rollback after a mid-expansion crash leaves no partial set.

## 11. Budget ledger

For every run, at all committed boundaries:

```text
available = budget_limit - budget_consumed - budget_reserved
budget_consumed + budget_reserved <= budget_limit
```

All operands are `u64`; implementations use checked arithmetic and never floating point.

Cost units are opaque host-defined integers. The engine has no token, currency, model, or pricing semantics and never converts between units. The ledger is durable admission/accounting, not a provider-side circuit breaker; every action/provider adapter must enforce the corresponding actual token/spend/request cap.

### 11.1 Reservation

N05/N58/A01 performs one atomic conditional update equivalent to:

```text
WHERE budget_limit - budget_consumed - budget_reserved >= declared_max_cost
SET budget_reserved = budget_reserved + declared_max_cost
```

The transaction also inserts the Started attempt and immutable Reserve ledger entry. User code cannot run before this commit. Concurrent claims serialize on the run CAS, so they cannot collectively reserve more than the limit.

If current `available < declared_max`, the store distinguishes:

```text
declared_max <= budget_limit - budget_consumed
    => shortage is only live reservations: N59 BudgetWaiting, run remains Running

declared_max > budget_limit - budget_consumed
    => impossible even if every reservation settles to zero:
       N27/N60 + R10 BudgetExhausted
```

Neither path creates an attempt or ledger entry. BudgetWaiting is retried by the deterministic scan after settlements; claim moves it directly to Running only when reservation succeeds, avoiding Ready/wait churn. If actual settlements raise consumed enough to make the request impossible, N60/R10 terminalizes it. Partial funding is forbidden.

### 11.2 Settlement

An accepted outcome with trusted actual `a`, where `0 <= a <= reservation r`, atomically:

```text
budget_reserved = budget_reserved - r
budget_consumed = budget_consumed + a
```

The settlement entry records `reserved_delta=-r`, `consumed_delta=a`, and the reason. Unused `r-a` becomes available; it is not a negative consumption entry.

UnknownOutcome, TimedOut, and Stale always charge `a=r`. Cancellation without a trusted, concurrently supplied actual cost also charges `r`. An action reporting `a>r` violates its contract: the store charges `r`, records ContractFailed, and never exceeds the run limit.

Every Started attempt is independently charged, including retries and an attempt that crashed before invocation was known to begin. An attempt cannot settle twice because terminal attempt rows are immutable. Terminal run completion requires `budget_reserved=0`.

There is no host refund, budget increase, raw reserve, raw settle, or negative cost in v0.1.

Per-entity schema caps alone are not adequate: one zero-cost 10,000-item Map with `max_attempts=100` can compose to 1,000,000 attempts. The immutable default run ceiling of 100,000 attempts, plus dynamic-node/event/object/lifetime ceilings, is the actual guard; zero declared cost bypasses none of them.

## 12. Two-store commit protocol

### 12.1 Durable object put

For each scoped payload, the object store:

1. canonicalizes JSON when the media type requires it and computes SHA-256 over final bytes;
2. writes bytes to a unique temporary file in the destination filesystem/directory;
3. flushes and `fsync`s the file;
4. publishes it with an atomic no-replace primitive to the scope-qualified content-addressed path;
5. `fsync`s the parent directory;
6. reopens the final path, streams it, and verifies size and digest;
7. returns an opaque `VerifiedObjectRef`.

Publish is strictly if-absent. An existing scoped digest path is never rewritten, replaced, truncated, or “repaired,” even with equal bytes. If it exists, the store discards the temp only after verifying existing bytes/metadata; a different size or bytes returns `ArtifactMetadataConflict`. It never returns `FailedReadProof` for this candidate-publication conflict because that proof is reserved for failed reads of already-committed refs. Platforms lacking `renameat2(RENAME_NOREPLACE)` must use another atomic no-replace construction such as link-if-absent plus directory sync, never ordinary replacing rename. Object keys include escaped tenant/namespace components; cross-scope deduplication is forbidden.

### 12.2 Control-plane commit

Only after all new objects required by a transition return verified refs may the engine invoke its domain command. That command commits one SQLite transaction containing every applicable:

```text
attempt outcome
node status and active-attempt fence
output/artifact metadata refs
budget settlement and summary
Map parent/children effects
frontier edge facts and readiness/skip propagation
run outcome
ordered per-run events
```

The SQLite adapter stores digests, sizes, media types, object keys, and small diagnostics/event payloads, never bulky object bytes. API construction prevents a row from accepting a bare unverified digest.

### 12.3 Failures and reads

A crash after object put and before SQLite commit leaves an unreachable orphan. This is acceptable; garbage collection is deferred beyond v0.1. No recovery code treats object existence as a committed workflow transition.

A committed row therefore references only an object verified before its transaction. On every read, size and digest are verified again. Missing/invalid bytes cause the object store to mint a `FailedReadProof` bound to scope, requested digest, error class, store-instance nonce, and unique proof nonce. Only that opaque proof can invoke `mark_corrupt_storage` and cause N47–N57/R14–R22. Caller assertions, exception strings, or bare digests are insufficient. The engine never silently reruns the producer, searches for a mutable same-name file, returns unverified bytes, or substitutes null.

## 13. Revision hashing and action pinning

### 13.1 Canonical normalized JSON

Definitions may be authored as JSON, YAML, or programmatic values, but revision identity is computed only after strict typed deserialization and normalization to JSON. The accepted canonicalization is RFC 8785 JSON Canonicalization Scheme with these v0.1 constraints:

- `definition_format_version` is required and exactly `"0.1"`;
- duplicate object keys, invalid Unicode, NaN, infinities, and values outside I-JSON are rejected;
- object keys are sorted by RFC 8785 rules; insignificant whitespace is absent;
- schema integer fields must be exactly representable under their declared bounds;
- `declared_max_cost_units`, which spans full `u64`, is represented in definition JSON as a canonical decimal string (`"0"` or a non-zero digit followed by digits), then parsed with checked `u64`;
- literal JSON numbers obey RFC 8785/I-JSON; authors needing larger exact values use strings and an action schema that declares strings;
- YAML aliases are resolved before typing, YAML non-string mapping keys are rejected, and YAML tags do not influence meaning.

The revision hash is:

```text
revision_hash = "sha256:" + lowercase_hex(SHA-256(canonical_definition_bytes))
```

The canonical bytes are stored as a verified Definition ArtifactRef. Raw YAML bytes, comments, key order, whitespace, anchors, and source filename never enter the hash.

The canonical document’s `definition_id` must equal the scoped WorkflowDefinition target passed to `publish_revision`; a mismatch is never normalized or aliased. Its `run_input_schema_digest` and `run_output_schema_digest` must resolve at publication to verified, digest-addressed SchemaDocument ArtifactRefs. Those immutable refs are stored on WorkflowRevision and reverified whenever used. A digest string without a durable schema object cannot be published.

### 13.2 Pin match

Each Action node and Map child action carries:

```text
name
contract_version
input_schema_digest
output_schema_digest
compatible_implementation_requirement
```

`compatible_implementation_requirement` is an exact semantic-compatibility digest, not an unbounded version range and not a binary/build hash. A registry implementation advertises:

```text
{
  name,
  contract_version,
  input_schema_digest,
  output_schema_digest,
  implementation_compatibility_digest
}
```

A pin matches only if all five fields are byte-for-byte equal after normalization. The digest identifies contract behavior: several rebuilt/optimized binaries may advertise the same digest only when the host certifies identical observable semantics. Any semantic change requires a new digest even if Rust symbol, contract version, or schemas are unchanged. Conversely, a harmless binary rebuild need not change it.

### 13.3 Check points and suspension

The engine checks all revision action pins:

- before R02 at run start;
- after every engine claim/takeover before recovering scheduling;
- before resuming from an approval or persisted retry;
- before R05 from BlockedIncompatible;
- again inside every attempt claim.

If any exact pin is unavailable, the engine stops new claims and commits R03/R04 with affected N29–N31/N61 after batch recovery/completion leaves no Started attempt. It never binds a merely newer or “close enough” registry entry.

Recheck is triggered automatically on every engine startup and may be triggered explicitly by the host’s `resume_compatible` API. Resume checks availability only: the registry must now contain an implementation advertising the already-pinned exact semantic digest and exact other pin fields. No parameter, operator flag, migration, or host assertion can override/substitute a mismatch. A full availability match restores states through R05/N32–N34/N62; otherwise the run stays blocked. `cancel_run` provides R13. In-flight runs never change revision or pins.

Host obligation: the old implementation or a semantically compatible implementation advertising the same digest must remain deployable/available while any nonterminal run pins it. Removing the last compatible implementation intentionally suspends those runs; operators must restore one or cancel them.

## 14. Definition JSON Schema draft

### 14.1 Normative schema

The definition format uses JSON Schema Draft 2020-12. Unknown fields are rejected at every level. This is the full v0.1 syntactic schema:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://dagger.dev/schemas/workflow-definition-0.1.json",
  "title": "dagger-workflow-core definition 0.1",
  "type": "object",
  "additionalProperties": false,
  "required": [
    "definition_format_version",
    "definition_id",
    "name",
    "run_input_schema_digest",
    "run_output_schema_digest",
    "entry_node_id",
    "nodes"
  ],
  "properties": {
    "definition_format_version": { "const": "0.1" },
    "definition_id": { "$ref": "#/$defs/id" },
    "name": { "type": "string", "minLength": 1, "maxLength": 200 },
    "description": { "type": "string", "maxLength": 4000, "default": "" },
    "run_input_schema_digest": { "$ref": "#/$defs/digest" },
    "run_output_schema_digest": { "$ref": "#/$defs/digest" },
    "entry_node_id": { "$ref": "#/$defs/id" },
    "nodes": {
      "type": "array",
      "minItems": 1,
      "maxItems": 1024,
      "items": { "$ref": "#/$defs/node" }
    }
  },
  "$defs": {
    "id": {
      "type": "string",
      "minLength": 1,
      "maxLength": 128,
      "pattern": "^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$"
    },
    "targets": {
      "type": "array",
      "minItems": 1,
      "maxItems": 64,
      "uniqueItems": true,
      "items": { "$ref": "#/$defs/id" }
    },
    "digest": {
      "type": "string",
      "pattern": "^sha256:[0-9a-f]{64}$"
    },
    "u64_decimal": {
      "type": "string",
      "pattern": "^(0|[1-9][0-9]{0,19})$",
      "x-dagger-maximum-decimal": "18446744073709551615"
    },
    "source_pointer": {
      "type": "string",
      "pattern": "^(|(?:/(?:[^~/]|~[01])*)+)$"
    },
    "target_pointer": {
      "type": "string",
      "pattern": "^/(?:[^~/]|~[01])*(?:/(?:[^~/]|~[01])*)*$"
    },
    "json_scalar": {
      "type": ["string", "number", "integer", "boolean", "null"]
    },
    "action_pin": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "name",
        "contract_version",
        "input_schema_digest",
        "output_schema_digest",
        "compatible_implementation_requirement"
      ],
      "properties": {
        "name": {
          "type": "string",
          "minLength": 1,
          "maxLength": 200,
          "pattern": "^[A-Za-z0-9][A-Za-z0-9._:/-]{0,199}$"
        },
        "contract_version": {
          "type": "string",
          "minLength": 1,
          "maxLength": 64,
          "pattern": "^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$"
        },
        "input_schema_digest": { "$ref": "#/$defs/digest" },
        "output_schema_digest": { "$ref": "#/$defs/digest" },
        "compatible_implementation_requirement": { "$ref": "#/$defs/digest" }
      }
    },
    "timeout_policy": {
      "type": "object",
      "additionalProperties": false,
      "required": ["timeout_ms"],
      "properties": {
        "timeout_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 86400000
        }
      }
    },
    "retry_policy": {
      "type": "object",
      "additionalProperties": false,
      "required": ["max_attempts", "backoff"],
      "properties": {
        "max_attempts": {
          "type": "integer",
          "minimum": 1,
          "maximum": 100
        },
        "backoff": {
          "oneOf": [
            { "$ref": "#/$defs/fixed_backoff" },
            { "$ref": "#/$defs/exponential_backoff" }
          ]
        }
      }
    },
    "fixed_backoff": {
      "type": "object",
      "additionalProperties": false,
      "required": ["kind", "delay_ms"],
      "properties": {
        "kind": { "const": "fixed" },
        "delay_ms": {
          "type": "integer",
          "minimum": 0,
          "maximum": 86400000
        }
      }
    },
    "exponential_backoff": {
      "type": "object",
      "additionalProperties": false,
      "required": ["kind", "initial_delay_ms", "multiplier", "max_delay_ms"],
      "properties": {
        "kind": { "const": "exponential" },
        "initial_delay_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 86400000
        },
        "multiplier": {
          "type": "integer",
          "minimum": 2,
          "maximum": 16
        },
        "max_delay_ms": {
          "type": "integer",
          "minimum": 1,
          "maximum": 86400000
        }
      }
    },
    "artifact_locator": {
      "oneOf": [
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["kind", "artifact_ref_id", "digest", "media_type"],
          "properties": {
            "kind": { "const": "literal" },
            "artifact_ref_id": { "$ref": "#/$defs/id" },
            "digest": { "$ref": "#/$defs/digest" },
            "media_type": {
              "type": "string",
              "minLength": 1,
              "maxLength": 200
            }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["kind", "pointer"],
          "properties": {
            "kind": { "const": "run_input" },
            "pointer": { "$ref": "#/$defs/source_pointer" }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["kind", "node_id", "pointer"],
          "properties": {
            "kind": { "const": "node_output" },
            "node_id": { "$ref": "#/$defs/id" },
            "pointer": { "$ref": "#/$defs/source_pointer" }
          }
        }
      ]
    },
    "binding_source": {
      "oneOf": [
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["kind", "value"],
          "properties": {
            "kind": { "const": "constant" },
            "value": true
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["kind", "pointer"],
          "properties": {
            "kind": { "const": "run_input" },
            "pointer": { "$ref": "#/$defs/source_pointer" }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["kind", "node_id", "pointer"],
          "properties": {
            "kind": { "const": "node_output" },
            "node_id": { "$ref": "#/$defs/id" },
            "pointer": { "$ref": "#/$defs/source_pointer" }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["kind", "source"],
          "properties": {
            "kind": { "const": "artifact_ref" },
            "source": { "$ref": "#/$defs/artifact_locator" }
          }
        }
      ]
    },
    "binding": {
      "type": "object",
      "additionalProperties": false,
      "required": ["target", "source"],
      "properties": {
        "target": { "$ref": "#/$defs/target_pointer" },
        "source": { "$ref": "#/$defs/binding_source" }
      }
    },
    "value_source": {
      "oneOf": [
        { "$ref": "#/$defs/binding_source" }
      ]
    },
    "map_binding_source": {
      "oneOf": [
        { "$ref": "#/$defs/binding_source" },
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["kind", "pointer"],
          "properties": {
            "kind": { "const": "map_item" },
            "pointer": { "$ref": "#/$defs/source_pointer" }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["kind"],
          "properties": {
            "kind": { "const": "map_index" }
          }
        }
      ]
    },
    "map_binding": {
      "type": "object",
      "additionalProperties": false,
      "required": ["target", "source"],
      "properties": {
        "target": { "$ref": "#/$defs/target_pointer" },
        "source": { "$ref": "#/$defs/map_binding_source" }
      }
    },
    "choice_case": {
      "oneOf": [
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["equals", "next"],
          "properties": {
            "equals": { "$ref": "#/$defs/json_scalar" },
            "next": { "$ref": "#/$defs/id" }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["in", "next"],
          "properties": {
            "in": {
              "type": "array",
              "minItems": 1,
              "maxItems": 100,
              "uniqueItems": true,
              "items": { "$ref": "#/$defs/json_scalar" }
            },
            "next": { "$ref": "#/$defs/id" }
          }
        }
      ]
    },
    "action_node": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "id",
        "kind",
        "action",
        "bindings",
        "retry",
        "timeout",
        "declared_max_cost_units",
        "next"
      ],
      "properties": {
        "id": { "$ref": "#/$defs/id" },
        "kind": { "const": "Action" },
        "action": { "$ref": "#/$defs/action_pin" },
        "bindings": {
          "type": "array",
          "maxItems": 1024,
          "items": { "$ref": "#/$defs/binding" }
        },
        "retry": { "$ref": "#/$defs/retry_policy" },
        "timeout": { "$ref": "#/$defs/timeout_policy" },
        "declared_max_cost_units": { "$ref": "#/$defs/u64_decimal" },
        "next": { "$ref": "#/$defs/targets" }
      }
    },
    "choice_node": {
      "type": "object",
      "additionalProperties": false,
      "required": ["id", "kind", "input", "selector", "cases", "default"],
      "properties": {
        "id": { "$ref": "#/$defs/id" },
        "kind": { "const": "Choice" },
        "input": { "$ref": "#/$defs/value_source" },
        "selector": { "$ref": "#/$defs/source_pointer" },
        "cases": {
          "type": "array",
          "minItems": 1,
          "maxItems": 100,
          "items": { "$ref": "#/$defs/choice_case" }
        },
        "default": { "$ref": "#/$defs/id" }
      }
    },
    "map_node": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "id",
        "kind",
        "items",
        "max_items",
        "max_concurrency",
        "action",
        "bindings",
        "retry",
        "timeout",
        "declared_max_cost_units",
        "next"
      ],
      "properties": {
        "id": { "$ref": "#/$defs/id" },
        "kind": { "const": "Map" },
        "items": { "$ref": "#/$defs/value_source" },
        "max_items": {
          "type": "integer",
          "minimum": 1,
          "maximum": 10000
        },
        "max_concurrency": {
          "type": "integer",
          "minimum": 1,
          "maximum": 1024
        },
        "action": { "$ref": "#/$defs/action_pin" },
        "bindings": {
          "type": "array",
          "maxItems": 1024,
          "items": { "$ref": "#/$defs/map_binding" }
        },
        "retry": { "$ref": "#/$defs/retry_policy" },
        "timeout": { "$ref": "#/$defs/timeout_policy" },
        "declared_max_cost_units": { "$ref": "#/$defs/u64_decimal" },
        "next": { "$ref": "#/$defs/targets" }
      }
    },
    "approval_node": {
      "type": "object",
      "additionalProperties": false,
      "required": ["id", "kind", "request", "gate", "next"],
      "properties": {
        "id": { "$ref": "#/$defs/id" },
        "kind": { "const": "Approval" },
        "request": { "$ref": "#/$defs/value_source" },
        "gate": {
          "type": "object",
          "additionalProperties": false,
          "required": ["expires_after_ms", "authorization"],
          "properties": {
            "expires_after_ms": {
              "type": "integer",
              "minimum": 1,
              "maximum": 31536000000
            },
            "on_expiry": {
              "type": "string",
              "enum": ["approve", "reject"],
              "default": "reject"
            },
            "authorization": {
              "$ref": "#/$defs/decision_authorization_policy"
            }
          }
        },
        "next": { "$ref": "#/$defs/targets" }
      }
    },
    "decision_authorization_policy": {
      "type": "object",
      "additionalProperties": false,
      "required": ["allowed_principal_ids", "allowed_role_ids"],
      "properties": {
        "allowed_principal_ids": {
          "type": "array",
          "maxItems": 256,
          "uniqueItems": true,
          "items": {
            "type": "string",
            "minLength": 1,
            "maxLength": 256
          }
        },
        "allowed_role_ids": {
          "type": "array",
          "maxItems": 256,
          "uniqueItems": true,
          "items": {
            "type": "string",
            "minLength": 1,
            "maxLength": 256
          }
        }
      },
      "anyOf": [
        {
          "properties": {
            "allowed_principal_ids": { "minItems": 1 }
          }
        },
        {
          "properties": {
            "allowed_role_ids": { "minItems": 1 }
          }
        }
      ]
    },
    "succeed_node": {
      "type": "object",
      "additionalProperties": false,
      "required": ["id", "kind", "output"],
      "properties": {
        "id": { "$ref": "#/$defs/id" },
        "kind": { "const": "Succeed" },
        "output": { "$ref": "#/$defs/value_source" }
      }
    },
    "fail_node": {
      "type": "object",
      "additionalProperties": false,
      "required": ["id", "kind", "code", "message"],
      "properties": {
        "id": { "$ref": "#/$defs/id" },
        "kind": { "const": "Fail" },
        "code": {
          "type": "string",
          "minLength": 1,
          "maxLength": 128,
          "pattern": "^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$"
        },
        "message": {
          "type": "string",
          "minLength": 1,
          "maxLength": 2000
        }
      }
    },
    "node": {
      "oneOf": [
        { "$ref": "#/$defs/action_node" },
        { "$ref": "#/$defs/map_node" },
        { "$ref": "#/$defs/choice_node" },
        { "$ref": "#/$defs/approval_node" },
        { "$ref": "#/$defs/succeed_node" },
        { "$ref": "#/$defs/fail_node" }
      ]
    }
  },
  "x-dagger-semantic-constraints": [
    "Apply all schema defaults before canonical hashing.",
    "Node IDs are unique and entry_node_id names exactly one node.",
    "All next/default/case targets exist and each Choice target is unique.",
    "The control graph and binding dependency graph are acyclic.",
    "Every node is reachable from entry_node_id.",
    "Every maximal active path ends in Succeed or Fail.",
    "Exactly one Succeed node exists and is reachable from entry_node_id; multiple Fail nodes are allowed.",
    "Every Choice declares a default target; fail-closed default behavior uses an explicit Fail node.",
    "Action, Map action, and root run schema digests resolve to durable supported-subset SchemaDocument objects.",
    "Action input schemas are JSON objects and explicit bindings cover every required leaf exactly once.",
    "Binding targets are unique and non-overlapping; source types assign without coercion.",
    "Every node_output source is a dominating strict ancestor on every activating path.",
    "Every literal artifact identity/digest/media tuple resolves to one verified ArtifactRef and ObjectRecord in the definition scope.",
    "Choice case scalar values do not overlap under exact JSON equality.",
    "Map max_concurrency is less than or equal to max_items.",
    "Retry exponential max_delay_ms is greater than or equal to initial_delay_ms.",
    "All decimal u64 values parse without overflow.",
    "Canonical definition size is at most 4 MiB; unknown structural fields are rejected; the schema has no raw-credential field; host-authored constants and text remain subject to host semantic secret hygiene.",
    "Every approval authorization policy has at least one principal or role.",
    "No catch, error edge, cycle, loop, tolerance threshold, or unbounded Map extension is accepted."
  ]
}
```

### 14.2 Mandatory semantic phase

JSON Schema alone cannot compare graph references, prove dominance, detect cycles, or enforce the canonical-document byte ceiling. The `x-dagger-semantic-constraints` array is therefore normative, and `publish_revision` must pass both Draft 2020-12 validation and every listed semantic constraint before a revision exists. An adapter that validates only the syntactic schema is non-conforming. Secret handling is deliberately not a semantic-content test: conformance checks the closed field vocabulary, unknown-field rejection, exact byte limits, and absence of structural raw-credential fields. The host remains responsible for secret hygiene in permitted constants and text as section 1.1 states.

Defaults are expanded before canonical hashing; in v0.1 these are root `description=""` and Approval `on_expiry="reject"`. Thus omitted and explicit defaults hash identically.

This accepted format has no property capable of expressing error edges, catches, cycles, child subworkflows, unbounded Map, failure tolerance, dynamic action names, arbitrary Choice expressions, mutable revision references, or action-controlled topology. Unknown extension fields are rejected.

The `next` array on Action, Map, and Approval is normal DAG fan-out: successful completion satisfies every listed edge. Choice case/default targets are singular because exactly one Choice edge activates. Succeed and Fail have no outgoing fields.

Exactly one reachable Succeed node is mandatory; zero or more than one is a publication error. Multiple Fail nodes are allowed. This makes one pinned run-output schema and one final artifact producer authoritative, independent of scheduler completion order.

Backoff for retry after attempt number `n` is:

```text
fixed:       delay_ms
exponential: min(max_delay_ms, initial_delay_ms * multiplier^(n - 1))
```

The engine uses checked integer arithmetic and the database completion time to persist `next_eligible_at`. There is no jitter in v0.1, ensuring deterministic virtual-clock tests and restart behavior.

### 14.3 Supported schema-document subset

The definition-format schema above may use full Draft 2020-12 machinery internally. Host-supplied root/action input/output SchemaDocument objects are deliberately restricted. After duplicate-key rejection and canonicalization, only these keywords are accepted:

```text
$schema          root only; exactly Draft 2020-12 URI
$defs            object of named supported-subset schemas
$ref             local only: "#/$defs/<escaped-name>"; reference graph acyclic
type             required on every non-$ref schema; one type or unique type array
const
enum             at most 256 canonical-unique values
properties       object schemas only; at most 1024 entries
required         object schemas only; unique names present in properties
additionalProperties
items            one schema for all array items
minItems
maxItems
uniqueItems
minLength
maxLength
pattern
minimum
maximum
```

Rules tightening those keywords:

- allowed primitive type names are `null`, `boolean`, `object`, `array`, `number`, `integer`, and `string`;
- every object schema sets `additionalProperties: false`; schema-driven inputs therefore have a closed field set;
- `$ref` cannot have siblings and cannot leave the document or form a cycle;
- array schemas require `items`; bounds are non-negative safe integers and `minItems <= maxItems`;
- `uniqueItems` equality is equality of canonical JSON bytes;
- string patterns use Rust `regex` UTF-8 syntax, at most 1024 bytes, with no look-around/backreference/engine-specific extension;
- numeric bounds are inclusive finite I-JSON numbers with `minimum <= maximum`;
- schema nesting depth is at most 64 and canonical SchemaDocument size is at most 1 MiB;
- annotations and behavior-changing/defaulting keywords are forbidden, including `$id`, `title`, `description`, `default`, `examples`, `format`, `multipleOf`, exclusive bounds, `prefixItems`, `contains`, property/pattern dependencies, `allOf`, `anyOf`, `oneOf`, `not`, `if/then/else`, and unevaluated/dynamic-reference keywords;
- validators never inject defaults, coerce values, resolve remote refs, consult locale, or treat `format` as an assertion.

The same subset validator and canonical bytes are used at publication, binding validation, action output validation, run creation, and Succeed output validation. A schema using any unlisted keyword is `SchemaSubsetUnsupported`, not partially interpreted.

## 15. Event catalogue

### 15.1 Envelope and allocation

Every event has the immutable envelope from section 1.12. `scope` and `run_id` are always required and are not repeated below. Correlation abbreviations are:

- `N`: `node_instance_id` required;
- `A`: `attempt_id` and `node_instance_id` required;
- `G`: `gate_id` and `node_instance_id` required;
- `R`: run-only; node/attempt/gate fields null.

`event_seq` is `WorkflowRun.last_event_seq + 1`, allocated inside the transition transaction. Every event-producing transaction mints one `batch_id` that has never appeared earlier in that scoped run and enforces database uniqueness on `(scope,run_id,batch_id)` for the complete run lifetime, across process sessions and engine generations. Its events carry `batch_index=0..batch_count-1` and identical `batch_count`. No batch ID or sequence range is consumed on rollback. Consumers group by `(scope,run_id,batch_id)` and verify contiguous indices to reconstruct atomic command boundaries.

Within a batch, cascades are lexical and deterministic:

1. R01 uses RunCreated, then static nodes by node ID.
2. Otherwise primary attempt/gate subjects sort by `(node_instance_id, attempt_number-or-0, attempt_id-or-gate_id)`; for the same subject, state-transition events precede observation events, then direct Node and Budget events follow. Thus a due completion's terminalizing A06 event precedes its A14 observation event.
3. Map parent transitions sort by parent node ID.
4. Edge events sort by edge ID.
5. derived Ready/Skipped/Cancelled nodes sort by node ID, with nested attempt/gate events using step 2;
6. the run event is last.

Batch creation first computes the full ordered list, then writes `batch_count` and contiguous sequence values. Recovery C10 follows this same order after selecting the complete attempt set.

Payloads are strict canonical JSON objects at most 65,536 bytes. Fields not listed are forbidden and validation rejects unknown fields before event allocation. Diagnostic bodies, credentials, permits, completion credentials, authentication contexts, and object bytes have no event field; only scoped digests/refs and bounded persistence-safe codes do. This is the mechanically enforced event-format rule; semantic secret hygiene for allowed host-authored strings remains the host's responsibility.

### 15.2 Closed EventType set

| Event type | Transition | Correlation | Required payload fields |
|---|---|---|---|
| `RunCreated` | R01 | R | `definition_id, revision_hash, input_digest, budget_limit, limits, create_request_fingerprint` |
| `NodeCreatedPending` | N01 | N | `definition_node_id, kind, incoming_total, topological_rank` |
| `NodeCreatedReady` | N02 | N | `definition_node_id, kind, topological_rank` |
| `RunStarted` | R02 | R | `revision_hash, compatibility_evidence_digest` |
| `RunBlockedIncompatible` | R03/R04 | R | `incompatibilities_digest, incompatible_reference_locations[], suspension_fingerprint` |
| `NodeBlockedIncompatible` | N29–N31/N61 | N | `blocked_from_status, action_reference_location, required_semantic_digest` |
| `RunResumedCompatible` | R05 | R | `compatibility_evidence_digest` |
| `NodeResumedCompatible` | N32–N34/N62 | N | `restored_status, available_semantic_digest` |
| `RunSucceeded` | R06 | R | `output_digest, consumed_cost_units` |
| `RunFailed` | R07 | R | `failure_kind, diagnostics_digest?` |
| `RunContractFailed` | R08 | R | `failure_kind, diagnostics_digest?` |
| `RunRetriesExhausted` | R09 | R | `node_instance_id, attempt_id, max_attempts` |
| `RunBudgetExhausted` | R10 | R | `node_instance_id, requested, available, limit_minus_consumed, permanently_infeasible=true` |
| `RunCancelled` | R11–R13 | R | `principal?, reason_code, prior_status`; `principal` is required only for host cancellation |
| `RunCorruptStorage` | R14–R22 | R | `bad_artifact_ref_id, bad_digest, error_class, corrupt_proof_fingerprint, store_instance_nonce_digest, prior_status, owner_node_id?` |
| `NodeBecameReady` | N03 | N | `incoming_satisfied, incoming_skipped, incoming_total` |
| `NodeRetryEligible` | N04 | N | `next_eligible_at, database_now` |
| `NodeAttemptClaimed` | N05/N58 | N | `attempt_id, invocation_id, attempt_number, worker_id` |
| `AttemptStarted` | A01 | A | `attempt_number, worker_id, engine_generation, deadline_at, declared_max_cost_units, idempotency_key_digest, bound_input_digest, completion_credential_digest` |
| `BudgetReserved` | A01 | A | `ledger_seq, amount, available_after` |
| `MapChildCreated` | N02M | N | `parent_map_instance_id, item_index, item_digest, topological_rank` |
| `MapExpanded` | N06 | N | `map_input_digest, expansion_digest, child_count, max_concurrency` |
| `MapZeroItemsSucceeded` | N07 | N | `map_input_digest, expansion_digest, aggregate_digest` |
| `MapSucceeded` | N08 | N | `child_count, aggregate_digest` |
| `ChoiceSelected` | N09 | N | `choice_input_digest, selector_value_digest, selection_kind, case_index?, edge_id` |
| `ApprovalRequested` | N11 | N | `gate_id, request_digest, expires_at, on_expiry, authorization_policy_digest` |
| `ApprovalApproved` | N12 | N | `gate_id, decision_payload_digest?, approval_output_digest, resolution_source` |
| `ApprovalRejected` | N13 | N | `gate_id, decision_payload_digest?, resolution_source` |
| `ApprovalExpiredApproved` | N14 | N | `gate_id, expires_at, approval_output_digest` |
| `ApprovalExpiredRejected` | N15 | N | `gate_id, expires_at` |
| `SucceedNodeReached` | N16 | N | `output_digest` |
| `FailNodeReached` | N17 | N | `code, message_digest` |
| `NodeSucceeded` | N18 | N | `attempt_id, output_digest, artifact_digests[]` |
| `NodeRetryScheduled` | N19/N22/N23 | N | `attempt_id, attempt_number, next_eligible_at, cause` |
| `NodeFailed` | N20 | N | `attempt_id, failure_kind, error_code, diagnostics_digest?` |
| `NodeContractFailed` | N21/N46/N64/N67 | N | `attempt_id?, failure_kind, diagnostics_digest?` |
| `NodeRetriesExhausted` | N24–N26 | N | `attempt_id, attempt_number, max_attempts, cause` |
| `NodeBudgetWaiting` | N59 | N | `requested, available, consumed, reserved, limit` |
| `NodeBudgetExhausted` | N27/N60 | N | `requested, available, limit_minus_consumed` |
| `NodeSkipped` | N28 | N | `incoming_skipped, incoming_total` |
| `NodeCancelled` | N35–N41/N63/N66 | N | `prior_status, terminal_run_status, reason_code` |
| `MapFailedFast` | N42 | N | `failed_child_id, child_failure_kind` |
| `MapContractFailed` | N43/N65 | N | `failure_kind, failed_child_id?` |
| `MapRetriesExhausted` | N44 | N | `failed_child_id, attempt_id, max_attempts` |
| `MapBudgetExhausted` | N45 | N | `failed_child_id, requested, available` |
| `NodeCorruptStorage` | N47–N57 | N | `bad_artifact_ref_id, bad_digest, error_class, corrupt_proof_fingerprint, prior_status` |
| `AttemptSucceeded` | A02 | A | `actual_cost_units, output_digest, artifact_digests[]` |
| `AttemptRetryableFailed` | A03 | A | `actual_cost_units, error_code, diagnostics_digest?` |
| `AttemptPermanentFailed` | A04 | A | `actual_cost_units, error_code, diagnostics_digest?` |
| `AttemptContractFailed` | A05 | A | `charged_cost_units, failure_kind, diagnostics_digest?` |
| `AttemptTimedOut` | A06 | A | `deadline_at, database_now, charged_cost_units` |
| `AttemptOutcomeUnknown` | A07 | A | `dead_engine_generation, recovery_generation, charged_cost_units` |
| `AttemptCancelled` | A08 | A | `reason_code, charged_cost_units` |
| `AttemptMarkedStale` | A09 | A | `active_attempt_id?, submitted_outcome_category, submitted_payload_digest, charged_cost_units` |
| `StaleCompletionObserved` | A10–A17 | A | `immutable_terminal_state, submitted_outcome_category, submitted_payload_digest, database_arrival_at`; at most one per attempt |
| `BudgetSettled` | A02–A09 | A | `ledger_seq, reservation_amount, consumed_amount, released_amount, reason, available_after` |
| `BudgetReservationRefused` | N27/N60 | N | `requested, consumed, reserved, limit, available, permanently_infeasible=true` |
| `ApprovalGateCreated` | G01 | G | `request_digest, expires_at, on_expiry, authorization_policy_digest` |
| `ApprovalGateApproved` | G02 | G | `principal, decision_payload_digest?, approval_output_digest, decision_fingerprint` |
| `ApprovalGateRejected` | G03 | G | `principal, decision_payload_digest?, decision_fingerprint` |
| `ApprovalGateExpiredApproved` | G04 | G | `expires_at, database_now, approval_output_digest` |
| `ApprovalGateExpiredRejected` | G05 | G | `expires_at, database_now` |
| `ApprovalGateCancelled` | G06 | G | `terminal_run_status, reason_code` |
| `EdgeSatisfied` | E01 | N for source; target in payload | `edge_id, from_node_id, to_node_id` |
| `EdgeSkipped` | E02 | N for source; target in payload | `edge_id, from_node_id, to_node_id, cause` |

The catalogue is closed. Adding an event type, payload field, outcome category, or transition requires a new contract version. Nullable fields are marked `?`; all others are mandatory. `idempotency_key_digest` is emitted instead of the raw key to reduce accidental correlation leakage outside the scope.

## 16. Tenant scope confinement

### 16.1 Key and query rules

Every logical primary key, foreign key, unique constraint, and index begins with `(tenant_id, namespace)`. All joins include both fields on both sides. No query relies on globally unique UUIDs. Every store method takes one `ExecutionScope`; nested IDs cannot override it.

This applies to:

- definitions, revisions/root schema refs, action pins, runs, ActionInvocations, nodes, edges, attempts, gates, ArtifactRefs, ObjectRecords, CommandReceipts, events/batches, ledger entries, migration-owned application data, and engine claims;
- point reads, mutations, approvals, cancellations, recovery scans, timeout/retry scans, compatibility scans, event pagination, all list filters, uniqueness checks, and idempotency checks;
- object put/get/verify paths and `VerifiedObjectRef`;
- event cursors and list-page tokens, which are authenticated/opaque encodings bound to scope;
- deterministic IDs: a hash collision or identical user ID in another scope remains a different database key.

The store copies scope predicates into every subquery. “Find attempt by attempt ID, then infer run scope” is forbidden. A foreign key such as an attempt-to-node relation includes `(tenant_id, namespace, run_id, node_instance_id)`.

The host authenticates principals, authorizes use of an `ExecutionScope`, and asks the crate to mint an `AuthenticatedPrincipal` capability bound to that specific scope. The crate does not authenticate users. Store-side validation that every supplied principal capability matches the command's `ExecutionScope` complements, and does not replace, host-side authentication; a capability minted for scope B is structurally invalid in scope A.

### 16.2 Two-scope adversarial conformance suite

Every adapter must pass the same black-box suite with scopes A and B populated using deliberately identical `definition_id`, `revision_hash`, `run_id`, `node_id`, `attempt_id`, `gate_id`, object digest, idempotency token, and event sequence values.

The suite must prove:

1. Point reads in A return only A’s entity and never B’s fields, status, existence, or timing.
2. Lists in A contain only A rows; filters, counts, sort order, cursors, and subsequent pages cannot cross into B.
3. A guessed B run/node/attempt ID supplied with scope A cannot complete, timeout, recover, suspend, resume, or mark B corrupt.
4. A guessed B gate ID supplied with scope A cannot read its request, approve, reject, expire, or cancel it.
5. A cancellation in A cannot change B run/node/attempt/gate states, signal B tokens, settle B reservations, or append B events.
6. Budget reservation and settlement in A cannot read or mutate B’s limit, consumed, reserved, or ledger sequence.
7. Recovery, retry, deadline, compatibility, and pending-gate scans under A never return B work.
8. Event reads/cursors from A never reveal B payloads or whether a B sequence exists. Reusing an A cursor under B fails `NotFound`/invalid cursor without data.
9. A `VerifiedObjectRef` minted in B is rejected by an A command; equal digest objects use different scope-qualified keys. A cannot read, validate, or infer B object metadata.
10. Engine claims are scope-qualified: A and B may each have one live engine, while two live engines in A fail closed.
11. Idempotency replay is scope-local: identical tokens in A and B do not conflict or deduplicate across scopes.
12. SQL tracing/static query review finds an explicit tenant and namespace predicate in every adapter statement, including conflict/update/delete branches and joins.
13. Derived operational views and every scheduler scan/cursor return only the supplied scope; reusing a cursor in the other scope fails.
14. Equal run/node IDs produce different external action idempotency keys because the versioned `dagger-idem-v1` hash length-prefixes both scope atoms; delimiter-bearing IDs and distinct Map child identities cannot alias.
15. A CompletionCredential, EnginePermit session token, AuthenticatedPrincipal capability, or FailedReadProof from B cannot authorize any A command.
16. Create/cancel CommandReceipts with equal tokens in A/B retain independent fingerprints, outcomes, and batch IDs.

For guessed cross-scope IDs, public results are indistinguishable from absence in the caller’s scope. The suite snapshots every B row, budget total, event count, and object before adversarial A operations and proves bit-for-bit logical non-mutation afterward.

## Frozen design decisions

1. RESOLVED R1: every Choice requires a default. Explicit fail-closed behavior routes that edge to a Fail node; no no-match outcome/transition/event remains.
2. RESOLVED R2: implementation matching uses the exact semantic-compatibility digest, not a build hash. Resume only proves the pinned digest is available and cannot override it; hosts retain compatible implementations while pinned runs exist.
3. RESOLVED R3: every maximal path ends explicitly and exactly one reachable Succeed node exists; multiple Fail nodes remain legal. Final output cannot depend on scheduler order.
4. RESOLVED R4: durable run status stays Running while nodes wait. The read API exposes computed Executing, AwaitingBudget, AwaitingApproval, RetryDelay, WaitingChildren, or Mixed phase with counts/next due time.
5. NEW: external action idempotency keys are a versioned `dagger-idem-v1` hash over the complete length-prefixed scope/run/node tuple plus full Map child identity; create/cancel separately use immutable, scope-bound CommandReceipts with closed replay outcomes.
6. NEW: engine acquisition and attempt completion use distinct store-minted 256-bit capabilities; instance and worker IDs are labels, never credentials.
7. NEW: a blocked run admits only resume, cancellation-class handling, proof-backed corruption, and append-only observation of an already-terminal attempt; every execution command gets the distinct blocked error.
8. NEW: recovery freezes and terminalizes the complete abandoned-attempt set before using the persisted canonical Kahn topological rank and documented tie-break tuple to choose one primary exhaustion outcome.
9. NEW: reservation-only shortage persists BudgetWaiting and retries direct admission; permanent exhaustion uses `declared_max > limit - consumed`.
10. NEW: ledger rows are durable admission/accounting only; action/provider adapters enforce the corresponding spend, token, and request caps.
11. NEW: default run ceilings are 20,000 dynamic nodes, 100,000 attempts, 1,000,000 events, 256 KiB/value, 32 artifacts/attempt, 1 GiB charged object refs, and 30 days.
12. NEW: event capacity reserves a terminal cascade, one observation per attempt, and one integrity override; breach cancels with `RunEventLimitExceeded`.
13. NEW: charged object accounting counts each execution-data ArtifactRef use despite deduplication; revision, literal, and bounded control evidence are excluded.
14. NEW: root/action schemas are durable digest-addressed refs interpreted only by the enumerated deterministic Draft 2020-12 subset.
15. NEW: ActionInvocation snapshots the full action pin and the exact canonical bound-input bytes/digest/derivation delivered to the action.
16. NEW: conservative pointer/assignability rules reject coercion and unproved patterns; literal artifacts pin identity/digest/media type and post-Choice consumers can bind only to dominators.
17. NEW: approval authorization accepts immutable principal/role allowlists, checked before the first-valid-decision CAS; successful approval emits the exact fixed ApprovalResult envelope and its digest participates in human-decision replay identity.
18. NEW: corruption requires an opaque failed-read proof, while object publication is atomic if-absent/no-replace and never repairs an existing digest path.
19. NEW: scheduler scans use deterministic scope-bound cutoff/keyset cursors; AwaitingBudget is distinct from Executing, and a zero-category Running operational view is a control-plane error.
20. NEW: event batches expose run-lifetime-unique batch ID/index/count, deterministic cascade order, and complete-batch pagination; terminal late completion is credential-authenticated and recorded at most once.
21. RETAINED: heartbeat is 5 seconds, expiry 20 seconds, database time is authoritative and operationally non-regressing/advancing, and a lost acquisition token recovers only through expiry; regression fails closed and suspends the wall-time unlock bound.
22. RETAINED: CorruptStorage is the sole terminal integrity override; Map remains bounded fail-fast Action fan-out; retries use no-jitter persisted backoff and conservatively charge unknown/timeout/stale attempts.
23. RESOLVED REVIEW FIX 1 (sections 1.1, 1.10, and 12.1): same-digest candidate publication with different size or bytes returns `ArtifactMetadataConflict`; `FailedReadProof` is reserved for failed reads of already-committed refs with closed classes `Missing` and `DigestInvalid`.
24. RESOLVED REVIEW FIX 2 (sections 1.10 and 5.3): `expire_approval` is included in the exhaustive ArtifactRef registering-command list because approved expiry commits its output ref.
25. RESOLVED REVIEW FIX 3 (sections 1.7, 5.3, and 7.1): the raw completion credential is returned exactly once by `claim_node_attempt` and delivered in `ActionContext`, but is never persisted durably, logged, or included in any event payload.
26. RESOLVED REVIEW FIX 4 (sections 1.1, 3.5, 5.3, 16.1, and 16.2): `AuthenticatedPrincipal` is bound to one `ExecutionScope`; `decide_approval` rejects a cross-scope capability before authorization-policy evaluation, complementing host authentication.
27. RESOLVED REVIEW FIX 5 (sections 3.4 and 15.1): state-transition events precede observation events for the same subject within a batch, including A06 before A14.
28. RESOLVED REVIEW FIX 6 (section 5.2): the first `release_engine_claim` expires the matching live claim; replay with the same expired token succeeds without a live-permit check, while a different session token can never release the claim.
29. FINAL FREEZE FIX: diagnostics use a closed envelope and mandatory 65,536-byte cap; malformed/oversized diagnostics are structured no-write command rejections, while semantic secret hygiene remains explicitly host-owned.
30. FINAL FREEZE FIX: an exact `suspend_incompatible` replay is detected from its persisted versioned request fingerprint before the BlockedIncompatible command fence; non-replays remain fenced.

Self-check result: all rev-2 P0/P1 decisions and the final freeze repairs are resolved with no unresolved placeholders. The document has 114 unique transition rows; every row names actor/command, CAS precondition, resulting event, and side effects. Crash classes cover all 114 IDs, including receipts, ActionInvocation, BudgetWaiting, topological-rank bulk recovery, event-cap cancellation, failed-read proof, object orphan, mid-Map, stale completion, ApprovalResult validation, and pending approval. Every transition ID is cross-referenced by the atomic command API and every primary event is present in the closed catalogue; budget side events are the only catalogue-only events. Event batches are unique for the complete scoped run lifetime. The embedded definition schema parses as Draft 2020-12 JSON, requires Choice default/approval authorization, and the mandatory semantic phase enforces acyclicity, exactly one Succeed, bounded Maps, dominance, durable schema resolution, and the supported schema subset. No schema field admits error edges, cycles accepted at publication, unbounded fan-out, arbitrary Choice code, compatibility override, implicit merge/coercion, or behavior absent from the transition model.
