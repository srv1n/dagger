# dagger-workflow-core — Build Plan (rev 2)

Status: W0 complete and frozen by the final document-only pass
(2026-07-28). W1 and W2 may start in parallel; W3 remains gated until both
are integrated and their definition/action/store-boundary outputs are
reconciled against the frozen contract.

## Product framing

Two automation modes exist in the host product (RZN):

1. Bounded workflow automation — the workflow definition owns control flow;
   LLM calls execute inside nodes but cannot invent topology. Deterministic
   control flow containing probabilistic nodes.
2. Agentic harness execution — the model owns the flow. Out of scope here.

This crate implements mode 1 only. Hosts provide actions, scheduling,
model calls, credentials, and artifact services.

## Locked decisions

| Decision | Choice | Rationale |
|---|---|---|
| Durability tier | Tier 2: single-process durable, WITH attempt-level fencing | Every transition persisted; restart resumes frontier without rerunning completed nodes; approval gates survive deploys. Distributed worker leases are deferred, but late-attempt rejection is NOT: every completion CASes against `NodeRun.active_attempt_id` (see Attempt fencing). Opening a second scheduler against the same control plane fails closed. |
| Budgets | Atomic reservation in engine, integer cost units | Reserve declared maximum before invocation, settle after, charge every attempt, charge full reservation on crash-unknown. `u64 cost_units`, never floating point. Engine never knows about tokens or models. |
| Tenancy | Store-enforced scope confinement | Host authenticates/authorizes; the store guarantees operations under scope A cannot address scope B. Composite keys and indexes include scope. Every store adapter runs the two-scope adversarial conformance suite. The crate is not an auth system. |
| Storage split | SQLite control plane + content-addressed payloads, object-first commit order | SQLite holds only small rows (statuses, counters, gates, revision pins, digests, budget ledger). Bulky data stored content-addressed outside the DB. Object bytes are durably written and digest-verified BEFORE the SQLite transaction that references them (see Two-store protocol). |
| Definition authoring | LLM-generated, human-editable | Published JSON Schema from day one. Strict deserialization (reject unknown fields). Structured, LLM-correctable validation errors. Boring, regular format — no DSL. |
| Revisions | Pin executable meaning, not just YAML | Revision hash is over a canonical normalized JSON representation with explicit `definition_format_version` — never raw YAML bytes. Each action reference pins name, contract_version, input/output schema digests, and a compatible-implementation requirement (see Revision completeness). In-flight runs finish on their pinned revision; no live-run migration. |
| Legacy code | Untouched | dag_flow / pubsub / work_queue / taskagent stay as-is. Deletion is a separate decision after the new crate proves out. |
| Table namespace | `dagger_workflow_*` | `dagger_*` is already occupied by task-core; the two must be co-embeddable in one host database. |
| Scale posture | Traits worded for ambition, v0.1 built for today | Async store trait, no single-process assumptions in signatures; attempt rows carry worker-id from day one. Distributed leases, Postgres, throughput work deferred (W12). |

## Required contracts (resolved by frozen W0)

These are decisions, not options. The frozen W0 contract turns each into
precise spec text, state-transition entries, and store command signatures.

### Attempt fencing (P0)

`NodeRun.active_attempt_id` names the only attempt allowed to complete.

```text
NodeRun.active_attempt_id = B

complete(A): CAS active_attempt_id == A → rejected, recorded as stale event
complete(B): CAS active_attempt_id == B → accepted
```

A timed-out attempt that ignores cooperative cancellation and later returns
success must never overwrite a newer attempt's state. Stale completions are
recorded (event + attempt row terminal state `Stale`), never applied.
Idempotency keys protect external side effects; fencing protects internal
state — both are required.

Every A01 attempt receives a distinct store-minted `CompletionCredential`.
Only its digest is persisted; possession of the raw per-attempt capability,
not scheduler generation or worker ID, authorizes `complete_attempt`.

Single-process invariant: engine startup takes an exclusive engine-instance
claim in the control plane (heartbeat/lease row with generation counter);
a second scheduler opening the same control plane fails closed with a
distinct error. The contract fixes claim recovery: heartbeat interval and
expiry, takeover of an expired claim, generation increment on takeover, and
the clock source (database clock, not process clocks). A crashed engine
cannot leave the store permanently locked under the explicit non-regressing,
advancing database-clock assumption; regression fails closed and weakens the
unlock bound until DB-now reaches the persisted expiry.

