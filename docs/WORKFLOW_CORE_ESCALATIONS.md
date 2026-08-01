# Workflow Core - open escalations requiring an architect ruling

Items here cannot be resolved inside the implementation because they require
changing something the contract froze, or because the contract is silent and the
choice is load-bearing. Each item states the defect, the evidence, why it cannot
be patched locally, and the options with a recommendation.

## E1. A transient object-read failure has no legal representation and is laundered into permanent corruption

**Status:** open, needs a ruling before alpha sign-off.
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

**Status:** open, lower priority than E1. Recommend folding into W11 acceptance.

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

**Status:** open. Owner directive, 2026-08-01: "this needs to work with server
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
