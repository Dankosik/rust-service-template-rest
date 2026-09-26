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
| `make migration-check` | Static append-only history (`BASE_REF` for a range; the worktree with untracked files by default) and the `migrate` crate's source-rule tests over the embedded set | toolchain |
| `make migration-history-self-test` | Self-test of `scripts/ci/migration-history-check.sh` | — |
| `ALLOW_HEAVY=1 make migration-validate RUNTIME_EXPECTED_COMMIT=<sha>` | Rehearse the image: `/migrate` against a fresh compose database, replay must be `no_change`, then `runtime-image-check` with the profile enabled; uses the local image default from `make/service.mk` | Docker, curl |
<!-- template:end postgres:commands-postgres -->

## Routing and aggregates

| Command | Does |
| --- | --- |
| `make plan` | Classify the worktree's changes since `BASE_REF` and print the route: files, surfaces, commands with reasons and cost, CI-owned steps, surfaces with nothing to run |
| `make verify` | Run that route's local steps under the validation lock; write an attempt record and, on a complete pass, a receipt under `<git-common-dir>/codex/verify` that is partial while CI-owned steps remain |
| `make changed-surfaces-check`, `make affected-crates-check`, `make validation-lock-self-test`, `make verify-check` | The validation scripts' self-tests |
| `ALLOW_FULL=1 make check` | The full repository gate under the lock: `fmt-check`, `lint`, `test`, `unused-deps`, `openapi-lint`, `check-instructions`, `docs-check`, selected profile checks, and the five self-tests |

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
| `TOOLS_ROOT` | Where the Cargo tools are built (default `<git-common-dir>/tools`) |
| `RUNTIME_IMAGE`, `CONTAINER_IMAGE`, `RUNTIME_EXPECTED_COMMIT`, `SBOM_OUTPUT` | Image targets' tag, scan target, expected `app.commit`, SBOM path |
<!-- template:begin postgres:commands-postgres-port -->
| `POSTGRES_PORT` | Host port of `make compose-up` (default `5432`); the proof scripts use an ephemeral port |
<!-- template:end postgres:commands-postgres-port -->

`make help` prints the current catalog; when this document and `make help`
disagree, `make/template.mk` is right and this document is stale.
