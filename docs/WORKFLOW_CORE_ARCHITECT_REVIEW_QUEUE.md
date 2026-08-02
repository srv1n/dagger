# Workflow Core - decisions taken under implementer judgement, pending architect review

This document exists so that work could continue without blocking on an architect
round trip, while leaving an exact record of every judgement call that was made
on the architect's behalf. Each entry states what was decided, the authority
relied on, what was rejected, and precisely what would have to change if the
ruling goes the other way.

Nothing here is presented as settled. Where an entry changes observable
behaviour, it says so, and it names the revert cost.

Read with `WORKFLOW_CORE_ESCALATIONS.md`, which holds the open questions that
were NOT decided locally.

---

## R1. E7 - the per-value ceiling now applies to an Action's own output commit

**Status:** implemented 2026-08-02. Behaviour change. Ruling requested.

### The question

Contract section 1.4 applies `max_inline_json_bytes_per_value` "before binding,
invocation, event-inline value, or **output commit**". Four sites enforced it:
`create_run` (run input), `claim_node_attempt` (bound input), `complete_map`
(aggregate), and `resolve_terminal_node` (Succeed output, added 2026-08-01).

`complete_attempt` enforced it on nothing. An Action's own committed result was
therefore unbounded by the per-value ceiling.

### Why it mattered in practice

The value was never unbounded in aggregate: `max_aggregate_object_bytes_per_run`
still charged it. So this was not a resource-exhaustion hole. It was a failure
ATTRIBUTION defect, demonstrated by `examples/guardrails.rs`.

Under a 2048-byte ceiling, a node committing a 4130-byte result succeeded. The
failure then surfaced in one of two places:

- at a downstream node that bound the value, which reported
  `InlineJsonLimitExceeded` against a node that had done nothing wrong; or
- at the Succeed node, one or more steps removed from the producer.

And in the case where nothing ever consumed the oversized value, no failure was
reported at all. The run carried a value the contract says is illegal, silently,
until something happened to touch it.

For an engine whose purpose is to tell an operator which node misbehaved,
pointing at the wrong node is a defect in the product, not a cosmetic issue.

### What was decided

Option A: enforce the ceiling on the Action's committed output at
`complete_attempt`, folded into the existing A05/N21/R08 branch.

### The authority relied on

Two things, in order.

1. **The plain reading of section 1.4.** An Action committing its output is an
   output commit. No reading of that sentence excludes it.
2. **Precedent inside the same command.** `complete_attempt` ALREADY enforces
   `max_artifacts_per_attempt` and `max_aggregate_object_bytes_per_run` at
   exactly this point, through exactly this transition. Transition N21
   (`Running` -> `ContractFailed`, `NodeContractFailed`, with A05 and R08) is
   defined for "output/schema/value/artifact or cost protocol violation" - the
   word "value" is already in the transition's own trigger list.

This is therefore a gap-fill in an established pattern, not a new behaviour. No
new transition, result variant, or error kind was introduced.

### What was rejected, and why it is a real alternative

Option B: leave the code and correct section 1.4's wording, on the theory that
intermediate node outputs are deliberately governed only by the aggregate
ceiling, and the per-value ceiling guards values crossing the engine's boundary
(in at `create_run`, into an action at `claim_node_attempt`, out at
`resolve_terminal_node`).

That reading is coherent. Under it the four original sites are exactly the
boundary crossings, and an intermediate value is an internal detail bounded only
by total run storage. If the architect intended that, then the added check is
wrong, and the correct fix is an erratum clause narrowing section 1.4.

Option B was not chosen because it accepts permanent failure misattribution, and
because the "boundary" theory does not explain why `complete_map` enforces the
ceiling on a Map aggregate, which is an intermediate value by the same logic.

### Cost of reversing

Low, and deliberately kept low. The change is one check per store plus its
conformance fixtures. Reverting means deleting the check, deleting the fixtures,
lowering `CASE_COUNT`, and adding the erratum clause. No type, transition, or
public signature changed, so nothing downstream depends on it.

### The one thing the architect should weigh

This makes the engine STRICTER. A workflow that ran yesterday can fail today, at
the producing node, with `InlineJsonLimitExceeded`.

