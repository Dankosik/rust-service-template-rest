SHELL := /bin/sh
.DEFAULT_GOAL := help

include tools/versions.env

SERVICE_PACKAGE ?= service
SERVICE_BIN ?= service
RUNTIME_IMAGE ?= service:ci
CONTAINER_IMAGE ?= $(RUNTIME_IMAGE)
LOCAL_CONFIG ?= env/config/local.toml
OPENAPI_FILE ?= api/openapi/service.yaml

CARGO ?= cargo
CARGO_FLAGS ?= --locked
BASE_REF ?= origin/main
export CARGO CARGO_FLAGS BASE_REF
export PYTHONDONTWRITEBYTECODE := 1

# template:begin source-template:make-initializer-inputs
ifneq (,$(filter template-init,$(MAKECMDGOALS)))
TEMPLATE_INIT_VARS := SERVICE_NAME REPOSITORY DESCRIPTION CODEOWNER DATABASE AUTHN \
	OUTBOUND_HTTP OUTBOUND_AUTH GRPC HTTP_IDEMPOTENCY JOBS MESSAGING OUTBOX \
	WEBHOOKS INBOUND_WEBHOOKS CACHE OBJECT_STORAGE SERVICE_AUTH AGENT_HARNESS
DATABASE ?= none
AUTHN ?= none
OUTBOUND_HTTP ?= none
OUTBOUND_AUTH ?= none
GRPC ?= none
HTTP_IDEMPOTENCY ?= none
JOBS ?= none
MESSAGING ?= none
OUTBOX ?= none
WEBHOOKS ?= none
INBOUND_WEBHOOKS ?= none
CACHE ?= none
OBJECT_STORAGE ?= none
SERVICE_AUTH ?= none
AGENT_HARNESS ?= all
$(foreach var,$(TEMPLATE_INIT_VARS),$(eval override $(var) := $$(value $(var))))
export $(TEMPLATE_INIT_VARS)
endif
# template:end source-template:make-initializer-inputs

TOOLS_ROOT ?= $(abspath $(or $(shell git rev-parse --git-common-dir 2>/dev/null),.git))/tools
export TOOLS_ROOT
ifeq ($(CI),true)
CARGO_DENY ?= cargo-deny
CARGO_NEXTEST ?= cargo-nextest
CARGO_SHEAR ?= cargo-shear
TYPOS ?= typos
ZIZMOR ?= zizmor
else
CARGO_DENY ?= $(TOOLS_ROOT)/cargo-deny-$(CARGO_DENY_VERSION)/bin/cargo-deny
CARGO_NEXTEST ?= $(TOOLS_ROOT)/cargo-nextest-$(CARGO_NEXTEST_VERSION)/bin/cargo-nextest
CARGO_SHEAR ?= $(TOOLS_ROOT)/cargo-shear-$(CARGO_SHEAR_VERSION)/bin/cargo-shear
TYPOS ?= $(TOOLS_ROOT)/typos-cli-$(TYPOS_VERSION)/bin/typos
ZIZMOR ?= $(TOOLS_ROOT)/zizmor-$(ZIZMOR_VERSION)/bin/zizmor
endif
CARGO_MUTANTS ?= $(TOOLS_ROOT)/cargo-mutants-$(CARGO_MUTANTS_VERSION)/bin/cargo-mutants
NEXTEST_PATH = $(if $(filter $(TOOLS_ROOT)/%,$(CARGO_NEXTEST)),env PATH="$(dir $(CARGO_NEXTEST)):$$PATH")
NEXTEST_TEST_THREADS ?= $(shell echo $$((2 * $$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2))))
NEXTEST_RUN = $(NEXTEST_PATH) $(CARGO) nextest run --no-tests=pass --test-threads=$(NEXTEST_TEST_THREADS) $(CARGO_FLAGS)

