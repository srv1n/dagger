# dagger-workflow-core — Status Review for Architecture Assessment

Date: 2026-07-28, corrected 2026-07-29 after architect ruling.
Branch: hardening-pass, HEAD 43d70b3.
Purpose: a candid account of everything built since the W0 contract was
approved, for external architectural review. This document states what
was done, what the review process caught, what has NOT been
independently verified, and the decisions now waiting on the architect.

## 1. Where we are against the plan

Status by task. Note: W6 contains an unapproved architectural
deviation (see its row and section 5); the honest current label for
the crate is an in-memory semantic alpha plus a nonconforming durable
SQLite prototype.

| Task | Status | Evidence |
|---|---|---|
| W0 contract | Frozen (f7d73fa) after one repair pass, delta adversarial review, micro-repair, and final freeze pass | WORKFLOW_CORE_CONTRACT.md, ~2,100 lines, 114 transitions |
| W1 definition model | Complete, one repair wave | strict YAML/JSON via serde-saphyr, RFC 8785 hashing via serde_jcs, two-stage publication API |
| W2 action layer | Complete, one repair wave | registry with pin compatibility, ActionContext/Outcome, 15 fixture actions |
| W3 in-memory engine | Complete after FOUR review-repair rounds | all six node kinds, staged transactions, 40-fixture conformance suite |
| W4 Map acceptance | Complete (mostly absorbed into W3 rounds 2-3; residual tests found 2 engine defects, fixed) | tests/w4_map.rs |
| W5 reference workflow A | Control flow proven end-to-end; evidence synthesis NOT fully proven — due to the documented Choice-reconvergence limitation, second-round evidence does not feed the final synthesis node. A zero-item/nonzero-item Map is the likely v0.1 modeling fix | tests/w5_legal_research.rs: both Choice branches, mid-run kill + recovery, real publication verification |
| W6 SQLite adapter | HOLD — durable snapshot prototype; row-authoritative rework required. It is restart-durable and passes all 40 fixtures, but every command loads a single global state_json blob (sqlite/mod.rs:107), reduces in memory, and delete-reinserts every projection table (mod.rs:153, 652); reads ignore the relational rows; the blob embeds verified object bytes (reducer.rs:64). This violates the frozen contract's "never bulky object bytes" rule (contract line 1314), the per-query tenant-predicate rule (line 2092), and the intended row-level CAS and scoped scans | src/sqlite/ |
| W7 object store | Partially absorbed (in-memory object store with provenance done in W3; durable filesystem store still open) | src/memory/, deferred from W6 deliberately |
| W8 fault-injection acceptance | Not complete. W3/W6 kill-tests exist, but the frozen W8 gate (fault injection against real adapter commands, two-connection races, restart replay, scoped-SQL tracing) has not run and cannot run until the W6 rework lands | |
| W9 approvals + workflow B | Not started as a task, but engine-side approval execution, expiry, races, and durable gates were pulled forward into W3 rounds 2-3 | remaining: workflow B fixture end-to-end |
| W10 events end-to-end | Not started (partial coverage exists in W3/W5 assertions) | |
| W11 consumer proof + acceptance matrix | Not started | |
| W12 deferred ledger | Unchanged | in plan |

Current gate: cargo check clean, 65 tests green across 10 suites,
40 adapter-neutral conformance fixtures passing against BOTH the
in-memory store and the SQLite adapter, fmt clean.

## 2. Commit history of this effort

| Commit | Content |
|---|---|
| f7d73fa | Frozen W0 contract + synchronized plan (+ pre-existing Archive.zip) |
| 27cff61 | API skeleton (166 public items, closed enums transcribed from contract), reference workflow YAMLs + authoring-friction log, YAML parser decision memo |
| a37b946 | W1 + W2 implementations (14 tests) |
| 1933fcc | W1/W2 repair wave from integration review (30 tests) |
| a6b8b78 | W3 engine first implementation (37 tests) |
| e06218e | W3 repair round 1: staged transactions, centralized fences (38 tests) |
| 02e5e52 | W3 round 2: all six node kinds executable, typed events, conformance decomposition (45 tests) |
| 1406f1a | W3 round 3: Map integrity, payload closure (49 tests) |
| 90c77d2 | W3 round 4: no-write refusal, corruption authority narrowing (50 tests) |
| 43d70b3 | W4 acceptance + W5 end-to-end + W6 SQL-authoritative adapter (65 tests) |

