# Workflow Core - open escalations requiring an architect ruling

Items here cannot be resolved inside the implementation because they require
changing something the contract froze, or because the contract is silent and the
choice is load-bearing. Each item states the defect, the evidence, why it cannot
be patched locally, and the options with a recommendation.

## E1. A transient object-read failure has no legal representation and is laundered into permanent corruption

**Status:** ruled and implemented. The type split landed in commit 5653849. The
three residuals it left open were ruled on 2026-08-01 and adopted as contract
erratum 0.1.1 (`docs/WORKFLOW_CORE_CONTRACT_ERRATUM_0_1_1.md`); see "Residual
rulings" at the end of this document.
**Found by:** external review of W6/W7, confirmed by direct source inspection.
**Severity:** high. A recoverable operational condition permanently destroys a
run's usable output.

### The defect

`FsObjectStore::get` maps every `io::Error` from `read_verified` onto
`FailedReadClass::Missing` and mints a proof:

    src/fs_object_store.rs:326-329
        let path = self.object_path(scope, requested);
        let verified = self.read_verified(&path).map_err(|_| ObjectReadError {
            proof: self.proof(scope, requested, FailedReadClass::Missing, None),
        })?;

`read_verified` (`src/fs_object_store.rs:170-187`) opens the file and streams it,
so its error set includes far more than "the object is not there":

- `EMFILE` / `ENFILE` - the process or system hit its file-descriptor limit.
- `EACCES` - permissions changed, or the store is being read by the wrong user.
- `EIO` - a transient device or network-filesystem error.
- `ENOMEM`, `EINTR`, and any error surfaced by a networked or fuse-backed mount.

Every one of those becomes a `Missing` proof. The same pattern repeats for the
media sidecar at `src/fs_object_store.rs:340-342`.

Because the proof is the only capability that can invoke `mark_corrupt_storage`
(contract section 12.3, line 1320), and the engine routes proofs faithfully
(`src/engine.rs` around line 1068), the consequence of running briefly out of
file descriptors is a run driven to `CorruptStorage` through R14-R22 / N47-N57.
Per the contract that is an integrity override: it invalidates usable output on
an already-`Succeeded` run (R17), and it is explicitly never repaired by
re-running the producer. A transient, self-correcting operational condition is
therefore converted into permanent, unrecoverable state corruption.

### Why this cannot be patched locally

The frozen read error type carries a proof and nothing else:

    src/artifact.rs:260-263
        pub struct ObjectReadError {
            /// Opaque proof minted for this failed read. Contract section 12.3.
            pub proof: FailedReadProof,
        }

and the closed error class admits only two values:

    src/artifact.rs:167-172
        pub enum FailedReadClass {
            Missing,
            DigestInvalid,
        }

So `ObjectStore::get` has exactly one way to fail, and that way asserts
corruption. There is no in-band representation for "I could not read this right
now, and I am making no claim about the object's integrity."

The contract's own vocabulary already distinguishes these cases and confirms the
current behaviour is wrong:

- Line 1320 scopes proofs narrowly: "Missing/invalid bytes cause the object
  store to mint a `FailedReadProof`." A descriptor exhaustion is neither missing
  bytes nor invalid bytes.
- Line 948 defines the correct category for this condition:
  "`StorageUnavailable` / `TransactionFailed` | Infrastructure failure; no
  uncommitted state may be treated as durable."
- Line 29 fixes the proof's error class as a "closed error class `Missing` or
  `DigestInvalid`".

The intent is unambiguous; the type system simply has no channel for it. Fixing
this means widening a frozen surface, which is the architect's call and not the
implementer's.

### Options

1. **Widen the read error into an enum.** Replace the struct with
   `enum ObjectReadError { Corrupt(FailedReadProof), Unavailable }`, or add a
   variant carrying no proof. Callers that today read `error.proof` must then
   handle the transient case by propagating `StorageUnavailable` rather than
   invoking `mark_corrupt_storage`. This is the honest fix and matches the
   existing error taxonomy at line 948. Cost: a breaking change to a frozen type
   plus every engine call site that routes proofs.

2. **Add a third `FailedReadClass` variant**, for example `Unavailable`, and
   forbid `mark_corrupt_storage` from accepting a proof of that class. Smaller
   type change, but it is a worse design: it keeps minting a capability whose
   entire purpose is to authorize corruption, then relies on a downstream check
   to refuse it. It also contradicts line 29, which declares the class closed.