NPX := npm_config_ignore_scripts=true npm_config_prefer_offline=true npm_config_audit=false npm_config_fund=false npm_config_update_notifier=false npx --yes
OASDIFF ?= go run github.com/oasdiff/oasdiff@v$(OASDIFF_VERSION)
GITLEAKS ?= go run github.com/zricethezav/gitleaks/v8@v$(GITLEAKS_VERSION)
GITLEAKS_FLAGS := --no-banner --redact --verbose --exit-code 1 --config .gitleaks.toml
ACTIONLINT ?= go run github.com/rhysd/actionlint/cmd/actionlint@v$(ACTIONLINT_VERSION)
RUFF ?= $(if $(shell command -v uvx 2>/dev/null),uvx ruff@$(RUFF_VERSION),pipx run ruff==$(RUFF_VERSION))
SHELL_FILES = $(wildcard $(shell git ls-files --cached --others --exclude-standard -- '*.sh'))
MARKDOWN_FILES = $(wildcard $(shell git ls-files --cached --others --exclude-standard -- '*.md' ':(exclude)vendor/**' ':(exclude).agents/adapters/**' ':(exclude)specs/**'))

INTEGRATION_LINT_FEATURES ?=
CHECK_EXTRAS ?=

# --- Core development ---

.PHONY: help build run test test-package fmt fmt-check lint lint-package profiling-check mutants clean check

help: ## List available commands
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z0-9_-]+:.*## / {printf "  %-28s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Build every workspace crate in debug mode
	$(CARGO) build --workspace $(CARGO_FLAGS)

run: ## Start the HTTP service locally with env/config/local.toml
	$(CARGO) run -p $(SERVICE_BIN) $(CARGO_FLAGS) -- --config $(LOCAL_CONFIG)

test: $(filter $(TOOLS_ROOT)/%,$(CARGO_NEXTEST)) ## Run workspace unit tests and doctests
	$(NEXTEST_RUN) --workspace
	$(CARGO) test --doc --workspace --no-fail-fast $(CARGO_FLAGS)

test-package: $(filter $(TOOLS_ROOT)/%,$(CARGO_NEXTEST)) ## Run one crate's tests; requires PKG=<crate>
	@test -n "$(PKG)" || { echo "test-package requires PKG=<crate>" >&2; exit 2; }
	$(NEXTEST_RUN) -p $(PKG)
	$(CARGO) test --doc -p $(PKG) --no-fail-fast $(CARGO_FLAGS)

fmt: ## Format every crate
	$(CARGO) $(CARGO_FLAGS) fmt --all
	@if [ -f tools/grpc-codegen/Cargo.toml ]; then $(CARGO) $(CARGO_FLAGS) fmt --manifest-path tools/grpc-codegen/Cargo.toml --all; fi

fmt-check: ## Check formatting against rustfmt
	$(CARGO) $(CARGO_FLAGS) fmt --all --check
	@if [ -f tools/grpc-codegen/Cargo.toml ]; then $(CARGO) $(CARGO_FLAGS) fmt --manifest-path tools/grpc-codegen/Cargo.toml --all --check; fi

lint lint-package: export RUSTDOCFLAGS := $(RUSTDOCFLAGS) -D warnings

lint: ## Run Clippy and rustdoc over all workspace targets
	$(CARGO) clippy --workspace --all-targets --keep-going $(INTEGRATION_LINT_FEATURES) $(CARGO_FLAGS) -- -D warnings
	$(CARGO) doc --keep-going --workspace --no-deps --document-private-items $(INTEGRATION_LINT_FEATURES) $(CARGO_FLAGS)
	@if [ -f tools/grpc-codegen/Cargo.toml ]; then \
		CARGO_TARGET_DIR="$${CARGO_TARGET_DIR:-$(CURDIR)/target/grpc-codegen}" $(CARGO) clippy --manifest-path tools/grpc-codegen/Cargo.toml --all-targets --keep-going $(CARGO_FLAGS) -- -D warnings && \
		CARGO_TARGET_DIR="$${CARGO_TARGET_DIR:-$(CURDIR)/target/grpc-codegen}" $(CARGO) doc --keep-going --manifest-path tools/grpc-codegen/Cargo.toml --no-deps --document-private-items $(CARGO_FLAGS); \
	fi

