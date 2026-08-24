.PHONY: agent-run-conformance codebasezip sqlite-write-load

agent-run-conformance:
	cargo test -p dagger-workflow-core --all-features --test agent_run_adversarial

sqlite-write-load:
	cargo test -p dagger-workflow-core --features sqlite --test w6_sqlite sqlite_write_path_load -- --ignored --exact --nocapture

codebasezip:
	@mkdir -p artifacts
	@repo="$$(basename "$$(git rev-parse --show-toplevel 2>/dev/null || pwd)")"; \
	stamp="$$(date -u +%Y%m%dT%H%M%SZ)"; \
	out="artifacts/$${repo}-codebase-$${stamp}.zip"; \
	tmp="$$(mktemp -d)"; \
	trap 'rm -rf "$$tmp"' EXIT; \
	git ls-files --cached --others --exclude-standard > "$$tmp/files.txt"; \
	printf '{\n  "repo": "%s",\n  "commit": "%s",\n  "branch": "%s",\n  "dirty": %s,\n  "created_at": "%s"\n}\n' \
	  "$$repo" \
	  "$$(git rev-parse HEAD 2>/dev/null || echo unknown)" \
	  "$$(git branch --show-current 2>/dev/null || echo unknown)" \
	  "$$(test -n "$$(git status --porcelain 2>/dev/null)" && echo true || echo false)" \
	  "$$stamp" > "$$tmp/MANIFEST.json"; \
	git status --short > "$$tmp/GIT_STATUS.txt" 2>/dev/null || true; \
	git diff --binary > "$$tmp/GIT_DIFF.patch" 2>/dev/null || true; \
	zip -q -y "$$out" -@ < "$$tmp/files.txt"; \
	zip -q -j "$$out" "$$tmp/MANIFEST.json" "$$tmp/GIT_STATUS.txt" "$$tmp/GIT_DIFF.patch"; \
	echo "$$out"
