# Portable standard commands shared by every service derived from this
# template. Service-specific commands belong in make/service.mk, never here.
#
# `--locked` refuses to update Cargo.lock implicitly, so a build never
# silently resolves different versions than the committed lockfile.

SHELL := /bin/sh
.DEFAULT_GOAL := help

CARGO ?= cargo
CARGO_FLAGS ?= --locked
SERVICE_BIN ?= service

.PHONY: help build run test test-package fmt fmt-check lint check clean

help: ## List available commands
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z0-9_-]+:.*## / {printf "  %-14s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Build every workspace crate in debug mode
	$(CARGO) build --workspace $(CARGO_FLAGS)

run: ## Start the HTTP service locally
	$(CARGO) run -p $(SERVICE_BIN) $(CARGO_FLAGS)

test: ## Run the ordinary workspace unit-test suite
	$(CARGO) test --workspace $(CARGO_FLAGS)

test-package: ## Run one crate's tests; requires PKG=<crate name>
	@test -n "$(PKG)" || { echo "test-package requires PKG=<crate name>" >&2; exit 2; }
	$(CARGO) test -p $(PKG) $(CARGO_FLAGS)

fmt: ## Format every crate
	$(CARGO) fmt --all

fmt-check: ## Fail when formatting differs from rustfmt output
	$(CARGO) fmt --all --check

lint: ## Clippy over all targets, warnings are errors
	$(CARGO) clippy --workspace --all-targets $(CARGO_FLAGS) -- -D warnings

check: fmt-check lint test ## Full local gate: formatting, lint, tests

clean: ## Remove build output
	$(CARGO) clean