lint-package: ## Run Clippy and rustdoc on one crate; requires PKG=<crate>
	@test -n "$(PKG)" || { echo "lint-package requires PKG=<crate>" >&2; exit 2; }
	$(CARGO) clippy -p $(PKG) --all-targets --keep-going $(CARGO_FLAGS) -- -D warnings
	$(CARGO) doc --keep-going --no-deps --document-private-items -p $(PKG) $(CARGO_FLAGS)

HOTPATH_PROFILE_FEATURES := hotpath,hotpath-alloc,hotpath-mcp,hotpath-cpu,hotpath/hotpath-prometheus
profiling-check: ## Lint and document optional hotpath profiling features
	$(CARGO) clippy $(CARGO_FLAGS) --manifest-path crates/service/Cargo.toml --all-targets --features hotpath -- -D warnings
	$(CARGO) clippy $(CARGO_FLAGS) --manifest-path crates/service/Cargo.toml --all-targets --features $(HOTPATH_PROFILE_FEATURES) -- -D warnings
	@for pkg in infra-http infra-telemetry infra-postgres infra-jobs infra-webhooks; do \
		if [ -f "crates/$$pkg/Cargo.toml" ]; then \
			$(CARGO) clippy $(CARGO_FLAGS) -p "$$pkg" --all-targets --features "$$pkg/hotpath" -- -D warnings && \
			RUSTDOCFLAGS="$(RUSTDOCFLAGS) -D warnings" $(CARGO) doc $(CARGO_FLAGS) -p "$$pkg" --no-deps --document-private-items --features "$$pkg/hotpath" || exit; \
		fi; \
	done
	RUSTDOCFLAGS="$(RUSTDOCFLAGS) -D warnings" $(CARGO) doc $(CARGO_FLAGS) --manifest-path crates/service/Cargo.toml --no-deps --document-private-items --features $(HOTPATH_PROFILE_FEATURES)

mutants: $(filter $(TOOLS_ROOT)/%,$(CARGO_MUTANTS) $(CARGO_NEXTEST)) ## Mutation testing for PKG=<crate> (optional FILE=<path>)
	@test -n "$(PKG)" || { echo "mutants requires PKG=<crate>" >&2; exit 2; }
	$(NEXTEST_PATH) $(CARGO_MUTANTS) mutants --test-tool nextest --package $(PKG) $(if $(FILE),--file $(FILE)) --cargo-arg=--locked --output target/mutants

clean: ## Remove build output
	$(CARGO) clean

# --- Pinned Cargo tool bootstrap ---

$(TOOLS_ROOT)/typos-cli-$(TYPOS_VERSION)/bin/typos:
	$(CARGO) install --locked --root "$(TOOLS_ROOT)/typos-cli-$(TYPOS_VERSION)" --version "$(TYPOS_VERSION)" typos-cli

$(TOOLS_ROOT)/%:
	$(CARGO) install --locked --root "$(TOOLS_ROOT)/$(firstword $(subst /, ,$*))" --version "$(patsubst $(notdir $@)-%,%,$(firstword $(subst /, ,$*)))" $(notdir $@)

# --- Quality, security, and contracts ---

.PHONY: deny unused-deps dependency-drift native-transport typos secret-scan secret-scan-history \
	actionlint zizmor shellcheck python-lint docs-check architecture-check monitoring-rules \
	openapi-generate openapi-check openapi-lint openapi-breaking openapi-breaking-since \
	agent-harness-sync check-instructions ledger-check

deny: $(filter $(TOOLS_ROOT)/%,$(CARGO_DENY)) ## Check advisories, licenses, bans, and sources (deny.toml)
	$(CARGO_DENY) --locked check