Late completion recording: NodeAttempt terminal states are immutable. If an
attempt is already terminal (e.g. `TimedOut`) when its late result arrives,
the store records a `StaleCompletionObserved` event referencing the attempt;
it does not rewrite the terminal attempt to `Stale`. `Stale` applies only to
attempts still nominally live whose completion loses the CAS.

### Tenant confinement (P0)

Every store key and query is scope-qualified. The store conformance suite
includes a two-scope adversarial test: scope A cannot read, list, mutate,
approve, or cancel anything under scope B, by ID guessing or by listing.
This is cheap insurance against one forgotten SQL predicate.

### Revision completeness (P0)

A `WorkflowRevision` pins, per action reference:

```text
name
contract_version
input_schema_digest
output_schema_digest
compatible_implementation_requirement
```

At run start AND at every resume, the registry is checked against these
pins. If the exact compatible implementation is absent, the run transitions
to `BlockedIncompatible` — it never silently binds newer behavior. This
matters concretely: `search:v1` must not resolve to newly deployed
different behavior after a week-long approval pause.

`BlockedIncompatible` is a recoverable suspended state, not terminal. The
contract defines who triggers the recompatibility check (host API call and
engine-startup recheck of all blocked runs) and the transition that resumes
the run when a compatible implementation reappears, plus the explicit host
operation to cancel a blocked run instead.

Revision hash = digest of canonical normalized JSON (sorted keys, explicit
`definition_format_version`), not raw YAML bytes.

### Dataflow semantics (P0)

