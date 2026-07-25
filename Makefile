# atelier — MCP-native headless pixel-art editor.
#
# This Makefile builds, tests, lints, and regenerates the generated tool docs —
# nothing else. Running the server, installing the daemon, and building the
# Docker image are direct commands (see the README); the binary's own
# subcommands are the interface (an installed user has no Makefile).

BIN := target/debug/atelier

.DEFAULT_GOAL := help
.PHONY: help build release test fmt fmt-check lint rustdoc-check check pre-commit-checks showcase-check docs docs-check site clean

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

check: pre-commit-checks test docs-check showcase-check ## Complete non-mutating local/CI gate

pre-commit-checks: ## Release metadata + format + clippy + rustdoc gate run by git hooks
	$(MAKE) fmt-check
	tools/release-check.sh --current
	$(MAKE) lint
	$(MAKE) rustdoc-check

showcase-check: build ## Replay all showcase journals and compare their exported GIFs
	tools/test-showcase-replays.sh

docs: build ## Regenerate the HTML tool reference (tools.html) from the live registry
	$(BIN) tools --html > site/tools.html
	@echo "wrote tools.html"

docs-check: build ## Verify that the generated HTML tool reference renders
	@$(BIN) tools --html >/dev/null

site: docs ## Assemble generated files into site/ for preview or Pages
	cp benchmarks/runs.json site/showcase/runs.json

clean: ## Remove build artifacts
	cargo clean
