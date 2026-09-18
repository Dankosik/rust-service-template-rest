# Portable standard commands shared by every service derived from this
# template. Service-specific commands belong in make/service.mk, never here.
#
# `--locked` refuses to update Cargo.lock implicitly, so a build never
# silently resolves different versions than the committed lockfile.

SHELL := /bin/sh
.DEFAULT_GOAL := help

# Every tool version is pinned once in tools/versions.env; CI reads the same
# file. `make tools-check` proves the pins resolve.
include tools/versions.env

CARGO ?= cargo
CARGO_FLAGS ?= --locked
SERVICE_BIN ?= service
# Baseline configuration for `make run`; deployments pass their own file or
# rely on APP__* environment variables.
LOCAL_CONFIG ?= env/config/local.toml
# Default comparison base for range-scoped gates (secret scan, verify); CI
# passes the event's base commit.
BASE_REF ?= origin/main
export BASE_REF
# Crate list for lint-changed and test-changed, as scripts/ci/affected-crates.sh
# prints it: PKGS="health infra-http".
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
VALIDATION_LOCK := bash scripts/ci/validation-lock.sh --
VERIFY := bash scripts/ci/verify.sh

# The OpenAPI document is generated from the Rust contract by the `openapi`
# binary and committed; `make test` refuses a stale copy. Redocly CLI lints
# the committed file (Node.js via npx), oasdiff compares it with the
# pull-request base (Go via `go run`).
OPENAPI_FILE := api/openapi/service.yaml
OPENAPI_BREAKING_APPROVALS ?= api/openapi/breaking-changes-approvals.txt
REDOCLY_CLI ?= npx --yes @redocly/cli@$(REDOCLY_CLI_VERSION)
OASDIFF ?= go run github.com/oasdiff/oasdiff@v$(OASDIFF_VERSION)

# Pinned Cargo tools. Locally each one is built once from crates.io into its
# own root under the Git common directory (shared by worktrees, untouched by
# `cargo clean`); the binary path is the prerequisite, so an installed version
# is never rebuilt and needs no PATH lookup or version check. CI installs the
# same versions as prebuilt binaries (taiki-e/install-action) and runs them
# from PATH.
TOOLS_ROOT ?= $(abspath $(or $(shell git rev-parse --git-common-dir 2>/dev/null),.git))/tools
ifeq ($(CI),true)
CARGO_DENY ?= cargo-deny
CARGO_SHEAR ?= cargo-shear
ZIZMOR ?= zizmor
else
CARGO_DENY ?= $(TOOLS_ROOT)/cargo-deny-$(CARGO_DENY_VERSION)/bin/cargo-deny
CARGO_SHEAR ?= $(TOOLS_ROOT)/cargo-shear-$(CARGO_SHEAR_VERSION)/bin/cargo-shear
ZIZMOR ?= $(TOOLS_ROOT)/zizmor-$(ZIZMOR_VERSION)/bin/zizmor
endif
# Only binaries under TOOLS_ROOT are build prerequisites; PATH names are not.
CARGO_TOOLS := $(filter $(TOOLS_ROOT)/%,$(CARGO_DENY) $(CARGO_SHEAR) $(ZIZMOR))

# Go tools resolve through the module checksum database; the first run
# compiles and caches them.
REQUIRE_GO = @command -v go >/dev/null 2>&1 || { echo "$@ requires Go for $(1)" >&2; exit 2; }
GITLEAKS ?= go run github.com/zricethezav/gitleaks/v8@v$(GITLEAKS_VERSION)
GITLEAKS_FLAGS := --no-banner --redact --verbose --exit-code 1 --config .gitleaks.toml
ACTIONLINT ?= go run github.com/rhysd/actionlint/cmd/actionlint@v$(ACTIONLINT_VERSION)
# Tracked and untracked shell scripts that exist in the worktree.
SHELL_FILES = $(wildcard $(shell git ls-files --cached --others --exclude-standard -- '*.sh'))

# Runtime image: one tag shared by build, lifecycle check, and scan.
RUNTIME_IMAGE ?= service:ci
CONTAINER_IMAGE ?= $(RUNTIME_IMAGE)
# Trivy keeps its vulnerability database in a named volume between runs.
TRIVY_CACHE_VOLUME ?= trivy-cache

.PHONY: help build run test test-package test-changed fmt fmt-check lint lint-changed \
	check check-unlocked check-skills clean \
	openapi-generate openapi-check openapi-lint openapi-breaking \
	tools-check deny unused-deps secret-scan secret-scan-history actionlint zizmor shellcheck \
	dockerfile-check runtime-image-build runtime-image-check container-security container-sbom \
	publish-image-metadata-check \
	plan verify verify-check changed-surfaces-check affected-crates-check validation-lock-self-test

help: ## List available commands
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z0-9_-]+:.*## / {printf "  %-20s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Build every workspace crate in debug mode
	$(CARGO) build --workspace $(CARGO_FLAGS)

