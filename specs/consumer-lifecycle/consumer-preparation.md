# Actual local consumer preparation

Status: **T4 Implemented** on 2026-10-06. These are committed preparation
inputs, not accepted consumer baselines, an accepted B upgrade, CI admission or
published releases. Completion owns their final validation, review and sealing.

## Named local ownership

Paths were recorded in this receipt before their creation. All are under
`/Users/daniil/Projects/consumer-lifecycle/20261006/`, outside the managed
lifecycle worktree. No existing repository was overwritten.

| Relative path under that root | Actual owner/use |
| --- | --- |
| `lifecycle-minimal` | Separately owned minimal consumer Git repository; clean `main` |
| `lifecycle-demo` | Separately owned durable consumer; clean `codex/source-preparation-b`; A retained on `main` and `codex/release-a-source` |
| `lifecycle-demo-render-f` | Separately owned complete pristine durable F render; clean `main` |
| `generation-target` | One task-owned absolute Cargo target, used serially; about 2.8 GiB |
| `cargo-home` | Scrubbed generation Cargo home; links only the existing registry/git caches; no copied credentials/config |
| `logs` | Full generation commands/output, metadata, manifests and identity records; about 8.7 MiB |

Each repository was cloned locally with `--no-local --no-checkout`, given its
own `main` and synthetic Git identity, and checked out at its exact selected
source before initialization. Native object transport creates independent Git
repositories, without alternates, copied configuration or shared worktree
administration. Remotes were removed. Local `core.hooksPath=/dev/null` prevents
inherited hooks; commit identity is `Consumer lifecycle rehearsal
<consumer-lifecycle@example.invalid>`. Separate CodeGraph indexes were
initialized and ignored only through each `.git/info/exclude`; no index enters
a pristine commit. Generated repository instructions, architecture/configuration
owners and native Git/Cargo/toolchain settings were reloaded.

Source F is frozen Implemented template commit
`ffc2be865c9447fb86c4331cdb51a77eaeebc211` at
`/Users/daniil/.codex/worktrees/consumer-lifecycle/rust-service-template-rest`.
Corrected A source is `2cb871895b9edd018205fc98223477e269fce2e9` from the same
trusted local object store. The task consumes A's admitted source identity;
F still needs Completion admission. No template runtime source, manifests,
lockfile, pins, CI, spec or Design was edited by T4.

## Exact choices and committed inputs

| Consumer | Identity and profiles |
| --- | --- |
| Minimal | `service_name=lifecycle-minimal`, `repository=https://github.com/Dankosik/rust-consumer-lifecycle-minimal`, `description=Synthetic minimal consumer rehearsal.`, `codeowner=@Dankosik`; all capability selectors `none`; `agent_harness=core` |
| Durable A and F | `service_name=lifecycle-demo`, `repository=https://github.com/Dankosik/rust-consumer-lifecycle-demo`, `description=Synthetic consumer lifecycle rehearsal.`, `codeowner=@Dankosik`; `database=postgres`, `jobs=postgres`, `messaging=nats-jetstream`, `outbox=postgres`; all other capability selectors `none`; `agent_harness=core` |

“All other capability selectors” means `authn`, `outbound_http`, `outbound_auth`,
`grpc`, `http_idempotency`, `webhooks`, `inbound_webhooks`, `cache` and
`object_storage`. Minimal additionally sets `database`, `jobs`, `messaging`
and `outbox` to `none`. No initialized profile or repository identity changed.

| Prepared object | Commit | Tree |
| --- | --- | --- |
| Minimal complete F render | `32f469707cdf1514fece814b23af82e6fa18b788` | `d531f25fe48b084ab4dc01405ac7b838ce4ad051` |
| Durable pristine A (`initial-render-a`) | `384815a674ea9e8f08f8f2e3d6b43a968f249ccd` | `110a60bcb079b42a816fa410288937844ebc5264` |
| Durable pristine F target | `17703400b5519195e169b204ae6646ac5befd80c` | `b0d29cf607c0f246a8db46f958e81f51f4a04681` |
| Consumer A 0.1.0 (`main`, `codex/release-a-source`) | `69c0385e2be74333b8b04fd720a2efd9adb61b24` | `1b8ae504348e74a8e7d409425a2b4973e0f3a826` |
| Consumer 0.1.1 source evolution (`codex/source-preparation-b`) | `739a1ffb27935fb1ccb81d628b2aab9e86051c6b` | `4f7a4f860cee8da63c9cfc4c25ae164d790e9108` |

