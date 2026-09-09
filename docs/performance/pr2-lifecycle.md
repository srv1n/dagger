# PR #2 lifecycle measurement

Raw data: [`pr2-lifecycle-raw.jsonl`](pr2-lifecycle-raw.jsonl). The Markdown
below is derived from that one machine-readable record; do not treat it as a
replacement for the raw samples.

## Setup

- Baseline: `1eaed59f7e7da3c79b12d6de2bc2e919601c046e` (`1beb395` plus the
  definition, Map cancellation, and lint corrections).
- Candidate: `b1cae1a5182f9822e510566407d3792217834b3a` (PR #2
  `6be02ef5ea5197bb54a22e782ec9c184f73d1750` plus those same corrections).
- Both checkouts used a locally generated, identical lockfile:
  `0f56da12529978c9d6edf9524b9434024909941514f2a8e7b843ac2c2d9dec70`.
  The repository intentionally does not track `Cargo.lock`.
- macOS 26.5, arm64; rustc 1.97.1; cargo 1.97.1; default features; warmed
  `--release` binaries invoked directly outside build timing.

`perf_core --json` was run in five baseline/candidate alternating pairs. Each
lifecycle creates an in-memory store, puts bytes, holds the requested number
of capability clones, reads and verifies bytes, then drops the values.
Every raw measurement has `correctness_checked: true`.

## Lifecycle results

Medians below are across the five per-run medians. Lower latency and higher
throughput are better.

| Operation | Baseline | Candidate | Verdict |
| --- | ---: | ---: | --- |
| 64 B create/read/drop, 0 clones | 15.176 us, 65.9k/s | 14.862 us, 67.3k/s | +2.1% throughput; noise-sized |
| 64 B lifecycle, 8 clones | 16.814 us, 59.5k/s | 16.417 us, 60.9k/s | +2.4% throughput; noise-sized |
| 1 MiB lifecycle, 8 clones | 6.667 ms, 150.0/s | 6.240 ms, 160.3/s | improvement: +6.9% throughput |
| 1 MiB reference clone | 20.576 us, 48.6k/s | 0.163 us, 6.13M/s | improvement: +12,508% throughput |
| 4 MiB reference clone | 100.066 us, 10.0k/s | 0.163 us, 6.12M/s | improvement: +61,161% throughput |

The lifecycle improvement is deliberately much smaller than clone-only speed:
object creation, read verification, and hashing still dominate the full path.

## Completed workflow

The existing `w3_engine::map_nodes_expand_schedule_children_and_complete_parent_through_ticks`
fixture was run in seven warmed alternating pairs with the same in-memory
store, object store, action behavior, and release test binary configuration.
It asserts `RunState::Succeeded` and the exact ordered Map aggregate on every
invocation.

| Metric | Baseline | Candidate | Verdict |
| --- | ---: | ---: | --- |
| Median wall latency | 11.116 ms | 10.992 ms | -1.1%; noise |
| Range | 10.767–15.105 ms | 10.464–13.341 ms | overlapping |
| Terminal/output checks | 7/7 | 7/7 | PASS |

CPU time was attempted through macOS `times`, but every short child process
rounded to zero; it is unmeasured. Allocation counts and peak memory are also
unmeasured: no allocation profiler or comparable process-level peak-memory
tool was added for this narrow benchmark.

`yaml_pipeline` was not used for timing: it flakes before terminal state on
both sides (1/5 baseline and 2/5 candidate failures), and PR #2 changes neither
that example nor the engine paths. This is a pre-existing fixture issue, not a
PR #2 performance result.

## Verdict

PR #2 materially improves capability cloning and modestly improves the
clone-heavy 1 MiB lifecycle. The representative completed workflow is within
noise and has no regression. No additional production optimization is
recommended from these measurements.
