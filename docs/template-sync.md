# Initialization and portable updates

Initialization gives a clean template checkout its service identity and selects
the existing database, authentication, outbound HTTP, HTTP idempotency,
background jobs, and agent-harness packs. Later synchronization adopts
portable tooling and instructions from a committed source checkout. The
[ownership manifest](../template-owned.paths) is the full-sync copy authority;
the service keeps its application, configuration and local policies.

## Initialize a service

Work in a clean Git checkout with Python 3.11+, Git, the pinned Rust toolchain,
and the locked dependency inputs already available locally. Initialization
uses offline Cargo metadata and the existing OpenAPI generator before changing
the checkout. Missing tools, dependencies or unsupported source shapes refuse
without target writes.

```sh
make template-init \
  SERVICE_NAME=catalog-api \
  REPOSITORY=https://github.com/example/catalog-api \
  DESCRIPTION='Catalog API' \
  CODEOWNER=@example/platform \
  DATABASE=none \
  AUTHN=none \
  OUTBOUND_HTTP=none \
  AGENT_HARNESS=claude
```

The direct entry accepts the same values:

```sh
scripts/init-module.sh --repo . \
  --service-name catalog-api \
  --repository https://github.com/example/catalog-api \
  --description 'Catalog API' \
  --codeowner @example/platform \
  --database none --authn none --outbound-http none --agent-harness claude
```

The four identity values are required. `DATABASE` defaults to `none` and accepts
`none` or `postgres`. `AGENT_HARNESS` defaults to `all` and accepts `core`,
`codex`, `claude`, `qwen`, `cursor`, `grok`, `opencode`, or `all`. Every combination is
supported. `core` keeps canonical instructions without a product adapter;
`all` keeps the six existing adapters. PostgreSQL remains disabled at runtime
until configured when its pack is retained. The local
[persistence authority](architecture/persistence.md) describes availability.

<!-- template:begin authn:docs-template-init-authn -->
`AUTHN` defaults to `none` and accepts `none`, `oidc-jwt`, or `oidc-introspection`; exactly one whole-file engine may be retained. The direct entry takes the same choice as `--authn`. `none` removes all authentication configuration, code, tests, dependencies, and adopter guidance. An initialized authentication profile still defaults to runtime `authn.mode = "none"`; a protected route contract requires a complete valid trust tuple before startup.
The introspection profile retains its complete optional positive-cache configuration and implementation, disabled by default. JWT-only and no-auth outputs remove that cache path along with introspection; selecting the profile does not enable caching at runtime.
<!-- template:end authn:docs-template-init-authn -->
<!-- template:begin outbound-http:docs-template-init-outbound -->
`OUTBOUND_HTTP` defaults to `none` and accepts `none` or `bounded`; the direct
entry takes `--outbound-http`. `bounded` retains the [client guide](outbound-http.md),
[decision record](outbound-http-decisions.md), crate, tests, and shared TLS
fixtures independently of authentication. `none` removes that pack. The readonly
request budget stays when auth or outbound needs it. The retained client uses a
trusted operator-selected HTTPS origin and normal system resolution; selection
supplies no provider configuration or automatic request.
<!-- template:end outbound-http:docs-template-init-outbound -->
<!-- template:begin http-idempotency:docs-template-init-http-idempotency -->
`HTTP_IDEMPOTENCY` defaults to `none` and accepts `none` or `postgres`; the
direct entry takes `--http-idempotency`. `postgres` retains the
[idempotency guide](http-idempotency.md), the record-store crate, the
profile migration, and tests, and requires `DATABASE=postgres` and an
authentication engine (`AUTHN=oidc-jwt` or `oidc-introspection`); an
unsupported combination or an unknown value is refused before any target
write, naming the unmet requirement. `none` removes the pack. Selection
supplies no retention value or automatic request; the boundary stays inert
until an operation opts in and the runtime retention is configured.
<!-- template:end http-idempotency:docs-template-init-http-idempotency -->
<!-- template:begin jobs:docs-template-init-jobs -->
`JOBS` defaults to `none` and accepts `none` or `postgres`; the direct entry
takes `--jobs`. `postgres` retains the [jobs guide](background-jobs.md), the
`infra-jobs` and `jobs-worker` crates, the `jobs` configuration section, the
migration, the jobs tests and the test-only fixture binary, the `/jobs-worker`
image entrypoint, and the architecture leaf. It requires `DATABASE=postgres`
and combines with every `AUTHN`, `OUTBOUND_HTTP`, and `HTTP_IDEMPOTENCY`
choice; `JOBS=postgres` without that database is refused before any target
write with `JOBS=postgres requires DATABASE=postgres`, and an unknown value
with `JOBS is unsupported`. `none` removes the pack. `JOBS` is a common
environment variable name (for example a parallelism setting): an ambient
value such as `JOBS=8` makes `make template-init` refuse with
`JOBS is unsupported`, so it must be unset or a valid choice. Selection
starts no worker; `/jobs-worker` runs only where it is deployed.
<!-- template:end jobs:docs-template-init-jobs -->


