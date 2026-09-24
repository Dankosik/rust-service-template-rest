# T1 — PostgreSQL-backed HTTP idempotency profile

Outcome:
Today the source template has no idempotency capability, an empty
`migrations/` set, three runtime profile selections (`DATABASE`, `AUTHN`,
`OUTBOUND_HTTP`), and a validation boundary of 96 canonical projections and 12
runtime graphs. After this task it offers a fourth, independent selection,
`HTTP_IDEMPOTENCY=none|postgres`, with `none` as the default:

- `postgres` requires `DATABASE=postgres` and an authentication engine. Its
  output retains a complete, documented pack: the `infra-idempotency-store`
  provider crate, the `infra_http::idempotency` seam with its four catalog
  codes, the `http_idempotency.retention` setting, composition-root and
  bootstrap wiring, one forward-only migration, inline and real-PostgreSQL
  tests, and the adopter guide. The pack is inert until an operation declares
  `x-idempotent: true` and is composed through `Composer::route`; that
  operation then behaves as the Specification states.
- `none` output contains no pack path, marker, configuration section, or
  dependency, and its generated contract and projected `Cargo.lock` equal the
  baseline `d24d173` output for the same selection.
- The validation boundary becomes 128 projections and 16 runtime graphs. The
  authentication, outbound HTTP, PostgreSQL, and hardened-chain contracts stay
  unchanged.

Consumes:
- [Intent](../intent.md) — local stage-10.3 delivery that stops before PR,
  push, merge, or deployment.