A's consumer commit is the direct child of the pristine A render. The 0.1.1
source-preparation commit is the direct child of consumer A. It still carries
A's template source; **the F template update has not been applied or accepted**.
The complete F render is retained separately as an actual target input.

| Pristine render | File count | Canonical render manifest SHA-256 |
| --- | --- | --- |
| `lifecycle-minimal` | 532 | `38f2d7dd28e09dff24e61c33c8122a3ce75f0bff27589fdbdecdae3454be53b5` |
| `lifecycle-demo` | 890 | `962abb4acde6a54399ce536e6bae11d36b3174da6b098bfdce5722ad017c751d` |
| `lifecycle-demo-render-f` | 892 | `fe6c62f598d23af1279a508a5c46448028b42a01a1ec9e387d6229a625fe9b26` |

The manifests contain native Git path/mode/blob identities, ordered by
`git ls-tree -r -z`; their JSON uses sorted keys, compact separators and a final
newline. They preserve executable/link modes. `logs/source-recipes.json`
records each selected source tree and each revision's own initializer,
`template_init.py`, `template_state.py`, profile inventory, toolchain and source
Cargo.lock as Git objects and SHA-256 bytes.

| Object | Cargo.lock SHA-256 | Initial template.lock SHA-256 |
| --- | --- | --- |
| Minimal F | `5fe3db777338f057e9b7333d219b51a2630738c800a461f37faa7dea84c8ebff` | `e1e3a4670e4914f62e8f134d882693b789f1052f35d960b7ae2996c3eae53e53` |
| Durable pristine A | `19de5bae5529e471006b099522b359da9787d91f5bd384cdb4220cdf08d5daea` | `313e390045cb52336777bdffd111b30dc9aac2305d9d43ba2da88c0e83901bfe` |
| Durable pristine F | `19de5bae5529e471006b099522b359da9787d91f5bd384cdb4220cdf08d5daea` | `788472d5a43d67b93e64de62ad33b65c8b98afbf9b02b39d93e981613bffcff3` |
| Consumer A | `86a71a99a463ebfedf1c28ffdee40c10d5b2f4585856834401b84361568e559b` | `313e390045cb52336777bdffd111b30dc9aac2305d9d43ba2da88c0e83901bfe` |
| Consumer B source preparation | `dde95ff3dc4ed5fe35f25a5624713cca64d98262c321390387b877ccd7c94829` | `313e390045cb52336777bdffd111b30dc9aac2305d9d43ba2da88c0e83901bfe` |

`template.lock` in A and B preparation remains byte-identical to pristine A's
initialization record. No repository contains fabricated `template.upgrade.json`
acceptance metadata. Creation of the render commits establishes available
objects, not accepted baseline custody.

## Public generation and consumer work

Every render invoked its selected source's own public `scripts/init-module.sh`
with the exact choices above. Full locked/offline metadata, pinned rustfmt,
OpenAPI execution and the selected harness generation completed; no cheap
projection or formatting bypass substituted for public initialization.
Generation ran serially through the task-owned absolute target. Environment
was explicitly scrubbed to local tools/cache paths, noninteractive Git settings
and task variables; publication/cloud credentials were not passed. The inherited
PATH was retained with `/Users/daniil/.cargo/bin` prepended, preserving `npx`.
`CARGO_PROFILE_DEV_DEBUG=line-tables-only` mirrors the existing workstation
development setting without copying ambient Cargo configuration.

| Full initialization | Exit | Elapsed seconds | Command/output under `logs/` |
| --- | --- | --- | --- |
| Minimal F | 0 | 47.64 | `minimal-command.json`, `minimal-generation.log` |
| Durable A | 0 | 23.82 | `durable-a-command.json`, `durable-a-generation.log` |
| Durable F | 0 | 10.69 | `durable-f-command.json`, `durable-f-generation.log` |

All three retained pins and actual tool readbacks agree:
`1.99.0-aarch64-apple-darwin`; `rustc 1.99.0 (b940084d7 2026-09-28)`;
`cargo 1.99.0 (5f94df478 2026-08-27)`. Both upstream sources also pin 1.99.0.
`logs/toolchain-identities.json` retains verbose outputs and each native
`rust-toolchain.toml` with its hash.