unused-deps: $(filter $(TOOLS_ROOT)/%,$(CARGO_SHEAR)) ## Fail on unused dependencies (cargo-shear)
	$(CARGO_SHEAR) --locked
	@if [ -f tools/grpc-codegen/Cargo.toml ]; then $(CARGO_SHEAR) --locked tools/grpc-codegen; fi

dependency-drift: ## Check vendored forks for advisories/yanks (UPSTREAM=1 adds release & pin drift)
	python3 scripts/check-dependency-drift.py $(if $(UPSTREAM),--upstream)

native-transport: ## Run locked serial regression tests for retained vendor crates
	python3 scripts/check-native-transport.py

typos: $(filter $(TOOLS_ROOT)/%,$(TYPOS)) ## Check source code and docs for typos
	$(TYPOS) --config .typos.toml

secret-scan: ## Scan worktree (locally) and commits since BASE_REF with gitleaks
	@if [ "$(CI)" != "true" ]; then $(GITLEAKS) dir $(GITLEAKS_FLAGS) .; fi
	@git cat-file -e "$(BASE_REF)^{commit}" 2>/dev/null || { echo "secret-scan base unavailable: $(BASE_REF)" >&2; exit 2; }
	$(GITLEAKS) git $(GITLEAKS_FLAGS) --log-opts="$(BASE_REF)..HEAD" .

secret-scan-history: ## Scan full HEAD ancestry with gitleaks
	@test "$$(git rev-parse --is-shallow-repository 2>/dev/null)" = "false" || { echo "secret-scan-history requires full history" >&2; exit 2; }
	$(GITLEAKS) git $(GITLEAKS_FLAGS) --log-opts=HEAD .

actionlint: ## Lint GitHub Actions workflows
	$(ACTIONLINT) -pyflakes=

zizmor: $(filter $(TOOLS_ROOT)/%,$(ZIZMOR)) ## Audit GitHub Actions workflows for security weaknesses
	$(ZIZMOR) --persona regular .github

shellcheck: ## Lint all shell scripts with ShellCheck
	@test -n "$(SHELL_FILES)" || exit 0; \
	if [ "$(CI)" = "true" ] || (command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1); then \
		docker run --rm --read-only --network none -v "$(CURDIR):/src:ro" -w /src "$(SHELLCHECK_IMAGE)" -x -- $(SHELL_FILES); \
	else \
		shellcheck -x -- $(SHELL_FILES); \
	fi

python-lint: ## Lint Python scripts with pinned ruff
	$(RUFF) check --no-cache .

docs-check: ## Check relative Markdown links and fragments offline with lychee
	@test -n "$(MARKDOWN_FILES)" || exit 0; \
	if [ "$(CI)" = "true" ] || (command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1); then \
		docker run --rm --read-only --network none -v "$(CURDIR):/src:ro" -w /src --entrypoint lychee "$(LYCHEE_IMAGE)" \
			--offline --include-fragments --no-progress --root-dir /src -- $(MARKDOWN_FILES); \
	else \
		lychee --offline --include-fragments --no-progress --root-dir "$(CURDIR)" -- $(MARKDOWN_FILES); \
	fi

architecture-check: ## Enforce workspace crate dependency rules and in-package #[path]
	python3 scripts/check-architecture.py

monitoring-rules: ## Validate Prometheus alert rules and fixtures with pinned promtool
	bash scripts/check-monitoring.sh

openapi-generate: ## Regenerate api/openapi/service.yaml from the Rust contract
	@set -eu; \
		bin=$$($(CARGO) build -q --workspace --bin openapi $(CARGO_FLAGS) --message-format=json-render-diagnostics \
			| jq -r 'select(.reason == "compiler-artifact" and .target.name == "openapi" and (.target.kind | index("bin")) and .executable != null) | .executable' \
			| tail -n 1); \
		"$$bin" >"$(OPENAPI_FILE).tmp" && mv "$(OPENAPI_FILE).tmp" "$(OPENAPI_FILE)"