Service names are lowercase ASCII, start with a letter, use single hyphens
between letters or digits, end in a letter or digit, and have at most 64
characters. Existing package/binary names and Rust reserved identifiers cannot
be used. Repository URLs are HTTPS GitHub owner/repository URLs with no extra
components, credentials, query, fragment or trailing slash. A code owner is
one `@user` or `@org/team` token. Descriptions are nonempty single-line text of
at most 256 Unicode characters without controls. Quotes and punctuation are
data. Duplicate or conflicting inputs and unknown choices refuse.

Initialization renames the service package and main executable while retaining
the `crates/service` directory, library name and `openapi` auxiliary executable.
It updates the repository identity, local commands/image, runtime identity,
OpenAPI annotations and generated document. It physically removes unselected
packs and source-only validation fixtures. The initializer itself remains
local for one-shot replay.

Review the resulting diff and commit it using the normal contribution process.
No initializer command stages, commits, resets, stashes or cleans files.

## Initialization record and replay

`template.lock` is the local, versioned JSON initialization record. It contains
identity, selected packs, the admitted local source HEAD, explicit local-checkout
provenance, and initialization state. It is service-owned and sync never edits
it. A complete record means that initialization postconditions passed; it is
not a build, test, CI or deployment receipt.

Repeating the exact initialization values checks identity and profile structure
and succeeds without rewriting ordinary service edits. A different selection,
incomplete record, malformed record, or inconsistent structure refuses. Profile
migration of an established service is outside this command's scope.
New schema-1 records contain exactly the `database`, `authn`, `outbound_http`,
`http_idempotency`, `jobs`, and `agent_harness` profile fields. The admitted
historical profile shapes are `database` + `agent_harness`, `database` +
`authn` + `agent_harness`, `database` + `authn` + `outbound_http` +
`agent_harness`, and `database` + `authn` + `outbound_http` +
`http_idempotency` + `agent_harness`; missing selections in those shapes mean
`none`. Matching historical replay preserves the original lock bytes. Partial
or unknown shapes refuse.


<!-- template:begin authn:docs-template-init-authn-lock -->
The lock records the selected `authn` value. Historical records without it
mean `none`; they do not imply JWT support. Changing the choice after
initialization is a refused profile migration.
<!-- template:end authn:docs-template-init-authn-lock -->

## Adopt a committed source

SOURCE and TARGET must be separate, nonoverlapping Git roots, addressed without
symlink root spellings. The command reads SOURCE HEAD once, reports that object
ID and uses its Git blobs throughout. It does not fetch or use uncommitted
source bytes. Applicable source dirt refuses; unrelated source work is ignored.

```sh
scripts/template-sync.sh --check --from /path/to/template --repo /path/to/service
scripts/template-sync.sh --apply --from /path/to/template --repo /path/to/service
```

