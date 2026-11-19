# Portable standard commands shared by every service derived from this
# template. Service-specific commands belong in make/service.mk, never here.

SHELL := /bin/sh
.DEFAULT_GOAL := help

# Every tool version is pinned once in tools/versions.env; CI reads the same
# file. `make tools-check` proves manifest shape, Cargo pins, Dockerfile parity, and cooldowns.
include tools/versions.env

CARGO ?= cargo
CARGO_FLAGS ?= --locked
export CARGO CARGO_FLAGS
# Default comparison base for range-scoped gates (secret scan, verify); CI
# passes the event's base commit.
BASE_REF ?= origin/main
export BASE_REF
PKGS ?=
REQUIRE_PKGS = @test -n "$(strip $(PKGS))" || { printf '%s requires PKGS="<crate> <crate>"\n' "$@" >&2; exit 2; }
# Not security boundaries: they stop an agent from launching a costly command
# by accident. ALLOW_FULL guards the full-repository aggregate, ALLOW_HEAVY
# the container-backed and history-wide commands. CI systems set CI=true,
# which satisfies both.
ALLOW_FULL ?=
ALLOW_HEAVY ?=
export ALLOW_FULL ALLOW_HEAVY
FULL_GUARD = @if [ "$(ALLOW_FULL)" != "1" ] && [ "$(CI)" != "true" ]; then printf 'refusing %s: set ALLOW_FULL=1 (CI sets CI=true)\n' "$@" >&2; exit 2; fi
HEAVY_GUARD = @if [ "$(ALLOW_HEAVY)" != "1" ] && [ "$(CI)" != "true" ]; then printf 'refusing %s: set ALLOW_HEAVY=1 (CI sets CI=true)\n' "$@" >&2; exit 2; fi
# One Git-common lock keeps CPU-heavy validations from overlapping.
VALIDATION_LOCK := python3 scripts/verify/lock.py --
VERIFY := python3 scripts/verify/runner.py
NPX := python3 scripts/checks/npx.py
OASDIFF ?= go run github.com/oasdiff/oasdiff@v$(OASDIFF_VERSION)

# Pinned Cargo tools. Locally each one is built once from crates.io into its
# own root under the Git common directory (shared by worktrees, untouched by
# `cargo clean`); the binary path is the prerequisite, so an installed version
# is never rebuilt and needs no PATH lookup or version check. CI installs the
# same versions as prebuilt binaries (taiki-e/install-action) and runs them
# from PATH.
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
# Only binaries under TOOLS_ROOT are build prerequisites; PATH names are not.
CARGO_TOOLS := $(filter $(TOOLS_ROOT)/%,$(CARGO_DENY) $(CARGO_NEXTEST) $(CARGO_SHEAR) $(TYPOS) $(ZIZMOR))
# `cargo nextest` finds its subcommand binary on PATH; a local pin is not there.
NEXTEST_PATH = $(if $(filter $(TOOLS_ROOT)/%,$(CARGO_NEXTEST)),env PATH="$(dir $(CARGO_NEXTEST)):$$PATH")
# nextest runs every test binary at once, where `cargo test` runs them one
# after another (CI audit 2026-11-02: 127 s serial, longest binary 41 s). It
# does not run doctests, so `cargo test --doc` follows. A crate with no tests
# passes, as it does under `cargo test`.
# Most long tests wait on timers and sockets, so two slots per core keep the
# cores busy: on four slots the suite took 57 s, on eight 34-37 s (10-core
# workstation, 2026-11-02).
NEXTEST_TEST_THREADS ?= $(shell echo $$((2 * $$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2))))
NEXTEST_RUN = $(NEXTEST_PATH) $(CARGO) nextest run --no-tests=pass --test-threads=$(NEXTEST_TEST_THREADS) $(CARGO_FLAGS)

# Go tools resolve through the module checksum database; the first run
# compiles and caches them.
REQUIRE_GO = @command -v go >/dev/null 2>&1 || { echo "$@ requires Go for $(1)" >&2; exit 2; }
GITLEAKS ?= go run github.com/zricethezav/gitleaks/v8@v$(GITLEAKS_VERSION)
GITLEAKS_FLAGS := --no-banner --redact --verbose --exit-code 1 --config .gitleaks.toml
ACTIONLINT ?= go run github.com/rhysd/actionlint/cmd/actionlint@v$(ACTIONLINT_VERSION)
# Python tools run from PyPI at the pinned version: uv locally, pipx on CI
# runners, which have it preinstalled.
RUFF ?= $(if $(shell command -v uvx 2>/dev/null),uvx ruff@$(RUFF_VERSION),pipx run ruff==$(RUFF_VERSION))
SHELL_FILES = $(wildcard $(shell git ls-files --cached --others --exclude-standard -- '*.sh'))
# Tracked and untracked Markdown files; `git ls-files` keeps target/ out,
# `vendor/` is archival upstream source, and `.agents/adapters/` holds repo-root
# bootstrap templates projected into `/`.
MARKDOWN_FILES = $(wildcard $(shell git ls-files --cached --others --exclude-standard -- '*.md' ':(exclude)vendor/**' ':(exclude).agents/adapters/**'))