openapi-lint: ## Lint committed OpenAPI document with Redocly CLI
	REDOCLY_SUPPRESS_UPDATE_NOTICE=true REDOCLY_TELEMETRY=off $(NPX) @redocly/cli@$(REDOCLY_CLI_VERSION) lint --config .redocly.yaml $(OPENAPI_FILE)

openapi-check: openapi-lint ## Fail when committed OpenAPI document is stale or fails lint
	$(CARGO) test -p $(SERVICE_BIN) $(CARGO_FLAGS) --test openapi

openapi-breaking: ## Compare OpenAPI document against BASE_OPENAPI=<file>
	@test -n "$(BASE_OPENAPI)" || { echo "openapi-breaking requires BASE_OPENAPI=<file>" >&2; exit 2; }
	$(OASDIFF) breaking --fail-on ERR "$(BASE_OPENAPI)" $(OPENAPI_FILE)
	@set -eu; work=$$(mktemp -d); trap 'rm -rf "$$work"' 0; \
		$(OASDIFF) upgrade --allow-external-refs=false --format json -- "$(BASE_OPENAPI)" >"$$work/base.json"; \
		$(OASDIFF) upgrade --allow-external-refs=false --format json -- "$(OPENAPI_FILE)" >"$$work/cand.json"; \
		python3 scripts/check-openapi-idempotency.py "$$work/base.json" "$$work/cand.json"

openapi-breaking-since: ## Compare OpenAPI document against BASE_REF
	@git cat-file -e "$(BASE_REF):$(OPENAPI_FILE)" 2>/dev/null || { echo "$(OPENAPI_FILE) unavailable at $(BASE_REF)" >&2; exit 2; }
	@base=$$(mktemp) && trap 'rm -f "$$base"' EXIT && git show "$(BASE_REF):$(OPENAPI_FILE)" >"$$base" && \
		$(MAKE) --no-print-directory openapi-breaking BASE_OPENAPI="$$base"

agent-harness-sync: ## Materialize selected agent harness files from .agents/
	python3 scripts/harness-sync.py --apply --repo .

check-instructions: ## Validate .agents/skills, roles, and materialized harness files
	python3 scripts/harness-sync.py --check --repo . $(if $(wildcard template/init.py),,--require-materialized)

ledger-check: ## Verify specs/<TASK>/tasks.md invariants and print execution waves (TASK=<slug|path>)
	@test -n "$(TASK)" || { echo "ledger-check requires TASK=<slug> or TASK=<path>" >&2; exit 2; }
	python3 scripts/check-ledger.py "$(if $(wildcard $(TASK)/tasks.md),$(TASK),$(if $(wildcard specs/$(TASK)/tasks.md),specs/$(TASK),$(if $(and $(wildcard $(TASK)),$(filter %.md,$(TASK))),$(TASK),specs/$(TASK))))"

# --- Container image ---

.PHONY: dockerfile-check image-build image-check image-scan image-sbom

dockerfile-check: ## Run BuildKit native checks on build/docker/Dockerfile
	bash scripts/image.sh dockerfile-check

image-build: ## Build runtime container image as RUNTIME_IMAGE
	bash scripts/image.sh build "$(RUNTIME_IMAGE)"

image-check: ## Smoke-test RUNTIME_IMAGE hardened (readiness, licenses, jemalloc, SIGTERM)
	bash scripts/image.sh check "$(RUNTIME_IMAGE)" "$(RUNTIME_EXPECTED_COMMIT)"

image-scan: ## Scan CONTAINER_IMAGE for fixable HIGH/CRITICAL vulnerabilities (Trivy)
	bash scripts/image.sh scan "$(CONTAINER_IMAGE)"