run: ## Start the HTTP service locally with env/config/local.toml
	$(CARGO) run -p $(SERVICE_BIN) $(CARGO_FLAGS) -- --config $(LOCAL_CONFIG)

test: ## Run the ordinary workspace unit-test suite
	$(CARGO) test --workspace $(CARGO_FLAGS)

test-package: ## Run one crate's tests; requires PKG=<crate name>
	@test -n "$(PKG)" || { echo "test-package requires PKG=<crate name>" >&2; exit 2; }
	$(CARGO) test -p $(PKG) $(CARGO_FLAGS)

test-changed: ## Run the tests of the crates in PKGS="<crate> <crate>"
	$(REQUIRE_PKGS)
	$(CARGO) test $(addprefix -p ,$(PKGS)) $(CARGO_FLAGS)

fmt: ## Format every crate
	$(CARGO) fmt --all

fmt-check: ## Fail when formatting differs from rustfmt output
	$(CARGO) fmt --all --check

lint: ## Clippy over all targets, warnings are errors
	$(CARGO) clippy --workspace --all-targets $(CARGO_FLAGS) -- -D warnings

lint-changed: ## Clippy over the crates in PKGS="<crate> <crate>", warnings are errors
	$(REQUIRE_PKGS)
	$(CARGO) clippy $(addprefix -p ,$(PKGS)) --all-targets $(CARGO_FLAGS) -- -D warnings

check-skills: ## Validate the shape of .agents/skills (frontmatter, budget, links)
	python3 scripts/check-skills.py

# $(TOOLS_ROOT)/<crate>-<version>/bin/<crate>: build the pinned crate once.
$(TOOLS_ROOT)/%:
	$(CARGO) install --locked --root "$(TOOLS_ROOT)/$(firstword $(subst /, ,$*))" --version "$(patsubst $(notdir $@)-%,%,$(firstword $(subst /, ,$*)))" $(notdir $@)

tools-check: $(CARGO_TOOLS) ## Prove tools/versions.env: shape, image digests, pinned Cargo tools resolve
	CARGO_DENY="$(CARGO_DENY)" CARGO_SHEAR="$(CARGO_SHEAR)" ZIZMOR="$(ZIZMOR)" bash scripts/ci/tools-check.sh

deny: $(filter $(TOOLS_ROOT)/%,$(CARGO_DENY)) ## Advisories, licenses, bans, and sources over the locked graph (deny.toml)
	$(CARGO_DENY) --locked check

unused-deps: $(filter $(TOOLS_ROOT)/%,$(CARGO_SHEAR)) ## Fail on a declared dependency no crate uses (cargo-shear)
	$(CARGO_SHEAR) --locked

secret-scan: ## Gitleaks over the worktree (locally) and the commits since BASE_REF
	$(call REQUIRE_GO,gitleaks@v$(GITLEAKS_VERSION))
	@if [ "$(CI)" != "true" ]; then $(GITLEAKS) dir $(GITLEAKS_FLAGS) .; fi
	@git cat-file -e "$(BASE_REF)^{commit}" 2>/dev/null || { echo "secret scan base is unavailable: $(BASE_REF)" >&2; exit 2; }
	$(GITLEAKS) git $(GITLEAKS_FLAGS) --log-opts="$(BASE_REF)..HEAD" .

secret-scan-history: ## Gitleaks over every commit on every branch; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	$(call REQUIRE_GO,gitleaks@v$(GITLEAKS_VERSION))
	$(GITLEAKS) git $(GITLEAKS_FLAGS) --log-opts=--all .

# The shellcheck and pyflakes integrations would run whatever binary the host
# has on PATH; they are off so the result is the same everywhere. Shell
# scripts get the pinned ShellCheck through `make shellcheck`.
actionlint: ## Lint GitHub Actions workflows
	$(call REQUIRE_GO,actionlint@v$(ACTIONLINT_VERSION))
	$(ACTIONLINT) -shellcheck= -pyflakes=

zizmor: $(filter $(TOOLS_ROOT)/%,$(ZIZMOR)) ## Audit GitHub Actions workflows for security weaknesses; GH_TOKEN enables the online audits
	$(ZIZMOR) --persona regular .

shellcheck: ## ShellCheck every shell script through the pinned container
	@test -n "$(SHELL_FILES)" || { echo "no shell scripts found; skipping ShellCheck"; exit 0; }
	docker run --rm --read-only --network none -v "$(CURDIR):/src:ro" -w /src "$(SHELLCHECK_IMAGE)" -x -- $(SHELL_FILES)

dockerfile-check: ## Lint build/docker/Dockerfile with BuildKit's built-in checks
	$(VALIDATION_LOCK) docker buildx build --check -f build/docker/Dockerfile .

runtime-image-build: ## Build the runtime image as RUNTIME_IMAGE from the repository context; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) bash scripts/ci/runtime-image-build.sh "$(RUNTIME_IMAGE)"

