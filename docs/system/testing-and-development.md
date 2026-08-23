---
subject: Testing and development
keywords: [test, clippy, format, ci, contribution]
part_of: overview
describes: [.github/workflows/ci.yml, tests, dagger-workflow-core/tests, Makefile]
status: canonical
created: 2026-08-23
last_verified: "2026-08-23 @ bdd41fa"
read_when: "You change code or prepare a review."
skip_when: "You only need a product overview."
---

# Testing and development

## Full check

Run the same commands as CI:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test --doc --workspace --all-features
cargo doc --workspace --all-features --no-deps
```

Run these commands after you finish the code edits. Fix all failures that your change causes.

## Focused checks

Test workflow core:

```sh
cargo test -p dagger-workflow-core --all-features
```

Test one workflow-core file:

```sh
cargo test -p dagger-workflow-core --features sqlite,conformance --test w6_sqlite
```

Test the root runtime:

```sh
cargo test -p dagger
```

## Test groups

The root `tests/` directory covers DAG Flow, Task Agent, Work Queue, and regression behavior.

The workflow-core test names show their main scope:

| Files | Main scope |
| --- | --- |
| `w1_*` | IDs, parsing, validation, and publication. |
| `w2_*` | Action contracts and outcomes. |
| `w3_*` | Engine and memory-store behavior. |
| `w4_map.rs` | Bounded Map behavior. |
| `w5_legal_research.rs` | Reference workflow execution. |
| `w6_sqlite.rs` | SQLite authority, scope, races, and query plans. |
| `w7_fs_object_store.rs` | File-system object storage. |
| `w8_recovery.rs` | Restart, takeover, and idempotency. |
| `w9_approvals.rs` | Durable approval gates. |
| `w10_budgets.rs` | Budgets and run ceilings. |
| `w11_integrity.rs` | Committed-object integrity. |
| `w11d_crash_model.rs` | Stateful file-system crash model. |

## Documentation check

Run:

```sh
tusker docs map
tusker docs status
tusker validate --vault ./.tusker --json
tusker skill doctor --strict --repo . --json
```

Check links and paths with `rg` before you remove or rename a document.

## Codebase bundle

Run `make codebasezip` only when a reviewer needs a repository bundle. The command creates a ZIP file in `artifacts/`.

Do not commit a new bundle unless the task requires it.