SBOM_OUTPUT ?= sbom.cdx.json
image-sbom: ## Generate CycloneDX SBOM for CONTAINER_IMAGE into SBOM_OUTPUT
	bash scripts/image.sh sbom "$(CONTAINER_IMAGE)" "$(SBOM_OUTPUT)"

# --- Optional capability profiles ---

# template:begin container-integration:make-integration-compile
INTEGRATION_SUITE ?= retained
.PHONY: integration-compile
integration-compile: ## Compile retained integration test suites without running them
	bash scripts/integration.sh --compile-$(INTEGRATION_SUITE)
# template:end container-integration:make-integration-compile

# template:begin postgres:make-postgres-profile
INTEGRATION_LINT_FEATURES += --features integration-tests/integration
CHECK_EXTRAS += migration-check
SQLX_CLI_BIN := $(TOOLS_ROOT)/sqlx-cli-$(SQLX_CLI_VERSION)/bin

$(SQLX_CLI_BIN)/cargo-sqlx:
	$(CARGO) install --locked --root "$(TOOLS_ROOT)/sqlx-cli-$(SQLX_CLI_VERSION)" --version "$(SQLX_CLI_VERSION)" --no-default-features --features postgres,rustls sqlx-cli

.PHONY: compose-up compose-down test-integration-db sqlx-prepare sqlx-check migration-check migration-validate

compose-up: ## Start local PostgreSQL from env/docker-compose.yml
	docker compose -f env/docker-compose.yml up -d --wait postgres

compose-down: ## Stop local compose services and remove volumes
	docker compose -f env/docker-compose.yml down -v --remove-orphans

test-integration-db: ## Run PostgreSQL integration suite against throwaway Compose
	bash scripts/integration.sh db

sqlx-prepare: $(SQLX_CLI_BIN)/cargo-sqlx ## Regenerate .sqlx/ offline query metadata
	PATH="$(SQLX_CLI_BIN):$$PATH" bash scripts/sqlx-prepare.sh

sqlx-check: $(SQLX_CLI_BIN)/cargo-sqlx ## Verify .sqlx/ matches current queries and migrations
	PATH="$(SQLX_CLI_BIN):$$PATH" bash scripts/sqlx-prepare.sh --check

migration-check: ## Verify append-only migration history, squawk lint, and migrate unit tests
	python3 scripts/migration-check.py --base "$(BASE_REF)"
	$(CARGO) test -p migrate $(CARGO_FLAGS)

migration-validate: ## Rehearse /migrate and service startup with RUNTIME_IMAGE against fresh PostgreSQL
	bash scripts/migration-validate.sh "$(RUNTIME_IMAGE)" "$(RUNTIME_EXPECTED_COMMIT)"
# template:end postgres:make-postgres-profile

# template:begin grpc:make-grpc-profile
BUF ?= go run github.com/bufbuild/buf/cmd/buf@v$(BUF_VERSION)
export BUF
.PHONY: grpc-generate grpc-check grpc-generator-deny

grpc-generate: ## Generate protobuf Rust contracts from api/proto/
	bash scripts/grpc.sh generate

grpc-check: ## Check protobuf format, lint, codegen drift, and breaking changes
	bash scripts/grpc.sh check

grpc-generator-deny: $(filter $(TOOLS_ROOT)/%,$(CARGO_DENY)) ## Check advisories/licenses for tools/grpc-codegen
	$(CARGO_DENY) --manifest-path tools/grpc-codegen/Cargo.toml --config tools/grpc-codegen/deny.toml --locked check
# template:end grpc:make-grpc-profile

# template:begin messaging:make-messaging-profile
INTEGRATION_LINT_FEATURES += --features infra-messaging/integration
.PHONY: test-integration-messaging messaging-go-compat

test-integration-messaging: ## Run NATS JetStream integration suite against throwaway Compose
	bash scripts/integration.sh messaging