- [Specification](../spec.md): [selection](../spec.md#outcome-and-selection),
  [declaration](../spec.md#declaring-an-idempotent-operation),
  [request identity](../spec.md#request-identity),
  [processing order](../spec.md#processing-order),
  [outcomes](../spec.md#arbitration-and-execution-outcomes),
  [concurrency](../spec.md#concurrency-and-one-transaction),
  [replay](../spec.md#replay), [catalog](../spec.md#problem-catalog),
  [configuration and lifecycle](../spec.md#configuration-startup-and-lifecycle),
  [schema, profile, sync, and guidance](../spec.md#schema-profile-sync-and-guidance),
  and [proof expectations](../spec.md#proof-expectations) — accepted behavior.
- [System design](../design/system.md):
  [dependency direction](../design/system.md#3-dependency-direction),
  [Rust API](../design/system.md#4-rust-api),
  [declaration and agreement](../design/system.md#5-declaration-and-agreement),
  [PostgreSQL](../design/system.md#6-postgresql),
  [stored format](../design/system.md#7-stored-success-format-1),
  [encoding and vectors](../design/system.md#8-canonical-encoding-digests-and-vectors),
  [HTTP boundary](../design/system.md#9-http-boundary),
  [configuration and lifecycle](../design/system.md#10-configuration-startup-lifecycle),
  [proof architecture](../design/system.md#11-proof-architecture),
  [profile machinery](../design/system.md#12-profile-machinery-summary-exact-owners-in-the-ownership-map),
  and [release closure](../design/system.md#13-release-closure-and-capacity)
  — closed mechanism, SQL, formats, vectors, and proof placement.
- [Ownership map](../design/ownership.md):
  [responsibilities](../design/ownership.md#responsibilities),
  [files](../design/ownership.md#files),
  [manifests and markers](../design/ownership.md#manifests-and-markers),
  [initializer and validation owners](../design/ownership.md#initializer-and-validation-owners),
  and [documentation owners](../design/ownership.md#documentation-owners) —
  placement, visibility, marker ids, cleanup, proof owners, and the
  final-validation list.
- [Technical Design Transition](../technical-design-transition.md) — the
  twelve fixed decisions, the bounded assumptions, and the two carried notes.
  The [review](../technical-design-review.md) and the
  [ownership panel](../design/ownership-review.md) are readiness evidence only.
- Repository owners this change triggers, each read immediately before its
  governed edit: [Repository Architecture](../../../docs/repo-architecture.md)
  and the leaves it selects for a new crate and new dependency edges;
  [Configuration Source Policy](../../../docs/configuration-source-policy.md)
  for the new key; [CI/CD Production Readiness](../../../docs/ci-cd-production-ready.md)
  for the fourth initializer part; the
  [initializer validation boundary](../../../docs/template-sync.md#validation-boundary);
  and the [contribution policy](../../../CONTRIBUTING.md#validate-a-change).
  Matching skills apply by their descriptions, for example
  `rust-dependencies` before the new crate and the `sha2` declaration, and
  `rust-delivery-platform` for the CI part.
- Current checkout `ac6a6ffe248cf6dac5232438876c564030316ed1`. The only
  change since the design baseline makes `Dsn::admit_with_environment`
  crate-private; no consumed input changes.
- A usable local Docker daemon and the local baseline commit `d24d173` —
  consumed at final acceptance only, by the database suites, graphs 13–16, and
  the one-shot `none` equality.

Provides:
- The complete retained pack and profile machinery named in the Outcome, with
  tests beside their owners; unverified until final validation.
- In this packet: the executor's record of the deliberate `Cargo.lock`
  regeneration and the final commands it selects for the delivery owner.

Boundary:
This one task holds the whole selectable profile. The store, seam,
configuration, composition root, migration, proof, initializer, validation
runner, routing, and guide are companion layers of one outcome, and none can
be accepted alone. Without the profile machinery, the initializer refuses the
pack's markers as unknown, and an unmarked pack would break every
`DATABASE=none` graph. Without the pack, the machinery would select an empty
profile. Parallel work comes from lanes inside this task, not from more
tasks. No lane or finished subresult creates a test or review gate.

This obligation map fixes coverage, not coding steps or test cases:

| Accepted obligation | Current-to-target delta and cleanup | Authority |
| --- | --- | --- |
| Record store | New crate `crates/infra-idempotency-store`: `lib.rs` is the facade and `Store`; `attempt.rs` holds arbitration, the one execution transaction, readback, the opaque `Tx`, and its only accessor `connection`; `maintenance.rs` holds the startup check and the bounded cleanup drain and loop. It reuses `in_tx_with`, `in_tx`, `retryable`, and `TxOptions` unchanged, is the only crate that names the table, and knows no HTTP. | System §4, §6, §10; ownership store rows |
| Profile schema | Empty `migrations/` gains `20260923000001_create_http_idempotency_records.sql` exactly as §6.1. The unmarked README sentence is reworded and a marked paragraph added. The service never creates schema at runtime. | System §6.1, §6.4; ownership schema row |
| Inbound seam | New `crates/infra-http/src/idempotency/` (`mod`, `declaration`, `compose`, `identity`, `fingerprint`, `execute`, `stored`, `openapi`) with the §4 public API and the §5 and §7–§9 rules. That includes the pinned vectors, the 100 ms readback reserve derived from `RequestDeadline`, 504 precedence through a pending future, exactly one outcome per valid fingerprint with the `abandoned` drop guard, and the 2xx wiring guard. `Tx` is re-exported; `connection` never is. A marked `pub mod idempotency;` registers it. | System §3–§5, §7–§9; ownership seam rows |
| Catalog | Four marker-scoped codes with their metadata, plus one unmarked `responses` module-doc sentence. | Spec catalog; ownership `problem.rs` row |
| Retention setting | New `crates/config/src/http_idempotency.rs` (vacant when empty, 1 min..=30 days, `required_retention`), marked registration, a marked loader test, and a commented example in `env/config/local.toml`. No other key. | Spec configuration; system §10 |
| Composition root | `contract()` becomes `contract(&mut Composer)` with `.merge(idempotency.components())`; `document()` renders with `Composer::inert()`; a `#[cfg(test)]` idempotent route; the unmarked `api.rs:157` edit to `document()`. The `openapi` binary is unchanged. | System §10 item 3; ownership composition-root row |
| Bootstrap | Bind the verifier (a marked `let verifier =` region before `authn:bootstrap-authn-prepare`); `prepare_http_idempotency` after pools; the `Prepared` field; the unmarked move of contract assembly above readiness admission with the pack-neutral comment; `activate_http_idempotency`, then `start_http_idempotency` (required retention, startup check, cleanup spawned on the existing tracker, the `http_idempotency_active` record). Every failure exits 1 before admission. Tests sit inside the existing `mod tests`. | System §10; ownership bootstrap row |
| Generated contract | Regenerate the committed `api/openapi/service.yaml` from the Rust contract with the existing generator, never by hand. In the source it gains exactly the four unreferenced response components. `none` outputs keep the baseline document. | Spec selection; system §5 |
| Contract proof | A marked region in `crates/service/tests/openapi.rs` with an independent walk of rules 1–6 and the reverse key rule, plus the agreement test. | System §5, §11.1 |
| Real-PostgreSQL proof | New `test/tests/http_idempotency/` (`main.rs` for P1–P8, `commit_proxy.rs`, `mounted.rs` for P9), marked dev-dependencies split between `http-idempotency` and `http-idempotency-mounted`, and a marked `test/README.md` line. Like `test/tests/postgres.rs`, the suite is gated on the `integration` feature, so `make test` runs none of it. No `test-support` feature on the store or `infra-http`, no JWT fixture, no production seam. | Spec P1–P9; system §11.1, §11.2 |
| Manifests and lock | Workspace entries (the `infra-idempotency-store` path, `sha2` 0.11.0 without default features) and the crate edges inside their markers. `Cargo.lock` is regenerated deliberately once and never edited by hand; validation stays `--locked`. The expected delta is one new local package and new edges of existing local packages, with no registry package. A changed registry dependency list would instead call for the guarded feature-edge rule. | Ownership manifests; system §2, §12 |
| Selection, lock, sync | `HTTP_IDEMPOTENCY` as a flag, an environment variable, and a `make/template.mk` default and export; refusals from one predicate in `template_state.py`; the five-field lock and the admitted historical shapes; the fourth inventory generation; the marker profiles `http-idempotency` and the derived `http-idempotency-mounted`; sync validation that never restores a pruned pack. | Spec profile and sync; system §12; ownership initializer owners |
| Validation boundary | The runner goes from 12 to 16 graphs, appending 13–16 with the idempotency database step and a Docker preflight; every graph's receipt line gains the two output digests, and candidate admission covers every new path. The projection checker goes from 96 to 128 projections, asserts the 8 refused selections, and asserts `none` purity. The init-safety, sync-canary, and purity suites and the self-tests gain their cases. | System §11.3; ownership initializer owners |
| Delivery routing | CI initializer parts go from 3 to 4: `http-idempotency` runs graphs 13–16 with Docker, restores the `database-postgres` cache, and saves none. Classifier rows and their self-test; `verify.sh` reason text and `requires_docker=true`; the `make/source.mk` help text. | System §11.3; ownership initializer owners |
| Adoption documentation | New `docs/http-idempotency.md` (whole path) and every owner in the documentation table, with its marker ids or unmarked wording, including the roadmap's stage-10 independence sentence. The guide carries the transition's note: identify the release by `app.version` in `service_starting` or by the platform's rollout status, not only by the operation count. | Ownership documentation owners; transition carried notes |

Proved unchanged, deferred, or not adopted (no implementation):
- `docs/architecture/integration.md`, which already states the adapter rule.
- `infra_http::protect`, the hardened chain, `in_tx` and `in_tx_with`, commit
  classification, pool budgets, readiness probes, shutdown stages, the
  `openapi` binary, `tools/versions.env`, and
  `scripts/ci/test-integration-db.sh`, which already forwards extra arguments
  such as `--test http_idempotency` to `cargo test`.
- `query!` with offline metadata, `sqlx-cli`, and per-query spans, deferred to
  the first feature-owned repository (only the documentation wording moves),
  and no migration-history exemption.
- A JWT fixture, a `test-support` feature on the store or `infra-http`, and
  the ownership panel's optional `compile_fail` guard for `Tx`.

Excluded: any product operation, non-PostgreSQL store, idempotency for
effects outside PostgreSQL, profile migration of an initialized service, other
stage-10 capability, portable instruction or skill change, new knob, readiness
probe, or shutdown stage, and any commit, PR, push, merge, publication, or
deployment.

Acceptance bookkeeping: the roadmap's stage-10 status and the 10.3 entry's
acceptance summary change only after every required check and the final
review pass. The delivery owner then writes them and may correct the stale
10.2 status text (PR #42 merged), which Definition assigned to it. A
`make docs-check` after that edit is its only check.

Mutable owners:
- Manifest set and lockfile: the root `Cargo.toml`, the manifests of
  `infra-idempotency-store`, `infra-http`, `service`, and
  `integration-tests`, and the one deliberate `Cargo.lock` regeneration.
  `crates/config` already depends on `humantime` and `humantime-serde`.
- Record store: the new crate's sources and inline tests
  (`crates/infra-idempotency-store/src/`) and the migration set
  (`migrations/`, including its README).
- Inbound seam: `crates/infra-http/src/idempotency/`,
  `crates/infra-http/src/problem.rs`, and `crates/infra-http/src/lib.rs`.
- Configuration: `crates/config/src/http_idempotency.rs`, `lib.rs`, and
  `load.rs`, and `env/config/local.toml`.
- Composition root: `crates/service/src/api.rs`,
  `crates/service/src/bootstrap/mod.rs`, `crates/service/tests/openapi.rs`,
  and the regenerated `api/openapi/service.yaml`.
- Database proof: `test/tests/http_idempotency/` and `test/README.md`.
- Profile machinery: `scripts/lib/template_state.py`, `template_init.py`,
  `template_sync.py`, and the inventory `template_profiles.json`;
  `make/template.mk`; and the suites `scripts/tests/template-init-safety.py`,
  `template-sync-canary.py`, `template-owned-purity.py`, and
  `template-profile-projections.py`.
- Validation and delivery routing: `scripts/ci/template-init-check.sh`,
  `changed-surfaces.sh`, and `verify.sh`,
  `scripts/tests/template-candidate-paths.txt`, `make/source.mk`, and
  `.github/workflows/ci.yml`.
- Documentation: `docs/http-idempotency.md` and the `docs/` owners in the
  ownership map's documentation table; the roadmap only for its independence
  sentence until acceptance bookkeeping. That table's `migrations/README.md`
  and `test/README.md` belong to the record-store and database-proof owners.
- This packet, where the executor appends its lockfile record and selected
  final commands. The Orchestrator alone writes `tasks.md`.

These owners are pairwise disjoint, and each file-level exclusive lock below
has exactly one of them as its writer.

Exclusive locks:
- The manifest set and lockfile: the root `Cargo.toml`, the manifests of
  `infra-idempotency-store`, `infra-http`, `service`, and `integration-tests`,
  and `Cargo.lock`.
- The marker inventory: `scripts/lib/template_profiles.json` with the marker
  ids and whole-path lists fixed by the ownership map. Every region any owner
  writes must match it before an initializer, projection, or matrix run; the
  initializer refuses unknown, missing, nested, or duplicate markers.
- The generated contract: `api/openapi/service.yaml`, regenerated only after
  the contract code settles.
- The migration chain: `migrations/` gains exactly one forward-only file.
- The shared validation lock and one Cargo target: at most one Cargo,
  initializer, or Docker-backed command runs at a time.

Final validation:
- Claim: in the assembled candidate, the retained pack meets the Specification
  on its real paths. That means the store boundary against real PostgreSQL
  with two independent pools, and the mounted router under the hardened chain
  with the real introspection verifier wherever that engine is retained. With
  no idempotent operation the pack is inert and adds only the four
  unreferenced response components. Startup refuses a broken agreement or a
  missing activation prerequisite before admission. The initializer selects,
  refuses, locks, replays, prunes, and syncs the profile as specified. Every
  `none` output equals the baseline contract and lock, with no pack path or
  marker. Selected output keeps the guide and its companion documentation with
  valid links; default output removes them.
- Checks: after T1 is Implemented and integrated, the delivery owner that the
  Orchestrator assigns (normally T1's Lead) runs one non-overlapping plan on
  one fixed candidate, and each command runs once. Exact commands stay
  executor-owned; the targets below are the existing ones the accepted sources
  name.
  - Ordinary criterion. This candidate is a mixed surface: Rust crates,
    manifests and the lockfile, scripts, a workflow, a migration, and
    documentation. [AGENTS.md](../../../AGENTS.md#validation-budget) therefore
    uses the route that `make plan` prints for the fixed candidate, and every
    local step of that route runs once. That output is authoritative. For the
    planned path list it printed 18 local steps:
    - the validation self-tests: `make changed-surfaces-check`,
      `make affected-crates-check`, `make validation-lock-self-test`, and
      `make verify-check`;
    - the dependency checks `make unused-deps` and `make deny`;
    - `make fmt-check` and the workspace `make lint`, `make build`, and
      `make test`. The manifest and lockfile change selects the whole
      workspace, and `make lint` includes the `integration` feature, so it
      covers the database suite.
    - `make openapi-check`, `make actionlint`, `make zizmor`, and a scoped
      `make shellcheck`;
    - `make migration-check` and `make migration-history-self-test`;
    - `make dockerfile-check`, because the new migration also selects the
      runtime-image surface;
    - `make docs-check`.

    It marked five steps CI-owned. `make template-init-check` stays CI-owned
    unless `ALLOW_FULL=1` is set. `make test-integration-db`, the runtime
    image build, `make migration-validate`, and `make container-security`
    stay CI-owned unless `ALLOW_HEAVY=1` is set. The route also contains every
    non-heavy check that the [proof expectations](../spec.md#proof-expectations)
    and the ownership map's final-validation list name, except the two
    additions below.
  - Local heavy runs the request requires, each once:
    - `ALLOW_HEAVY=1 make test-integration-db` covers P1–P9 and the existing
      PostgreSQL proof in the source.
    - `ALLOW_FULL=1 make template-init-check` covers the source suites, the
      128 projections, and the 16 runtime graphs. Each graph is initialized,
      built, and tested once, and graphs 13–16 also run their idempotency
      database step.
    - The one-shot `none` equality initializes a `d24d173` clone for the 12
      `none` selections with the runner's identity values. It compares the
      clone's `api/openapi/service.yaml` and `Cargo.lock` digests with the
      receipts for graphs 1–12.

    The matrix and the equality initializations share one absolute
    `CARGO_TARGET_DIR`, for example the checkout's `target/`.
  - Other named additions:
    - `make secret-scan`, the contribution policy's secret gate for a
      manifest and lockfile change;
    - the runner's `--self-test`, a proof owner in the ownership map that no
      route step or CI job runs.
  - Execution form: run each command once, one by one, cheap and CPU steps
    before the heavy runs, and keep one result record per command. Do not run
    `make verify`: it puts the matrix before the Rust steps, stops at the first
    failure, and repeats every step after a repair. Its receipt would also be
    an aggregate claim that the ownership map does not add. Running commands
    one by one also leaves `tasks.md` and this packet writable while
    validation runs.
  - Final review under [Review](../../../docs/spec-first-workflow/shared/review.md):
    one fresh independent reviewer bound to
    [Implementation Review](../../../docs/spec-first-workflow/phases/implementation-review.md#final-delivery)
    for the fixed candidate. It is required because the behavior affects data
    integrity, concurrency safety, and a forward-only migration, and because
    the Intent asks for independent review. It runs alongside validation, stays
    read-only, and runs no Cargo, initializer, or Docker work.
  - Not run locally: the runtime image build, `make migration-validate`, and
    `make container-security` stay CI-owned, since system design §13 and the
    ownership map add no image claim. They and every CI result are reported as
    pending CI, not claimed.
    No `ALLOW_FULL=1 make check` or other aggregate runs on top, and no build
    runs per harness.
  - A repair reruns only the invalidated scope under the
    [Evidence Contract](../../../docs/spec-first-workflow/shared/evidence-contract.md),
    for example `--runtime-graphs` for the affected graphs. A scoped rerun is
    never relabeled as the full aggregate.
- Observable:
  - The source build and every workspace test pass, including the inline seam,
    store, configuration, bootstrap, and contract tests (the pinned vectors,
    the key-pattern parity, the `Attempted` and `ReadBack` mapping, and
    agreement in both directions).
  - `make openapi-check` passes, and the regenerated document differs from
    `HEAD` only by the four unreferenced components.
  - The source database suite passes P1–P9 and the existing PostgreSQL proof
    with no skipped test.
  - The matrix receipt records passing source suites, the projection
    self-test, 128 projections, and 16 graphs with one passing
    initialization, build, and test each. Graphs 13–16 also pass their
    idempotency database step, with P9 in 15–16. Every graph line carries
    `openapi_sha256` and `cargo_lock_sha256`.
  - All 24 digests for graphs 1–12 equal the `d24d173` outputs.
  - Every other local step of the route (including `make fmt-check` and the
    workspace `make lint`), `make secret-scan`, and the runner's
    `--self-test` pass, and the final review returns PASS on the same
    candidate.

  Missing Docker, a failed required check, or an open blocking finding leaves
  Completion `Blocked` with the exact unverified claim. Implementation alone
  never satisfies this section.

Reopen if:
Research, for changed sqlx drop, cancel, or commit semantics, a PostgreSQL
major-version change in the proof image, RFC publication of the draft, or a new
verified-caller mechanism. The Specification, for any observable change,
including a selection or combination rule, waiting, error replay, scope,
grammar, stored format, encoding, or digest domains, or a shared change that
would break the `none` equality. Technical Design, when implementation
disproves a pinned API, statement, lock behavior, cancellation path, marker
layout, or proof mechanism. Intake, for a changed outcome or authority.
Planning, only when a repair cannot fit this task's boundary. Mechanical
locators, marker id spellings (the map allows renames), lane splits, and lock
bookkeeping stay with execution. A missing input returns to the Lead and the
Orchestrator; no technical choice goes to the user.

## Execution custody

Execution state that a later actor must recover lives under
`.git/claude/http-idempotency/` in the Git common directory. It is untracked,
survives session restarts and scratchpad loss, and sits outside this bundle,
so bundle Cleanup leaves it intact. It is evidence custody, not a second
ledger.

- `implementation/` holds the records Implementation keeps for possible final
  reuse: the lockfile regeneration command with its registry-package
  comparison, and any bounded coding-feedback command with its result and
  exercised scope.
- `delivery/` holds the final-validation record:
  - the fixed candidate manifest: `HEAD` plus the mode and SHA-256 of every
    changed or added path outside `specs/http-idempotency/`, with the
    manifest's own SHA-256 as the candidate fingerprint;
  - the selected plan with each check's state;
  - one log and one [Evidence Result V1](../../../docs/spec-first-workflow/interfaces/evidence-result-v1.md)
    record per executed check, including its environment
    (`CARGO_TARGET_DIR`, `ALLOW_*`, Docker);
  - the path and SHA-256 of each receipt the existing runner writes under
    `.git/codex/template-init/`;
  - the `none`-equality record: the baseline commit, the identity values, the
    24 digests on both sides, and the verdict;
  - the final review record and the Completion result.
- The ledger's Execution and Resume fields and the Completion result point
  here. The implementation itself is the working tree; nothing is committed.

## Disjoint writable scopes

This is a non-canonical working checklist. The Lead chooses lanes, and any
grouping of the mutable owners above stays disjoint. The lock holders
integrate serially:

- The manifest set and the one lockfile regeneration come first, so that lanes
  can compile against the closed §4 API.
- Every owner writes its marker regions with the ownership map's ids. The
  profile-machinery owner writes the inventory, and the Lead reconciles all
  regions against it after the lanes join and before any initializer run.
- The composition-root owner regenerates the contract after the seam's
  `openapi.rs` settles.
- The routing owner admits every new untracked path: the new crate, the seam
  directory, the test directory, the configuration file, the migration, and
  the guide. Otherwise the matrix silently omits it, because the runner copies
  only tracked files plus that allowlist.
- Code-writing lanes may run in parallel; Cargo, initializer, and Docker
  commands never overlap. Compile-only diagnostics that cover the database
  suite need the `integration-tests/integration` feature.

## Executor record

Written by the T1 Lead at Implemented. Custody:
`.git/claude/http-idempotency/implementation/`.

Cargo.lock regeneration (once, before any lane started):

- Command: `cargo update --workspace --offline` (cargo 1.98.1), after the
  final manifest set: the root `Cargo.toml` markers, the new store manifest,
  and the `infra-http`, `service`, and `integration-tests` manifests (the
  last also gained the unmarked `net` feature on its existing `tokio`
  dev-dependency, which the commit proxy and the P9 fixture server use).
- Lock SHA-256 `816e628f…cdc92d` before, `694977b5…f32c4be` after.
- Registry comparison: 495 registry packages before and after; none added
  or removed, and no registry package changed its dependency list or
  checksum. The delta is the new local package `infra-idempotency-store` and
  new edges of `infra-http` (+store, +`sha2 0.11.0`), `service` (+store),
  and `integration-tests` (+store and the eight P9 dev-dependencies). No
  guarded feature-edge rule is needed; the `none` equality stays the oracle.

Selected final commands (from `make plan` on the implemented candidate:
18 local steps, 5 CI-owned), each once, one by one, in this order:

1. `make changed-surfaces-check`, `make affected-crates-check`,
   `make validation-lock-self-test`, `make verify-check`
2. `make fmt-check`, `make unused-deps`, `make deny`, `make secret-scan`
3. `make build`, `make lint`, `make test`, `make openapi-check`
4. `make actionlint`, `make zizmor`,
   `make shellcheck SHELL_FILES='scripts/ci/changed-surfaces.sh scripts/ci/template-init-check.sh scripts/ci/verify.sh'`
5. `make migration-check`, `make migration-history-self-test`,
   `make dockerfile-check`, `make docs-check`
6. `bash scripts/ci/template-init-check.sh --self-test`
7. `ALLOW_HEAVY=1 make test-integration-db`
8. `ALLOW_FULL=1 CARGO_TARGET_DIR=<checkout>/target make template-init-check`
9. The one-shot `none` equality: a `d24d173` clone initialized for graphs
   1-12 with the runner's identity values, which now carry the idempotency
   segment (`matrix-<database>-<authn>-<outbound_http>-none-core`, the
   matching repository and description, `@example/platform`, harness
   `core`, no `--http-idempotency` flag), initialize only, the same absolute
   `CARGO_TARGET_DIR`; compare `api/openapi/service.yaml` and `Cargo.lock`
   digests with the `openapi_sha256` and `cargo_lock_sha256` of the graph
   1-12 receipt lines.

Not run locally (pending CI): `make runtime-image-build`,
`make migration-validate`, and `make container-security`. No `make verify`
and no aggregate on top.