TRIVY_CACHE_VOLUME ?= trivy-cache

# The only portable registry of standard target names. Local service recipes
# may not override these names; profile blocks extend it statically so
# `scripts/template/sync.py` can inspect it without executing Make.
TEMPLATE_STANDARD_TARGETS := help template-sync build run test test-package test-changed mutants fmt fmt-check lint lint-changed \
	check check-unlocked check-instructions ledger-check clean \
	agent-harness-sync agent-harness-check \
	openapi-generate openapi-check openapi-lint openapi-breaking openapi-breaking-since \
	tools-check deny dependency-drift unused-deps typos secret-scan secret-scan-history actionlint zizmor shellcheck python-lint docs-check profiling-check \
	dockerfile-check runtime-image-build runtime-image-check container-security container-sbom \
	publish-image-metadata-check integration-compile monitoring-rules \
	native-transport-regressions \
	plan verify install-hooks verify-check changed-surfaces-check affected-crates-check validation-lock-self-test \
	duplication-check architecture-check quality-check-self-test
# template:begin postgres:make-postgres-standard-targets
TEMPLATE_STANDARD_TARGETS += compose-up compose-down test-integration-db sqlx-prepare sqlx-check migration-check migration-history-self-test migration-validate
# template:end postgres:make-postgres-standard-targets
# template:begin grpc:make-grpc-standard-targets
TEMPLATE_STANDARD_TARGETS += grpc-generate grpc-check grpc-generator-deny
# template:end grpc:make-grpc-standard-targets
# template:begin runtime-progress:make-runtime-progress-standard-targets
TEMPLATE_STANDARD_TARGETS += runtime-progress-proof
# template:end runtime-progress:make-runtime-progress-standard-targets
# template:begin messaging:make-messaging-targets
TEMPLATE_STANDARD_TARGETS += test-integration-messaging messaging-go-compat
# template:end messaging:make-messaging-targets
# template:begin cache:make-cache-standard-targets
TEMPLATE_STANDARD_TARGETS += test-integration-cache
# template:end cache:make-cache-standard-targets
# template:begin object-storage:make-object-storage-standard-targets
TEMPLATE_STANDARD_TARGETS += test-integration-object-storage test-object-storage-conformance
# template:end object-storage:make-object-storage-standard-targets
# template:begin outbound-auth:make-outbound-auth-standard-targets
TEMPLATE_STANDARD_TARGETS += test-integration-oauth
# template:end outbound-auth:make-outbound-auth-standard-targets

MONITORING_RULE_CHECK_TARGETS := monitoring-rules
monitoring-rules: ## Check Prometheus rules and their deterministic fixtures with pinned promtool
	bash scripts/checks/monitoring_rules.sh

SOURCE_CHECK_TARGETS ?=
# Retained profiles extend this without changing portable lint semantics.
INTEGRATION_LINT_FEATURES ?=

# template:begin postgres:make-postgres-profile
include make/profile-postgres.mk
# template:end postgres:make-postgres-profile

.PHONY: $(TEMPLATE_STANDARD_TARGETS)

help: ## List available commands
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z0-9_-]+:.*## / {printf "  %-20s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

SYNC_MODE ?= --check
template-sync: ## Synchronize portable template files from TEMPLATE_SOURCE=<path> (SYNC_MODE=--check|--apply)
	@test -n "$(TEMPLATE_SOURCE)" || { echo "template-sync requires TEMPLATE_SOURCE=<path to template>" >&2; exit 2; }
	python3 scripts/template/sync.py $(SYNC_MODE) --from "$(TEMPLATE_SOURCE)" --repo . $(SYNC_FLAGS)

build: ## Build every workspace crate in debug mode
	$(CARGO) build --workspace $(CARGO_FLAGS)

run: ## Start the HTTP service locally with env/config/local.toml
	$(CARGO) run -p $(SERVICE_BIN) $(CARGO_FLAGS) -- --config $(LOCAL_CONFIG)

test: $(filter $(TOOLS_ROOT)/%,$(CARGO_NEXTEST)) ## Run the ordinary workspace unit-test suite
	TEST_PACKAGE_SELECTION=--workspace $(NEXTEST_RUN) --workspace
	$(CARGO) test --doc --workspace --no-fail-fast $(CARGO_FLAGS)

