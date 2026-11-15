# Initialization and portable updates

Initialization gives a clean template checkout its service identity and selects
the existing database, authentication, outbound HTTP, outbound machine
authentication, gRPC, HTTP idempotency, background jobs, messaging, outbox,
outbound webhooks, inbound webhooks, cache, object storage, service
authentication, and agent-harness packs. Later synchronization adopts portable
tooling and instructions from a committed source checkout. The
[ownership manifest](../template-owned.paths) is the full-sync copy authority;
the service keeps its application, configuration and local policies.
For complete runtime updates and rendered-baseline custody, run
`scripts/template-upgrade.sh` (`docs/template-upgrade.md` in an admitted
external template checkout). The portable manifest does not gain runtime
ownership.

## Initialize a service

Start with a clean Git checkout, Python 3.11+, Git, the pinned Rust toolchain,
and fetched lockfile dependencies (`cargo fetch --locked`). The initializer
prunes `Cargo.lock` offline via `cargo metadata --offline`, verifies every
remaining package's name, version, source, and checksum against the template
lock, formats the staged Rust code, and regenerates OpenAPI with
`--locked --offline` before mutating the checkout.

```sh
cargo fetch --locked
make template-init \
  SERVICE_NAME=catalog-api \
  REPOSITORY=https://github.com/example/catalog-api \
  DESCRIPTION='Catalog API' \
  CODEOWNER=@example/platform \
  DATABASE=none \
  AUTHN=none \
  OUTBOUND_HTTP=none \
  OUTBOUND_AUTH=none \
  GRPC=none \
  HTTP_IDEMPOTENCY=none \
  JOBS=none \
  MESSAGING=none \
  OUTBOX=none \
  WEBHOOKS=none \
  INBOUND_WEBHOOKS=none \
  CACHE=none \
  OBJECT_STORAGE=none \
  SERVICE_AUTH=none \
  AGENT_HARNESS=claude
```

The direct entry accepts the same values in kebab-case:

```sh
scripts/init-module.sh --repo . \
  --service-name catalog-api \
  --repository https://github.com/example/catalog-api \
  --description 'Catalog API' \
  --codeowner @example/platform \
  --database none --authn none --outbound-http none --outbound-auth none \
  --grpc none --http-idempotency none --jobs none --messaging none \
  --outbox none --webhooks none --inbound-webhooks none --cache none \
  --object-storage none --service-auth none --agent-harness claude
```

The four identity values are required (`SERVICE_NAME` Cargo-safe and not a Rust
keyword, `REPOSITORY` plain HTTPS GitHub owner/repo URL, `CODEOWNER` `@user` or
`@org/team`, single-line `DESCRIPTION` up to 256 characters; invalid, duplicate,
or conflicting inputs refuse before writing). `DATABASE` defaults to `none`
(`none` | `postgres`). `AGENT_HARNESS` defaults to `all` (`core` for canonical
instructions only, `codex`, `claude`, `qwen`, `cursor`, `grok`, `opencode`, or
`all`).

