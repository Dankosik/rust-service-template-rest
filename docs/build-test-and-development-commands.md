# Build, Test, And Development Commands

The human-facing explanation of every make target. `make/template.mk` owns
the composition (`make help` lists the targets from it); a derived service
adds its own in `make/service.mk`. Every Cargo command runs with `--locked`,
so a lockfile change is part of a change, never a side effect of running
one. [AGENTS.md](../AGENTS.md#validation-budget) owns which of these a change
needs; [Validation Routing](validation-routing.md) selects beyond that.

## Everyday development

| Command | Does | Needs |
| --- | --- | --- |
| `make build` | `cargo build --workspace` in debug | toolchain |
| `make run` | Start the service with `env/config/local.toml` (`text` logs, `127.0.0.1` listeners) | toolchain |
| `make test` | The workspace test suite, including the process tests of the built binary and the OpenAPI drift and contract tests; the database tests compile with their feature off and need no Docker | toolchain |
| `make test-package PKG=<crate>` | One crate's tests | toolchain |
| `make test-changed PKGS="<crate> <crate>"` | The tests of the crates `scripts/ci/affected-crates.sh` prints for a change | toolchain |
| `make fmt` / `make fmt-check` | rustfmt over the workspace; the check fails on a diff | toolchain |
| `make lint` | clippy over all targets at the workspace lint levels, warnings as errors; includes the database tests through `--features integration-tests/integration` | toolchain |
| `make lint-changed PKGS="<crate> <crate>"` | The same clippy over the selected crates | toolchain |
| `make clean` | `cargo clean` | toolchain |

Crate names are package names: `service`, `service-config` (the
`crates/config` directory), `health`, `infra-http`, `infra-telemetry`.

<!-- template:begin authn:docs-commands-authn -->
With an authentication profile retained, `infra-bearerauthn` is also a package name for `make test-package`. Its default-off `test-support` feature exists only for consuming dev-dependencies that mount the real verifier in tests; it is not part of the ordinary release build or a production configuration knob.
<!-- template:end authn:docs-commands-authn -->
<!-- template:begin outbound-http:docs-commands-outbound -->
With `OUTBOUND_HTTP=bounded`, `infra-outbound-http` is available to
`make test-package PKG=infra-outbound-http`; the [guide](outbound-http.md)
owns construction and operation policy. Its default-off `test-support` feature
is only for a consuming dev-dependency that uses the literal-loopback HTTP mock
constructor; it is not a production configuration knob.
<!-- template:end outbound-http:docs-commands-outbound -->
<!-- template:begin outbound-auth:docs-commands-outbound-auth -->
With `OUTBOUND_AUTH=oauth2-client-credentials`,
`infra-oauth2-client-credentials` is available to
`make test-package PKG=infra-oauth2-client-credentials`. Its tests consume
bounded outbound HTTP's dev-only mock support; the profile adds no runtime
command or provider call. `ALLOW_HEAVY=1 make test-integration-oauth` is the
Keycloak proof when the current Make owner retains that target; it starts a
throwaway container, or uses a running one named by `OAUTH_TEST_KEYCLOAK_URL`,
and certifies no other provider. [Outbound machine
authentication](outbound-machine-authentication.md) owns construction and
compatibility limits.
<!-- template:end outbound-auth:docs-commands-outbound-auth -->


## Readability and crate boundaries

| Command | Does | Needs |
| --- | --- | --- |
| `make duplication-check` | Scan all handwritten main-workspace Rust and reject substantial clones outside reviewed source bounds | Python 3, pinned Cargo, Git, Node.js (`npx`) |
| `make duplication-report OUTPUT=<directory>` | Write gated-source and dedicated-test reports; default `target/quality-reports`; never renew admissions | Same tools |
| `make architecture-check` | Check all declared workspace dependency edges against service policy | Python 3, pinned Cargo |
| `make quality-check-self-test` | Temporary checker fixtures plus small real jscpd and Clippy smoke cases | Python 3, Git, pinned Cargo/Clippy (`rustup`), Node.js (`npx`) |
| `make template-quality-projections` | Source-template-only checker proof on four renamed retained/removed profile representatives, independent of harness variants | Same tools; runner snapshots the current candidate privately |

Install the toolchain with `rustup toolchain install` and provide Node.js with
`npx`; the wrapper resolves jscpd at `JSCPD_VERSION` in `tools/versions.env`
(5.4.0). No global npm installation or npm project is needed. Missing tools,
version mismatch, invalid policy, malformed reports, and unexpected empty
source scope fail the selected check.

Clippy's `excessive_nesting` limit in `clippy.toml` is six. A seventh nested
block fails the existing lint target; an exception belongs on the smallest
owning function with a concrete reason. The security JSON table declares each
expected result directly; the gRPC provider fixture returns early on a failed
TLS handshake. Both stay below the limit without an allowance.
The introspection provider's `Fixture::new` and the messaging handler-token
cancellation test retain function-local allowances: their nested connection
tasks, bounded exchanges, teardown and callback ownership are explicit parts
of those test harnesses. Reconsider them when an independently reusable
exchange or duplicated callback emerges; extraction solely to lower a depth
counter adds no behavioral owner.

`.jscpd.json` selects Rust, mild matching, 100 tokens and a line-index distance
of 15 (normally 16 inclusive lines). The gated scan includes inline tests;
dedicated test, bench, example and fixture files have a separate report-only
scan. Generated gRPC Rust is excluded by its exact generated owner. Reports
measure copied text, not semantic similarity or overall test quality.

`quality/duplication-baseline.json` is reviewed service policy. Each admission
names its reason, source occurrences, bounded text and token ceilings. Harmless
shifts and deletion-only shrinkage can retain admission; growth, changed code,
new paths and extra copies fail. To change an admission, inspect both reported
ranges and deliberately update the case, its anchors and its reason. Neither
checking, reporting, initialization nor CI refreshes the baseline. Removing a
profile leaves unused admissions harmless; it does not increase another case's
allowance. Keep the calibrated config and both `quality/` policies service-owned
when syncing portable checker scripts.

The diagnosed repetitions have individual maintenance decisions:

| Admission | Decision and reason | Reconsider when |
| --- | --- | --- |
| P1 histogram scaffolding | Keep beside cache and object-storage metrics; their dimensions and provider ownership differ. | A real shared metric owner replaces both implementations without joining provider lifecycles. |
| P2 cleanup failure mapping | Keep transaction-outcome mapping beside each cleanup operation and its log domain. | Both owners deliberately adopt the same failure policy and reporting owner. |
| P3 periodic cleanup | Keep SQL, cadence, cancellation and metrics together in their provider. | A shared lifecycle owner is needed for behavior, beyond removing copied lines. |
| P4 native stop receivers | Keep receiver declarations and the Unix install entry in each composition root; each retains its native streams through runtime shutdown and owns its signal-error and stop policy. | An accepted shared signal lifecycle owner is needed for behavior beyond these native declarations. |
| T1 actor and T5 shutdown scenarios | Keep independent expected cases at JWT/introspection and service/worker boundaries. | The scenario contract itself changes; repeated assertions alone do not justify shared policy code. |
| T2–T4 metric recorders | Inbound/outbound webhook tests share one crate-private `cfg(test)` counter-key recorder. HTTP and JWT retain their own recorders, with no cross-crate test dependency. | Recorder responsibilities converge across an existing shared test owner, or their observed events diverge. |

The webhook fixture lives outside the independently removed direction modules.
The factored quality projection check compiles its inbound-only and outbound-only
test consumers; it does not rebuild the harness matrix.

The [boundary policy](architecture/boundaries.md#executable-dependency-policy)
owns allowed crate directions. Architecture checks inspect declarations,
including optional, target-specific, aliased and build dependencies, regardless
of the active features or host.

## Contract

| Command | Does | Needs |
| --- | --- | --- |
| `make openapi-generate` | Render `api/openapi/service.yaml` from the handlers' `#[utoipa::path]` annotations through the `openapi` binary | toolchain |
| `make openapi-lint` | Redocly CLI over the committed document (`.redocly.yaml`) | Node.js (`npx`) |
| `make openapi-check` | `openapi-lint` plus the `service` contract tests: byte-exact drift, every operation's security decision, closed problem schemas | toolchain, Node.js |
| `make openapi-breaking BASE_OPENAPI=<file>` | oasdiff breaking-change comparison; `api/openapi/breaking-changes-approvals.txt` lists accepted breaks | Go (`go run`) |

## Dependency, secret, and workflow gates

| Command | Does | Needs |
| --- | --- | --- |
| `make deny` | cargo-deny: advisories, licenses, bans, sources over the locked graph under `deny.toml` | Cargo tool (built once) |
| `make unused-deps` | cargo-shear: a declared dependency no crate uses fails | Cargo tool (built once) |
| `make secret-scan` | Gitleaks over the worktree and the commits since `BASE_REF` (`origin/main`) | Go |
| `ALLOW_HEAVY=1 make secret-scan-history` | Gitleaks over every commit on every branch | Go |
| `make actionlint` | Workflow syntax and expression checks (host shellcheck and pyflakes integrations off) | Go |
| `make zizmor` | Workflow security audit; `GH_TOKEN=$(gh auth token)` enables the online audits | Cargo tool (built once) |
| `make shellcheck` | ShellCheck over every tracked script; `SHELL_FILES='a.sh b.sh'` scopes it | Docker |
| `make docs-check` | Every relative Markdown link and `#fragment` resolves; offline | Docker |
| `make check-skills` | The shape of `.agents/skills/*` (decision and workflow classes) | Python 3 |
| `make agent-roles-check`, `make codex-agents-check`, `make claude-skills-check`, `make qwen-skills-check` | The generated harness carriers (`.codex`, `.claude`, `.qwen`, `.grok`, `.cursor`, `.opencode`) are byte-stable against `.agents/roles`, `.agents/codex-project.toml`, and `.agents/skills`; the `*-sync` twins regenerate them | — |
| `make check-instructions` | `check-skills` plus the four carrier checks; what CI runs on the `agent_instructions` surface | Python 3 |
| `make tools-check` | `tools/versions.env` shape and digests; each Cargo tool reports its pin; the Dockerfile `ARG` defaults and `FROM` tags agree with the manifest and `rust-toolchain.toml` | Cargo tools (built once) |
<!-- template:begin grpc:docs-command-grpc -->
| `make grpc-generate` | Generate committed protobuf Rust and its descriptor set from Buf's descriptor set, built with imports, with stock tonic-prost-build | Go, Rust |
| `make grpc-check` | Buf format/lint, generation drift and exact base FILE compatibility; heavy and CI-owned by default | Same tools; `GRPC_BASE_REF` in CI |
<!-- template:end grpc:docs-command-grpc -->

The Cargo tools (`cargo-deny`, `cargo-shear`, `zizmor`) build from
crates.io into `<git-common-dir>/tools/<crate>-<version>` the first time a
target needs them (about six minutes in total), then never again for that
version. CI installs the same versions as prebuilt binaries.

## Image

| Command | Does | Needs |
| --- | --- | --- |
| `make dockerfile-check` | BuildKit's built-in Dockerfile checks | Docker |
| `ALLOW_HEAVY=1 make runtime-image-build` | Build the production image; `VCS_REF`, `APP_VERSION`, `SOURCE_URL`, `SOURCE_DATE_EPOCH`, `RUNTIME_IMAGE_CACHE_FROM`, `RUNTIME_IMAGE_CACHE_TO` are honoured | Docker with BuildKit |
| `ALLOW_HEAVY=1 make runtime-image-check RUNTIME_EXPECTED_COMMIT=<sha>` | Start it `--read-only --cap-drop=ALL --security-opt=no-new-privileges`, await `/health/ready`, assert `app.commit`, require exit `0` from `docker stop --time 45` | Docker, curl |
| `ALLOW_HEAVY=1 make container-security` | Trivy: fixable HIGH and CRITICAL findings fail; Debian and `rustbinary` targets | Docker |
| `ALLOW_HEAVY=1 make container-sbom SBOM_OUTPUT=sbom.cdx.json` | CycloneDX SBOM of the image | Docker |
| `make publish-image-metadata-check` | Self-test of the publication naming and tag promotion script | — |

<!-- template:begin postgres:commands-postgres -->
## PostgreSQL

| Command | Does | Needs |
| --- | --- | --- |
| `make compose-up` / `make compose-down` | Start or drop the local PostgreSQL from `env/docker-compose.yml` (`postgres://app:app@127.0.0.1:${POSTGRES_PORT:-5432}/app?sslmode=disable`) | Docker |
| `ALLOW_HEAVY=1 make test-integration-db` | The database-backed proof: a throwaway compose PostgreSQL on an ephemeral port, `cargo test -p integration-tests --features integration` with `DATABASE_URL`, teardown; `REQUIRE_DOCKER=1` fails instead of refusing without Docker | Docker |
| `make sqlx-prepare` | Regenerate `.sqlx/`, the statement metadata `sqlx::query!` builds against: a throwaway compose PostgreSQL, the migrations applied, every checked statement described | Docker |
| `ALLOW_HEAVY=1 make sqlx-check` | Fail when `.sqlx/` differs from what the statements and the migrations produce now | Docker |
| `make migration-check` | Static append-only history (`BASE_REF` for a range; the worktree with untracked files by default), Squawk over the migrations the change adds, and the `migrate` crate's source-rule tests over the embedded set | toolchain, Node.js (`npx`) when a migration is added |
| `make migration-history-self-test` | Self-test of `scripts/ci/migration-history-check.sh` | — |
| `ALLOW_HEAVY=1 make migration-validate RUNTIME_EXPECTED_COMMIT=<sha>` | Rehearse the image: `/migrate` against a fresh compose database, replay must be `no_change`, then `runtime-image-check` with the profile enabled; uses the local image default from `make/service.mk` | Docker, curl |
<!-- template:end postgres:commands-postgres -->

## Routing and aggregates

| Command | Does |
| --- | --- |
| `make plan` | Classify the worktree's changes since `BASE_REF` and print the route: files, surfaces, commands with reasons and cost, CI-owned steps, surfaces with nothing to run |
| `make verify` | Run that route's local steps under the validation lock; write an attempt record and, on a complete pass, a receipt under `<git-common-dir>/codex/verify` that is partial while CI-owned steps remain |
| `make changed-surfaces-check`, `make affected-crates-check`, `make validation-lock-self-test`, `make verify-check` | The validation scripts' self-tests |
| `ALLOW_FULL=1 make check` | The full repository gate under the lock: `fmt-check`, `lint`, `test`, `unused-deps`, `openapi-lint`, `check-instructions`, `docs-check`, `duplication-check`, `architecture-check`, `quality-check-self-test`, selected profile checks, and the validation self-tests |

In the source template, `ALLOW_FULL=1 make template-init-check` checks 208
canonical projections and initializes/builds/tests twenty-six distinct runtime
representatives. It does not need `ALLOW_HEAVY`. The source runner's
`--projections-only` mode, `make template-init-projections`, performs the
focused projection/equality check without Cargo; it is not a complete
public-initialization or runtime receipt. The
[initializer guide](template-sync.md#validation-boundary) owns this distinction.
<!-- template:begin http-idempotency:docs-commands-http-idempotency -->
With the idempotency pack retained, eight of those twenty-six representatives
(graphs 13-16 and 23-26) also run the retained idempotency database suite and
need a usable Docker daemon; the runner refuses before any target write when
one is missing.
<!-- template:end http-idempotency:docs-commands-http-idempotency -->
<!-- template:begin jobs:docs-commands-jobs -->
With the jobs pack retained, graphs 17-26 also run the jobs database suite
(graphs 23-26 with its joint HTTP idempotency module) and need a usable
Docker daemon. `infra-jobs` and `jobs-worker` are package names for
`make test-package` (`PKG=infra-jobs`, `PKG=jobs-worker`). A local worker
beside `make run` needs its own listeners and the PostgreSQL variables
`make run` uses (`APP__POSTGRES__ENABLED=true` and `APP__POSTGRES__DSN`; see
`env/config/local.toml`).
Once the schema is migrated, run:

```sh
APP__HTTP__ADDR=127.0.0.1:8081 APP__OBSERVABILITY__METRICS__ADDR=127.0.0.1:9091 cargo run --locked -p jobs-worker -- --config env/config/local.toml
```

It runs only once a service registers its kinds; the template's worker
refuses with `no job kind is registered`. See the
[guide](background-jobs.md#configure-and-size-the-worker).
<!-- template:end jobs:docs-commands-jobs -->
<!-- template:begin messaging:docs-commands-messaging -->
With `MESSAGING=nats-jetstream`, `domain-events` and `infra-messaging` are
package names for `make test-package`. `ALLOW_HEAVY=1 make
test-integration-messaging` is the real NATS proof when the current Make owner
retains that target; it requires Docker and does not certify a deployment.
Use `make help` for the exact retained command surface.
<!-- template:end messaging:docs-commands-messaging -->
<!-- template:begin cache:docs-commands-cache -->
With `CACHE=redis`, `infra-cache` is a package name for `make test-package`.
`ALLOW_HEAVY=1 make test-integration-cache` is the Valkey proof when the
current Make owner retains that target; it requires Docker and does not
certify a deployment. A local server is
`docker compose -f env/docker-compose.yml up -d valkey`.
Use `make help` for the exact retained command surface. See the
[guide](cache.md).
<!-- template:end cache:docs-commands-cache -->
<!-- template:begin object-storage:docs-commands-object-storage -->
With `OBJECT_STORAGE=s3`, `infra-object-storage` is a package name for
`make test-package`. `ALLOW_HEAVY=1 make test-integration-object-storage` is
the versitygw proof when the current Make owner retains that target; it
requires Docker and does not certify a provider. A local emulator is
`docker compose -f env/docker-compose.yml up -d --wait versitygw` (root
credentials `template`/`template-secret`, port `VERSITYGW_PORT`, default 7070,
bucket `template-bucket` already created).
`make test-object-storage-conformance PROVIDER=amazon_s3|cloudflare_r2|railway|s3_compatible`
runs the ignored live-provider test against the bucket named by the
service's `APP__OBJECT_STORAGE__*` variables. It writes under a unique prefix,
refuses unless `OBJECT_STORAGE_CONFORMANCE_WRITES=allow` is set, and needs
separate authorization for that bucket. See the [guide](object-storage.md).
<!-- template:end object-storage:docs-commands-object-storage -->

## Guards and variables

| Variable | Meaning |
| --- | --- |
| `ALLOW_FULL=1` | Opt into `make check`, and keep the source initializer matrix local in `make verify`; not a routine follow-up to every edit |
| `ALLOW_HEAVY=1` | Opt into image targets, retained-profile database proof, and the history-wide secret scan, which `make verify` otherwise leaves to CI |
<!-- template:begin postgres:commands-require-docker -->
| `REQUIRE_DOCKER=1` | Make a missing Docker daemon fail `test-integration-db` and `migration-validate` instead of refusing with exit 2; CI sets it |
<!-- template:end postgres:commands-require-docker -->
| `CI=true` | Set by CI; satisfies both guards, resolves the Cargo tools from `PATH`, and skips the worktree half of `secret-scan`. Do not set it locally |
| `BASE_REF` | Comparison base for `plan`, `verify`, and `secret-scan` (default `origin/main`) |
| `PKG` / `PKGS` | One crate for `test-package`; a space-separated list for `lint-changed` and `test-changed` |
| `VERIFY_FORCE=1` | Rerun `make verify` even when an identical receipt exists |
| `VALIDATION_LOCK_DIR` | Override the admission pathname for isolated validation; despite its historical name this is a regular file, with a permanent sibling guard |
| `BUILD_CACHE` | Compiler-cache mode: `inherit` (default) preserves caller context; `sccache` enables the explicit task-local cache |
| `BUILD_CACHE_BIN` | Optional sccache executable for `BUILD_CACHE=sccache`; otherwise the helper resolves `sccache` from `PATH` |
| `BUILD_CACHE_DIR` | Required task-owned cache directory for `BUILD_CACHE=sccache`; never a shared worktree target directory |
| `BUILD_CACHE_SIZE` | Optional private-cache cap, default `1G` |
| `BUILD_MIN_FREE_BYTES` | Optional explicit free-space threshold in bytes; unset means no guessed reserve |
| `TOOLS_ROOT` | Where the Cargo tools are built (default `<git-common-dir>/tools`) |
| `RUNTIME_IMAGE`, `CONTAINER_IMAGE`, `RUNTIME_EXPECTED_COMMIT`, `SBOM_OUTPUT` | Image targets' tag, scan target, expected `app.commit`, SBOM path |
<!-- template:begin postgres:commands-postgres-port -->
| `POSTGRES_PORT` | Host port of `make compose-up` (default `5432`); the proof scripts use an ephemeral port |
<!-- template:end postgres:commands-postgres-port -->

`make help` prints the current catalog; when this document and `make help`
disagree, `make/template.mk` is right and this document is stale.

Validation custody follows [Delivery Validation](validation/delivery.md).
The shared entrypoint waits up to 900 seconds (exit 75 on timeout); verified
nested calls retain the originating generation across Git roots. Current
Docker/Compose owners register work before submission and confirm scoped
completion. Unknown cleanup or build completion retains quarantine (exit 74).
Only an outer verifier whose supervisor confirms full custody completion may
publish a reusable receipt; nested runs retain pending-custody attempts. Legacy
directory clients interoperate on normal admission, but their own reclamation
races are not repaired. The delivery owner establishes legacy completion before
first shared-path activation and owns any later contested-state reconciliation.

`BUILD_CACHE=inherit` keeps the direct Cargo invocation and caller-selected
supported wrapper/output settings unchanged. If their execution context cannot
be projected, verification records it as unknown and does not reuse or publish
an exact-context receipt. `BUILD_CACHE=sccache` is an explicit task-local mode:
it requires the pinned tool, a private cache directory and a joined foreground
server; it never installs a tool, changes global Cargo configuration, or
replaces an incompatible wrapper.
