---
subject: Testing and development
keywords: [test, clippy, format, ci]
part_of: System overview
describes: [.github/workflows/ci.yml, dagger-workflow-core/tests, run_tests.sh, Makefile]
status: canonical
created: 2026-08-23
last_verified: 2026-08-23 @ ef3df5d2232f1a7b2365b99287e80f31b7d510ee
read_when: "You change code or prepare a review."
skip_when: "You only need a system overview."
---

# Testing and development

## Required checks

Run these commands after a code change:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test --doc --workspace --all-features
cargo doc --workspace --all-features --no-deps
```

CI runs these commands. `./run_tests.sh` also runs `cargo check` for all targets and features.

## Focused checks

Run all package tests:

```sh
cargo test -p dagger-workflow-core --all-features
```

Run one integration test:

```sh
cargo test -p dagger-workflow-core --features sqlite,conformance --test w6_sqlite
```

## Integration test areas

| Files | Behavior |
| --- | --- |
| `w1_definition.rs`, `w1_ids.rs` | IDs, parsing, validation, and publication. |
| `w2_action_contract.rs` | Action pins, inputs, outputs, and diagnostics. |
| `w3_engine.rs`, `w3_memory.rs` | Engine and in-memory store behavior. |
| `w4_map.rs` | Bounded Map behavior. |
| `w5_legal_research.rs` | Reference workflow execution. |
| `w6_sqlite.rs` | SQLite authority, scope, races, and query plans. |
| `w7_fs_object_store.rs` | Local file-system object storage. |
| `w8_recovery.rs` | Restart, takeover, fencing, and idempotency. |
| `w9_approvals.rs` | Durable approval gates. |
| `w10_budgets.rs` | Budgets, events, and run limits. |
| `w11_integrity.rs` | Committed-object integrity. |
| `w11d_crash_model.rs` | The stateful file-system crash model. |

## Documentation checks

Run these commands after a documentation or Tusker knowledge change:

```sh
tusker docs map
tusker docs status
tusker validate --vault ./.tusker --json
tusker skill doctor --strict --repo . --json
```

Use `rg` to check links and paths after a rename or deletion.

## Code bundle

Run `make codebasezip` only when a reviewer needs a repository bundle. The command writes a ZIP file to the ignored `artifacts/` directory.

Do not commit a generated bundle.