Consumer A adds one feature-owned public `GET /lifecycle` operation returning
HTTP 200 JSON with the required string `message`, initially
`Hello from lifecycle-demo A.`. The `rehearsal` crate uses existing declared
axum/serde/utoipa and HTTP contracts. It joins the service router and the
consumer's local architecture policy; its behavior/decision is documented in
`docs/consumer-rehearsal.md`. A also changes the existing local `http.addr`
to `127.0.0.1:18080`. B's source evolution changes the greeting to
`Hello from lifecycle-demo B.`, workspace version to 0.1.1 and the corresponding
14 local workspace package versions in Cargo.lock. The public response shape
and local configuration remain unchanged.

One mounted-router test was authored for the public status/media-type/body
contract. The existing assembled-document operation test now includes the
consumer route, detecting a lost service merge. Existing generated-document
and transport proof is reused. These tests have not been executed in T4.
The source annotation owns the regenerated OpenAPI at both versions.

Lock authoring was a bounded consumer-only transformation: native
`cargo metadata --locked --offline --no-deps` supplied workspace identities and
declarations; the trusted existing initializer's guarded lock parser,
record/edge updates and sort semantics supplied the existing projection
method. Only the missing local `rehearsal` record and its service edge were
added for A; B replaced only exact local workspace versions 0.1.0 → 0.1.1.
No new resolver, reusable projector, dependency upgrade or download workaround
was introduced. Every registry/git and non-workspace vendored package record,
including version/source/checksum/dependencies, stayed exact. Full
`cargo metadata --locked --offline --format-version 1` accepted both graphs;
comparison of all 505 normalized external/vendor nodes found unchanged
resolved dependencies and features, including after B's version change.
The authoring and graph comparison records are retained under `logs/`.

Bounded implementation feedback was
`cargo check -p rehearsal -p lifecycle-demo --all-targets --locked --offline`
(exit 0, 37.98 seconds), after A's cross-crate route/type/import changes.
This compiled affected production/test code; it executed no tests or runtime
and is not final acceptance. Required metadata/format/OpenAPI generation also
succeeded for A and B consumer changes. Observed retained-source warnings are
`vendor/sqlx-core/src/pool/inner.rs:229` (`fetch_update` deprecated under pinned
Rust 1.99) and `crates/service/src/bootstrap/shutdown.rs:308` (`is_empty` unused
in this generated lib-test profile). T4 did not silence or patch those owners;
Completion must handle any selected-gate consequence through the root.

In both F renders the updater CLI/library/proof are absent as source-only
content; `docs/template-upgrade.md` and `scripts/ci/image-results.py` remain.
The updater is to be invoked from an explicitly admitted trusted template
checkout. No consumer-local helper is promoted to tool authority.

## Published-worker input closure

No new consumer registration or adapter is required for the accepted
published-worker observation. This conclusion follows the actual retained A/F
worker and Config contracts, not an assumed empty-scaffold startup:

- `crates/jobs-worker/src/main.rs` supplies an empty service registration.
  With outbox retained, `bootstrap::register_capabilities` admits its built-in
  publisher even when ordinary `jobs` and typed `messages` registrations are
  absent (`bootstrap.rs:523`).
- `bootstrap::outbox_publisher` calls
  `infra_messaging::outbox::registry(messaging.producer())` and creates its
  own one-slot `Engine::new` when there is no ordinary jobs engine
  (`bootstrap.rs:423`). The unchanged outbox-only capacity rule requires
  three PostgreSQL pool connections; the prepared local baseline has four
  (`crates/config/src/jobs.rs:74`).
- No typed message handlers means `messages=None` and
  `messaging_options(config, false).consumer=None` (`bootstrap.rs:676`).
  Producer admission still validates connection policy and source stream.
  It does not require consumer durable/filter/DLQ names. There is no new
  producer-mode flag or bypass.

The execution owner supplies the existing runtime inputs for each verified
A/B/rollback-A `/jobs-worker` on the isolated source network:

