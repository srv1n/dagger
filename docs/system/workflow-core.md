---
subject: Workflow core
keywords: [bounded engine, scheduler, actions, state]
part_of: overview
describes: [dagger-workflow-core/src, dagger-workflow-core/examples, dagger-workflow-core/tests]
status: canonical
created: 2026-08-23
last_verified: "2026-08-23 @ bdd41fa"
read_when: "You build or review dagger-workflow-core."
skip_when: "You work on the root dagger package."
---

# Workflow core

Workflow core executes a published workflow revision. It keeps data for each customer separate.

The code calls this boundary an `ExecutionScope`. It contains a tenant ID and a namespace. The store uses the scope with each durable ID.

## Main flow

1. Parse a JSON or YAML definition.
2. Validate the graph and all fields.
3. Resolve schemas and action pins.
4. Publish an immutable revision.
5. Put the run input in the object store.
6. Create the run and its static nodes.
7. Acquire the engine claim for the scope.
8. Start the run.
9. Call `tick` until the run stops or waits.
10. Read the final output through the committed-object reader.

The examples contain complete host code. Start with `dagger-workflow-core/examples/yaml_pipeline.rs`.

## Node types

| Node | Behavior |
| --- | --- |
| `Action` | Calls a registered implementation. |
| `Map` | Creates a bounded action child for each array item. |
| `Choice` | Selects the first matching case or the required default. |
| `Approval` | Waits for an authorized decision or an expiry result. |
| `Succeed` | Commits the one root output. |
| `Fail` | Commits an explicit domain failure. |

A definition must have exactly one reachable `Succeed` node. It can have more than one `Fail` node.

## Action contract

An action supplies an `ActionDescriptor`. The descriptor contains its name, contract version, input schema digest, output schema digest, and implementation compatibility digest.

The engine builds canonical input bytes. The action returns one `ActionOutcome`.

An outcome can succeed, fail for a retryable reason, fail permanently, or report a contract failure. A successful outcome can include artifacts, cost, and diagnostics.

The engine checks the output before it commits the result.

## Engine claim

One live engine generation owns one scope. `acquire_scope` gets the claim. `heartbeat_scope` renews it. `release_scope` gives it up.

The claim prevents two scheduler generations from changing the same scope at the same time.

## Recovery

The store records attempts, retries, deadlines, approvals, events, and command receipts.

After a takeover, the engine recovers lower-generation attempts. It does not treat an unknown action outcome as success.

The action receives an idempotency key and a completion credential. A late completion must pass the completion fence.

## Host duties

The host must supply authenticated principals for approval decisions.

The host must keep action pins available for active runs. The engine blocks an incompatible run instead of selecting a different implementation.

The host must set run limits and a budget before it creates a run.

The host must not put raw secrets in workflow definitions, action input, diagnostics, or events.
