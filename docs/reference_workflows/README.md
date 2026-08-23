# Reference workflows

These YAML files are workflow-core test fixtures.

- `intel_digest.yaml` covers actions, a Choice, and terminal nodes.
- `legal_research.yaml` covers the repository legal-research workflow.

The files contain placeholder action digests. Test code replaces the digests before publication.

Do not use the placeholder digests in a deployed workflow.

Run the fixture checks:

```sh
cargo test -p dagger-workflow-core --test w1_definition
cargo test -p dagger-workflow-core --test w5_legal_research
```
