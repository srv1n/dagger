# YAML Parser Decision for `dagger-workflow-core`

This memo records the comparison required by `WORKFLOW_CORE_PLAN.md`: W1 needs YAML, but must not default to archived `serde_yaml`. Facts were checked live against docs.rs, project documentation, and RustSec on 2026-07-28; maintenance and young-crate maturity judgments can become stale.

## Decision criteria

Weight security/resource controls and faithful strict-Serde behavior highest, then actionable diagnostics and maintainer health, then dependency cost. Broad YAML support is negative unless W1 needs it: accept one bounded document, anchors/aliases, and ordinary mappings/sequences/scalars; reject merge keys, includes, duplicate keys, and surprising implicit coercions.

## Comparison

### `serde-saphyr` (the usable Serde member of the broader Saphyr family)

- **Maintenance/trust:** Actively released through July 2026, with a 1.0 release candidate, fuzz/audit claims, Miri, and a large YAML test-suite conversion. It is young and appears maintainer-concentrated, so trust is promising rather than battle-tested. Despite its name it is not the official Saphyr Serde layer; it now uses the pure-Rust `granit-parser`, a Saphyr-parser fork ([project relationship](https://docs.rs/crate/serde-saphyr/latest#project-relationship)).
- **Serde/strictness:** Direct typed deserialization; supports externally, internally, and untagged derived enums (with documented `Spanned<T>` limitations), tuple variants, and Serde-driven `deny_unknown_fields`. Duplicate keys are errors by default. Diagnostics include locations, snippets, anchor origins, and user-facing formatting. W1 must still conformance-test its exact internally tagged node enum and unknown-field errors.
- **YAML/risk:** Anchors, aliases, and configurable merge keys are supported. Configure merge keys to `Error`, leave includes/features off, and retain aliases only for W1's required canonicalization fixture. Default budgets cover events, nodes, nesting, scalars, documents, anchors, aliases, and input bytes; separate alias replay/expansion/depth caps directly address billion-laughs-style bombs ([options](https://docs.rs/serde-saphyr/latest/serde_saphyr/options/struct.Options.html)). Type-directed scalars avoid turning `NO` into a boolean for a `String`; set `strict_booleans = true` so boolean fields accept only `true`/`false`.
- **Footprint/license:** Deserialize-only mode is moderate: `serde_core`, `granit-parser`, `annotate-snippets`, `smallvec`, `num-traits`, `ahash`, and encoding support, with serializer-only dependencies excluded. Library code claims no unsafe. MIT OR Apache-2.0.

### `serde_norway`

- **Maintenance/trust:** A close hard fork of `serde_yaml`, with normal public repository history and CI/audit configuration, but its latest docs.rs release is from December 2024. Calling it maintained in July 2026 would be generous; reassess before use.
- **Serde/strictness:** Mature `serde_yaml`-compatible surface: tagged/untagged Serde enums and `deny_unknown_fields` work; duplicate mapping keys are rejected. Errors carry locations but are less corrective than `serde-saphyr`.
- **YAML/risk:** Anchors/aliases work; merge expansion requires the `Value::apply_merge` path rather than ordinary direct typed parsing. Inherited repetition limiting mitigates alias bombs, but there is no configurable whole-document budget comparable to `serde-saphyr`; enforce byte/node limits outside it. YAML 1.1-style implicit typing retains Norway-problem risk, especially through `Value`.
- **Footprint/license:** Small: `indexmap`, `itoa`, `ryu`, `serde`, and its Rust port `unsafe-libyaml-norway`. Despite the dependency's name it is Rust code containing unsafe and expands the audit surface. MIT OR Apache-2.0 ([metadata](https://docs.rs/crate/serde_norway/latest)).

### `serde_yaml_ng`

- **Maintenance/trust:** A conservative, openly developed `serde_yaml` fork, but the latest docs.rs release is from May 2024. It preserves provenance better than `serde-yml`, yet currently looks dormant, not a durable maintenance answer.
- **Serde/strictness:** Essentially the established `serde_yaml` implementation: derived tagged/untagged enums, `deny_unknown_fields`, and duplicate-key rejection are complete. Its compatibility-first API gives predictable but mostly developer-oriented line/column errors.
- **YAML/risk:** YAML 1.1 only, so implicit `YES`/`NO`/`ON`/`OFF` typing is a portability hazard. Anchors/aliases work, merge expansion is opt-in through `Value::apply_merge`, and a repetition limit rejects exponential alias expansion; no configurable total input/node budget is exposed.
- **Footprint/license:** Small: `indexmap`, `itoa`, `ryu`, `serde`, and `unsafe-libyaml`; the last is unmaintained and uses unsafe Rust. MIT only ([manifest](https://docs.rs/crate/serde_yaml_ng/latest/source/Cargo.toml.orig)).

### `saphyr` / proposed `saphyr-serde`

- **Maintenance/trust:** The multi-maintainer Saphyr project is active, with July 2026 releases and YAML-test-suite coverage. Its README still says `saphyr-serde` is “soon-to-be”; no official Serde crate is available as of this lookup ([project page](https://docs.rs/crate/saphyr/latest)).
- **Serde/strictness:** `saphyr` supplies parser events and a YAML DOM, not a Serde `Deserializer`; therefore Serde enum semantics and `deny_unknown_fields` would require a substantial adapter. The separately maintained `serde-saphyr` above is not that official adapter.
- **YAML/risk:** Strong YAML 1.2 coverage avoids legacy boolean coercions and supports anchors/aliases. Raw Saphyr exposes parser/DOM mechanics rather than application-level duplicate-key and resource policies; an adapter would own those security decisions. Merge behavior should be rejected explicitly.
- **Footprint/license:** Moderate: `saphyr-parser`, `hashlink`, `ordered-float`, `thiserror`, and optional `encoding_rs`, plus any custom Serde adapter. MIT OR Apache-2.0, with bundled upstream license notices.

### `serde-yml`

- **Maintenance/trust:** Do not use. The project was criticized for obscured fork provenance, disabled issue tracking, questionable AI-generated changes, and poor maintainer response. More decisively, versions through 0.0.12 are covered by [RUSTSEC-2025-0068](https://rustsec.org/advisories/RUSTSEC-2025-0068) for unsoundness and the repository was archived. The current 0.0.13 is explicitly deprecated and only a compatibility shim over `noyalib`; that emergency shim does not repair the crate's trust history.
- **Serde/strictness:** Old releases cloned the `serde_yaml` surface, including enums, `deny_unknown_fields`, and duplicate rejection; the shim claims common-call compatibility but removed internals. Depending on a deprecated indirection adds behavioral/version ambiguity for no benefit.
- **YAML/risk:** Old releases inherited aliases, merge preprocessing, YAML 1.1 coercions, and repetition limits plus the advisory. The new shim claims pure Rust, YAML 1.2 booleans, and safer behavior, but should be assessed as `noyalib` directly if ever considered.
- **Footprint/license:** Old graph used the problematic `libyml`; the shim uses `noyalib` plus `serde`. MIT OR Apache-2.0 ([deprecation manifest](https://docs.rs/crate/serde_yml/latest/source/Cargo.toml.orig)).

### Archived baseline: `serde_yaml`

- **Maintenance/trust:** Upstream was archived and the final release was marked deprecated in March 2024. Its original maintainer and history were highly trusted, but there is nobody accepting fixes; this is the explicit plan-level exclusion.
- **Serde/strictness:** The compatibility benchmark: broad derived tagged/untagged enum behavior, working `deny_unknown_fields`, and duplicate-key errors with locations.
- **YAML/risk:** Anchors/aliases and explicit `Value::apply_merge`; a repetition cap mitigates alias bombs. It lacks configurable aggregate budgets and follows YAML 1.1 scalar rules, retaining the Norway problem.
- **Footprint/license:** Small: `indexmap`, `itoa`, `ryu`, `serde`, and unmaintained `unsafe-libyaml`. MIT OR Apache-2.0 ([current metadata](https://docs.rs/crate/serde_yaml/latest)).

### JSON-only in v0.1

- **Maintenance/trust:** `serde_json` is mature, actively maintained, and already planned. This has the smallest parser supply-chain risk, but contradicts W1's frozen “YAML + programmatic construction” acceptance scope and alias-normalization fixture unless the plan is deliberately revised.
- **Serde/strictness:** Complete tagged/untagged enums and `deny_unknown_fields`, with familiar errors. Duplicate struct fields error, but duplicate keys deserialized into maps/`Value` are last-wins; strict ingestion needs a duplicate-detecting pass.
- **Features/risk:** No anchors, aliases, merge keys, tags, or implicit typing, eliminating YAML-specific bombs and ambiguity. Still enforce input bytes, nesting, and collection-size limits around parsing.
- **Footprint/license:** No additional parser beyond planned `serde_json`. MIT OR Apache-2.0.

## Ranked recommendation

1. **`serde-saphyr`, deserialize-only, behind a wrapper**: best match for bounded hostile-ish input and corrective diagnostics; run W1 conformance and adversarial tests before pinning.
2. **JSON-only v0.1**: safest format, but only if maintainers explicitly amend W1 rather than silently violating it.
3. **`serde_norway`**: compatible fallback if activity resumes; currently too stale and less controllable.
4. **Raw `saphyr`**: healthy parser, but building and maintaining a Serde adapter is unjustified.
5. **`serde_yaml_ng`**: compatible but dormant, YAML 1.1, and tied to unmaintained `unsafe-libyaml`.
6. **`serde_yaml`**: archived baseline, forbidden by the plan.
7. **`serde-yml`**: deprecated, advisory history, and unacceptable provenance/maintainer trust.

Choose `serde-saphyr` because its typed, single-pass design, actionable snippets, duplicate rejection, strict boolean mode, explicit merge-key policy, and configurable parse/alias budgets line up with LLM-authored strict config far better than feature breadth does. W1 should pin deserialize-only features, reject multiple documents/merge keys/includes, cap raw bytes and all budgets well below library defaults, allow only bounded aliases, and test tagged and untagged nodes, unknown fields, duplicate keys, `NO`/`YES`/`ON`/`OFF`, deep nesting, alias bombs, and stable normalized JSON.

## Fallback plan

W1 must expose a crate-owned `definition::yaml::YamlParser` trait (or equivalently private `definition::yaml::parse<T>` module boundary) whose only public outcome is the crate's typed definition plus crate-owned structured diagnostics; no parser-specific `Value`, error, option, or tag type may escape. Keep parser configuration and YAML-to-diagnostic translation in that module, with shared conformance fixtures against the boundary. If the chosen crate becomes unmaintained, gains an advisory, misses two expected maintenance reviews, or fails a required fixture, freeze upgrades, evaluate the next ranked maintained implementation, run the same corpus/adversarial/hash tests, swap the adapter, and leave all call sites and canonical-JSON hashing untouched.