3. **Narrow the mapping and retry in the store**, treating only
   `ErrorKind::NotFound` as `Missing` and retrying other errors internally with
   a bounded backoff before giving up. This does not solve the problem, because
   after the retries are exhausted the store still has only one way to fail, and
   it would then mint a false proof anyway. It also silently adds latency inside
   a call the engine may make while holding other resources.

4. **Accept the behaviour and document it.** Only defensible if the object store
   is guaranteed to live on a local disk owned exclusively by this process, with
   descriptor limits provably above the engine's concurrency. That guarantee
   does not exist for an embeddable library whose host chooses the storage path.

### Recommendation

Option 1. It is the only choice that makes the type say what the contract
already says. The blast radius is bounded: `get` is the sole producer of
`ObjectReadError`, and the engine is its sole consumer, so the change is
mechanical once the shape is ruled on.

Whatever is chosen, the narrowing in option 3 should also be applied, so that
`ErrorKind::NotFound` is the only condition that ever produces a `Missing`
proof, and a digest mismatch remains the only condition that produces
`DigestInvalid`.

### Also needed regardless of the ruling

A test proving that a non-`NotFound` I/O error does not mint a proof. This
requires injecting a read failure that is not a missing file - for example by
exhausting descriptors, by revoking read permission on the object file, or by
routing the store root through a fault-injecting filesystem shim. No such
harness exists today, which is why this defect survived three review rounds.

## E2. fsync ordering in the object store is argued, not proven

**Status:** ruled 2026-08-01. Folded into W11 as gates W11-D (stateful crash
model, mandatory, with negative controls) and W11-E (real abrupt-crash
qualification on a block device). The ruling confirms this document's claim: a
userspace model alone proves the protocol against its model and cannot support a
crash-durability claim on a named filesystem. Process SIGKILL is explicitly not a
crash-consistency test. Remains open as engineering work; no longer open as a
question.

The W7 put protocol now fsyncs every directory level it creates beneath an
already-durable base (`src/fs_object_store.rs:462`), and fsyncs the containing
directory on the `AlreadyExists` branch of both the object and media links
(`:235`, `:257`). Those corrections are believed correct by inspection.

They are not proven by test. An fsync is not observable through the standard
library, so the tests added alongside them exercise the branches but would also
pass against the unfixed code. The only new test that genuinely fails when its
subject breaks is the base-escape guard.

Proving the durability ordering needs a harness that can observe or intercept
the syscalls: an `LD_PRELOAD`-style shim, a fault-injecting filesystem, or a
crash-consistency checker that snapshots the directory at each interruption
point. Building one is a real piece of work and should be scoped deliberately as
part of the W11 acceptance matrix rather than improvised inside W7.

Until that exists, no document should claim the put protocol's crash-safety is
test-proven. It is code-reviewed, which is a weaker and honest claim.

## E3. Server embedding is a first-class target, and parts of the design currently assume a single local process

**Status:** ruled 2026-08-01. A storage support matrix is now normative (erratum
0.1.1 section C.3): FsObjectStore is local-filesystem only, NFS/EFS are
unsupported for the durability tier unless independently qualified, and remote
object stores get a distinct profile. Consequence 4 below is therefore settled.
Server-shaped acceptance became W11-B, W11-F, W11-G, and W11-H. Remains open as
engineering work. Owner directive, 2026-08-01: "this needs to work with server
implementations."
**Severity:** high for scope. It does not invalidate the frozen model, but it
changes what "done" means for W11 acceptance and it raises the priority of E1.

### What changes when the host is a server

The crate has so far been proven as an embeddable library driven by a single
local process against a local filesystem. A server host differs in four ways
that the current evidence does not cover:

1. **The object store may be remote.** S3, GCS, or a blob service rather than a
   local disk.
2. **There may be many concurrent tenants** in one process, and many processes
   or replicas against one control plane.
3. **The process is long-lived**, so leaks, unbounded loads, and lock-hold
   duration matter in a way they do not for a short CLI run.
4. **The filesystem, when used, may be networked** (NFS, EFS), where `fsync`
   and `link` semantics are weaker than local POSIX.

### Consequence 1: E1 is a hard blocker, not a latent bug

`ObjectStore` (`src/artifact.rs:292-315`) is already storage-agnostic: the
methods are async and no signature names a filesystem type, so a remote
implementation is possible by construction. But the error surface is asymmetric:

- `put` returns `ObjectStoreError`, which has `StorageUnavailable`.
- `get` returns `ObjectReadError`, which carries only a `FailedReadProof`.

