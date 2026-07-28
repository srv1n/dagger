# Reference workflows

These files are definition-format `0.1` authoring fixtures for W5 and W9.
Their digest strings are deterministic, sha256-shaped placeholders derived
from fixture names. They are not publishable until W1/W2 provides the matching
durable supported-subset SchemaDocument objects and mock registry entries.

## Bounded legal research

File: `legal_research.yaml`

Run input is `{ "legal_question": string }`. The root output is the canonical
ArtifactRef object returned at `/report_artifact_ref` by citation validation.
Initial and follow-up query arrays both have `maxItems: 3` in their fixture
schemas; the two Map nodes independently repeat that bound with
`max_items: 3`.

### Required mock actions

| Action name | One-line fixture contract |
|---|---|
| `legal.generate_initial_queries` | Deterministic LLM fixture: input `{ question, max_queries }`; output `{ queries }`, a string array of length 0 through 3. |
| `legal.search` | Input `{ question, query, round }`; output one canonical evidence object containing the query, source records, and excerpts. |
| `legal.summarize_evidence` | Input `{ question, evidence }`; output `{ findings, gaps, needs_second_round }`, with `needs_second_round` Boolean. |
| `legal.generate_followup_queries` | Deterministic LLM fixture: input `{ question, gaps, max_queries }`; output `{ queries }`, a string array of length 0 through 3. |
| `legal.merge_second_round` | Input `{ question, initial_summary, second_round_evidence }`; output a merged `{ findings, gaps, needs_second_round }` summary. |
| `legal.synthesize_report` | Input `{ question, initial_summary, second_round_binding_status }`; output `{ draft_report, citation_claims }`. |
| `legal.validate_citations` | Input `{ question, draft_report, citation_claims }`; output `{ report_artifact_ref }`, where the value is a canonical ArtifactRef. |

The mock `legal.generate_initial_queries` and
`legal.generate_followup_queries` implementations must enforce the requested
bound even though the corresponding output schemas also cap the arrays.
`legal.search` is reused by both Maps with the same five-field action pin.

### Binding paths

| Consumer | Target or value | Source |
|---|---|---|
| `generate_initial_queries` | `/question` | Run input `/legal_question` |
| `generate_initial_queries` | `/max_queries` | Constant `3` |
| `search_initial_queries.items` | Whole items array | `generate_initial_queries` output `/queries` |
| Initial Map child | `/query` | Whole `map_item` at pointer `""` |
| Initial Map child | `/question` | Run input `/legal_question` |
| Initial Map child | `/round` | Constant `1` |
| `summarize_initial_evidence` | `/evidence` | Whole `search_initial_queries` aggregate at pointer `""` |
| `summarize_initial_evidence` | `/question` | Run input `/legal_question` |
| `choose_second_round.input` | Whole Choice input | Whole `summarize_initial_evidence` output at pointer `""` |
| `choose_second_round.selector` | Selected scalar | `/needs_second_round` within the bound Choice input |
| `generate_followup_queries` | `/gaps` | `summarize_initial_evidence` output `/gaps` |
| `generate_followup_queries` | `/max_queries` | Constant `3` |
| `generate_followup_queries` | `/question` | Run input `/legal_question` |
| `search_followup_queries.items` | Whole items array | `generate_followup_queries` output `/queries` |
| Follow-up Map child | `/query` | Whole `map_item` at pointer `""` |
| Follow-up Map child | `/question` | Run input `/legal_question` |
| Follow-up Map child | `/round` | Constant `2` |
| `merge_second_round` | `/initial_summary` | Whole `summarize_initial_evidence` output at pointer `""` |
| `merge_second_round` | `/question` | Run input `/legal_question` |
| `merge_second_round` | `/second_round_evidence` | Whole `search_followup_queries` aggregate at pointer `""` |
| `synthesize_report` | `/initial_summary` | Whole `summarize_initial_evidence` output at pointer `""` |
| `synthesize_report` | `/question` | Run input `/legal_question` |
| `synthesize_report` | `/second_round_binding_status` | Constant `unavailable_after_choice_reconvergence_in_v0_1` |
| `validate_citations` | `/citation_claims` | `synthesize_report` output `/citation_claims` |
| `validate_citations` | `/draft_report` | `synthesize_report` output `/draft_report` |
| `validate_citations` | `/question` | Run input `/legal_question` |
| `research_succeeded.output` | Whole run output | `validate_citations` output `/report_artifact_ref` |