| Existing input | Concrete rehearsal selection |
| --- | --- |
| `APP__APP__ENV` | `local` so the explicitly permitted local synthetic broker policy applies |
| `APP__POSTGRES__ENABLED` | `true`, after the matching published `/migrate` admits/applies the retained migration set |
| `APP__POSTGRES__DSN` | The synthetic source database DSN, passed through runtime environment; credential values excluded from evidence |
| `APP__POSTGRES__MAX_CONNECTIONS` | `4`, satisfying the outbox-only minimum of 3 |
| `APP__MESSAGING__URLS` | The source Compose network's NATS URL, e.g. `nats://nats:4222` when that alias is selected |
| `APP__MESSAGING__ALLOW_PLAINTEXT` | `true`, scoped to this local synthetic network |
| `APP__MESSAGING__ALLOW_UNAUTHENTICATED` | `true`, scoped to the existing local synthetic broker |
| `APP__MESSAGING__SOURCE_STREAM` | `LIFECYCLE_SOURCE` |
| `APP__MESSAGING__MAX_PAYLOAD_BYTES` | `1 KiB`, matching the source actor's prepared-event bound |
| `APP__HTTP__ADDR` | `0.0.0.0:8080` inside each isolated container; any host probe binding remains loopback/ephemeral |

The payload setting is necessary: the existing actor provisions the source
with `max_message_size=9 * 1024` (payload plus header allowance), so its bound
is 1 KiB rather than the worker's untouched 256 KiB default. Existing startup
admission must enforce this relationship. Consumer durable/filter/DLQ inputs
are omitted from this publisher-only process. Runtime launch inputs do not
change the committed consumer's local HTTP binding or add source code.

The already authored source-only `test/examples/consumer_lifecycle_actor.rs`
supports `enqueue event ID`: its normal transaction records the synthetic
intent and calls `PreparedEvent::enqueue`. Its separate `consume` process
handles the stable v1 `lifecycle.created` event and records the logical-ID
effect. For the published-worker observation, do not run the actor's `worker`
mode against that queue: the actual verified image's `/jobs-worker` must be
the publisher. Use distinct finite IDs for A, B and rollback A and retain
their intent/publication/effect evidence. Record source actor binary identity
and fixture consumption separately from each image digest/commit and actual
worker publication. Native migration, provider admission and these operations
still require Completion execution; this section records source/configuration
closure only.

## Persistent compatibility inputs

The complete pristine A/F inventories are byte-identical for `migrations`,
`crates/infra-jobs`, `crates/infra-messaging`, `crates/jobs-worker`,
`crates/domain-events`, `crates/infra-postgres` and `crates/migrate`.
Consumer changes add no migration, job kind or durable event/handler version.
This comparison supplies source compatibility inputs; it does not observe a
database, worker, broker or rollback.

| Retained SQL migration | SHA-256 (both A and F) |
| --- | --- |
| `20260924000001_create_background_jobs.sql` | `b03c6c39c634a58efcbfad94882696cbec7c0b8594cb6877ba8dc7199be04b52` |
| `20261001120000_add_background_job_errors.sql` | `e5d958b0a68471aefb74b445cd6ea155d68a20ec708aa7c05142423131ed0e99` |
| `20261002150001_add_background_job_recovery_history.sql` | `394573e1fb70b0d3a2ccb4aac1858f8d0df2ea52d693924b1021c3c3e2cb2f53` |
| `20261002150002_index_failed_background_jobs.sql` | `a9801474170ac31663e92114a7925219b8ca30b6d1f3efb968b7875674f503bd` |

The corrected positive pair remains source `2cb8718…` → F. The separate
historical negative pair `67be869…` → `2cb8718…` has not been substituted for
it. Published distinct-digest A → B → A and durable recovery are unobserved.

## Capability and resource inventory

Initial disk availability was 6.8 GiB. At handoff `/System/Volumes/Data` has
about 3.3 GiB available; the named task target is about 2.8 GiB, repositories
about 20/34/34 MiB and logs about 8.7 MiB. Shared Cargo and Docker caches were
preserved. No runtime or paid host was installed.

Docker was unavailable at the initial probe; the root recovered the existing
runtime during preparation. Readback now reports Docker Server 29.4.0,
`linux/aarch64`, 10 CPUs and 8,393,289,728 bytes runtime memory; Compose v5.1.2.
Only `testcontainers/ryuk:0.14.0` (2.11 MB) appeared in the local image listing.
No provider containers, image builds, pulls or runtime scenarios were started
by T4. Native installed tools are PostgreSQL `pg_dump`/`pg_restore` 18.3 and
NATS CLI v0.5.0. The retained Compose providers are PostgreSQL `18` at digest
`4ef4dbc939d61acea57712655ddb4b4ab27419c913f94cca0cd57cb3ea3c2280`
and NATS `2.15.0-alpine3.22` at digest
`ac8f88a6494bffc2c2a5289a0ca61cb28a9145c11ba5677cf24265d07f46d8d4`.
Their actual server/runtime versions, native backup capability and
`linux/amd64` published-image execution have not been exercised. Capacity and
these required runtime inputs must be reconciled by the final execution owner.
The existing source-only `scripts/ci/consumer-lifecycle-check.sh` explicitly
requires at least 8 GiB free on its rehearsal volume for historical builds and
archives. The current 3.3 GiB does not satisfy that later execution input;
T4 did not bypass the guard or remove shared caches to manufacture capacity.

