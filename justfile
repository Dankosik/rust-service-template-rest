set shell := ["bash", "-euo", "pipefail", "-c"]

export PYTHONDONTWRITEBYTECODE := "1"

service_package := env("SERVICE_PACKAGE", "service")
service_bin := env("SERVICE_BIN", "service")
runtime_image := env("RUNTIME_IMAGE", "service:ci")
container_image := env("CONTAINER_IMAGE", runtime_image)
local_config := env("LOCAL_CONFIG", "env/config/local.toml")
openapi_file := env("OPENAPI_FILE", "api/openapi/service.yaml")
base_ref := env("BASE_REF", "origin/main")
nextest_threads := env("NEXTEST_TEST_THREADS", `echo $((2 * $(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)))`)

# List available recipes
default:
    @just --list

# --- Core Rust development ---

# Build every workspace crate in debug mode
build:
    cargo build --workspace --locked

# Start the HTTP service locally
run:
    cargo run -p "{{service_bin}}" --locked -- --config "{{local_config}}"

# Run workspace unit tests and doctests
[positional-arguments]
test *args:
    cargo nextest run --no-tests=pass --test-threads="{{nextest_threads}}" --locked --workspace "$@"
    cargo test --doc --workspace --no-fail-fast --locked

# Run one crate's tests
[positional-arguments]
test-package pkg *args:
    cargo nextest run --no-tests=pass --test-threads="{{nextest_threads}}" --locked -p "$1" "${@:2}"
    cargo test --doc -p "$1" --no-fail-fast --locked

# Run tests only for crates affected since base (fail-closed fallback to workspace)
[positional-arguments]
test-changed base=base_ref *args:
    python3 -m scripts.checks.rust_selection test --base "$1" --working-tree --allow-empty -- "${@:2}"

# Run Clippy and rustdoc only for crates affected since base
lint-changed base=base_ref:
    python3 -m scripts.checks.rust_selection lint --base "{{base}}" --working-tree --allow-empty

# Report net test-line growth since base against the advisory PR budget
test-budget base=base_ref:
    python3 -m scripts.checks.test_budget --base "{{base}}" --working-tree

# Format every crate
fmt:
    cargo --locked fmt --all
    # template:begin grpc:just-fmt-grpc-codegen
    cargo --locked fmt --manifest-path tools/grpc-codegen/Cargo.toml --all
    # template:end grpc:just-fmt-grpc-codegen

# Check formatting against rustfmt
fmt-check:
    cargo --locked fmt --all --check
    # template:begin grpc:just-fmt-check-grpc-codegen
    cargo --locked fmt --manifest-path tools/grpc-codegen/Cargo.toml --all --check
    # template:end grpc:just-fmt-check-grpc-codegen

# Run Clippy and rustdoc over all workspace targets
lint:
    #!/usr/bin/env bash
    set -euo pipefail
    features=()
    # template:begin integration:just-lint-integration-tests
    features+=(--features integration-tests/integration)
    # template:end integration:just-lint-integration-tests
    # template:begin messaging:just-lint-messaging
    features+=(--features infra-messaging/integration)
    # template:end messaging:just-lint-messaging
    # template:begin cache:just-lint-cache
    features+=(--features infra-cache/integration)
    # template:end cache:just-lint-cache
    # template:begin object-storage:just-lint-object-storage
    features+=(--features infra-object-storage/integration)
    # template:end object-storage:just-lint-object-storage
    # template:begin outbound-auth:just-lint-outbound-auth
    features+=(--features infra-oauth2-client-credentials/integration)
    # template:end outbound-auth:just-lint-outbound-auth
    # template:begin temporal:just-lint-temporal
    features+=(--features infra-temporal/integration)
    # template:end temporal:just-lint-temporal
    cargo clippy --workspace --all-targets --keep-going ${features[@]+"${features[@]}"} --locked -- -D warnings
    RUSTDOCFLAGS="${RUSTDOCFLAGS:+${RUSTDOCFLAGS} }-D warnings" \
        cargo doc --keep-going --workspace --no-deps --document-private-items ${features[@]+"${features[@]}"} --locked
    # template:begin grpc:just-lint-grpc-codegen
    CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-{{justfile_directory()}}/target/grpc-codegen}" \
        cargo clippy --manifest-path tools/grpc-codegen/Cargo.toml --all-targets --keep-going --locked -- -D warnings
    CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-{{justfile_directory()}}/target/grpc-codegen}" \
        RUSTDOCFLAGS="${RUSTDOCFLAGS:+${RUSTDOCFLAGS} }-D warnings" \
        cargo doc --keep-going --manifest-path tools/grpc-codegen/Cargo.toml --no-deps --document-private-items --locked
    # template:end grpc:just-lint-grpc-codegen