## Scheduled intelligence digest

File: `intel_digest.yaml`

The host owns scheduling and creates a run with `{ "trigger": object }`.
The root output is the canonical ArtifactRef returned at
`/published_artifact_ref` by the idempotent publisher. The three feed nodes
become Ready in the same frontier reduction after `prepare_trigger`; the
normalizer is their ordinary fan-in consumer and is not an artificial join
gate.

### Required mock actions

| Action name | One-line fixture contract |
|---|---|
| `intel.prepare_trigger` | Input `{ trigger }`; output `{ trigger }` unchanged, providing the single-entry fan-out required by format `0.1`. |
| `intel.fetch_feed_alpha` | Input `{ feed_name, trigger }`; output `{ items }` for the alpha fixture feed. |
| `intel.fetch_feed_beta` | Input `{ feed_name, trigger }`; output `{ items }` for the beta fixture feed; its W9 implementation fails retryably on the first attempt. |
| `intel.fetch_feed_gamma` | Input `{ feed_name, trigger }`; output `{ items }` for the gamma fixture feed. |
| `intel.normalize_deduplicate` | Input `{ feeds: { alpha, beta, gamma }, trigger }`; output `{ retained_items, stats }` with deterministic ordering and deduplication. |
| `intel.summarize_item` | Input `{ item, item_index, trigger }`; output one normalized item summary. |
| `intel.compile_report` | Input `{ deduplication_stats, summaries, trigger }`; output `{ report, approval_request }`. |
| `intel.publish` | Input `{ approval, channel, report }`; idempotently publishes using `ActionContext.idempotency_key` and outputs `{ published_artifact_ref }`. |

`intel.publish` must treat the engine-provided logical-node idempotency key as
the external publication key. The `approval` input is the exact engine-owned
ApprovalResult envelope, not action-defined approval data.

### Binding paths

| Consumer | Target or value | Source |
|---|---|---|
| `prepare_trigger` | `/trigger` | Run input `/trigger` |
| Each feed fetch | `/feed_name` | Constants `alpha`, `beta`, and `gamma`, respectively |
| Each feed fetch | `/trigger` | `prepare_trigger` output `/trigger` |
| `normalize_and_deduplicate` | `/feeds/alpha` | `fetch_feed_alpha` output `/items` |
| `normalize_and_deduplicate` | `/feeds/beta` | `fetch_feed_beta` output `/items` |
| `normalize_and_deduplicate` | `/feeds/gamma` | `fetch_feed_gamma` output `/items` |
| `normalize_and_deduplicate` | `/trigger` | `prepare_trigger` output `/trigger` |
| `summarize_retained_items.items` | Whole items array | `normalize_and_deduplicate` output `/retained_items` |
| Map child | `/item` | Whole `map_item` at pointer `""` |
| Map child | `/item_index` | `map_index` |
| Map child | `/trigger` | `prepare_trigger` output `/trigger` |
| `compile_report` | `/deduplication_stats` | `normalize_and_deduplicate` output `/stats` |
| `compile_report` | `/summaries` | Whole `summarize_retained_items` aggregate at pointer `""` |
| `compile_report` | `/trigger` | `prepare_trigger` output `/trigger` |
| `approve_report.request` | Whole approval request | `compile_report` output `/approval_request` |
| `publish_report` | `/approval` | Whole `approve_report` ApprovalResult at pointer `""` |
| `publish_report` | `/channel` | Constant `fixture-intelligence-digest` |
| `publish_report` | `/report` | `compile_report` output `/report` |
| `digest_succeeded.output` | Whole run output | `publish_report` output `/published_artifact_ref` |