`--check` never writes the target. Exit `0` means selected parity, `1` means
drift, and `2` means refusal or an operational failure. `--apply` admits the
same complete plan, applies it, verifies it, and leaves changes uncommitted.
Commit the adopted diff before checking parity again: dirty owned paths refuse
even if their bytes already match the source. An already-current clean target
is a no-op.

Use `--instructions-only` with either mode to adopt the portable bootstrap,
workflow/harness documents, canonical skills/roles and selected adapter views.
It leaves Makefiles, scripts, tool pins, the manifest, receipts and unselected
adapter data untouched. Dirty unselected tooling and a legacy Makefile do not
block this mode. Success means instruction parity only.

## Ownership and preservation

Manifest files are replaced as whole files; directory entries end in `/` and
own their descendants, including deletion of target-only owned content. Removing
an entry from the source manifest relinquishes ownership of that target path.
Standalone scripts are individual entries, so service siblings remain local.
Generated adapters are rendered by the committed source helpers. Full sync
prunes only the declared paths of unselected adapters and cannot restore an
absent database pack.

<!-- template:begin authn:docs-template-init-authn-sync -->
Portable sync never restores a pruned authentication engine, runtime configuration, or profile-marked adopter documentation; the target lock and profile policy remain authoritative.
<!-- template:end authn:docs-template-init-authn-sync -->
<!-- template:begin outbound-http:docs-template-init-outbound-sync -->
Portable sync cannot restore a pruned outbound pack, its shared test fixtures,
or profile-marked guide and decision record. The target lock remains authoritative.
<!-- template:end outbound-http:docs-template-init-outbound-sync -->
<!-- template:begin http-idempotency:docs-template-init-http-idempotency-sync -->
Portable sync cannot restore a pruned idempotency pack, its schema
migration, configuration section, or profile-marked guide. The target lock
remains authoritative.
<!-- template:end http-idempotency:docs-template-init-http-idempotency-sync -->
<!-- template:begin jobs:docs-template-init-jobs-sync -->
Portable sync cannot restore a pruned jobs pack, its migration, its
configuration section, or its guide. The target lock remains authoritative.
<!-- template:end jobs:docs-template-init-jobs-sync -->


Application/Cargo sources, configuration, secrets, OpenAPI, migrations, README,
CODEOWNERS, initialization provenance, CI activation and local architecture,
validation and deployment policy stay service-owned. Put local Make data and
nonstandard recipes in `make/service.mk`; standard targets belong to
`make/template.mk`. Full sync refuses unsplit root Make recipes, unsafe service
extension syntax and standard-target overrides. It parses this structure as
data and never executes target Makefiles or hooks.

To preserve an additional canonical skill, put an empty regular
`.service-owned` file directly inside its real `.agents/skills/<name>/`
directory beside a valid `SKILL.md`. Its name must not collide with a source
skill. That entire skill tree, including local dirt, is preserved; selected
Claude/Qwen discovery links are rendered from it. Malformed markers, unsafe
links or colliding names refuse. Unmarked target-only skills remain owned
drift and may be removed after normal dirty-path admission.

Claude settings own only `env.CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH`; Qwen settings
own only `model.maxSubagentDepth`. Sync replaces or inserts that JSON value
while preserving all existing other bytes. Duplicate keys, non-finite values,
invalid JSON, or nonobject roots/managed parents refuse. Settings values are
never included in diagnostics. Codex project config remains an exact generated
view; machine-specific settings live outside it.

## Refusal and recovery

Both commands preflight all selected writes/removals, generated outputs, path
types, parents and ownership before target mutation. Tracked/staged dirt or
untracked/ignored overlap refuses, except valid service-owned skills and their
canonical discovery links. Unrelated files are preserved. Unsafe manifest paths,
symlink parents, ordinary symlinks, submodules, type collisions, missing local
authorities, invalid projections and helper failures also refuse.

Commands require exclusive access to their selected source/target paths while
running. Admission is not filesystem transaction isolation from other writers.
Deterministic refusal leaves target bytes and Git state unchanged.