## Completion handoff and remaining effects

The next owner is the root delivery/Completion owner. After assembled template
admission, validate/review the actual baseline and consumer A source, then use
the supported trusted updater adoption against source `2cb8718…`, consumer A
`69c0385…`, and original initialization commit `384815a…`. The consumer checkout
is currently on the separate B source-preparation branch; select A explicitly
before baseline adoption. Keep adoption isolated until accepted.

After A custody is sealed, apply the single authored B evolution commit
`739a1ff…` through ordinary clean Git integration, preserving the accepted
control record. Invoke supported `prepare` against accepted A and exact F,
resolve generated/source owners, validate/review the actual candidate, and
seal accepted B. This sequence changes native Git commit identities;
Completion must record the actual A/B seals and their unchanged runtime-content
identities. These prepared commits are not those future acceptance commits.
No `adopt`, `prepare`, `accept`, publication, registry verification or provider
proof was run by T4. All T4 processes have joined; no descendant was spawned.

Final consumer build/tests, upgrade validation/review, metadata seals,
exact-source CI/CodeQL, native publication trust, distinct running digests,
rollback and real durable recovery remain pending. Do not infer them from
full initialization or compile-only feedback. Preserve the pristine renders,
A/B source branches and their logs for the Completion owner.

The external envelope remains the concrete proposal in
[release/recovery Design](design/release-recovery.md#proposed-external-envelope):

- Create only private `Dankosik/rust-consumer-lifecycle-demo` with `main`, owner
  `@Dankosik`; minimal remains local. Confirm private-repository Enterprise
  Cloud attestation support before publication; visibility changes are not a
  fallback without user authority.
- Publish only `ghcr.io/dankosik/rust-consumer-lifecycle-demo`, refs `v0.1.0` and
  `v0.1.1`, plus the native workflow's `main`, `sha-<12>`, `latest` and
  run-scoped candidate tags, signatures, provenance and SBOM attestations.
- Enable native Actions, required dependency graph and
  `ENABLE_GHCR_PUBLISH=true`; use action-scoped package/OIDC/attestation writes.
  Keep credential values out of evidence.
- Reuse existing local Docker for verified `linux/amd64` digests and synthetic
  data only. No paid host, domain, public ingress or production data.
- Incremental cash ceiling zero, included quota only: two complete version
  CI/publication cycles plus at most one retry per failed version (four cycles
  maximum), within existing job timeouts. Read available plan/quota first.
- Retain source/CI/release evidence, both verified digests, attestations and
  synthetic archives for 30 days after final acceptance. At most one source
  and one restore Compose project. Retain rollback artifacts until cleanup is
  accepted; no repository/package deletion is implicit.

No authority for these remote effects is inferred here. The root presents the
remaining visibility/account/spending/retention consequences only after actual
admitted local A/B preparation, as the accepted task packet requires.

## Reopenable local records

All entries below are under the named root's `logs/`. These are T4 preparation
records; they are not invented validation/review verdicts.

| Record | SHA-256 |
| --- | --- |
| `preparation-identities.json` | `e6629ae3448581f619067929bf34a7418f3d8b551545d9d5a5679af1ce5a8c1c` |
| `source-recipes.json` | `53a75d0e250ca673c69d186ebaf6475fbbe70becaa051486489c12e51d2c9f60` |
| `toolchain-identities.json` | `4d1772a527e9b12f6004cef3e909ab52fea25ee0334ba2e3d6839425aa30ec6f` |
| `consumer-a-lock-authoring.json` | `db857d1e055e8766cec118d46b974399aeb162204ff85e99b454a0dd8156b624` |
| `consumer-b-lock-authoring.json` | `83cfdfa3513c1ecbc01c963bb09884d1df76d2ac250e733c838f2a4ca9b764f4` |
| `external-graph-comparison.json` | `3584fcebad061bb9b46e984fd8a1f0c505c91601b6b81c66732a6809cf397846` |
| `version-graph-comparison.json` | `87d010b5084ed345580fe1e68e247ac85ea0d7979aa1d65e169b40a3f7d42f85` |
