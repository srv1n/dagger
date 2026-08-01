# Workflow Core contract - normative erratum 0.1.1

Applies to: `docs/WORKFLOW_CORE_CONTRACT.md`, contract version 0.1.

Status: normative. This erratum amends contract 0.1 and is binding wherever it
conflicts with the base text. Contract 0.1 is not rewritten in place; the frozen
document stands as published and this erratum records what changed and why.

Source: external architect ruling on escalations E1 residuals, E2, and E3,
collected 2026-08-01 (handoff `cgpt_msa4pbyi_0d99f8c0`). Every amendment below
was independently confirmed against source before adoption.

Every amendment is grouped by the base section it changes.

---

## A. Object corruption is not control-plane corruption

### A.1 Section 5.5, error taxonomy

Add the error:

    CommittedObjectCorrupt { bad_ref: ArtifactRef, proof: FailedReadProof }

An adapter returns this when a committed prerequisite object fails verification
while hydrating a command. It carries the exact committed typed use that failed
and the original proof minted by the first failed read.

`CorruptControlPlane` never represents object-store corruption. It is reserved
for an impossible durable control-plane projection. Mapping a failed object read
onto it is a defect.

`FailedReadClass` remains closed to `Missing` and `DigestInvalid`. No amendment.

### A.2 Section 12.3, failures and reads

The first proof must propagate unchanged, together with the exact committed
`ArtifactRef` it was minted against. A second read must not be required to
recover or replace a discarded proof.

This is a correctness requirement, not an optimization. A repeated read may
observe restored bytes, a transient outage, a different digest failure, a
different store-instance identity, or a different component of the same
revision. A proof authorizes the observation that minted it, and must not be
discarded and reacquired later.

Because hydration may deduplicate schema references, an adapter must retain
enough identity to return an `ArtifactRef` that genuinely failed. Distinct typed
uses may share a digest; returning the wrong one marks the wrong component.

### A.3 Section 5.1, boundary and transaction contract

An adapter may verify already-committed prerequisite objects before opening the
command transaction. A failed verification returns the exact ref and the original
proof, and the requested command commits no effects.

This preserves the base rule that a command returning an error has no partial
control-plane effects. Hydration runs before `BEGIN IMMEDIATE`, so an adapter
must not implicitly apply a corruption transition inside a failed command. The
corruption transition is a separate explicit command.

### A.4 Section 5.3, runtime commands

- `create_run`: proof-bearing prerequisite corruption is a no-write pre-run
  failure. No run or control-plane state is created and no corruption command is
  attempted, because the target run does not yet exist. The proof remains
  available for diagnostics.
- `complete_map`: proof-bearing prerequisite corruption obliges the existing-run
  caller to invoke `mark_corrupt_storage` and await its commit before surfacing
  the integrity result.
- The same rule binds any future command that hydrates committed objects.

A storage-unavailable hydration failure remains `StorageUnavailable` and
authorizes no transition.

---

## B. Read responsibility and the host boundary

### B.1 Section 5.4, read API

Section 5.4 currently combines two different things in one sentence: that
point and list reads never mutate state, and that failed verified object reads
require callers to mark corruption. Those belong to different operations.

Split the section:

- **Control-plane projection reads** never mutate. The `WorkflowStore` point and
  list methods return control-plane projections and do not dereference object
  payloads. The mutation obligation does not apply to them.
- **Composite committed-payload reads** may issue the separate corruption
  command after a failed verification, and are the only reads that carry the
  obligation.

### B.2 Section 12.3, responsible party

Name the party responsible for discharging the corruption obligation:

- the engine, for scheduler and internal reads;
- the embedder, for host reads;
- the recovery component, for audits.

A host read is a committed payload read performed by the embedding boundary on
behalf of an existing run. The `mark_corrupt_storage` commit precedes the host
integrity response.

Host reads must use the crate-owned composite read operation. The low-level
`ObjectStore::get` is not sufficient on its own for a host read performed on
behalf of a run.

Mark-failure precedence: when the corruption command itself fails, the composite
read returns the mark failure, not the integrity result. Returning the integrity
result would falsely imply the run output had already been invalidated.

A standalone administrative read of a definition or revision with no run context
cannot fabricate a run transition. It returns a proof-bearing integrity error and
is handled by a separate administrative integrity mechanism.

### B.3 Section 3.2, R14-R22

Define the host read path as above, and state that the `mark_corrupt_storage`
commit precedes the host integrity response.

### B.4 Section 3.3, N47-N57

A node corruption transition requires an explicit, verified run/node/ref
ownership relationship. Absence of a node owner does not waive run-to-ref
validation.

`mark_corrupt_storage` must establish that the supplied ref is reachable from the
named run through one of these closed relationships:

- run input;
- run output;
- node output, node artifact, or node diagnostics belonging to that run;
- Map child output attributed to its parent;
- approval payload for that run;
- immutable definition, revision, or schema ref used by that run.

If none holds, the command returns `InvalidFailedReadProof`. Without this, a
valid proof for an unrelated same-scope registered artifact can be paired with
any run in scope and drive it to `CorruptStorage`.

### B.5 Section 5.5, composite read outcomes

The public composite read defines four distinct outcomes: verified data;
storage unavailable; corruption successfully applied; and failure to apply
corruption. They are not collapsible.

---

## C. Storage profiles

### C.1 Section 12.1, publication

Section 12.1 is written as a filesystem algorithm while the `ObjectStore` trait
is storage-agnostic. Split it into a backend-neutral guarantee set and two
profiles.

**Backend-neutral publication guarantees.** Content digest over final bytes;
atomic conditional creation with no replacement; durable-service acknowledgment
before success; immutable metadata; read verification; retry convergence after an
ambiguous result.

**Local filesystem profile.** Temporary file; file sync; link-if-absent;
parent-directory sync; reopen and verify.

**Remote object profile.** Provider-supported conditional create; data and media
metadata committed as one atomic object or through one authoritative manifest;
read-after-write verification before returning a committable capability; a
timeout after a conditional write resolved by reading and verifying the
candidate; authoritative absence only when endpoint identity, credentials, and
consistency semantics make the absence conclusive.

For every backend: all transport, authorization, timeout, and partial-stream
failures produce `StorageUnavailable` and mint no proof. Only authoritative
absence may mint `Missing`. Only a completed mismatching read may mint
`DigestInvalid`.

### C.2 The durability safety property, corrected

The property "no visible-before-durable window" does not hold and must not be
claimed, if "visible" means visible in the filesystem namespace. A link-and-fsync
protocol necessarily links the entry before the containing directory can be
synced. Raw namespace visibility and crash durability are not atomic POSIX
properties.

The binding property is narrower:

> No successful publication response and no committable `VerifiedObjectRef` may
> escape before all required durability barriers and post-publication
> verification have completed.

This is a capability property, not a namespace property.

### C.3 Storage support matrix

`FsObjectStore` is a local-filesystem profile only, and is supported only on
explicitly qualified local filesystems.

NFS/EFS-backed use is **unsupported** for the durability tier unless
independently qualified against its actual mount and server configuration. A
networked filesystem is not a substitute for the remote object profile: it
exposes filesystem calls while weakening the assumptions the link-and-directory-
sync proof depends on.

A backend not listed as qualified is excluded from the durability claim. It may
not remain inside the claim by implication.

---

## D. What this erratum does not change

The frozen state model, transition tables, scope confinement rules, transaction
boundaries, and the closed `FailedReadClass` are unchanged. No renumbering of
R or N transitions. The Tier 2 durability statement in section 0 stands.