<!-- template:begin authn:docs-template-init-authn -->
`AUTHN` (`--authn`) defaults to `none` (`none` | `oidc-jwt` | `oidc-introspection`); at most one engine is retained, defaulting at runtime to `authn.mode = "none"`. `oidc-introspection` retains its optional disabled-by-default positive cache; `oidc-jwt` and `none` prune it.
<!-- template:end authn:docs-template-init-authn -->
<!-- template:begin outbound-http:docs-template-init-outbound -->
`OUTBOUND_HTTP` (`--outbound-http`) defaults to `none` (`none` | `bounded`); `bounded` retains the [decision record](decisions/outbound-http-decisions.md), crate, tests, and shared TLS fixtures.
<!-- template:end outbound-http:docs-template-init-outbound -->
<!-- template:begin outbound-auth:docs-template-init-outbound-auth -->
`OUTBOUND_AUTH` (`--outbound-auth`) defaults to `none` (`none` | `oauth2-client-credentials`); selecting it retains its [decision record](decisions/outbound-machine-authentication-decisions.md), crate, tests, and persists `OUTBOUND_HTTP=bounded` even when passed as `none`.
<!-- template:end outbound-auth:docs-template-init-outbound-auth -->
<!-- template:begin grpc:docs-template-init-grpc -->
`GRPC` (`--grpc`) defaults to `none` (`none` | `enabled`) and retains the native transport, schemas/contracts, codegen tool, and example (without starting a listener/client). `none` prunes exclusive surfaces while retaining shared HTTP/TLS ownership; the OAuth bridge stays only when both profiles are selected.
<!-- template:end grpc:docs-template-init-grpc -->
<!-- template:begin http-idempotency:docs-template-init-http-idempotency -->
`HTTP_IDEMPOTENCY` (`--http-idempotency`) defaults to `none` (`none` | `postgres`); `postgres` requires `DATABASE=postgres` and an `AUTHN` engine, retaining the store crate, migration, and tests (inert until an operation opts in and retention is configured).
<!-- template:end http-idempotency:docs-template-init-http-idempotency -->
<!-- template:begin jobs:docs-template-init-jobs -->
`JOBS` (`--jobs`) defaults to `none` (`none` | `postgres`); `postgres` requires `DATABASE=postgres` and retains `infra-jobs`, `jobs-worker`, config, migration, tests, and `/jobs-worker`. Because `JOBS` is a common shell variable (e.g. `JOBS=8`), unset non-profile ambient `JOBS` values before running `make template-init`.
<!-- template:end jobs:docs-template-init-jobs -->
<!-- template:begin messaging:docs-template-init-messaging -->
`MESSAGING` (`--messaging`) defaults to `none` (`none` | `nats-jetstream`, valid with `DATABASE=none` and `JOBS=none`), retaining typed events, adapter, Compose NATS, and CI gates. `OUTBOX` (`--outbox`) defaults to `none` (`none` | `postgres`) and requires `DATABASE=postgres`, `JOBS=postgres`, and `MESSAGING=nats-jetstream`.
<!-- template:end messaging:docs-template-init-messaging -->
<!-- template:begin cache:docs-template-init-cache -->
`CACHE` (`--cache`) defaults to `none` (`none` | `redis`, independent of PostgreSQL/jobs), retaining `infra-cache`, `[cache]`, and Compose Valkey proof.
<!-- template:end cache:docs-template-init-cache -->
<!-- template:begin object-storage:docs-template-init-object-storage -->
`OBJECT_STORAGE` (`--object-storage`) defaults to `none` (`none` | `s3`, independent of PostgreSQL/jobs), retaining `infra-object-storage`, `[object_storage]`, and Compose versitygw / conformance proofs.
<!-- template:end object-storage:docs-template-init-object-storage -->
<!-- template:begin service-auth:docs-template-init-service-auth -->
`SERVICE_AUTH` (`--service-auth`) defaults to `none` (`none` | `jwt`); `jwt` requires `GRPC=enabled` and retains `infra-serviceauth`, `[service_auth]`, gRPC admission, and client binding (inert until `service_auth.mode = "jwt"`).
<!-- template:end service-auth:docs-template-init-service-auth -->

Initialization renames the service package and main binary (keeping `crates/service/`, library name, and `openapi`), drops unselected packs, `source_only` files, and unused `[workspace.dependencies]` (refusing standalone comments outside profile markers), and leaves `init-module.sh` for one-shot replay. Review and commit the diff yourself; the initializer never stages or commits. Cargo lock pruning via `cargo metadata --offline` replaced the hand-maintained feature projector (byte-identical on all 68 runtime graphs on 2026-11-10); reopen a manual projector only if Cargo pruning changes a retained package on a supported graph.

## Initialization record and replay

`template.lock` is the service-owned JSON initialization record (identity,
profiles, admitted source HEAD/provenance, and state). It records initialization
postconditions only — `Cargo.toml`, `Cargo.lock`, source, and typed config
remain runtime/dependency truth. Sync never edits `template.lock`.

Repeating exact initialization values verifies identity and structure without
touching ordinary service edits; changing profiles, partial/unknown shapes, or
malformed records refuses (profile migration is out of scope).
[The initializer](../scripts/lib/template_init.py) (`InitInputs.profiles()`)
emits all current fields, while [the lock reader](../scripts/lib/template_state.py)
(`validate_profiles()`) normalizes admitted historical locks to `none` while
preserving original historical bytes on replay.

<!-- template:begin messaging:docs-template-init-messaging-lock -->
`template.lock` records `messaging` (historical locks default to `none`; changing it after initialization is refused).
<!-- template:end messaging:docs-template-init-messaging-lock -->