Against a local disk, a transient read failure is rare. Against a networked
store, HTTP 503, throttling, connection reset, DNS failure, expired credentials
and IAM propagation delays are routine and self-correcting. Under the current
type every one of them is classified `Missing`, mints a corruption proof,
invokes `mark_corrupt_storage`, and permanently invalidates a succeeded run's
output through R17, which the contract forbids repairing.

A networked object store therefore cannot be implemented against this trait
until E1 lands. This is not a quality concern; it is a correctness barrier to
the stated deployment target.

The E1 ruling's classification rule already generalizes correctly, and the
implementation must be written so a remote store can honour it:

- authoritative absence of the object (a definitive 404 from a consistent
  store) may mint `Missing`;
- a completed read whose digest differs mints `DigestInvalid`;
- everything else, including every transport and authorization failure, is
  `StorageUnavailable` with no proof.

Note the ruling's store-instance-nonce requirement is what makes "authoritative
absence" meaningful for a remote store: a 404 from a store the process can no
longer positively identify is not authoritative absence.

### Consequence 2: single-writer control plane needs a stated deployment rule

The SQLite adapter is a single-writer design: `BEGIN IMMEDIATE` per command, and
an engine claim with lease and heartbeat (`acquire_engine_claim`,
`heartbeat_engine_claim`, `EngineAlreadyLive`) that admits one live engine per
scope. That is correct and sufficient for one server process, and it is the
mechanism that makes multi-replica deployment safe, because a second replica is
refused the claim rather than corrupting state.

What is missing is a written statement of the supported topology. It should say
explicitly which of these is supported for v0.1:

- one process, many tenants, SQLite control plane and local or remote objects;
- many processes against one SQLite file (not recommended; SQLite write
  contention plus network-filesystem locking is the classic corruption story);
- many replicas with exactly one holding the engine claim, others idle or
  serving reads;
- a future non-SQLite control plane, which the `WorkflowStore` trait already
  permits by construction.

Whichever is chosen, the read-only path matters: a replica that serves reads
while another holds the claim must be proven not to mutate state and not to
require the claim. The store's read methods already use snapshot transactions,
so this is a matter of proving and documenting it, not of new mechanism.

### Consequence 3: W11 acceptance must include a server-shaped consumer

The current W11 description proves a consumer, an acceptance matrix, and a
resolver/pinner. For a server target it must also demonstrate, at minimum:

- a non-filesystem `ObjectStore` implementation passing the same conformance
  fixtures, which is the real test of whether the trait is implementable
  off-disk. A fault-injecting in-process fake is sufficient; it does not need
  to be a real cloud client, but it must exercise transient failure and prove
  no proof is minted;
- concurrent multi-tenant execution proving scope isolation under load rather
  than only in single-threaded fixtures;
- engine claim contention across processes: second claimant refused, lease
  expiry and takeover, no double execution;
- a long-run check that memory and open descriptors are stable, which is what
  the bounded-load work was for and what no test currently asserts.

### Consequence 4: networked filesystem caveat must be documented

If the filesystem object store is used on NFS or EFS, the durability argument in
E2 weakens further: `fsync` on a directory may not have the same guarantee, and
`link` may not be atomic. Either state that the filesystem store is supported
only on local POSIX filesystems, or add the caveat to its documentation. This
is a documentation obligation, not new code.

### Recommendation

Fold E3 into the same wave as E1 rather than treating it as separate work. The
E1 implementation should be written and reviewed with a remote store as the
motivating case, since that is what makes the transient branch load-bearing.
Then extend the W11 acceptance matrix with the four items above, and record the
supported topology in the plan.

---

# Residual rulings adopted 2026-08-01

External architect ruling on the E1 residuals, E2, and E3. Verdict: alpha
sign-off blocked, no fundamental redesign required. Every finding below was
independently confirmed against source before adoption. Normative text lives in
`docs/WORKFLOW_CORE_CONTRACT_ERRATUM_0_1_1.md`; this section records provenance
and the defects the review surfaced.

## Ruled

- **Q1, proof transport.** Option A: add
  `StoreError::CommittedObjectCorrupt { bad_ref, proof }`. Do not re-read to
  reconstruct the signal, and do not classify object corruption as
  `CorruptControlPlane`. Widening an exhaustively matched enum is desirable here:
  it forces every command caller to decide whether it has a run to transition.
- **Q2, host boundary.** The embedder owns the host response boundary, but the
  crate must own the safety mechanism. A crate-owned composite reader performs
  the read and, on corruption, commits `mark_corrupt_storage` before returning
  the integrity result. An embedder-implemented trait was rejected: a server can
  implement it correctly and still bypass it in an endpoint.