The contract defines exactly how action inputs are assembled. Binding sources:
literal constants, run input, upstream node output (JSON pointer into a
named upstream's output), artifact refs. Bindings are explicit per input
field; there is no implicit merge, no scope-chain lookup, no type coercion.
A binding that references a skipped or failed upstream is a validation
error where statically detectable and a defined runtime failure otherwise.

### Choice and reconvergence (P0)

- Choice evaluates once, against a pinned input digest; the evaluation and
  selected edge are persisted.
- Exactly one outgoing edge activates. All other outgoing paths become
  inactive.
- Nodes with no active incoming path transition to `Skipped`.
- Skipped/inactive paths do not block reconvergence: a reconverged node is
  ready when every incoming edge is either satisfied (upstream terminal
  success) or skipped, and at least one is satisfied.
- A default branch is REQUIRED (ruling 2026-07-28): fail-closed must be
  explicit — a definition wanting no-match-to-fail routes its default to a
  Fail node. There is no no-match runtime failure path.
- Exactly one reachable Succeed node per definition (multiple Fail nodes
  allowed): concurrent Succeed nodes would make the final artifact
  scheduler-order dependent.

### Map identity and contract (P0)

Child NodeRun ID:

```text
hash(workflow_run_id, map_node_instance_id, item_index, item_digest)
```

IDs are stable across reconstruction of the SAME run and differ across
separate runs. (The rev-1 formula omitting run ID was wrong.)

The contract fixes: zero-item result (empty aggregate, parent succeeds);
duplicate-item handling; ordered aggregation of child outputs; per-map max
concurrency; parent completion condition; child failure policy (fail-fast
vs tolerance threshold); cancellation behavior; budget reservation for
children; idempotent expansion (re-expansion after crash converges to the
same child set).

### Hard budgets (P0)

```text
available = limit - consumed - reserved
```

1. Atomically reserve the action's declared maximum before invocation.
2. Admission refusal semantics (ruling 2026-07-28): insufficiency caused
   only by active reservations is TEMPORARY — the node waits for
   settlement. Terminal BudgetExhausted only when the declared max cannot
   fit even after all outstanding reservations settle
   (declared_max > limit - consumed). The ledger is durable accounting;
   action/provider adapters enforce actual spend/token limits.
3. Settle actual cost and release unused reservation after completion.
4. Every started attempt is charged independently, including retries.
5. Crash with unknown cost: charge the full reservation.

Reservation prevents N concurrent actions from all observing sufficient
balance and collectively overspending. Reporting alone cannot.

### Two-store commit protocol (P0)

1. Write payload to temporary object storage; flush; publish with an atomic
   no-replace primitive; sync the directory; verify final bytes and digest.
   An existing mismatched digest path returns `ArtifactMetadataConflict` and
   is never overwritten or repaired.
2. Commit ONE SQLite transaction containing: attempt outcome, node status,
   output digest/ref, budget settlement, frontier changes, ordered event.

A crash before the SQLite commit may orphan an object — acceptable,
GC-able later (W12). A committed row must never reference an object whose
durable put was incomplete. A referenced object found missing or
digest-invalid at read time transitions the run to `CorruptStorage`; it
never silently reruns the action, and the transition requires an opaque
object-store-minted `FailedReadProof`.

Consequence for the store trait: it exposes atomic domain commands
(`complete_attempt`, `record_choice`, `expand_map`, `decide_approval`, …),
not low-level CRUD the engine could accidentally split across transactions.

### Retry accounting (P1)

Every STARTED attempt consumes the retry ceiling — including timeouts and
crash-unknown attempts (fixing task-core's counter-bypass defect).
`next_eligible_at` is persisted; backoff is honored across restart. Tested
with a virtual clock.

### Approval races (P1)

First valid decision wins, across approve/reject, expiry, and run
cancellation — whichever commits first in the control plane is the
decision; later arrivals fail closed with a distinct error. `on_expiry` is
part of the gate definition (expire-as-reject default). Duplicate identical
decision is an idempotent no-op. Host decisions carry an
`AuthenticatedPrincipal` capability bound to the exact ExecutionScope and
must satisfy the gate's immutable principal/role authorization policy before
CAS. Approval emits the fixed engine-owned `ApprovalResult` envelope; its
output digest is part of the decision fingerprint.

### Event ordering (P1)

Per-run monotonic `event_seq`, allocated inside the same state-transition
transaction that produces the event. Every transaction emits one complete
batch with run-lifetime-unique `batch_id` and zero-based
`batch_index`/identical `batch_count`. Consumers get total order per run and
can reconstruct atomic command boundaries.

### Retry exhaustion (P1) — RESOLVED for v0.1

Exhaustion of a node's retry ceiling fails the run. Error edges are
deferred entirely to W12; the v0.1 definition schema does not admit them.
Partial continuation is not permitted.

### Deterministic bulk recovery (P1)

Publication assigns definition-node `topological_rank` with Kahn's algorithm
and lexical node-ID tie-breaking. Every NodeRun persists that rank at
creation; Map children inherit their parent's rank. Recovery terminalizes
the complete abandoned-attempt set before selecting a primary exhaustion by
persisted `(topological_rank, map_item_index_or_minus_one, node_instance_id,
attempt_number, attempt_id)`. Row or hash-map iteration order is never
meaning.

### Persistence-safe formats (P1)

Conformance is mechanical, not a fake "detect all secrets" promise:
definitions, event payloads, diagnostics, and domain errors have closed
field vocabularies and exact byte/count limits; unknown fields are rejected;
credential capabilities have no persistent/event field. The host remains
responsible for semantic secret hygiene in permitted constants and text.
Diagnostics use the fixed envelope and mandatory 65,536-byte canonical cap;
malformed or oversized diagnostics are structured no-write rejections.

### Closed outcome vocabulary (P1)

The contract enumerates the complete closed set of terminal/suspended outcomes — not
just categories. At minimum: `BudgetExhausted`, `BlockedIncompatible`,
`Cancelled`, `RetriesExhausted`, `CorruptStorage`, `UnknownOutcome`
(crash-unknown attempt), stale completion (`Stale` attempt state +
`StaleCompletionObserved` event), and contract-validation failures. Every
member gets a state-table row and an event type; no open-ended "other".

### Idempotency key scope

Run-scoped, not attempt-scoped, and SCOPE-BOUND (ruling 2026-07-28): the
external key is `dwf-idem-v1:` plus SHA-256 under the explicit
`dagger-idem-v1` domain over a complete `u64_be(length) || bytes` tuple of
tenant atom, namespace atom, run ID, and node-instance ID. Map children append
the child tag, parent ID, binary item index, and item digest, each
length-prefixed. It is shared across all retries of that node; delimiters
cannot alias. `create_run` and `cancel_run` separately persist scope-bound
`CommandReceipt` fingerprints and closed outcomes so exact token replays
return the committed result.

### Run-level immutable limits (ruling 2026-07-28)

Every run pins at creation: max total dynamic node instances, max total
attempts, max total events, max inline JSON bytes per value, max artifacts
per attempt, max aggregate object bytes, max run lifetime. Per-entity caps
alone compose to ~1M attempts for a zero-cost 10,000-item Map; these
seven immutable `RunLimits` ceilings are the guard. Temporary reservation
pressure is `BudgetWaiting`; `BudgetExhausted` is permanent only when
`declared_max > budget_limit - budget_consumed`. A Running run with only
BudgetWaiting nodes derives operational phase `AwaitingBudget`.

## Architecture verdict (Codex deep-read, 2026-07-28)

Recommendation (b): new crate `dagger-workflow-core`; task-core untouched.
The deep-read's "~82% new code" figure is an estimate, not evidence — the
supported claim is that the engine core (revisions, attempt ledger, frontier
reconstruction, durable approvals, budgets, object refs) does not exist in
the repo and the reusable material is peripheral. Full report in Codex
session 019fa749-e9a4-7650-b1d9-fbb583551dea.

Why not extend task-core: it is a dynamic task queue, not a workflow runtime.
No definition/revision entity (sqlite_storage.rs:79-138); runtime-mutable
topology — every put deletes/recreates dependency edges
(sqlite_storage.rs:1041-1077); attempts collapsed to a counter, and recovery
retries do not increment it, so crashes can bypass the retry ceiling
(recovery.rs:129-157); no approval model; payloads inline in SQLite.

Why not seed from src/storage: flow_runs/node_runs naming is right but the
implementation is skeletal — no revisions, no attempt rows, no transition or
recovery API after create_node_run (src/storage/sqlite_storage.rs:131-271);
artifacts are not actually content-addressed (no digest computed,
sqlite_storage.rs:273-294).

Confirmed defects in legacy code (do not carry forward):
- HITL approvals are process-memory oneshot senders; nothing persisted
  (src/work_queue/hitl.rs:74-128; branch pause state src/dag_flow/branch.rs:68-97).
- dag_flow resume re-executes with a fresh empty executed set — persisted
  state never reconstructs the frontier (dag_flow.rs:1547-1621, 3209-3233).
- task-core embedded migration drops all tables in one committed transaction,
  restores in another — crash between loses all data
  (task-core/src/sqlite_storage.rs:211-310).
- work_queue pipeline compiler compiles into the legacy Graph model and
  inherits full-DAG resume; throwaway (pipeline.rs:81-227).

## Steal-list (adapt, don't import blindly)

| Pattern | Source |
|---|---|
| Caller-provided Pool<Sqlite> constructor + pool accessor | task-core/src/sqlite_storage.rs:50-74 |
| Namespaced table convention (new namespace: `dagger_workflow_*`) | task-core/src/sqlite_storage.rs:76-200 |
| CAS status update + event insert in one transaction | task-core/src/sqlite_storage.rs:1381-1442 |
| Current-output + history + event transactional pattern (blobs → digests) | task-core/src/sqlite_storage.rs:1161-1237 |
| Storage-trait boundary shape (rewritten as atomic domain commands) | task-core/src/storage.rs:8-85 |
| Name-addressed action registry with schema hooks | src/coord/action.rs:121-149, src/coord/registry.rs:11-58 |
| Cycle/dependency/action validation | src/dag_flow/dag_flow.rs:3674-3710 |
| Retry-policy vocabulary (unify the two existing models) | src/dag_flow/dag_flow.rs:59-82, src/dag_flow/dag_builder.rs:54-96 |
| Deterministic hash-suffixed fan-out IDs (extended with run ID per Map contract) | src/work_queue/batch.rs:239-302 |
| FlowRun/NodeRun naming; outbox as a design idea only (schema declaration exists, no implemented writer) | src/storage/schema.sql:5-58 |
| ExecSpec/ExecResult/ExecHost as a future optional built-in action | src/work_queue/exec.rs:21-180 (deferred, W12) |

Do NOT copy: task-core's destructive embedded migration; use a namespaced
migration ledger, each version applied atomically.

## Reference points in mature systems (borrow semantics, not machinery)

- Temporal/Restate: ordered durable history; persist results before
  advancing; durable timers rather than reconstructed sleeps.
- DBOS: completed steps never rerun; unfinished steps are at-least-once —
  exactly the distinction W8's fixtures prove.
- AWS Step Functions: explicit Choice defaults; Map aggregation and failure
  thresholds; retry/catch vocabulary.
- Trigger.dev: explicit idempotency scope (run-scoped keys).

Do not copy: arbitrary-code replay, distributed consensus, large expression
languages, AWS-compatible surface area. A declarative reducer over persisted
transitions is enough.

## Entity model

WorkflowDefinition → WorkflowRevision (immutable, canonical-JSON hash,
pins action contracts and canonical node ranks) → WorkflowRun (pins revision,
scope, budget ledger, seven RunLimits) → NodeRun (persists topological rank,
active_attempt_id, status incl. Skipped / BlockedIncompatible) → NodeAttempt
(immutable rows; worker-id field; per-attempt CompletionCredential digest;
terminal states incl. Stale) · ActionInvocation (exact canonical bound input
ref/digest/derivation) · ApprovalGate · ArtifactRef/ObjectRecord ·
CommandReceipt · WorkflowEvent (per-run event_seq and atomic batch envelope).
Node kinds v0.1: Action, Map, Choice, Approval, Succeed, Fail. No cyclic
graphs — bounded rounds explicitly unrolled via Choice.

Execution guarantee: at-least-once action invocation with versioned
scope-bound logical-node idempotency keys, per-attempt CompletionCredential
fencing, and exact durable bookkeeping.
Exactly-once external side effects are explicitly NOT guaranteed;
side-effecting actions must use the idempotency key.

## Task breakdown

Sequencing (frozen): W0 is complete. W1 and W2 start in parallel. They then
enter an integration gate that reconciles generated definition types/schema,
action contracts, ActionInvocation, CompletionCredential, idempotency, and
shared errors against the frozen document. W3 starts only after both W1 and
W2 are INTEGRATED; W0 approval alone is insufficient. W7's trait exists from
W3 onward (in-memory impl); W6 ∥ W7 durable impls.

### W0 — Contract document (COMPLETE/FROZEN)
One canonical design doc resolving every "Required contracts" item above:
entity model; complete state-transition table (every state, every legal
transition, every actor); crash matrix (for each transition: crash-before /
crash-after behavior on restart); atomic store command API; action
invocation contract; dataflow binding spec; Choice/reconvergence spec;
Map contract; budget reservation ledger; approval race rules; canonical
revision hashing + action pinning; definition JSON Schema draft; closed
outcome vocabulary; singleton-claim recovery; BlockedIncompatible
lifecycle; late-completion recording.
Status/acceptance: complete and frozen by the final document-only pass.
W1+ cite it; deviations require an explicit contract revision before code
diverges. The freeze confirms every transition has actor/CAS/event, every
crash boundary has recovery, command/schema/catalogue cross-references agree,
and no unresolved P0 decision remains.

### W1 — Crate scaffold + definition model (∥ W2, after frozen W0)
New workspace member `dagger-workflow-core`, importable standalone.
Tagged node enum (Action/Map/Choice/Approval/Succeed/Fail); serde with
deny_unknown_fields; canonical-JSON normalization + revision hashing with
`definition_format_version`; JSON Schema generation; YAML + programmatic
construction; structural validation per W0 (duplicate IDs, missing deps,
cycles, unresolved action schema pins, choice targets + required default,
unbounded map, binding references to nonexistent/skippable upstreams, timeouts, retry
policies, graph size). Structured, LLM-correctable validation errors naming
node, field, and valid alternatives. Apply schema defaults before hashing,
and compute canonical Kahn `topological_rank` with lexical node-ID tie-breaks
for every definition node.
Accept: invalid-definition test table covering every rejection class;
schema file generated and committed; rank test proves lexical tie-break
stability. Canonical-hash fixtures prove whitespace, comments, object-key
order, YAML aliases, and omitted versus expanded schema defaults normalize to
the same hash. Array order remains meaning: no test or claim may normalize
arbitrary array reordering.

### W2 — Action registry + invocation contract (∥ W1)
Name-addressed ActionRegistry with contract_version and input/output schema
digests; compatibility check API (used at run start and resume →
BlockedIncompatible). ActionContext: ExecutionScope, run id, revision hash,
node-instance id, attempt number + attempt id, versioned length-prefixed
scope-bound idempotency key, per-attempt `CompletionCredential`, deadline,
cancellation token, budget handle (declared max). `ActionInvocation` freezes
the exact canonical bound input ref, byte digest, size, and binding-derivation
digest delivered to the action. ActionOutcome is typed JSON output/artifacts
or structured Retryable/Permanent error with the closed DiagnosticsEnvelope.
Implement the mechanical persistence-safe format rules and host semantic
secret-hygiene boundary; do not claim content-based secret detection.
Accept: mock actions receive byte-identical ActionInvocation input; key
fixtures cover scope separation, delimiter ambiguity, retry stability, and
Map child identity; a raw CompletionCredential completes only its own
attempt and is never persisted/logged; malformed/over-65,536-byte diagnostics
are structured no-write rejections; cancellation/deadline work; incompatible
registry yields BlockedIncompatible.

### W3 — In-memory engine (after W1 + W2 integration)
Frontier scheduler with configurable concurrency bound; Action/Choice/
Succeed/Fail; dataflow binding per W0; attempt fencing via
active_attempt_id CAS (stale completion rejected + recorded); retries with
persisted next_eligible_at semantics and virtual-clock tests; every started
attempt consumes the ceiling; timeouts; run cancellation propagating tokens;
budget reserve/settle ledger with refusal on insufficient available;
retry-exhaustion → run failure (error edges deferred to definition support
if a future contract admits them); `BudgetWaiting` for reservation-only
shortage versus permanent `BudgetExhausted`; all seven immutable RunLimits;
`AwaitingBudget` derived phase; scope-bound `CommandReceipt` replay for
create_run/cancel_run; per-run event_seq and run-lifetime-unique event
batches (`batch_id/index/count`). Runs against InMemoryStore AND
InMemoryObjectStore implementing the real traits — the store conformance
suite (including the two-scope adversarial test and stale-fencing test) is
born here and reused by W6/W7.
Accept: concurrency-bound, retry-taxonomy, virtual-clock backoff,
CompletionCredential fencing, cancellation, temporary/permanent budget
admission, AwaitingBudget derivation, each RunLimits guard, create/cancel
receipt replay/conflict, exact `suspend_incompatible` replay returning before
the blocked fence with no write, non-replay fence rejection, event batch
uniqueness/contiguity, and concurrent budget-reservation tests green.

### W4 — Map fan-out
Full W0 Map contract: bounded expansion over JSON-pointer-selected input;
child IDs hash(run_id, map_node_instance_id, item_index, item_digest);
zero-item, duplicate-item, ordered aggregation, per-map concurrency,
failure policy, child budget reservation, idempotent expansion. Expansion
represented through the conformance store; each child inherits its parent's
persisted topological rank, receives its own ActionInvocation and
CompletionCredential per attempt, and derives the section 7.1 scope-bound
external key from complete Map identity. Real durability is proven in W8.
Accept: over-limit refusal; same-run reconstruction yields identical child
IDs; different runs yield different IDs; zero-item and failure-policy tests.
Duplicate items at distinct indices must produce distinct children/keys;
child attempts must respect Map concurrency, BudgetWaiting/permanent
BudgetExhausted, and the run dynamic-node/attempt ceilings.

### W5 — Reference workflow A: bounded legal research
Fixture with deterministic mock actions: question → ≤3 queries → Map search
→ summarize (findings/gaps/needs_second_round) → Choice (with default) →
one conditionally selected unrolled follow-up round → synthesize → validate citations →
report ArtifactRef. Exercises reconvergence: skipped branch must not block
the synthesis node. Capture ActionInvocation input digests and atomic event
batches so the fixture is useful to every adapter.
Accept: both Choice branches covered; skipped-path reconvergence asserted;
green on the in-memory engine; ActionInvocation bytes/digests are stable and
every observed batch has unique batch_id with contiguous index/count.

### W6 — SQLite control-plane adapter (∥ W7)
sqlx; host-pool injection + standalone open; `dagger_workflow_*` tables;
versioned migration ledger, each migration atomic and restartable; atomic
domain commands (single-transaction completion: attempt outcome, node
status, output ref, budget settlement, frontier, event batch with
event_seq/batch_id/index/count); scope-qualified keys and indexes; immutable
NodeAttempt rows with CompletionCredential digests; immutable
ActionInvocation rows; scope-bound create/cancel CommandReceipts; all seven
RunLimits; persisted NodeRun topological ranks;
digest/ref payload columns only; engine-instance claim (second engine fails
closed).
Passes the full W3 conformance suite.
Accept: conformance suite green (incl. two-scope adversarial and
stale/CompletionCredential fencing); migration kill-test;
host-table-survival test; concurrent BudgetWaiting versus BudgetExhausted
test at the SQL layer; create/cancel receipt replay across reopen; database
uniqueness for `(scope,run_id,batch_id)`; deterministic bulk-recovery query
orders by persisted topological rank; second-engine rejection plus
non-regressing-clock fail-closed test.

### W7 — Durable object store (∥ W6)
ObjectStore trait (exists since W3): content-addressed put/get, digest
computed by the store, verified on read. Filesystem impl:
write-temp → flush/fsync → atomic no-replace publication → directory fsync →
reopen/verify, before any SQLite reference commits. Existing mismatched bytes
return `ArtifactMetadataConflict` and are never overwritten. A committed
missing/digest-invalid read mints a scope/digest/store-nonce-bound
`FailedReadProof`; only that capability can drive CorruptStorage.
Accept: digest-verification and concurrent same-digest publication tests;
no-replace conflict preserves original bytes and returns
ArtifactMetadataConflict; commit-order kill between object put and SQLite
commit leaves an orphan and consistent DB; forged/cross-scope
FailedReadProof is rejected while a valid proof applies CorruptStorage;
SQLite row-size assertion.

### W8 — Crash recovery
Startup reconstruction of the run frontier purely from the ledger. Kill
fixtures (fresh store/runtime instance) at adversarial points:
- after node completion: completed nodes never rerun;
- after object put but before SQLite commit: node re-invoked with the SAME
  versioned scope-bound idempotency key; orphan object tolerated;
- crash-unknown attempt: consumes retry ceiling AND full budget reservation;
- stale-attempt completion arriving after restart: authenticated only by its
  CompletionCredential and rejected/observed by fencing;
- timeout → retry-ceiling exhaustion path;
- mid-Map expansion: re-expansion converges to identical child set;
- multi-attempt takeover: terminalize the complete abandoned set, then choose
  the primary exhaustion by persisted topological rank and tie-break tuple;
- crash-after-every-transition sweep driven by the W0 crash matrix.
Accept: all fixtures green; randomized row/iteration order cannot alter the
bulk-recovery outcome or ordered batch, ActionInvocation input digest remains
stable across retry, and late credentials cannot mutate a newer attempt.

### W9 — Durable approvals + reference workflow B
ApprovalGate rows: gate id, run/node ids, request payload digest, status,
expiry + on_expiry, decision, decision payload digest, deciding principal,
timestamp. First-valid-decision-wins across approve/reject vs expiry vs run
cancellation; duplicate identical decision idempotent; conflicting/replayed
decision fails closed. Host decisions require a scope-bound
AuthenticatedPrincipal that satisfies the immutable gate authorization
policy. Approved nodes emit only the exact canonical engine-owned
ApprovalResult; the output digest participates in the decision fingerprint.
Restart while waiting preserves the gate and resumes only the correct
downstream frontier.
Workflow B fixture: 3 mock feeds fetched concurrently (one injected
transient failure proving retry) → normalize/dedupe → Map summarize →
report → durable approval gate → idempotent mock publish → artifact ref.
Accept: approval-survives-restart; duplicate/conflict; approval-vs-expiry
and approval-vs-cancellation races; workflow B green end-to-end including
kill-and-restart at the gate; cross-scope/unauthorized principals cannot
perturb CAS; altered ApprovalResult bytes/digest are rejected and exact
replay emits no second event batch.

### W10 — Budgets + event correlation end-to-end
Budget ledger visible in run rows; reservation-only pressure →
BudgetWaiting/AwaitingBudget while permanent infeasibility →
BudgetExhausted; enforce and expose all seven immutable RunLimits. Event
stream is totally ordered per run via event_seq, grouped into atomic
run-lifetime-unique batches (`batch_id`, `batch_index`, `batch_count`), with
stable run/node/attempt correlation across retries and restart.
Accept: temporary wait resumes after settlement while permanent exhaustion
cannot revive; each RunLimits ceiling has a boundary fixture; ordering,
batch uniqueness/contiguity/complete-page behavior, and correlation hold
over a run containing retries, stale attempts, bulk takeover recovery, and
a restart.

### W11 — Consumer proof + acceptance matrix
Minimal fixture crate importing only dagger-workflow-core; compiles with
zero legacy dagger modules; compiled in both minimal/default-feature and
SQLite-feature configurations; registers actions; runs workflows A and B
through kill-and-restart. Deliverables: acceptance matrix mapping every W0
requirement to a test; dependency footprint report (direct + transitive
counts, feature matrix, compile-time observation); public API summary.
Accept: the matrix has explicit rows for per-attempt CompletionCredential,
versioned scope-bound idempotency derivation, canonical ActionInvocation
bytes/digest, create/cancel CommandReceipt replay, BudgetWaiting versus
BudgetExhausted and AwaitingBudget, all seven RunLimits, FailedReadProof,
atomic no-replace/ArtifactMetadataConflict, AuthenticatedPrincipal gate
policy plus ApprovalResult, event batch ID/index/count lifetime uniqueness,
and deterministic bulk recovery by persisted topological rank. Both store
implementations and both reference workflows pass those rows without
linking a legacy runtime module.

W11 is a mandatory pre-alpha gate. It passes only when every applicable gate
below is green. Gates are defined by the architect ruling adopted as contract
erratum 0.1.1 (`docs/WORKFLOW_CORE_CONTRACT_ERRATUM_0_1_1.md`).

- **W11-A — Integrity propagation.** The proof-bearing store error preserves the
  first proof and the exact committed ref through hydration to the command
  caller. Red fixture: corrupt a hydrated schema and assert the original proof
  arrives, not a re-minted one.
- **W11-B — Host boundary.** A server-shaped endpoint proves corruption is
  committed before response emission, that unavailability causes no transition,
  and that a failed mark is never presented as applied corruption.
- **W11-C — Relationship authority.** Arbitrary same-scope proofs cannot corrupt
  unrelated runs. Red fixture: two runs, one corrupt registered artifact, no
  owner node; the unrelated mark must return InvalidFailedReadProof.
- **W11-D — Local protocol model.** Exhaustive stateful crash simulation over the
  publication protocol, with negative controls. See below.
- **W11-E — Local qualification.** Every locally supported filesystem profile
  passes real abrupt-crash testing against a block device, or is explicitly
  excluded from the support claim.
- **W11-F — Remote conformance.** A non-filesystem adapter passes conditional
  publication, failure classification, retry, and tenant isolation fixtures.
- **W11-G — Deployment adapter.** The concrete server storage adapter passes live
  integration tests against the selected service.
- **W11-H — Server topology.** Multi-tenant load, cross-process engine-claim
  contention, and long-run descriptor/memory stability, per escalation E3.

W11-D requires a stateful crash model implemented with FUSE, syscall
interception, or an equivalent test filesystem. It must represent file data
durability separately from directory-entry durability: fsyncing a temporary file
makes inode contents durable, and only fsyncing the containing directory
establishes the link entry. Crashes must be injectable after every state-mutating
syscall, immediately before each barrier, immediately after each successful
barrier, and immediately before the public method returns. The model must not
simply discard every unsynced change -- real filesystems persist some unbarriered
changes and lose others, so the harness explores permitted combinations where
unsynced entries disappear, survive, or survive in one directory while another
does not. Every restart constructs a fresh store instance in a fresh
process-equivalent state.

W11-D must include negative controls. For each critical barrier, a mutated build
omits the fsync, moves it after the successful return, or makes it a no-op, and
at least one crash fixture must fail for each mutation. Without this the harness
only replays the implementation's own assumptions.

A process SIGKILL is not a crash-consistency test: the kernel page cache and
filesystem keep running, so unsynced data can still be written after the process
dies. A userspace model without W11-E supports the claim "protocol-model
verified" and may not support "crash-durable on local filesystem X".

### W12 — Deferred ledger (explicitly out of v0.1)
- Distributed worker leases (attempt fencing is IN v0.1; multi-process
  scheduling is not).
- Orphan-object garbage collection.
- Postgres store adapter.
- Cyclic graphs / native bounded-loop node.
- Error edges (fail-the-run is the only v0.1 exhaustion behavior).
- Cron/event triggers (host-owned; crate stays trigger-agnostic).
- ExecSpec port as an optional built-in action.
- Choice expression language beyond JSON-pointer + equality/enum.
- Legacy module deletion decision.

## Dependency posture
Target: serde, serde_json, thiserror, tokio (rt features only),
sqlx (sqlite, behind a feature), sha2, schemars (or hand-written schema),
plus one YAML parser chosen deliberately: do NOT default to serde_yaml —
its upstream repository is archived and unmaintained. W1 must record a
short comparison of maintained alternatives (e.g. serde_yaml_ng,
serde-yml, saphyr-based options) covering maintenance status and security
posture before choosing.
No LLM, HTTP, cloud, sandbox, CLI, or observability SDK dependencies.
Each substantial addition beyond this list requires a recorded justification.