<!-- template:begin cache:docs-template-init-cache-lock -->
`template.lock` records `cache` (historical locks default to `none`; changing it after initialization is refused).
<!-- template:end cache:docs-template-init-cache-lock -->

<!-- template:begin object-storage:docs-template-init-object-storage-lock -->
`template.lock` records `object_storage` (historical locks default to `none`; changing it after initialization is refused).
<!-- template:end object-storage:docs-template-init-object-storage-lock -->

<!-- template:begin service-auth:docs-template-init-service-auth-lock -->
`template.lock` records `service_auth` (historical locks default to `none`; changing it after initialization is refused).
<!-- template:end service-auth:docs-template-init-service-auth-lock -->

<!-- template:begin authn:docs-template-init-authn-lock -->
`template.lock` records `authn` (historical locks default to `none`, not JWT; changing it after initialization is refused).
<!-- template:end authn:docs-template-init-authn-lock -->

## Adopt a committed source

SOURCE and TARGET must be separate, nonoverlapping Git roots (no symlink root
spellings). Sync reads SOURCE `HEAD` Git blobs once (refusing applicable source
dirt; ignoring unrelated work). Check out the template's `green` branch as
SOURCE: `main` can hold merges whose [post-merge proof](ci-cd-production-ready.md#red-main)
is still running or red.

```sh
scripts/template-sync.sh --check --from /path/to/template --repo /path/to/service
scripts/template-sync.sh --apply --from /path/to/template --repo /path/to/service
```

`--check` is read-only (`0` parity, `1` drift, `2` refusal/failure). `--apply`
applies, verifies, and leaves changes uncommitted; commit before re-checking
(`dirty` owned paths refuse even if bytes match). `--instructions-only` syncs
only bootstrap, workflow/harness docs, canonical skills/roles, and selected
adapters without touching Makefiles, scripts, tool pins, or the manifest.

## Ownership and preservation

Manifest files replace whole files; directory entries ending in `/` own all
descendants (deleting target-only files). Removing a manifest entry relinquishes
ownership. Standalone scripts are listed individually so service siblings stay
local. Profile-marked sections in portable owners are rendered against
`template.lock` before purity checks and parity comparison; an already
initialized source must match the target's capability selections. Full sync
regenerates selected adapters and prunes unselected adapter paths.

<!-- template:begin authn:docs-template-init-authn-sync -->
Sync never restores a pruned authentication profile; `template.lock` remains authoritative.
<!-- template:end authn:docs-template-init-authn-sync -->
<!-- template:begin outbound-http:docs-template-init-outbound-sync -->
Sync never restores a pruned outbound HTTP profile; `template.lock` remains authoritative.
<!-- template:end outbound-http:docs-template-init-outbound-sync -->
<!-- template:begin outbound-auth:docs-template-init-outbound-auth-sync -->
Sync never restores a pruned OAuth2 outbound-auth profile; `template.lock` remains authoritative.
<!-- template:end outbound-auth:docs-template-init-outbound-auth-sync -->
<!-- template:begin http-idempotency:docs-template-init-http-idempotency-sync -->
Sync never restores a pruned HTTP idempotency profile; `template.lock` remains authoritative.
<!-- template:end http-idempotency:docs-template-init-http-idempotency-sync -->
<!-- template:begin jobs:docs-template-init-jobs-sync -->
Sync never restores a pruned background jobs profile; `template.lock` remains authoritative.
<!-- template:end jobs:docs-template-init-jobs-sync -->

Application/Cargo sources, config, secrets, OpenAPI, migrations, README,
CODEOWNERS, `template.lock`, CI activation, `.railway/`, and local policy docs
remain service-owned; runtime changes (`Cargo.toml`, `vendor/`, `Dockerfile`)
require service-owned adoption. Keep local Make rules in `make/service.mk` and
standard targets in `make/template.mk`; full sync refuses unsplit root Make
recipes, unsafe extension syntax, or standard-target overrides (parsed as data,
never executed).

Portable validation scripts source optional service-owned hooks (append-only to
caller variables; portable owners keep custody and cleanup):

| Hook | Sourced by | Defines |
| --- | --- | --- |
| `scripts/ci/service-changed-surfaces.sh` | `changed-surfaces.sh` | `service_changed_surfaces FILE`, which calls `mark` for service paths |
| `scripts/ci/service-database.sh` | `migration-history-check.sh`, `sqlx-prepare.sh`, `test-integration-db.sh`, `migration-validate.sh` | `service_migration_directories`; `service_sqlx_init`/`service_sqlx_group`; `service_db_before_tests`/`service_db_after_tests`; `service_migration_validate` (`run_migrate NAME ENTRYPOINT`) |
| `scripts/ci/service-runtime-image.sh` | `runtime-image-build.sh`, `runtime-image-check.sh` | `service_runtime_image_build_arguments`; `service_runtime_image_configure`/`service_runtime_image_observe` for `RUNTIME_IMAGE_ROLE` |
| `scripts/ci/service-runtime-binaries.json` | `runtime-image-inventory.py` | Additional image paths mapped to `{"crate", "source"}` |

`INTEGRATION_LINT_FEATURES += --features <crate>/integration` in
`make/service.mk` adds lint coverage; `make lint-changed` applies each feature
only when its crate is selected.

To preserve a local canonical skill under `.agents/skills/<name>/` (not colliding
with source skills), place an empty regular `.service-owned` file beside a valid
`SKILL.md`; sync preserves that tree and renders selected Claude/Qwen discovery
links. Claude/Qwen settings sync only `env.CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH`
and `model.maxSubagentDepth` lexically, preserving all other bytes and refusing
duplicate keys or non-JSON/non-finite inputs. Codex project config remains an
exact generated view.

## Refusal and recovery

Commands preflight all target paths, types, links, locks, and helpers before
any write. Any tracked/staged dirt or untracked overlap (except valid
`.service-owned` skills), symlinks, submodules, or invalid projections refuse
without modifying target bytes or Git state. Exclusive access during execution
is required. If I/O fails mid-write, inspect the diff: recover failed
initialization in a fresh clean checkout (keeping the interrupted one for
comparison) or reconcile sync diffs manually; there is no automatic rollback.

## Design choices

Initializer and sync are Bash entrypoints over stdlib-only Python 3.11+ and Git
plumbing (no third-party templating engine, Git library, or network lookup).
Copier and `cargo-generate` were rejected for lacking exact-manifest ownership,
selective dirty refusal, and one-shot physical pruning; `git archive` was
rejected because export attributes can alter committed bytes; whole-file JSON
serialization was rejected for consumer settings to preserve unrelated bytes.
`scripts/lib/template_profiles.json` is source-only closed inventory, never
adopted by sync. Reopen if a maintained tool meets this contract at lower cost.

## Validation boundary

<!-- template:begin webhooks-common:docs-template-sync-webhooks -->
## Webhook profile projection

`WEBHOOKS` (`--webhooks`, `none` | `durable`) and `INBOUND_WEBHOOKS`
(`--inbound-webhooks`, `none` | `standard-webhooks`) default to `none`; both
require `DATABASE=postgres` and `JOBS=postgres`, and outbound additionally
requires `OUTBOUND_HTTP=bounded`. Retained capabilities are runtime-inert until
configured. The lock records both choices (historical locks default to `none`,
no post-init migration), and the initializer prunes unselected directional
crates, migrations, routes, docs, and worker registrations.
<!-- template:end webhooks-common:docs-template-sync-webhooks -->

Use `make help` and the [validation router](validation-routing.md) for ordinary
development. The source template additionally owns
`make template-owned-purity-check` and `ALLOW_FULL=1 make template-init-check`
(with `scripts/ci/template-init-check.sh`, `scripts/ci/initializer-matrix.py`,
and `scripts/tests/template-profile-projections.py` as canonical runtime/matrix
authorities).
<!-- template:begin outbound-auth:docs-template-init-outbound-auth-proof -->
OAuth representatives cover standalone, authenticated, messaging, and full
PostgreSQL neighboring packs without multiplying the harness/database matrix;
only PostgreSQL selections enable the integration-test feature.
<!-- template:end outbound-auth:docs-template-init-outbound-auth-proof -->
`make template-init-projections` /
`bash scripts/ci/template-init-check.sh --projections-only` run projection checks
without Cargo builds; `--source-checks` keeps source safety/purity/sync checks.
These source runners are stripped from generated services. Local results do not
claim CI, publication, or deployment; [CI/CD policy](ci-cd-production-ready.md)
remains release authority.