HOTPATH_PROFILE_FEATURES := hotpath,hotpath-alloc,hotpath-mcp,hotpath-cpu,hotpath/hotpath-prometheus
profiling-check: ## Lint and document the optional hotpath profiling configurations
	$(CARGO) clippy $(CARGO_FLAGS) --manifest-path crates/service/Cargo.toml --all-targets --features hotpath -- -D warnings
	$(CARGO) clippy $(CARGO_FLAGS) --manifest-path crates/service/Cargo.toml --all-targets --features $(HOTPATH_PROFILE_FEATURES) -- -D warnings
	@for pkg in infra-http infra-telemetry infra-postgres infra-jobs infra-webhooks; do \
		if [ -f "crates/$$pkg/Cargo.toml" ]; then \
			$(CARGO) clippy $(CARGO_FLAGS) -p "$$pkg" --all-targets --features "$$pkg/hotpath" -- -D warnings && \
			RUSTDOCFLAGS="$(RUSTDOCFLAGS) -D warnings" $(CARGO) doc $(CARGO_FLAGS) -p "$$pkg" --no-deps --document-private-items --features "$$pkg/hotpath" || exit; \
		fi; \
	done
	RUSTDOCFLAGS="$(RUSTDOCFLAGS) -D warnings" $(CARGO) doc $(CARGO_FLAGS) --manifest-path crates/service/Cargo.toml --no-deps --document-private-items --features $(HOTPATH_PROFILE_FEATURES)
	$(CARGO) tree $(CARGO_FLAGS) --manifest-path crates/service/Cargo.toml --features $(HOTPATH_PROFILE_FEATURES) -e features -i hotpath

test-package: $(filter $(TOOLS_ROOT)/%,$(CARGO_NEXTEST)) ## Run one crate's tests; requires PKG=<crate name>
	@test -n "$(PKG)" || { echo "test-package requires PKG=<crate name>" >&2; exit 2; }
	TEST_PACKAGE_SELECTION='-p $(PKG)' $(NEXTEST_RUN) -p $(PKG)
	$(CARGO) test --doc -p $(PKG) --no-fail-fast $(CARGO_FLAGS)

test-changed: $(filter $(TOOLS_ROOT)/%,$(CARGO_NEXTEST)) ## Run the tests of the crates in PKGS="<crate> <crate>"
	$(REQUIRE_PKGS)
	TEST_PACKAGE_SELECTION='$(addprefix -p ,$(PKGS))' $(NEXTEST_RUN) $(addprefix -p ,$(PKGS))
	$(CARGO) test --doc $(addprefix -p ,$(PKGS)) --no-fail-fast $(CARGO_FLAGS)

# cargo-mutants builds a copy of the workspace, so the run takes the shared
# validation lock; `--in-place` would leave a mutated tree behind an interrupt.
# It runs the tests through nextest, as `make test` does, so a surviving mutant
# means no test that `make test` runs fails.
mutants: $(filter $(TOOLS_ROOT)/%,$(CARGO_MUTANTS) $(CARGO_NEXTEST)) ## List the mutations no test catches in PKG=<crate>; FILE=<path> narrows it to one source file
	@test -n "$(PKG)" || { echo "mutants requires PKG=<crate name>" >&2; exit 2; }
	$(VALIDATION_LOCK) $(NEXTEST_PATH) $(CARGO_MUTANTS) mutants --test-tool nextest --package $(PKG) $(if $(FILE),--file $(FILE)) --cargo-arg=--locked --output target/mutants

native-transport-regressions: ## Run locked serial native transport regressions
	$(VALIDATION_LOCK) env CARGO_TARGET_DIR="$${CARGO_TARGET_DIR:-$(CURDIR)/target}" python3 scripts/checks/native_transport.py

INTEGRATION_SUITE ?= retained
INTEGRATION_COMPILE = @echo "No retained integration suites to compile."
# template:begin container-integration:make-compile-runner
INTEGRATION_COMPILE = bash scripts/profiles/integration.sh --compile-$(INTEGRATION_SUITE)
# template:end container-integration:make-compile-runner
integration-compile: ## Compile retained integration suites without running them (INTEGRATION_SUITE=retained|services)
	$(INTEGRATION_COMPILE)

# template:begin messaging:make-messaging-profile
INTEGRATION_LINT_FEATURES += --features infra-messaging/integration

test-integration-messaging: ## JetStream adapter proof against a throwaway Compose NATS; ALLOW_HEAVY=1, REQUIRE_DOCKER=1 to fail without Docker
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) bash scripts/profiles/integration.sh messaging

messaging-go-compat: ## Compare the JetStream wire with the pinned Go service's actual encoder and decoder; requires Go and network
	$(call REQUIRE_GO,the pinned Go wire source)
	@if [ -n "$(GO_WIRE_DIR)" ]; then \
		work="$(GO_WIRE_DIR)" && mkdir -p "$$work"; \
	else \
		work=$$(mktemp -d) && trap 'rm -rf "$$work"' EXIT; \
	fi && \
		bash scripts/profiles/messaging_go_compat.sh generate "$$work/records.json" && \
		GO_WIRE_FIXTURES="$$work/records.json" GO_WIRE_RUST_EXPORT="$$work/rust-export.json" \
			$(CARGO) test $(CARGO_FLAGS) -p infra-messaging --test wire_compat -- --ignored --exact rust_production_wire_exports_for_go && \
		bash scripts/profiles/messaging_go_compat.sh verify-rust "$$work/records.json" "$$work/rust-export.json" && \
		bash scripts/profiles/messaging_go_compat.sh check "$$work/records.json"
