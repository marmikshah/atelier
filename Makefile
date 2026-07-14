# atelier — MCP-native headless pixel-art editor.
#
# This Makefile builds, tests, lints, and regenerates the generated tool docs —
# nothing else. Running the server, installing the daemon, and building the
# Docker image are direct commands (see the README); the binary's own
# subcommands are the interface (an installed user has no Makefile).

BIN := target/release/atelier

.DEFAULT_GOAL := help
.PHONY: help build release test fmt lint check pre-commit-checks docs docs-check clean

help: ## List available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

build: ## Debug build
	cargo build

release: ## Optimized release build → target/release/atelier
	cargo build --release

test: ## Run the test suite
	cargo test

fmt: ## Format all sources
	cargo fmt --all

lint: ## Clippy with warnings denied
	cargo clippy --all-targets -- -D warnings

check: fmt lint test ## Format + clippy + tests (use before committing)

pre-commit-checks: ## Format-check + clippy gate — exactly what the git hooks run.
	cargo fmt --all -- --check
	cargo clippy --all-targets -- -D warnings

docs: release ## Regenerate the HTML tool reference (tools.html) from the live registry
	$(BIN) tools --html > tools.html
	@echo "wrote tools.html"

docs-check: release ## Fail if tools.html is stale (CI drift guard)
	@$(BIN) tools --html | diff -q - tools.html >/dev/null || { \
		echo "tools.html is stale — run 'make docs' and commit"; exit 1; }

clean: ## Remove build artifacts
	cargo clean