# Run Clippy and rustdoc on one crate
lint-package pkg:
    cargo clippy -p "{{pkg}}" --all-targets --keep-going --locked -- -D warnings
    RUSTDOCFLAGS="${RUSTDOCFLAGS:+${RUSTDOCFLAGS} }-D warnings" \
        cargo doc --keep-going --no-deps --document-private-items -p "{{pkg}}" --locked

# Lint and document optional hotpath profiling features
profiling-check:
    #!/usr/bin/env bash
    set -euo pipefail
    lib_pkgs=()
    lib_feats=()
    for pkg in infra-http infra-telemetry infra-postgres infra-jobs infra-webhooks; do
        if [[ -f "crates/${pkg}/Cargo.toml" ]]; then
            lib_pkgs+=(-p "${pkg}")
            lib_feats+=("${pkg}/hotpath")
        fi
    done
    if ((${#lib_pkgs[@]} > 0)); then
        joined_feats=$(IFS=,; echo "${lib_feats[*]}")
        cargo clippy --locked "${lib_pkgs[@]}" --all-targets --keep-going --features "${joined_feats}" -- -D warnings
        RUSTDOCFLAGS="${RUSTDOCFLAGS:+${RUSTDOCFLAGS} }-D warnings" \
            cargo doc --locked "${lib_pkgs[@]}" --no-deps --document-private-items --keep-going --features "${joined_feats}"
    fi
    full_feats="hotpath,hotpath-alloc,hotpath-mcp,hotpath-cpu,hotpath/hotpath-prometheus"
    cargo clippy --locked --manifest-path crates/service/Cargo.toml --all-targets --features hotpath -- -D warnings
    cargo clippy --locked --manifest-path crates/service/Cargo.toml --all-targets --features "${full_feats}" -- -D warnings
    RUSTDOCFLAGS="${RUSTDOCFLAGS:+${RUSTDOCFLAGS} }-D warnings" \
        cargo doc --locked --manifest-path crates/service/Cargo.toml --no-deps --document-private-items --features "${full_feats}"

# Mutation testing for one crate (optional file filter)
[positional-arguments]
mutants pkg file="":
    #!/usr/bin/env bash
    set -euo pipefail
    extra=()
    if [[ -n "${2:-}" ]]; then
        extra=(--file "$2")
    fi
    exec mise exec "cargo:cargo-mutants@${CARGO_MUTANTS_VERSION:-27.1.0}" -- \
        cargo-mutants mutants --test-tool nextest --package "$1" \
        ${extra[@]+"${extra[@]}"} --cargo-arg=--locked --output target/mutants

# Remove build output
clean:
    cargo clean

# --- Quality, security, and contracts ---

# Check advisories, licenses, bans, and sources (deny.toml)
deny:
    cargo deny --locked check

# Fail on unused dependencies
unused-deps:
    cargo-shear --locked
    # template:begin grpc:just-unused-deps-grpc-codegen
    cargo-shear --locked tools/grpc-codegen
    # template:end grpc:just-unused-deps-grpc-codegen

# Check vendored forks integrity (pass --online or --upstream for network checks)
[positional-arguments]
vendor-drift *args:
    python3 -m scripts.checks.vendor drift "$@"

# Run locked serial native transport regressions for retained vendor crates
[positional-arguments]
native-transport *args:
    python3 -m scripts.checks.vendor transport "$@"

# Check source code and docs for typos
typos:
    typos --config .typos.toml

# Scan worktree (locally) and commits since base with gitleaks
secret-scan base=base_ref:
    #!/usr/bin/env bash
    set -euo pipefail
    flags=(--no-banner --redact --verbose --exit-code 1 --config .gitleaks.toml)
    if [[ "${CI:-}" != "true" ]]; then
        gitleaks dir "${flags[@]}" .
    fi
    git cat-file -e "{{base}}^{commit}" 2>/dev/null || { echo "secret-scan base unavailable: {{base}}" >&2; exit 2; }
    gitleaks git "${flags[@]}" --log-opts="{{base}}..HEAD" .

# Scan full HEAD ancestry with gitleaks
secret-scan-history:
    test "$(git rev-parse --is-shallow-repository 2>/dev/null)" = "false" || { echo "secret-scan-history requires full history" >&2; exit 2; }
    gitleaks git --no-banner --redact --verbose --exit-code 1 --config .gitleaks.toml --log-opts=HEAD .

# Lint GitHub Actions workflows
actionlint:
    actionlint -pyflakes=

# Audit GitHub Actions workflows for security weaknesses
zizmor:
    zizmor --persona regular .github

# Lint shell scripts with ShellCheck
shellcheck:
    #!/usr/bin/env bash
    set -euo pipefail
    files=()
    while IFS= read -r -d '' f; do files+=("$f"); done < <(git ls-files -z --cached --others --exclude-standard -- '*.sh')
    ((${#files[@]} == 0)) || shellcheck -x -- "${files[@]}"

# Lint Python scripts with ruff
python-lint:
    ruff check --no-cache .

# Check relative Markdown links and fragments offline
docs-check:
    #!/usr/bin/env bash
    set -euo pipefail
    files=()
    while IFS= read -r -d '' f; do files+=("$f"); done < <(
        git ls-files -z --cached --others --exclude-standard -- '*.md' \
            ':(exclude)vendor/**' ':(exclude).agents/adapters/**' ':(exclude)specs/**'
    )
    ((${#files[@]} == 0)) || lychee --offline --include-fragments --no-progress --root-dir "{{justfile_directory()}}" -- "${files[@]}"

# Enforce workspace crate dependency rules and in-package #[path]
architecture-check:
    python3 -m scripts.checks.architecture

# Validate Prometheus alert rules and fixtures
monitoring-rules:
    promtool check rules env/monitoring/*.rules.yml
    promtool test rules env/monitoring/tests/*-rules.yml

# Run unit tests for repository scripts
scripts-test:
    python3 -m unittest discover -s scripts/tests -p 'test_*.py'

# Regenerate api/openapi/service.yaml from the Rust contract
openapi-generate:
    #!/usr/bin/env python3
    import json, pathlib, subprocess
    out = subprocess.check_output(["cargo", "build", "-q", "--workspace", "--bin", "openapi", "--locked", "--message-format=json-render-diagnostics"], text=True)
    bins = [o["executable"] for line in out.splitlines() if (o := json.loads(line)).get("reason") == "compiler-artifact" and o["target"]["name"] == "openapi" and "bin" in o["target"]["kind"] and o.get("executable")]
    spec = subprocess.check_output([bins[-1]])
    dst = pathlib.Path("{{openapi_file}}")
    tmp = dst.with_suffix(dst.suffix + ".tmp")
    tmp.write_bytes(spec)
    tmp.replace(dst)

# Lint committed OpenAPI document with Redocly CLI
openapi-lint:
    redocly lint --config .redocly.yaml "{{openapi_file}}"

# Fail when committed OpenAPI document is stale or fails lint
openapi-check: openapi-lint
    cargo test -p "{{service_bin}}" --locked --test openapi

# Compare OpenAPI document against a base OpenAPI file
openapi-breaking base_openapi:
    oasdiff breaking --fail-on ERR "{{base_openapi}}" "{{openapi_file}}"
    # template:begin http-idempotency:just-openapi-idempotency
    work=$(mktemp -d); trap 'rm -rf "$work"' EXIT; \
    oasdiff upgrade --allow-external-refs=false --format json -- "{{base_openapi}}" >"$work/base.json" && \
    oasdiff upgrade --allow-external-refs=false --format json -- "{{openapi_file}}" >"$work/cand.json" && \
    python3 -m scripts.checks.openapi_idempotency "$work/base.json" "$work/cand.json"
    # template:end http-idempotency:just-openapi-idempotency

# Compare OpenAPI document against a git ref
openapi-breaking-since base=base_ref:
    #!/usr/bin/env bash
    set -euo pipefail
    git cat-file -e "{{base}}:{{openapi_file}}" 2>/dev/null || { echo "{{openapi_file}} unavailable at {{base}}" >&2; exit 2; }
    base_file=$(mktemp)
    trap 'rm -f "${base_file}"' EXIT
    git show "{{base}}:{{openapi_file}}" >"${base_file}"
    "{{just_executable()}}" openapi-breaking "${base_file}"

# --- Container image ---

# Run BuildKit native checks and rust-toolchain pin check on Dockerfile
dockerfile-check:
    python3 -m scripts.image dockerfile-check

# Build runtime container image
image-build image=runtime_image:
    python3 -m scripts.image build "{{image}}"

# Smoke-test runtime image hardened (readiness, licenses, jemalloc, SIGTERM)
[positional-arguments]
image-check image=runtime_image commit="":
    python3 -m scripts.image check "$1" "$2"

# Scan container image for fixable HIGH/CRITICAL vulnerabilities (Trivy)
image-scan image=container_image:
    python3 -m scripts.image scan "{{image}}"

# Generate CycloneDX SBOM for container image
image-sbom image=container_image output="sbom.cdx.json":
    python3 -m scripts.image sbom "{{image}}" "{{output}}"

# --- Optional capability profiles ---

# template:begin integration:just-integration-compile
# Compile retained integration test suites without running them
integration-compile suite="retained":
    python3 -m scripts.integration compile --suite "{{suite}}"
# template:end integration:just-integration-compile

# template:begin postgres:just-postgres-profile
# Start local PostgreSQL from env/docker-compose.yml
compose-up:
    docker compose -f env/docker-compose.yml up -d --wait postgres

# Stop local compose services and remove volumes
compose-down:
    docker compose -f env/docker-compose.yml down -v --remove-orphans

# Run PostgreSQL integration suite against throwaway Compose
[positional-arguments]
test-integration-db *args:
    python3 -m scripts.integration db -- "$@"

# Regenerate .sqlx/ offline query metadata
sqlx-prepare:
    python3 -m scripts.integration sqlx-prepare

# Verify .sqlx/ matches current queries and migrations
sqlx-check:
    python3 -m scripts.integration sqlx-prepare --check

# Verify append-only migration history, squawk lint, and migrate unit tests
migration-check base=base_ref:
    python3 -m scripts.checks.migrations --base "{{base}}"
    cargo test -p migrate --locked

# Rehearse /migrate and service startup with runtime image against fresh PostgreSQL
[positional-arguments]
migration-validate image=runtime_image commit="":
    python3 -m scripts.image check --with-migrations "$1" "$2"
# template:end postgres:just-postgres-profile

# template:begin grpc:just-grpc-profile
# Generate protobuf Rust contracts from api/proto/
grpc-generate:
    python3 -m scripts.grpc generate

# Check protobuf format, lint, codegen drift, and breaking changes
grpc-check base=base_ref:
    python3 -m scripts.grpc check --base "{{base}}"

# Check advisories/licenses for tools/grpc-codegen
grpc-generator-deny:
    cargo deny --manifest-path tools/grpc-codegen/Cargo.toml --config tools/grpc-codegen/deny.toml --locked check
# template:end grpc:just-grpc-profile

# template:begin messaging:just-messaging-profile
# Run NATS JetStream integration suite against throwaway Compose
[positional-arguments]
test-integration-messaging *args:
    python3 -m scripts.integration messaging -- "$@"
# template:end messaging:just-messaging-profile

# template:begin cache:just-cache-profile
# Run Valkey cache integration suite against throwaway Compose
[positional-arguments]
test-integration-cache *args:
    python3 -m scripts.integration cache -- "$@"
# template:end cache:just-cache-profile

# template:begin object-storage:just-object-storage-profile
# Run S3 adapter suite against throwaway Compose versitygw
[positional-arguments]
test-integration-object-storage *args:
    python3 -m scripts.integration object-storage -- "$@"

# Run live S3 conformance suite (requires OBJECT_STORAGE_CONFORMANCE_WRITES=allow)
test-object-storage-conformance provider:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{provider}}" in amazon_s3|cloudflare_r2|railway|s3_compatible) ;; *) echo "invalid provider" >&2; exit 2 ;; esac
    test "${OBJECT_STORAGE_CONFORMANCE_WRITES:-}" = "allow" || { echo "set OBJECT_STORAGE_CONFORMANCE_WRITES=allow" >&2; exit 2; }
    OBJECT_STORAGE_CONFORMANCE_WRITES=allow OBJECT_STORAGE_CONFORMANCE_PROVIDER="{{provider}}" \
        cargo test -p infra-object-storage --features integration --test conformance --locked -- --ignored --no-capture
# template:end object-storage:just-object-storage-profile

# template:begin outbound-auth:just-outbound-auth-profile
# Run OAuth2 integration suite against throwaway Keycloak
[positional-arguments]
test-integration-oauth *args:
    python3 -m scripts.integration oauth -- "$@"
# template:end outbound-auth:just-outbound-auth-profile

# template:begin temporal:just-temporal-profile
# Run Temporal integration suite against throwaway Compose
[positional-arguments]
test-integration-temporal *args:
    python3 -m scripts.integration temporal -- "$@"
# template:end temporal:just-temporal-profile

# template:begin source-template:just-template-targets
# Materialize selected agent harness files from .agents/
agent-harness-sync harness="all":
    python3 -m template.harness --apply --repo . --harness "{{harness}}"

# Validate .agents/skills, roles, and materialized harness files
check-instructions harness="all":
    python3 -m template.harness --check --repo . --harness "{{harness}}"

# Verify or refresh JetStream Go wire-compatibility golden fixtures (pass --update to refresh)
[positional-arguments]
messaging-go-compat *args:
    python3 -m template.messaging_go_compat "$@"

# Run Linux CPU-quota runtime progress proof
runtime-progress-proof:
    python3 -m template.runtime_progress

# Initialize a new service from this template once
[positional-arguments]
template-init *args:
    python3 -m template.init --repo . "$@"

# Fast check of canonical profile projections (no Cargo)
[positional-arguments]
template-check *args:
    python3 -m template.check --projections "$@"

# Build, lint, and test canonical profile matrix
[positional-arguments]
template-matrix *args:
    python3 -m template.check --matrix "$@"

# Build and smoke-test release images for representative profiles
[positional-arguments]
template-artifacts *args:
    python3 -m template.check --artifacts "$@"

# Run unit tests for template initializer, harness, and CI helpers
template-test:
    python3 -m unittest discover -s template/tests -p 'test_*.py'
# template:end source-template:just-template-targets

# Run full local quality gate
check: fmt-check lint test unused-deps typos openapi-lint docs-check architecture-check monitoring-rules scripts-test
    # template:begin postgres:just-check-postgres
    "{{just_executable()}}" migration-check
    # template:end postgres:just-check-postgres
    # template:begin source-template:just-check-template
    "{{just_executable()}}" check-instructions
    "{{just_executable()}}" template-check
    "{{just_executable()}}" template-test
    # template:end source-template:just-check-template