runtime-image-check: ## Start RUNTIME_IMAGE hardened, await readiness, assert RUNTIME_EXPECTED_COMMIT, stop inside the grace budget; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) bash scripts/ci/runtime-image-check.sh "$(RUNTIME_IMAGE)" "$(RUNTIME_EXPECTED_COMMIT)"

container-security: ## Trivy over CONTAINER_IMAGE: fixable HIGH and CRITICAL findings fail; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) docker run --rm \
		-v /var/run/docker.sock:/var/run/docker.sock \
		-v "$(TRIVY_CACHE_VOLUME):/root/.cache/trivy" \
		-e DOCKER_HOST=unix:///var/run/docker.sock \
		-e TRIVY_DB_REPOSITORY \
		"$(TRIVY_IMAGE)" image \
		--cache-dir /root/.cache/trivy \
		--quiet \
		--severity HIGH,CRITICAL \
		--scanners vuln \
		--ignore-unfixed \
		--exit-code 1 \
		--format table \
		"$(CONTAINER_IMAGE)"

# The SBOM describes the shipped artifact: Debian packages plus the Rust
# dependency list cargo-auditable embedded in the binary.
SBOM_OUTPUT ?= sbom.cdx.json
container-sbom: ## Write a CycloneDX SBOM of CONTAINER_IMAGE to SBOM_OUTPUT with Trivy; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) docker run --rm \
		-v /var/run/docker.sock:/var/run/docker.sock \
		-v "$(TRIVY_CACHE_VOLUME):/root/.cache/trivy" \
		-v "$(CURDIR):/out" \
		-e DOCKER_HOST=unix:///var/run/docker.sock \
		-e TRIVY_DB_REPOSITORY \
		"$(TRIVY_IMAGE)" image \
		--cache-dir /root/.cache/trivy \
		--quiet \
		--scanners vuln \
		--format cyclonedx \
		--output "/out/$(SBOM_OUTPUT)" \
		"$(CONTAINER_IMAGE)"

publish-image-metadata-check: ## Self-test of the publication naming and tag promotion
	bash scripts/ci/publish-image-metadata.sh self-test

openapi-generate: ## Regenerate api/openapi/service.yaml from the Rust contract
	@tmp="$$(mktemp)" && $(CARGO) run -q -p $(SERVICE_BIN) --bin openapi $(CARGO_FLAGS) > "$$tmp" && mv "$$tmp" $(OPENAPI_FILE)

openapi-check: openapi-lint ## Fail when the committed document is stale or fails lint
	$(CARGO) test -p $(SERVICE_BIN) $(CARGO_FLAGS) --test openapi

openapi-lint: ## Lint and validate the committed document with Redocly CLI
	@command -v npx >/dev/null 2>&1 || { echo "openapi-lint requires Node.js (npx) for @redocly/cli@$(REDOCLY_CLI_VERSION)" >&2; exit 2; }
	REDOCLY_SUPPRESS_UPDATE_NOTICE=true REDOCLY_TELEMETRY=off npm_config_prefer_offline=true $(REDOCLY_CLI) lint --config .redocly.yaml $(OPENAPI_FILE)

openapi-breaking: ## Compare the document with BASE_OPENAPI=<file> for breaking changes
	@test -n "$(BASE_OPENAPI)" || { echo "openapi-breaking requires BASE_OPENAPI=<path to the base document>" >&2; exit 2; }
	$(call REQUIRE_GO,oasdiff@v$(OASDIFF_VERSION))
	@if [ -s "$(OPENAPI_BREAKING_APPROVALS)" ]; then \
		$(OASDIFF) breaking --fail-on ERR --err-ignore "$(OPENAPI_BREAKING_APPROVALS)" "$(BASE_OPENAPI)" $(OPENAPI_FILE); \
	else \
		$(OASDIFF) breaking --fail-on ERR "$(BASE_OPENAPI)" $(OPENAPI_FILE); \
	fi

plan: ## Print the verification route for the changed surfaces without running it
	$(VERIFY) --plan

verify: ## Run the route for the changed surfaces and record a receipt
	$(VERIFY)

verify-check: ## Self-test of scripts/ci/verify.sh
	$(VERIFY) --self-test

changed-surfaces-check: ## Self-test of the surface classifier
	bash scripts/ci/changed-surfaces.sh --self-test

affected-crates-check: ## Self-test of the affected-crate planner
	bash scripts/ci/affected-crates.sh --self-test

validation-lock-self-test: ## Self-test of the validation lock
	bash scripts/ci/validation-lock.sh --self-test

check: ## Full repository gate under the validation lock; ALLOW_FULL=1 (CI sets CI=true)
	$(FULL_GUARD)
	$(VALIDATION_LOCK) $(MAKE) check-unlocked

check-unlocked: fmt-check lint test unused-deps openapi-lint check-skills \
	changed-surfaces-check affected-crates-check validation-lock-self-test verify-check

clean: ## Remove build output
	$(CARGO) clean
