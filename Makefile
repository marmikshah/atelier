# atelier — MCP-native headless pixel-art editor.
#
# This Makefile builds, tests, lints, and regenerates the generated tool docs —
# nothing else. Running the server, installing the daemon, and building the
# Docker image are direct commands (see the README); the binary's own
# subcommands are the interface (an installed user has no Makefile).

BIN := target/debug/atelier
DOC_HTML := target/atelier-tools.html

.DEFAULT_GOAL := help
.PHONY: help build release test fmt fmt-check lint rustdoc-check check pre-commit-checks docs docs-check clean

help: ## List available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

build: ## Debug build
	cargo build --locked -p atelier

release: ## Optimized release build → target/release/atelier
	cargo build --release --locked -p atelier

test: ## Run the complete test suite
	cargo test --locked

fmt: ## Format all sources
	cargo fmt --all

fmt-check: ## Check formatting without changing files
	cargo fmt --all -- --check

lint: ## Clippy with warnings denied
	cargo clippy --locked --all-targets -- -D warnings

rustdoc-check: ## Check public API documentation and intra-doc links
	RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps

check: pre-commit-checks test docs-check ## Complete non-mutating local/CI gate

pre-commit-checks: ## Release metadata + format + clippy + rustdoc gate run by git hooks
	$(MAKE) fmt-check
	tools/release-check.sh --current
	$(MAKE) lint
	$(MAKE) rustdoc-check

docs: build ## Generate a local HTML tool reference under target/
	$(BIN) tools --html > $(DOC_HTML)
	@echo "wrote $(DOC_HTML)"

docs-check: build ## Verify that the generated HTML tool reference renders
	@$(BIN) tools --html >/dev/null

clean: ## Remove build artifacts
	cargo clean