# template:end messaging:make-messaging-profile

# template:begin cache:make-cache-profile
INTEGRATION_LINT_FEATURES += --features infra-cache/integration

test-integration-cache: ## Valkey adapter proof against a throwaway Compose Valkey; ALLOW_HEAVY=1, REQUIRE_DOCKER=1 to fail without Docker
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) bash scripts/profiles/integration.sh cache
# template:end cache:make-cache-profile

# template:begin object-storage:make-object-storage-profile
INTEGRATION_LINT_FEATURES += --features infra-object-storage/integration

test-integration-object-storage: ## S3 adapter proof against a throwaway Compose versitygw; ALLOW_HEAVY=1, REQUIRE_DOCKER=1 to fail without Docker
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) bash scripts/profiles/integration.sh object-storage

# Writes to a real bucket under a unique prefix with the service's own
# APP__OBJECT_STORAGE__* variables; never part of an aggregate or CI.
test-object-storage-conformance: ## Live-provider conformance; PROVIDER=amazon_s3|cloudflare_r2|railway|s3_compatible and OBJECT_STORAGE_CONFORMANCE_WRITES=allow
	@case "$(PROVIDER)" in amazon_s3|cloudflare_r2|railway|s3_compatible) ;; *) printf '%s requires PROVIDER=amazon_s3|cloudflare_r2|railway|s3_compatible\n' "$@" >&2; exit 2 ;; esac
	@test "$(OBJECT_STORAGE_CONFORMANCE_WRITES)" = allow || { printf 'refusing %s: it writes to a real bucket; set OBJECT_STORAGE_CONFORMANCE_WRITES=allow\n' "$@" >&2; exit 2; }
	$(VALIDATION_LOCK) env OBJECT_STORAGE_CONFORMANCE_WRITES=allow OBJECT_STORAGE_CONFORMANCE_PROVIDER=$(PROVIDER) \
		$(CARGO) test -p infra-object-storage --features integration --test conformance $(CARGO_FLAGS) -- --ignored --no-capture
# template:end object-storage:make-object-storage-profile

# template:begin outbound-auth:make-outbound-auth-profile
INTEGRATION_LINT_FEATURES += --features infra-oauth2-client-credentials/integration

test-integration-oauth: ## OAuth adapter proof against a throwaway Keycloak container; ALLOW_HEAVY=1, REQUIRE_DOCKER=1 to fail without Docker
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) bash scripts/profiles/integration.sh oauth
# template:end outbound-auth:make-outbound-auth-profile

fmt: ## Format every crate
	$(CARGO) $(CARGO_FLAGS) fmt --all
	@if [ -f tools/grpc-codegen/Cargo.toml ]; then $(CARGO) $(CARGO_FLAGS) fmt --manifest-path tools/grpc-codegen/Cargo.toml --all; fi

fmt-check: ## Fail when formatting differs from rustfmt output
	$(CARGO) $(CARGO_FLAGS) fmt --all --check
	@if [ -f tools/grpc-codegen/Cargo.toml ]; then $(CARGO) $(CARGO_FLAGS) fmt --manifest-path tools/grpc-codegen/Cargo.toml --all --check; fi

# Clippy's `-D warnings` does not reach rustdoc; `RUSTDOCFLAGS="-D warnings"`
# enforces `broken_intra_doc_links`, `private_intra_doc_links`,
# `redundant_explicit_links`, and HTML/table checks across public and private
# items. Cargo's `build.warnings = "deny"` would cover both, but it also fails
# on the vendored crates, which stay byte-identical to upstream.
lint lint-changed: export RUSTDOCFLAGS := $(RUSTDOCFLAGS) -D warnings

lint: ## Clippy over all targets and rustdoc, warnings as errors
	$(CARGO) clippy --workspace --all-targets --keep-going $(INTEGRATION_LINT_FEATURES) $(CARGO_FLAGS) -- -D warnings
	$(CARGO) doc --keep-going --workspace --no-deps --document-private-items $(INTEGRATION_LINT_FEATURES) $(CARGO_FLAGS)
	@if [ -f tools/grpc-codegen/Cargo.toml ]; then \
		CARGO_TARGET_DIR="$${CARGO_TARGET_DIR:-$(CURDIR)/target/grpc-codegen}" $(CARGO) clippy --manifest-path tools/grpc-codegen/Cargo.toml --all-targets --keep-going $(CARGO_FLAGS) -- -D warnings && \
		CARGO_TARGET_DIR="$${CARGO_TARGET_DIR:-$(CURDIR)/target/grpc-codegen}" $(CARGO) doc --keep-going --manifest-path tools/grpc-codegen/Cargo.toml --no-deps --document-private-items $(CARGO_FLAGS); \
	fi