- **Q3, durability evidence.** A stateful crash model is mandatory and is
  sufficient to prove the publication algorithm against that model. It is not
  sufficient to claim crash durability on a named filesystem, which additionally
  requires abrupt VM or block-device qualification. `FsObjectStore` is a
  local-filesystem profile only; NFS/EFS are unsupported unless independently
  qualified; remote object stores need a distinct conditional-publication and
  authoritative-read profile.

## Defects the review surfaced, all confirmed against source

1. **Hydration laundered the proof.** `sqlite/mod.rs` mapped
   `ObjectReadError::Corrupt(_)` onto `CorruptControlPlane` and discarded the
   proof, having already deduplicated schema refs to bare digests so the exact
   `ArtifactRef` was unrecoverable. `CorruptControlPlane` is reserved for an
   impossible durable control-plane projection, so this was also the wrong
   semantic category.
2. **`get` minted a committable capability with no durability barrier.**
   Publication links the final data entry before `finalize_candidate` syncs the
   containing directory, and `get` performed no barrier at all. A second process
   knowing the digest could obtain a committable `VerifiedObjectRef` for an entry
   that was not yet durable. A process-local lock cannot fix this, because the
   target deployment is multi-process.
3. **Unrelated-run poisoning.** `mark_corrupt_storage` validated scope, digest,
   store-instance nonce, and ref registration, then loaded the run
   independently. With `owner_node_id = None`, nothing established that the ref
   was reachable from the named run, so a valid proof for any same-scope
   artifact could drive any run in scope to `CorruptStorage`.

## Correction adopted into the durability claim

The property "no visible-before-durable window" was wrong as written and must
not be claimed. Namespace visibility before the barrier is normal and
unavoidable for a link-and-fsync protocol. The binding property is a capability
property: no successful publication response and no committable
`VerifiedObjectRef` may escape before all required durability barriers and
post-publication verification have completed.

## Not independently verified by the reviewer

The review environment had no Cargo binary, so the ruling rests on the clean
bundle's source, contract, escalation record, and test contents rather than a
fresh test execution. Test-suite state is ours to establish.

## E4. Two obligations from erratum 0.1.1 that the crate cannot enforce

**Status:** open. Raised by the implementation wave of 2026-08-01, after the
architect ruling was adopted. Neither blocks the wave; both block an honest
alpha claim.

### E4.1 The `create_run` no-write rule has no enforcement point

Erratum section A.4 requires that proof-bearing prerequisite corruption at
`create_run` be a no-write pre-run failure: no run created, no corruption command
attempted, proof retained for diagnostics.

The engine has no `create_run` call site. `create_run` exists only on the
`WorkflowStore` trait, so hosts call it directly. The rule is therefore a host
obligation, and the crate can document it but cannot enforce it. It is currently
recorded as a doc comment on `EngineError::Store`, which is a comment, not a
guarantee.

This is the same shape as the host-read obligation the ruling already assigned to
the embedder, so it likely wants the same treatment: name the responsible party
in the contract and cover it with a server-shaped W11-B fixture. Confirming that
is an architect call, not an implementer's.

### E4.2 The read-path barrier is a cost the ruling's own alternative removes

Closing the premature-capability window (ruling Q3) was implemented by having
`get` establish the publication barrier before minting a committable ref. That is
correct and it is the smallest change, but it puts two directory fsyncs on every
successful read. On local SSD that is tens of microseconds once warm; on
network-attached storage it is milliseconds, and it does not batch because each
`get` syncs its own directory handles. For a read-heavy workload over a hot
object the barriers dominate, since the bytes themselves come from page cache.

The architect's first alternative removes the cost structurally: if `get`
returned a read-only capability that cannot be committed as a durable reference,
the read path needs no barrier and only the put path pays. That is a change to
the `artifact` contract types and their consumers in both object stores and the
engine, so it is a deliberate wave rather than an improvisation inside this one.

Recommendation: take the type split before alpha, and treat the current barrier
as the correct conservative default until then. Do not benchmark-tune around it;
the fix is structural.

## E5. Suspected create_run parity divergence between the oracle and the SQLite adapter

**Status:** CLOSED 2026-08-01. Suspicion refuted as stated; the underlying defect
was real and sat one command earlier. See the resolution at the end of this
section.

The in-memory store is the semantic oracle for the SQLite adapter, and the
conformance suite exists to hold them identical. While writing a new fixture, the
same `create_run` call was observed to succeed against the memory store and fail
against SQLite with `SchemaSubsetUnsupported`, suggesting SQLite validates run
input against the pinned root schema at creation and memory does not.