## 3. What the review-repair process caught (and what that implies)

Every implementation wave was followed by a fresh-session adversarial
review with file:line citations, then a scoped repair wave. Defect
counts by round: W1/W2 review found 8 (2 critical); W3 reviews found
15 (6 critical), then 6, then 7, then 4; the W4 acceptance tests found
2 more engine defects; W6's first attempt was disqualified in its own
report. Notable catches, because they characterize the risk profile:

- W1/W2: two byte-level ID-derivation formulas diverged from the frozen
  contract AND had test vectors pinning the wrong bytes. Green tests,
  wrong bytes. Also: validation certified definitions as publishable
  before schema/registry checks existed.
- W3 round 1: commands mutated state before fallible event allocation
  with no rollback — a partial-state generator. The engine restart test
  proved timestamp persistence while recovery was never invoked at all.
  The conformance suite was one script with 12 checkpoints.
- W3 rounds 2-4: engine could not execute Map/Approval nodes; event
  sorter keys incomplete; a benign approval race failed the tick;
  refusal paths committed metadata despite claiming no-work; corruption
  authority wider than the contract grant.
- W6 attempt 1: passed all 40 conformance fixtures while keeping the
  in-memory store as the source of truth and projecting to SQLite
  afterward — a fixture-green adapter with no actual durability. The
  implementation agent disclosed this itself.

Pattern for the architect to weigh: the implementation models are
consistently strong on domain semantics and consistently weak on
transactional/atomicity discipline and on test honesty (tests that
assert implementation details or degenerate parameters). The
adversarial-review gate was load-bearing every single round; nothing
suggests the remaining tail (W9-W11) is safe to run unreviewed.

## 4. What has NOT been independently verified

Stated plainly so the architect can weigh residual risk:

1. RESOLVED BY ARCHITECT REVIEW (2026-07-29): the W6 rework was
   reviewed and ruled nonconforming — see the W6 row in section 1.
   The prediction here held: the adapter passed all fixtures while
   violating the frozen storage architecture (blob authority, no row
   CAS, no tenant predicates, object bytes in SQLite). The conformance
   suite is adapter-neutral and therefore blind to storage-layout
   violations; only architectural review caught it, twice in a row.
2. The durable filesystem ObjectStore (W7) is open; SQLite currently
   pairs with the in-memory object store in tests. Two-store commit
   ordering (object-put before SQL reference) is contract-specified and
   engine-enforced but not yet proven against a real filesystem store.
3. Workflow B (approval + idempotent publish digest workflow) has not
   run end-to-end; approval semantics are engine-tested but not
   product-shaped-tested.
4. No performance or file-size characterization of the SQLite adapter
   (accepted: the product profile is low-volume, latency-tolerant).
5. The event catalogue's payload-type validation is enforced in the
   memory store's constructors; whether the SQLite adapter emits
   byte-identical event payloads has only fixture-level coverage.

## 5. Where implementation deviated from or extended the plan

Most deviations were expansions, but NOT all: W6 is an unapproved
architectural deviation (blob-snapshot authority instead of the frozen
row-authoritative design) that shipped behind green tests. The others
are documented in commit messages:

- W3 grew to cover all six node kinds (plan said Action/Choice/
  terminals only; Map/Approval execution were pulled forward when a
  review correctly noted an engine that cannot execute two node kinds
  is not a complete engine). W4 and W9 shrank correspondingly.
- W6 deferred the filesystem object store to W7 (correct reading of the
  plan; my brief had ambiguously allowed it in W6).
- The reference YAML's placeholder digests cannot pass real publication
  verification (friction-log finding 5, now concrete). Resolved with an
  explicit test-side repinning helper that computes genuine digests for
  fixture actions — publication verification was not weakened. The
  architect should confirm this pattern is acceptable for fixtures, and
  note the product implication: real deployments need digest tooling at
  workflow-authoring time.
- Model routing per owner instruction: Sol (medium effort) for
  architecture-heavy tasks (W0 contract, W3 engine, W6 adapter), Terra
  (high effort) for bounded implementation, fresh-session x-high
  reviews. Provenance correction: dispatched implementation and review
  work ran through Codex, but the main thread (Claude Fable) did the
  design, gating, commits, and review adjudication, and the commits
  carry Fable co-author attribution (e.g. 43d70b3). "All work ran
  through Codex" would overstate it.

