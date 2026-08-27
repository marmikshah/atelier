# atelier — MCP-native headless pixel-art editor.
#
# This Makefile builds, tests, lints, and regenerates the generated tool docs —
# nothing else. Running the server, installing the daemon, and building the
# Docker image are direct commands (see the README); the binary's own
# subcommands are the interface (an installed user has no Makefile).

BIN := target/debug/atelier
DOC_MD := docs/tools.md

.DEFAULT_GOAL := help
.PHONY: help build release test fmt fmt-check lint rustdoc-check check pre-commit-checks docs docs-check showcase-check clean

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

docs: build ## Regenerate the committed Markdown tool reference
	$(BIN) tools --markdown > $(DOC_MD)
	@echo "wrote $(DOC_MD)"

docs-check: build ## Verify the committed tool reference matches the registry
	@$(BIN) tools --markdown | diff -u $(DOC_MD) - \
		|| { echo "docs/tools.md is stale — run 'make docs'"; exit 1; }

showcase-check: build ## Replay all 70 showcase recipes and compare GIFs byte-for-byte
	tools/showcase-check.sh

clean: ## Remove build artifacts
	cargo clean
