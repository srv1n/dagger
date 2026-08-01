# ChatGPT Handoff Profile

Use this file for repo-specific handoff hints that should not live in the reusable skill.

## Project routing

- ChatGPT Project: set in ../.chatgpt-handoff.json
- Preferred handoff kinds: review, dev, design, architect

## Repo quirks

- Add build, test, packaging, or review constraints here.
- Add known risky paths or generated files to avoid here.
- Keep durable task state in Tusker when this repo uses Tusker.

## Do not put here

- Active ChatGPT thread ids; those belong in .chatgpt-handoff/threads.jsonl.
- Browser session details or secrets.
- Generic improvements to the handoff workflow; patch the canonical skill instead.