lint-changed: ## Clippy and rustdoc, warnings as errors, over the crates in PKGS="<crate> <crate>"
	$(REQUIRE_PKGS)
	$(CARGO) clippy $(addprefix -p ,$(PKGS)) --all-targets --keep-going $(foreach feature,$(filter-out --features,$(INTEGRATION_LINT_FEATURES)),$(if $(filter $(firstword $(subst /, ,$(feature))),$(PKGS)),--features $(feature))) $(CARGO_FLAGS) -- -D warnings
	$(CARGO) doc --keep-going --no-deps --document-private-items $(addprefix -p ,$(PKGS)) $(foreach feature,$(filter-out --features,$(INTEGRATION_LINT_FEATURES)),$(if $(filter $(firstword $(subst /, ,$(feature))),$(PKGS)),--features $(feature))) $(CARGO_FLAGS)

agent-harness-sync: ## Materialize selected agent harness carriers from canonical .agents sources
	python3 scripts/harness/sync.py --apply --repo .

agent-harness-check: check-instructions ## Verify canonical .agents sources and materialized harness carriers

check-instructions: ## Validate .agents/skills, canonical roles, and materialized harness carriers
	python3 scripts/harness/sync.py --check --repo . $(if $(wildcard make/source.mk),,--require-materialized)

ledger-check: ## Verify specs/<TASK>/tasks.md invariants and print execution waves (TASK=<slug|path>)
	@test -n "$(TASK)" || { echo "ledger-check requires TASK=<slug> or TASK=<path>" >&2; exit 2; }
	python3 scripts/verify/check_ledger.py "$(if $(wildcard $(TASK)/tasks.md),$(TASK),$(if $(wildcard specs/$(TASK)/tasks.md),specs/$(TASK),$(if $(and $(wildcard $(TASK)),$(filter %.md,$(TASK))),$(TASK),specs/$(TASK))))"

$(TOOLS_ROOT)/typos-cli-$(TYPOS_VERSION)/bin/typos:
	$(CARGO) install --locked --root "$(TOOLS_ROOT)/typos-cli-$(TYPOS_VERSION)" --version "$(TYPOS_VERSION)" typos-cli

$(TOOLS_ROOT)/%:
	$(CARGO) install --locked --root "$(TOOLS_ROOT)/$(firstword $(subst /, ,$*))" --version "$(patsubst $(notdir $@)-%,%,$(firstword $(subst /, ,$*)))" $(notdir $@)

tools-check: $(CARGO_TOOLS) ## Prove tools/versions.env shape, Cargo pins, Dockerfile parity, and 7-day cooldowns
	CARGO_DENY="$(CARGO_DENY)" CARGO_NEXTEST="$(CARGO_NEXTEST)" CARGO_SHEAR="$(CARGO_SHEAR)" TYPOS="$(TYPOS)" ZIZMOR="$(ZIZMOR)" python3 scripts/checks/tools.py

deny: $(filter $(TOOLS_ROOT)/%,$(CARGO_DENY)) ## Advisories, licenses, bans, and sources over the locked graph (deny.toml)
	$(CARGO_DENY) --locked check

dependency-drift: ## Vendored forks: patch applied, no advisory on the base, upstream review recorded; UPSTREAM=1 adds upstream and tool-pin drift
	bash scripts/checks/dependency_drift.sh $(if $(UPSTREAM),--upstream)

unused-deps: $(filter $(TOOLS_ROOT)/%,$(CARGO_SHEAR)) ## Fail on a declared dependency no crate uses (cargo-shear)
	$(CARGO_SHEAR) --locked
	@if [ -f tools/grpc-codegen/Cargo.toml ]; then $(CARGO_SHEAR) --locked tools/grpc-codegen; fi

typos: $(filter $(TOOLS_ROOT)/%,$(TYPOS)) ## Check source code, contracts, and documentation for typos
	$(TYPOS) --config .typos.toml

secret-scan: ## Gitleaks over the worktree (locally) and the commits since BASE_REF
	$(call REQUIRE_GO,gitleaks@v$(GITLEAKS_VERSION))
	@if [ "$(CI)" != "true" ]; then $(GITLEAKS) dir $(GITLEAKS_FLAGS) .; fi
	@git cat-file -e "$(BASE_REF)^{commit}" 2>/dev/null || { echo "secret scan base is unavailable: $(BASE_REF)" >&2; exit 2; }
	$(GITLEAKS) git $(GITLEAKS_FLAGS) --log-opts="$(BASE_REF)..HEAD" .

secret-scan-history: ## Gitleaks over HEAD and every reachable ancestor; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	@if ! shallow=$$(git rev-parse --is-shallow-repository 2>/dev/null) || [ "$$shallow" != false ] || \
		! git cat-file -e 'HEAD^{commit}' 2>/dev/null; then \
		echo "secret scan requires complete, available HEAD history" >&2; exit 2; fi
	$(call REQUIRE_GO,gitleaks@v$(GITLEAKS_VERSION))
	$(GITLEAKS) git $(GITLEAKS_FLAGS) --log-opts=HEAD .