messaging-go-compat: ## Verify JetStream wire compatibility against pinned Go service codec
	@set -eu; \
		if [ -n "$(GO_WIRE_DIR)" ]; then work="$(GO_WIRE_DIR)"; mkdir -p "$$work"; \
		else work=$$(mktemp -d); trap 'rm -rf "$$work"' EXIT; fi; \
		bash scripts/messaging-go-compat.sh generate "$$work/records.json"; \
		GO_WIRE_FIXTURES="$$work/records.json" GO_WIRE_RUST_EXPORT="$$work/rust-export.json" \
			$(CARGO) test $(CARGO_FLAGS) -p infra-messaging --test wire_compat -- --ignored --exact rust_production_wire_exports_for_go; \
		bash scripts/messaging-go-compat.sh verify-rust "$$work/records.json" "$$work/rust-export.json"; \
		bash scripts/messaging-go-compat.sh check "$$work/records.json"
# template:end messaging:make-messaging-profile

# template:begin cache:make-cache-profile
INTEGRATION_LINT_FEATURES += --features infra-cache/integration
.PHONY: test-integration-cache
test-integration-cache: ## Run Valkey cache integration suite against throwaway Compose
	bash scripts/integration.sh cache
# template:end cache:make-cache-profile

# template:begin object-storage:make-object-storage-profile
INTEGRATION_LINT_FEATURES += --features infra-object-storage/integration
.PHONY: test-integration-object-storage test-object-storage-conformance

test-integration-object-storage: ## Run S3 adapter suite against throwaway Compose versitygw
	bash scripts/integration.sh object-storage

test-object-storage-conformance: ## Live S3 conformance (PROVIDER=amazon_s3|cloudflare_r2|railway|s3_compatible OBJECT_STORAGE_CONFORMANCE_WRITES=allow)
	@case "$(PROVIDER)" in amazon_s3|cloudflare_r2|railway|s3_compatible) ;; *) echo "invalid PROVIDER" >&2; exit 2 ;; esac
	@test "$(OBJECT_STORAGE_CONFORMANCE_WRITES)" = allow || { echo "set OBJECT_STORAGE_CONFORMANCE_WRITES=allow" >&2; exit 2; }
	OBJECT_STORAGE_CONFORMANCE_WRITES=allow OBJECT_STORAGE_CONFORMANCE_PROVIDER=$(PROVIDER) \
		$(CARGO) test -p infra-object-storage --features integration --test conformance $(CARGO_FLAGS) -- --ignored --no-capture
# template:end object-storage:make-object-storage-profile

# template:begin outbound-auth:make-outbound-auth-profile
INTEGRATION_LINT_FEATURES += --features infra-oauth2-client-credentials/integration
.PHONY: test-integration-oauth
test-integration-oauth: ## Run OAuth2 integration suite against throwaway Keycloak
	bash scripts/integration.sh oauth
# template:end outbound-auth:make-outbound-auth-profile

# template:begin runtime-progress:make-runtime-progress-profile
.PHONY: runtime-progress-proof
runtime-progress-proof: ## Run Linux CPU-quota progress proof
	bash scripts/runtime-progress.sh
# template:end runtime-progress:make-runtime-progress-profile

# template:begin source-template:make-template-targets
CHECK_EXTRAS += template-check template-test
.PHONY: template-init template-check template-matrix template-artifacts template-test

template-init: ## Initialize a new service from this template once
	@python3 template/init.py --repo .

template-check: ## Fast check of canonical profile projections (no Cargo)
	python3 template/check.py --projections

template-matrix: ## Build, lint, and test canonical profile matrix
	python3 template/check.py --matrix

template-artifacts: ## Build and smoke-test release images for representative profiles
	python3 template/check.py --artifacts

template-test: ## Run unit tests for repository scripts and initializer
	python3 -m unittest discover -s template -p 'test_*.py'
# template:end source-template:make-template-targets

check: fmt-check lint test unused-deps typos openapi-lint check-instructions docs-check architecture-check monitoring-rules $(CHECK_EXTRAS) ## Run full local quality gate
