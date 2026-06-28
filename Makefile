# atelier — MCP-native headless pixel-art editor.
#
# Bare `make` builds release and runs the HTTP MCP server (the default).
# `make help` lists everything.

BIN  := target/release/atelier
BIND ?= 127.0.0.1:8765
HOME_DIR ?= $(HOME)/.atelier

.DEFAULT_GOAL := run
.PHONY: help run serve stdio build release test fmt lint check pre-commit-checks branding hooks clean install daemon daemon-status daemon-uninstall

help: ## List available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

run: serve ## Default: release build, then run the HTTP MCP server

serve: release ## Run the streamable HTTP MCP server (BIND, HOME_DIR overridable)
	@echo "atelier HTTP MCP → http://$(BIND)/mcp   (home: $(HOME_DIR))"
	ATELIER_HOME=$(HOME_DIR) $(BIN) --http $(BIND)

stdio: release ## Run the stdio MCP server (client spawns the binary)
	ATELIER_HOME=$(HOME_DIR) $(BIN)

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

branding: ## Regenerate the brand art (the gallery is entirely recipe-made).
	@echo "atelier's README art is agent-made; replay docs/examples/*.json to regenerate."

hooks: ## Point git at the canonical .githooks (pre-commit + pre-push).
	git config core.hooksPath .githooks
	@echo "✓ core.hooksPath → .githooks"

clean: ## Remove build artifacts
	cargo clean

daemon: release ## Install + start the background daemon (launchd / systemd --user)
	$(BIN) service install --bind $(BIND) --home $(HOME_DIR)

daemon-status: ## Show daemon state
	$(BIN) service status

daemon-uninstall: ## Stop + remove the daemon
	$(BIN) service uninstall

install: release ## Print the command to register with Claude Code over HTTP
	@echo "1) start the server:  make serve   (or 'make daemon' for background)"
	@echo "2) register client:   claude mcp add --transport http atelier http://$(BIND)/mcp"