That was judged acceptable because the crate is pre-alpha with no production
consumers, and because a value the contract calls illegal was previously being
committed. Taking the break now is cheaper than taking it after a host depends
on the loose behaviour. If there is an external consumer already relying on
oversized intermediate values, this decision should be revisited before alpha.

---

## R2. Judgement calls made while repairing the object-integrity and
## contract-failure defects

These were smaller, but each is a place where the contract admitted more than
one implementation and one was chosen.

### R2.1 `ContractValidationApplied` kept as an error rather than becoming a result

The variant is documented "Runtime validation atomically produced
ContractFailed" and was returned as `Err` from inside the transaction closure,
where the guard discarded all staged state. Nothing was applied, at roughly
twenty sites across both stores. N46, N64, N67 and N21 were effectively
unimplemented.

The fix could have converted these to `Ok` with a result enum. It did not.
Section 5.3 names `ContractValidationApplied` and `RunLimitApplied` as the
command-specific ERRORS of `expand_map`, `decide_approval`, `expire_approval`,
`resolve_terminal_node` and `fail_contract`. The variant name is the contract, so
the transaction guards were changed to commit staged state for those two
variants, and every returning site now installs the transition first.

Where section 5.3 names a RESULT instead, the result was used:
`claim_node_attempt` returns `ClaimNodeAttemptResult::RunLimitApplied`, and
`complete_attempt` folds its ceilings into `CompleteAttemptResult::TerminalRun`.
The shapes therefore differ by command, deliberately, following 5.3 rather than
imposing uniformity.

**For review:** `complete_map` and `resolve_terminal_node` apply correctly but
return `Ok` where section 5.3 documents an error. That is a spec-versus-code
divergence with correct durable state. It was left alone rather than churned.
The architect should say which side moves.

### R2.2 Four unreachable duplicate ceiling checks made fail-closed

With the guards now committing on applied variants, a duplicate check returning
an applied error WITHOUT installing the transition would commit a half-written
claim. The four provably-unreachable duplicates were changed to
`StoreError::TransactionFailed` with a comment naming why. If the reachability
analysis is wrong, they now error loudly instead of corrupting state.

### R2.3 One `w11_integrity` assertion relaxed

`hydration_corruption_delivers_the_first_proof_and_the_exact_ref` asserted the
corruption proof names `run_input_schema_ref`. Once hydration correctly covered
BOTH root schemas, and because that fixture uses identical bytes for both, there
are two equally valid typed uses of the armed digest and which one hydration
reaches first is incidental ordering.

The assertion was relaxed to "one of the two pinned root schema uses". Typed-use
discrimination is owned by the sibling case
`hydration_names_the_typed_use_whose_read_actually_failed`, which corrupts the
first and second reads separately and still holds exactly. The first-proof and
no-run-created guarantees in the original case were left strict.

This is the only assertion weakened in two days of work, and it is recorded here
because relaxing assertions is normally how coverage silently dies.

---

## R3. Standing note on what the conformance harness can and cannot prove

Recorded here because it changed how the work was done, and the architect should
know the limit when reading any parity claim.

`src/conformance.rs` asserts that the in-memory oracle and the SQLite adapter
behave identically. Over the last two days, three defects were invisible to it:

- **E5** - no fixture exercised `publish_revision` on the oracle at all.
- **The Succeed-output hole** - both stores were wrong IDENTICALLY, so parity
  held perfectly while both violated N16.
- **The `ContractValidationApplied` defect** - same shape, roughly twenty sites,
  both stores.

`sqlite/reducer.rs` is a near-verbatim copy of `memory/mod.rs` for most commands.
That makes agreement between them close to worthless as correctness evidence: the
harness proves the copy is faithful, not that the original is right.

Parity remains valuable and did catch a real regression - when the contract-
failure fix landed in the reducer but not in the SQLite persistence path,
conformance broke immediately and correctly. The rule that emerged is:

**Parity is evidence about agreement. Only the contract is evidence about
correctness.**

Every fixture added in this period is required to name the source mutation that
breaks it, and wherever practical that mutation was applied, observed red, and
reverted. Entries in this queue were reviewed against the contract text directly,
not against the other store.