# pyflakes would run whatever binary the host has on PATH, so it stays off;
# workflow `run:` steps get the pinned ShellCheck container (or local
# shellcheck when Docker is unavailable, matching `make shellcheck`).
actionlint: ## Lint GitHub Actions workflows and their inline shell steps
	$(call REQUIRE_GO,actionlint@v$(ACTIONLINT_VERSION))
	@work=$$(mktemp -d "$${TMPDIR:-/tmp}/actionlint.XXXXXXXX") && \
	trap 'rm -rf "$$work"' EXIT HUP INT TERM && \
	if [ "$(CI)" = "true" ] || (command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1); then \
		printf '%s\n' '#!/bin/sh' 'exec docker run -i --rm --read-only --network none -v "$(CURDIR):$(CURDIR):ro" -w "$(CURDIR)" "$(SHELLCHECK_IMAGE)" "$$@"' >"$$work/shellcheck"; \
	elif command -v shellcheck >/dev/null 2>&1; then \
		printf '%s\n' '#!/bin/sh' 'exec shellcheck "$$@"' >"$$work/shellcheck"; \
	else \
		echo "$@ requires Docker ($(SHELLCHECK_IMAGE)) or local shellcheck" >&2; exit 2; \
	fi && \
	chmod +x "$$work/shellcheck" && \
	$(ACTIONLINT) -shellcheck="$$work/shellcheck" -pyflakes=

# Every executed workflow and local action is owned by this .github tree.
# Vendored upstream automation stays archival source, outside this gate's inputs.
zizmor: $(filter $(TOOLS_ROOT)/%,$(ZIZMOR)) ## Audit GitHub Actions workflows for security weaknesses; GH_TOKEN enables the online audits
	$(ZIZMOR) --persona regular .github

shellcheck: ## ShellCheck every shell script through the pinned container (or local shellcheck when Docker is unavailable)
	@test -n "$(SHELL_FILES)" || { echo "no shell scripts found; skipping ShellCheck"; exit 0; }; \
	if [ "$(CI)" = "true" ] || (command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1); then \
		docker run --rm --read-only --network none -v "$(CURDIR):/src:ro" -w /src "$(SHELLCHECK_IMAGE)" -x -- $(SHELL_FILES); \
	elif command -v shellcheck >/dev/null 2>&1; then \
		shellcheck -x -- $(SHELL_FILES); \
	else \
		echo "$@ requires Docker ($(SHELLCHECK_IMAGE)) or local shellcheck" >&2; exit 2; \
	fi

python-lint: ## Lint the Python scripts for defects with the pinned ruff
	@command -v uvx >/dev/null 2>&1 || command -v pipx >/dev/null 2>&1 || { echo "$@ requires uv or pipx for ruff@$(RUFF_VERSION)" >&2; exit 2; }
	$(RUFF) check --no-cache .

DOCS_CHECK_PART ?= all
docs-check: export DOCS_CHECK_PART := $(DOCS_CHECK_PART)
docs-check: ## Image inputs and offline links (DOCS_CHECK_PART=all|links|image-inputs)
	@case "$$DOCS_CHECK_PART" in \
		all|image-inputs) python3 scripts/checks/image_inputs.py ;; \
		links) ;; \
		*) echo "$@ expects DOCS_CHECK_PART=all|links|image-inputs" >&2; exit 2 ;; \
	esac
ifneq ($(DOCS_CHECK_PART),image-inputs)
	@run_lychee() { \
		root="$$1"; shift; \
		if [ "$(CI)" = "true" ] || (command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1); then \
			docker run --rm --read-only --network none -v "$$root:/src:ro" -w /src --entrypoint lychee "$(LYCHEE_IMAGE)" \
				--offline --include-fragments --no-progress --root-dir /src -- "$$@"; \
		elif command -v lychee >/dev/null 2>&1; then \
			(cd "$$root" && lychee --offline --include-fragments --no-progress --root-dir "$$root" -- "$$@"); \
		else \
			echo "$@ requires Docker ($(LYCHEE_IMAGE)) or local lychee" >&2; exit 2; \
		fi; \
	}; \
	if [ -n "$(MARKDOWN_FILES)" ]; then run_lychee "$(CURDIR)" $(MARKDOWN_FILES) || exit; fi; \
	if [ -f make/source.mk ]; then \
		stage="$$(python3 -c 'import pathlib, tempfile; print(pathlib.Path(tempfile.mkdtemp(prefix="docs-check-")).resolve())')" && \
		trap 'rm -rf "$$stage"' EXIT && \
		python3 -c 'import os, shutil, subprocess, sys; dst = sys.argv[1]; files = subprocess.check_output(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", ".", ":(exclude)vendor/**"]).split(b"\0"); [os.makedirs(os.path.dirname(os.path.join(dst, p)), exist_ok=True) or shutil.copy2(p, os.path.join(dst, p)) for r in files if r and os.path.isfile(p := r.decode()) and not os.path.islink(p)]' "$$stage" && \
		python3 scripts/harness/sync.py --apply --repo "$$stage" >/dev/null && \
		harness_md="$$(cd "$$stage" && find CLAUDE.md QWEN.md Grok.md .claude .qwen .cursor .grok .opencode .agents/agents .agents/rules -type f -name '*.md' 2>/dev/null)" && \
		run_lychee "$$stage" $$harness_md; \
	fi
