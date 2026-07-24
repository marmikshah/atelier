# atelier — MCP-native headless pixel-art editor.
#
# This Makefile builds, tests, lints, and regenerates the generated tool docs —
# nothing else. Running the server, installing the daemon, and building the
# Docker image are direct commands (see the README); the binary's own
# subcommands are the interface (an installed user has no Makefile).

BIN := target/release/atelier

.DEFAULT_GOAL := help
.PHONY: help build release test fmt lint rustdoc-check check pre-commit-checks docs docs-check clean

help: ## List available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

build: ## Debug build
	cargo build --locked -p atelier

release: ## Optimized release build → target/release/atelier
	cargo build --release --locked -p atelier

test: ## Run the test suite (unit tests + example-recipe replays)
	cargo test --locked
	cargo build --locked -p atelier
	tools/test-examples.sh

fmt: ## Format all sources
	cargo fmt --all

lint: ## Clippy with warnings denied
	cargo clippy --locked --all-targets -- -D warnings

rustdoc-check: ## Check public API documentation and intra-doc links
	RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps

check: fmt lint rustdoc-check test ## Format + clippy + rustdoc + tests

pre-commit-checks: ## Release metadata + format + clippy + rustdoc gate run by git hooks
	cargo fmt --all -- --check
	tools/release-check.sh --current
	cargo clippy --locked --all-targets -- -D warnings
	RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps

docs: release ## Regenerate the HTML tool reference (tools.html) from the live registry
	$(BIN) tools --html > site/tools.html
	@echo "wrote tools.html"

docs-check: release ## Fail if tools.html is stale (CI drift guard)
	@$(BIN) tools --html | diff -q - site/tools.html >/dev/null || { \
		echo "tools.html is stale — run 'make docs' and commit"; exit 1; }

clean: ## Remove build artifacts
	cargo clean