The fixture was made schema-valid to isolate the change under test, so nothing in
the suite currently exercises the divergence. That is exactly why it needs
recording: a parity gap that no fixture covers is invisible to the mechanism
built to catch parity gaps.

If real, the oracle is weaker than the implementation, which is the dangerous
direction. Every parity claim rests on the oracle being at least as strict as
what it certifies.

Next step is not a ruling. It is a fixture: submit a run input that violates the
pinned root schema and assert both stacks reject it identically. Whichever stack
is wrong then gets fixed against the contract, not against the other stack.

### E5 resolution

`create_run` does not diverge. Both stores validate run input against the pinned
root schema with byte-identical logic (`memory/mod.rs:2860`,
`sqlite/reducer.rs:2956`), and the two `schema_accepts` bodies are line-for-line
identical.

The divergence was in `publish_revision`. SQLite validated the root input/output
SchemaDocuments and every action-pin schema against the supported subset
(`sqlite/reducer.rs:2610-2611`, `:2682-2683`); memory validated none of them and
would pin any well-formed JSON object as a schema. The observed
`SchemaSubsetUnsupported` was therefore the right error from the right cause,
attributed to the wrong call.

Contract section 5.2 lists `SchemaSubsetUnsupported` among `publish_revision`'s
preconditions, and section 14 requires the same subset validator at publication,
binding validation, action output validation, run creation, and Succeed output
validation. SQLite was conforming; the oracle was not. Memory now runs
`validate_schema_document` at all three contract-mandated points.

Covered by conformance case 44, `pinned_root_schema_subset_enforced`
(`CASE_COUNT` 43 -> 44). Red-proven: removing the publish-time validation from
memory fails the case with "out-of-subset root schema was published". The
run-input half of the case was already green on both stacks and is a regression
guard, not a bug reveal.

**The lesson generalises past this instance.** The oracle was weaker than the
implementation it certifies, which is the direction that makes parity claims
worthless rather than merely noisy. A parity harness cannot detect a gap in a
command no fixture exercises, so the absence of a failing conformance case is not
evidence of parity. E6 is the same class of defect, found by looking rather than
by testing.

## E6. The oracle accepts unverified revision structure that SQLite re-derives

**Status:** open, confirmed by source inspection 2026-08-01. Not yet fixed.

`sqlite/reducer.rs:2603-2606` passes the caller's `parsed_revision` through
`parse_canonical_revision` (`reducer.rs:382-433`) and uses the re-derived value
thereafter. That helper enforces four properties:

1. the canonical bytes are valid UTF-8 JSON parseable as a definition;
2. the bytes are exactly `canonical_definition_json(parsed)`, i.e. genuinely RFC
   8785 canonical;
3. the caller-supplied typed definition canonicalizes to those same bytes;
4. the caller-supplied `topological_ranks` equal `canonical_topological_ranks`
   (lexical Kahn).

`memory/mod.rs:2507` takes `command.parsed_revision.definition.definition_id` on
faith and builds the revision from the supplied struct at `:2613-2634`. None of
the four properties is checked. A host can therefore publish, against the oracle,
a revision whose stored node ranks, entry node, and node set disagree with the
canonical bytes its `revision_hash` is derived from.

This is worse than a publication-hygiene gap. Section 1.5 makes the persisted
rank the recovery-order key used in sections 3.4 and 4, so an unverified rank
corrupts deterministic bulk recovery ordering -- the exact property W8's
recovery fixtures and the W11 acceptance matrix are supposed to certify.

Deliberately not fixed in the E5 pass. The fix flips memory's publish outcome
from success to `RevisionInvalid` for any hand-built fixture whose typed
definition does not canonicalize byte-exactly, and default expansion for the root
`description` and Approval `on_expiry` fields is an easy trap. Every `w1`-`w7`
engine fixture publishes against the memory store, so the change needs those test
files in scope. Conformance fixtures are unaffected, since they already satisfy
SQLite; the blast radius is memory-only tests.

Scope evidence that this is the last gap of its kind: after the E5 fix, the two
reducers differ by exactly two SQLite-only free functions,
`parse_canonical_revision` and its `revision_invalid` helper. The `StoreError`
variant multisets differ by `RevisionInvalid` 5-vs-0 (this gap), plus memory
raising marginally more `NotFound` (52/51) and `InvalidField` (20/17) -- the safe
direction, with the `InvalidField` surplus confined to memory's snapshot serde
code, which SQLite has no counterpart for. Those three were not chased
exhaustively.