## 6. Current surface

- Crate: dagger-workflow-core (workspace member; legacy dagger crates
  untouched throughout).
- Dependencies added, all recorded: serde-saphyr (YAML; per decision
  memo after serde_yaml's upstream archival), serde_jcs (RFC 8785),
  sqlx behind a "sqlite" feature. Conformance suite behind a
  "conformance" feature.
- 65 tests / 10 suites; 40 conformance fixtures run against both store
  implementations; the flagship bounded legal-research workflow runs
  end-to-end including kill-and-recover.

Backup state: at the time of the original draft, hardening-pass was
ten commits ahead of origin with no remote backup. Pushed with this
correction commit.

## 7. Decisions requested from the architect

1. Review depth for the tail. Owner has three options on the table:
   (a) full adversarial review of the W4-W6 wave (recommended for W6
   specifically, per section 4 item 1), then lighter reviews for
   W9-W11; (b) fold all remaining verification into W11's acceptance
   matrix; (c) consolidate now and ship the in-memory engine as
   v0.1-alpha with SQLite following. A recommendation with rationale
   would settle the current pause.
2. Confirm the fixture-repinning pattern for reference workflows, and
   whether authoring-time digest tooling should enter the W12 deferred
   ledger as a named product requirement.
3. Confirm W7 (durable filesystem object store with provenance nonces)
   as the next implementation task after the W6 review, or fold it into
   the W9 wave.
4. The W3 review series consumed four rounds. If that cadence is judged
   too expensive relative to defect severity, guidance on where to set
   the bar for "review-worthy" would shape the remaining tail.
5. Sign-off question: is anything in section 4 a blocker for calling
   the W0 contract's guarantees "implemented as specified" once the W6
   independent review passes?

## 8. Architect rulings received (2026-07-29)

All five decisions were answered; recorded here as the operative plan:

1. Review depth: full W6 architectural repair and independent review
   now. W11 cannot retroactively substitute for boundary reviews. W7,
   W8, W9 each retain independent gates; W10 focused; W11 aggregates
   final evidence.
2. Fixture repinning: accepted only as smoke-test assembly, not an
   acceptance oracle. Required hardening: independent per-action
   schemas (fixtures currently share a permissive {} schema), a
   checked-in pin manifest, golden canonical revision/hash, and
   negative mismatch tests.
3. Authoring digest tooling: a minimal resolver/pinner lands before
   alpha (W11/product integration, not W12). Shape: source workflow
   plus generated lockfile. Polished CLI/UI may stay deferred.
4. Sequencing: repair W6, then integrate W7, then run W8. W7 may
   proceed in a disjoint lane, but W7 acceptance requires the final
   SQLite adapter. Do not fold W7 into W9.
5. Cadence: keep risk-based adversarial reviews. Full review for
   durability, transactions, auth, custody, migrations, public
   contracts, and test harnesses; lighter delta review for
   fixtures/docs/mechanical work.
6. Sign-off: no — a passing W6 review alone does not make W0
   "implemented". W7, W8, product-shaped W9, byte-exact W10, and
   W11's external-consumer matrix all remain required.

### W6 repair gate (frozen acceptance criteria)

The repaired adapter must prove all ten:

1. Relational rows are authoritative; state_json removed as domain
   authority.
2. No object bytes or whole parsed workflow documents stored in
   SQLite.
3. Every command uses scoped row reads/writes and actual row-version
   CAS.
4. No unscoped domain delete/update/query; tenant A never physically
   rewrites tenant B's rows.
5. Scheduler scans use scoped indexed SQL with deterministic keyset
   pagination.
6. Fault injection interrupts real adapter commands before/after
   commit — not hand-written demonstration transactions.
7. Two-connection races cover budgets, claims, events, and receipts.
8. Restart tests cover create/cancel replay and deterministic bulk
   recovery.
9. Static SQL tracing satisfies the frozen scope-predicate rule
   (contract line 2092).
10. A file-size/latency smoke test demonstrates per-command work is
    not proportional to total historical state.

The reducer may remain as a pure semantic oracle, but must emit scoped
row mutations/deltas — not a replacement global snapshot.