## Authoring-friction log

These are format problems encountered while authoring the fixtures, not
proposals silently encoded as extension fields.

1. **Conditional data cannot cross Choice reconvergence.** Section 8.3
   requires every `node_output` binding source to dominate its consumer and
   explicitly says v0.1 has no phi or branch-value merge. Section 9 skips the
   unselected branch. Consequently, the shared `synthesize_report` node cannot
   bind `merge_second_round` output: that node is skipped on the default path.
   This conflicts with W5's stronger intent that the conditional second-round
   evidence feed one shared synthesis. The YAML keeps the required unrolled
   round and reconvergence contract-valid, but the merged result is not
   available to final synthesis; the explicit status constant makes that loss
   visible rather than pretending the value was merged.

2. **A single entry cannot directly fan in run input to three roots.**
   Section 14.1 requires one `entry_node_id`, and section 14.2 requires every
   node to be reachable from it. The format has no virtual Start node. W9's
   three initially parallel feed actions therefore require the otherwise
   redundant `intel.prepare_trigger` Action to fan out three normal edges.

3. **"Explicit per-field bindings" is not uniform across node kinds.**
   Section 8.1 defines ordered target assignments for Action and Map-child
   inputs, but section 14.1 models Choice `input`, Map `items`, Approval
   `request`, and Succeed `output` as one `value_source`, not a binding array.
   Those nodes can select one whole value or pointer but cannot assemble a
   composite object from several sources.

4. **Map is an Action fan-out, not a mapped subworkflow.** Sections 10.1 and
   14.1 allow exactly one pinned child Action per Map. The legal workflow must
   unroll query generation, Map search, and merge as separate static nodes;
   none of that sequence can live inside a Map body.

5. **Reference digests are insufficient for publication.** Sections 13.1,
   13.2, 14.1, and 14.3 require every root and action schema digest to resolve
   to a durable supported-subset SchemaDocument and every action pin to match
   a registry implementation on all five fields. The requested deliverables
   contain no schema documents or registry fixture, so the deterministic
   digests are intentionally non-publishable placeholders until W1/W2/W5/W9
   installs those objects.

6. **Scheduling is outside the definition.** W9 calls the digest scheduled,
   but W12 explicitly makes cron and event triggers host-owned, while section
   14.2 lists no trigger field. The YAML can only bind the host-supplied
   `/trigger` run input; it cannot declare a schedule.

7. **Approval output is fixed and cannot carry the compiled report.**
   Sections 3.5 and 14.1 make a successful Approval output the engine-owned
   ApprovalResult envelope. `publish_report` must therefore bind the compiled
   report separately from `compile_report` and bind the approval envelope from
   `approve_report`; an author cannot shape a combined gate output.

8. **Approval rejection and rejecting expiry have no graph edge.** Sections
   3.5 and 14.2 define rejection as a run-terminal transition and give
   Approval only a success `next` array. The definition cannot route rejection
   to an explicit Fail node or an audit action. The authored graph's sole
   maximal control path ends at its one Succeed node, while rejection
   terminalizes through R07 outside authorable topology.

9. **Failure handling cannot be authored.** Section 14.2 rejects error edges,
   catches, and tolerance thresholds. Search, feed, summarization, validation,
   and publication failures therefore fail or exhaust the run according to
   the closed runtime transitions; neither reference workflow can attach a
   cleanup, fallback, or failure-reporting branch.

10. **Action execution traits are registry conventions, not definition
    fields.** Sections 13.2 and 14.1 let an author pin an action but provide no
    field that marks it as LLM-backed or externally idempotent. W5's LLM query
    generation is therefore a property of the two registered mock actions,
    while W9's idempotent publication depends on the mock publisher honoring
    the section 7.3 `ActionContext.idempotency_key` obligation.
