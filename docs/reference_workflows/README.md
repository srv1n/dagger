# Reference workflows

The YAML files in this directory are test fixtures.

- `intel_digest.yaml` has actions, a Choice, and terminal nodes.
- `legal_research.yaml` has the legal-research workflow that the integration test runs.

The files contain placeholder action digests. Test code replaces the digests before publication.

Do not publish the placeholder values.

```sh
cargo test -p dagger-workflow-core --test w1_definition
cargo test -p dagger-workflow-core --test w5_legal_research
```