Unexpected I/O after writes start can leave command-produced changes. An
interrupted initializer keeps an incomplete lock and never reports success;
sync reports partial application. Inspect the diff and diagnostic. Recover
initialization in a fresh clean template checkout, preserving the interrupted
checkout for comparison; reconcile sync-produced changes explicitly. There is
no automatic rollback, reset, retry or destructive resume.

## Validation boundary

<!-- template:begin webhooks-common:docs-template-sync-webhooks -->
## Webhook profile projection

`WEBHOOKS` accepts `none` or `durable`; `INBOUND_WEBHOOKS` accepts `none` or
`standard-webhooks`, and both default to `none`. Each selected direction requires
`DATABASE=postgres` and `JOBS=postgres`; outbound additionally requires
`OUTBOUND_HTTP=bounded`. Invalid values or combinations refuse before target
mutation. A retained capability remains runtime-inert until endpoints/bindings
are configured.

The lock records both choices. Historical locks infer them as `none` only through
the existing compatible historical shapes; changing an established choice remains
a refused profile migration. The initializer removes absent shared protocol,
directional crate/dependency, docs, migration, route, test, and marker edges.
Outbound-only removes ingress/receipt material; inbound-only does not retain
outbound HTTP or DNS solely for webhooks.
<!-- template:end webhooks-common:docs-template-sync-webhooks -->

Use the service's [command policy](build-test-and-development-commands.md) and
[validation router](validation-routing.md) for ordinary development.
The source template additionally owns `make template-owned-purity-check` and
`ALLOW_FULL=1 make template-init-check`. It checks 368 cheap canonical
profile/harness projections, then initializes and validates 46 runtime graphs.
Graphs 1--26 are the existing baseline. Graphs 27--46 add five auth/idempotency
blocks, each ordered as inbound-only without bounded outbound HTTP, inbound-only
with it, outbound-only with it, and both directions with it: graphs 27--30 use
`AUTHN=none HTTP_IDEMPOTENCY=none`; 31--34 use `oidc-jwt/none`; 35--38 use
`oidc-introspection/none`; 39--42 use `oidc-jwt/postgres`; and 43--46 use
`oidc-introspection/postgres`. The three full new shapes are 27 (inbound), 29
(outbound), and 30 (both). The other seventeen retain locked/offline metadata
plus `cargo check --workspace --all-targets --features integration-tests/integration`
proof. Inbound-focused graphs also run the existing service OpenAPI test and
`inert_inbound_webhook_route_rejects_unknown_endpoint_without_signature_work`
lifecycle test. This focused proof avoids repeating a full workspace/database
suite. Exact non-harness tree equality proves that other harness choices do not
alter runtime or contract-generation inputs.
Quality, dependency, image and database gates retain their own scopes; the
initializer command does not repeat the full aggregate per harness. Every
initialization in one run shares one absolute Cargo target (an explicit
`CARGO_TARGET_DIR`, or the run's private one), so the locked dependency graph
compiles once. CI retains eight partitions. Its recorded graph plan assigns the
new graphs by observed duration and retains each graph's selected full or focused
proof; it does not redesign unrelated pipeline work. `make verify` leaves it to
CI unless `ALLOW_FULL=1`. A change to projected text alone runs only
`make template-init-projections`, locally and in CI.

`bash scripts/ci/template-init-check.sh --projections-only` records the
focused 368-projection proof without Cargo or full/heavy admission. It does
not claim 368 public-CLI initializations or builds. `--source-checks` keeps
the source safety/purity/sync route. The public initializer always performs
its complete locked metadata, formatting and OpenAPI preflight before
changing a target; neither focused proof mode changes that command. These
source runners are removed from generated services, so standard checks
cannot recurse into them.

Local results describe their fixed candidate and commands. They do not claim a
remote CI run, publication or deployment. The service's
[CI/CD policy](ci-cd-production-ready.md) remains the authority for release gates.