endif

dockerfile-check: ## Native BuildKit Dockerfile checks
	$(VALIDATION_LOCK) bash scripts/image/build.sh --check

runtime-image-build: ## Build the runtime image as RUNTIME_IMAGE from the repository context; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) bash scripts/image/build.sh "$(RUNTIME_IMAGE)"

runtime-image-check: ## Start RUNTIME_IMAGE hardened, await readiness, assert RUNTIME_EXPECTED_COMMIT, stop inside the grace budget; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) bash scripts/image/check.sh "$(RUNTIME_IMAGE)" "$(RUNTIME_EXPECTED_COMMIT)"

# template:begin runtime-progress:make-runtime-progress-target
runtime-progress-proof: ## Frozen Linux release CPU-quota proof; retains every sample; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) bash scripts/profiles/runtime_progress.sh
# template:end runtime-progress:make-runtime-progress-target

container-security: ## Admit binary inventories, then fail on fixable HIGH/CRITICAL findings; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) env TRIVY_CACHE_VOLUME="$(TRIVY_CACHE_VOLUME)" bash scripts/image/scan.sh security "$(CONTAINER_IMAGE)"

SBOM_OUTPUT ?= sbom.cdx.json
container-sbom: ## Write admitted per-binary CycloneDX to SBOM_OUTPUT; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) env TRIVY_CACHE_VOLUME="$(TRIVY_CACHE_VOLUME)" bash scripts/image/scan.sh sbom "$(CONTAINER_IMAGE)" "$(SBOM_OUTPUT)"

publish-image-metadata-check: ## Self-test of the publication naming and tag promotion
	python3 -m unittest scripts.image.test_publication.PublishMetadataTests

# template:begin grpc:make-grpc-targets
BUF ?= go run github.com/bufbuild/buf/cmd/buf@v$(BUF_VERSION)
export BUF

grpc-generate: ## Generate committed protobuf Rust from pinned Buf descriptors
	$(call REQUIRE_GO,buf@v$(BUF_VERSION))
	$(VALIDATION_LOCK) bash scripts/profiles/grpc_generate.sh

grpc-check: ## Check protobuf format, lint, generation drift and PR-base compatibility; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	$(call REQUIRE_GO,buf@v$(BUF_VERSION))
	$(VALIDATION_LOCK) bash scripts/profiles/grpc_check.sh

grpc-generator-deny: $(filter $(TOOLS_ROOT)/%,$(CARGO_DENY)) ## Advisories, licenses, bans, and sources over the standalone generator's locked graph
	$(CARGO_DENY) --manifest-path tools/grpc-codegen/Cargo.toml --config tools/grpc-codegen/deny.toml --locked check
# template:end grpc:make-grpc-targets

# `cargo run -p` resolved the package's own feature set and rebuilt about a
# hundred dependencies the workspace build already holds (41 s in CI);
# `build --workspace` reuses them.
openapi-generate: ## Regenerate api/openapi/service.yaml from the Rust contract
	@set -eu; \
		work=$$(mktemp -d "$$(dirname "$(OPENAPI_FILE)")/.openapi.XXXXXXXX"); \
		trap 'rm -rf "$$work"' 0; trap 'exit 1' HUP INT TERM; \
		$(CARGO) build -q --workspace --bin openapi $(CARGO_FLAGS) --message-format=json-render-diagnostics >"$$work/messages"; \
		generator=$$(python3 scripts/image/cargo_artifact.py --manifest crates/service/Cargo.toml --name openapi --kind bin <"$$work/messages"); \
		"$$generator" >"$$work/service.yaml"; \
		mv "$$work/service.yaml" "$(OPENAPI_FILE)"

openapi-check: openapi-lint ## Fail when the committed document is stale or fails lint
	$(CARGO) test -p $(SERVICE_BIN) $(CARGO_FLAGS) --test openapi

openapi-lint: ## Lint and validate the committed document with Redocly CLI
	@command -v npx >/dev/null 2>&1 || { echo "openapi-lint requires Node.js (npx) for @redocly/cli@$(REDOCLY_CLI_VERSION)" >&2; exit 2; }
	REDOCLY_SUPPRESS_UPDATE_NOTICE=true REDOCLY_TELEMETRY=off $(NPX) @redocly/cli@$(REDOCLY_CLI_VERSION) lint --config .redocly.yaml $(OPENAPI_FILE)

