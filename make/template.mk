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
# Baseline configuration for `make run`; deployments pass their own file or
# rely on APP__* environment variables.
LOCAL_CONFIG ?= env/config/local.toml

# The OpenAPI document is generated from the Rust contract by the `openapi`
# binary and committed; `make test` refuses a stale copy. Redocly CLI lints
# the committed file (Node.js via npx), oasdiff compares it with the
# pull-request base (Go via `go run`). Versions are pinned here until the
# delivery stage introduces one tool manifest.
OPENAPI_FILE := api/openapi/service.yaml
OPENAPI_BREAKING_APPROVALS ?= api/openapi/breaking-changes-approvals.txt
REDOCLY_CLI_VERSION := 2.53.3
REDOCLY_CLI ?= npx --yes @redocly/cli@$(REDOCLY_CLI_VERSION)
OASDIFF_VERSION := 1.32.1
OASDIFF ?= go run github.com/oasdiff/oasdiff@v$(OASDIFF_VERSION)

.PHONY: help build run test test-package fmt fmt-check lint check check-skills clean \
	openapi-generate openapi-check openapi-lint openapi-breaking

help: ## List available commands
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z0-9_-]+:.*## / {printf "  %-14s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Build every workspace crate in debug mode
	$(CARGO) build --workspace $(CARGO_FLAGS)

run: ## Start the HTTP service locally with env/config/local.toml
	$(CARGO) run -p $(SERVICE_BIN) $(CARGO_FLAGS) -- --config $(LOCAL_CONFIG)

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

check-skills: ## Validate the shape of .agents/skills (frontmatter, budget, links)
	python3 scripts/check-skills.py

openapi-generate: ## Regenerate api/openapi/service.yaml from the Rust contract
	@tmp="$$(mktemp)" && $(CARGO) run -q -p $(SERVICE_BIN) --bin openapi $(CARGO_FLAGS) > "$$tmp" && mv "$$tmp" $(OPENAPI_FILE)

openapi-check: openapi-lint ## Fail when the committed document is stale or fails lint
	$(CARGO) test -p $(SERVICE_BIN) $(CARGO_FLAGS) --test openapi

openapi-lint: ## Lint and validate the committed document with Redocly CLI
	@command -v npx >/dev/null 2>&1 || { echo "openapi-lint requires Node.js (npx) for @redocly/cli@$(REDOCLY_CLI_VERSION)" >&2; exit 2; }
	REDOCLY_SUPPRESS_UPDATE_NOTICE=true REDOCLY_TELEMETRY=off npm_config_prefer_offline=true $(REDOCLY_CLI) lint --config .redocly.yaml $(OPENAPI_FILE)

openapi-breaking: ## Compare the document with BASE_OPENAPI=<file> for breaking changes
	@test -n "$(BASE_OPENAPI)" || { echo "openapi-breaking requires BASE_OPENAPI=<path to the base document>" >&2; exit 2; }
	@command -v go >/dev/null 2>&1 || { echo "openapi-breaking requires Go for oasdiff@v$(OASDIFF_VERSION)" >&2; exit 2; }
	@if [ -s "$(OPENAPI_BREAKING_APPROVALS)" ]; then \
		$(OASDIFF) breaking --fail-on ERR --err-ignore "$(OPENAPI_BREAKING_APPROVALS)" "$(BASE_OPENAPI)" $(OPENAPI_FILE); \
	else \
		$(OASDIFF) breaking --fail-on ERR "$(BASE_OPENAPI)" $(OPENAPI_FILE); \
	fi

check: fmt-check lint test openapi-lint check-skills ## Full local gate: formatting, lint, tests, OpenAPI lint, skills

clean: ## Remove build output
	$(CARGO) clean