openapi-breaking: ## Compare the document with BASE_OPENAPI=<file> for breaking changes
	@test -n "$(BASE_OPENAPI)" || { echo "openapi-breaking requires BASE_OPENAPI=<path to the base document>" >&2; exit 2; }
	$(call REQUIRE_GO,oasdiff@v$(OASDIFF_VERSION))
	$(OASDIFF) breaking --fail-on ERR "$(BASE_OPENAPI)" $(OPENAPI_FILE)
	@set -eu; \
		work=$$(mktemp -d); \
		trap 'rm -rf "$$work"' 0; trap 'exit 1' HUP INT TERM; \
		$(OASDIFF) upgrade --allow-external-refs=false --format json -- "$(BASE_OPENAPI)" >"$$work/base.json"; \
		$(OASDIFF) upgrade --allow-external-refs=false --format json -- "$(OPENAPI_FILE)" >"$$work/candidate.json"; \
		python3 scripts/checks/openapi_idempotency.py "$$work/base.json" "$$work/candidate.json"

openapi-breaking-since: ## Compare the document with its committed version at BASE_REF for breaking changes
	@git cat-file -e "$(BASE_REF):$(OPENAPI_FILE)" 2>/dev/null || { echo "$@: $(OPENAPI_FILE) is unavailable at $(BASE_REF)" >&2; exit 2; }
	@base=$$(mktemp) && trap 'rm -f "$$base"' EXIT && git show "$(BASE_REF):$(OPENAPI_FILE)" >"$$base" && \
		$(MAKE) --no-print-directory openapi-breaking BASE_OPENAPI="$$base"

duplication-check: ## Reject substantial Rust clones outside inline jscpd ignores
	@command -v npx >/dev/null 2>&1 || { echo "duplication-check requires Node.js (npx) for jscpd@$(JSCPD_VERSION)" >&2; exit 2; }
	$(NPX) jscpd@$(JSCPD_VERSION) --config .jscpd.json .

architecture-check: ## Enforce declared workspace dependency directions and in-package #[path] inclusions
	python3 scripts/checks/architecture.py

quality-check-self-test: ## Exercise checker fixtures and native-tool smoke cases
	python3 -m unittest scripts/tests/test_quality.py

plan: ## Print the verification route for the changed surfaces without running it
	$(VERIFY) --plan

verify: ## Run the route for the changed surfaces and record a receipt
	$(VERIFY)

# Five of seven failed pull-request runs in the 2026-11-02 CI audit failed a
# check this route runs locally. The hook reuses a matching verify receipt, so
# a push after `make verify` costs nothing; a push that only deletes refs skips.
install-hooks: ## Run `make verify` before every push from this clone; `git push --no-verify` skips it
	@hook=$$(git rev-parse --path-format=absolute --git-path hooks)/pre-push; \
	if test -e "$$hook" && ! grep -q 'make install-hooks' "$$hook"; then echo "install-hooks: $$hook exists and is not ours; leaving it" >&2; exit 1; fi; \
	printf '%s\n' '#!/bin/sh' '# Installed by `make install-hooks`; `git push --no-verify` skips it.' \
		'while read -r _ sha _ _; do' '	[ "$$sha" = 0000000000000000000000000000000000000000 ] || exec make verify' 'done' >"$$hook"; \
	chmod +x "$$hook"; \
	echo "install-hooks: $$hook runs make verify before each push"

VERIFY_TEST_MODULES := scripts/tests/test_verify.py scripts/tests/test_checks_and_image.py \
	scripts/checks/test_make.py scripts/image/test_publication.py scripts/harness/test_sync.py \
	scripts/verify/test_ledger.py \
	scripts/tests/test_image_inputs.py scripts/tests/test_image_inventory.py
# template:begin profile-runner-tests:make-verify-tests
VERIFY_TEST_MODULES += scripts/tests/test_profile_runners.py
# template:end profile-runner-tests:make-verify-tests

verify-check: ## Self-test of the verification router, checks, and image scripts
	python3 -m unittest $(VERIFY_TEST_MODULES)
	@if test -f make/source.mk; then \
		python3 -m unittest scripts.tests.template-init-safety.SourceMakeGateTests && \
		python3 scripts/template/matrix.py --self-test && \
		bash scripts/template/check_init.sh --self-test; \
	fi

changed-surfaces-check: ## Self-test of the surface classifier
	python3 -m unittest scripts/tests/test_verify.py -k Surfaces

affected-crates-check: ## Self-test of the affected-crate planner
	python3 -m unittest scripts/tests/test_verify.py -k AffectedCrates

validation-lock-self-test: ## Self-test of the validation lock
	python3 -m unittest scripts/tests/test_lock.py

check: ## Full repository gate under the validation lock; ALLOW_FULL=1 (CI sets CI=true)
	$(FULL_GUARD)
	$(VALIDATION_LOCK) $(MAKE) check-unlocked

check-unlocked: fmt-check lint test unused-deps typos openapi-lint check-instructions docs-check \
	duplication-check architecture-check quality-check-self-test \
	$(POSTGRES_CHECK_TARGETS) $(MONITORING_RULE_CHECK_TARGETS) $(SOURCE_CHECK_TARGETS) \
	validation-lock-self-test verify-check

clean: ## Remove build output
	$(CARGO) clean
