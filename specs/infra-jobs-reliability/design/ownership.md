# Ownership Map V1

Status: ready

This map closes placement for [the selected mechanism](system.md). Paths below
are relative to this worktree. Existing package boundaries stay; features gain
no provider dependency. The public operator API belongs to the table adapter,
CLI projection/lifecycle to its binary, and configuration-source semantics to
`service-config`. Those distinct present responsibilities justify the adopted adapter, CLI
and operator modules; no shared lifecycle crate, generic command framework, test-only
production trait or new service boundary is introduced.

The exact immutable foundation and remaining D1–D6 corrections are in
[System Design](system.md). This map adopts its actual files instead of
reimplementing equivalent interfaces. Existing panel PASS remains applicable
to unchanged responsibility/crate boundaries; only adoption and the named
corrections require the focused delta review.

## Responsibilities

| Responsibility | Affected path and current evidence | Semantic owner / exact package-file action | Dependency, composition and generated boundary | Cleanup | Proof owner | Reopen condition |
| --- | --- | --- | --- | --- | --- | --- |
| R1 Attempt and completion custody | claim.rs reserves permits; attempt.rs drops slots before persist and owns an unbounded Vec plus one batch writer. | infra-jobs: change attempt.rs to the adopted private AttemptSlots/CompletionRegistration/CompletionBatch, preserving batching and completing D1 disposal-before-notification. claim.rs needs no change for the archive representation. | Existing Tokio/standard Arc/mutex/oneshot support; no new dependency or task owner. | Queue guard removes abandoned queued entries; batch Drop releases transferred custody; existing supervisor tracker joins all. | Local infra-jobs lifecycle units where deterministic; existing real-DB test/tests/jobs/execution.rs for observed write blocking/deadlines. | Boundedness cannot coexist with retained batching semantics. |
| R2 Recovery state and history | Existing table, claim sequence, JSONB errors, in_tx and partial live unique index. | adopt migrations adding same-row recovery_history and the failed-kind index; operator.rs owns inspect/recovery SQL, archive-before-reset and provisional typed outcomes; lib.rs exports it. No claim/failure-history writer change is required. | Only infra-jobs names background_jobs in runtime code. Migrations own schema, SQLx metadata derives from fixed SQL. Public opaque-Tx recovery is provisional; jobs-worker owns its in_tx/commit receipt. No business closure replay. | Completed retention or explicit discard removes the row/archive; redrive archives active errors atomically before resetting them. Existing sequence is never reset. | adopt test/tests/jobs/operator.rs including its outbox-identity cases and existing migration/history proof; rerun any invalidated existing messaging integration surface. | New external restore incarnation, history retention mandate, or a caller unable to preserve provisional-result/failure semantics. |
| R3 Retention and process observation | maintenance.rs deletes completed/failed and samples own registry; engine.rs Peer list shares listener/retention but starts multiple samplers. | infra-jobs adopt completed retention, failed metric and sole union sampler; finish D2 peer admission invalidation and locked publication membership recheck in engine.rs/maintenance.rs. | One Engine::new tree with all other engines built beside it; existing recorder and background tracker. No dynamic unknown-kind labels or observer service. | Existing cancellation and guard failure channel; no peer sampler left active. | jobs execution/process and outbox integration observation surfaces. | Multiple independent engine trees in one process become supported, or measured observer cost needs a new contract. |
| R4 Operator configuration projection | config/load.rs merge<T> and load_migration already own projections; config/jobs.rs owns jobs-section declarations. | service-config jobs.rs adds JobsOperatorConfig; load.rs adds load_jobs_operator and source-rule coverage; lib.rs exports both behind jobs markers. | Projection contains only existing PostgresConfig. No crate imports infra-jobs, no CLI operation semantics enter config, no new config key. | One immutable snapshot per invocation; secrets remain redacted and drop with it. | config loader unit tests for precedence, ignored provider sections, secret/name refusal and PostgreSQL validation. | Operator actually requires another section. |
| R5 Operator process and CLI | jobs-worker lib.rs run currently calls LoadOptions then full start; bootstrap.rs owns normal admission; shutdown.rs owns Signals. | adopt cli.rs for Clap grammar, operator.rs for caller transaction/JSON/one-shot lifecycle, and lib.rs for pre-start dispatch/exit projection. Finish D3–D5 in these existing owners. | Reuse LoadOptions by flattening. Keep the resolved workspace clap/std+derive+help+usage+error-context edge for retained worker CLI; serde_json/std and operator command definitions remain jobs-marked. No new version, no SQL inside binary. | Signals before I/O; pool close bounded to 5s; existing runtime shutdown 1s. No listeners/tasks/exporter/registration. | jobs-worker module units and built-binary process tests; jobs real-DB process suite for admitted commands with unavailable unrelated providers. | A new operator transport or long-running resource is accepted. |
| R6 Startup check reuse | Foundation maintenance::check_startup takes PgPool plus an optional-writability flag. | infra-jobs maintenance.rs owns unconditional UTF8/writable/READ COMMITTED admission over PgPool; engine.rs delegates and operator.rs exposes it. Remove the foundation-only optional-writability flag and make every command use the canonical check (D3). | No new connection abstraction or read-replica authority. History admission remains migrate::verify_history in the binary. | Existing 5s startup check and safe StartupError. | Existing startup tests plus operator refusal through process suite. | Session requirements differ materially between supported operations. |
| R7 Profile and documentation closure | scripts/lib/template_profiles.json names explicit jobs removals/markers and existing runtime projections. | Update that inventory for added modules, migrations, markers and docs; test/tests/jobs/main.rs includes operator module; current projection assertions and CI representatives consume changes. Update docs/background-jobs.md, docs/architecture/async.md, docs/architecture/runtime-lifecycle.md, docs/configuration-source-policy.md, docs/postgres-transactional-outbox.md, migrations/README.md and impacted module docs. | Inventory/markers are canonical; generated initialized trees, Cargo.lock and .sqlx are derived through existing tools. Current CI/classifier owns gate selection. No new matrix cross-product. | jobs:none prunes operator module/commands/serde_json/exports/migrations/tests; messaging-only retains cli.rs/Clap loader-only worker path. | Existing template-profile projections, source/runtime representatives and docs-check; assembled review. | New profile dependency or a changed generated-owner contract is required. |

Non-mechanical reuse choices: R1 uses the existing completer plus standard/Tokio
ownership at resolved Tokio 1.53.1; removing batching is the strongest simpler
alternative, rejected for the present measured group-commit benefit unless
correctness requires it. R2 uses current SQLx 0.9 transaction and PostgreSQL
sequence/index semantics; a second recovery store is unnecessary. R4 uses the
current generic loader projection; duplicating parsing or using full Config
would violate its boundary. R5 uses existing Clap 4.6.7 derive/flatten rather
than hand parsing. [Supporting evidence](../research/mechanism-evidence.md)
records exact authorities. Parity proof is the relevant R1-R7 falsifier and
unchanged normal worker invocation; new library versions are not needed.

## Files

Each row names the first real artifact or exact existing responsibility, not a
placeholder directory. `pub(crate)` types remain in their owner; only the
operator request/result API crosses from infra-jobs into jobs-worker.

| Path | Responsibilities | Present reason | Declarations / visibility | Call-path role | Lifecycle/error owner | Allowed dependencies | Forbidden responsibilities |
| --- | --- | --- | --- | --- | --- | --- | --- |
| crates/infra-jobs/src/attempt.rs | R1 | Adopt full-attempt custody; complete D1 batch disposal ordering. | Private AttemptSlots/CompletionRegistration/CompletionBatch and Completions; no new public supervisor surface. | supervise -> run_attempt -> persist -> direct/batched write. | Own guards and captured deadlines; retain OperationError/unknown semantics. | Current crate deps only. | CLI parsing, config, business replay. |
| crates/infra-jobs/src/operator.rs (adopted) | R2, R6 | Table-owned safe inspection and recovery, independent of handlers. | Public input/result/error structs/enums and functions; validated Inspection limits/handled-kind admission remains adapter-owned; SQL helpers private. | jobs-worker caller transaction -> adapter inspect/page or locked recovery -> provisional result -> caller commit. | Adapter owns 2s statement bounds and safe errors/provisional results; binary caller owns 12s operation/cancellation, commit classification and pool lifecycle. | Existing infra-postgres/sqlx/uuid/standard collections and errors; no config or binary deps. | JSON stdout formatting, new credentials, handlers, NATS, schema migration. |
| crates/infra-jobs/src/maintenance.rs | R3, R6 | Completed retention, process-union samples, pool-only session check. | Existing crate-private functions; safe metrics remain adapter-owned. | Process-duty engine maintenance and shared startup check. | Existing tracked task/cancellation, operation bounds and last-good semantics. | Existing deps. | Fleet registry, auto recovery, failed deletion. |
| crates/infra-jobs/src/engine.rs | R3, R6 | Process peer metadata, sole sampler and unchanged engine startup delegate. | Preserve public constructors/start/drain; peer snapshot/invalidation remains private. | new/beside -> start; same failure/teardown hooks. | Existing process-duty/Started ownership. | Existing deps. | Operator connection/bootstrap or dynamic unknown labels. |
| crates/infra-jobs/src/lib.rs | R2 | Export operator module and update contract docs. | pub mod operator; preserve current exports. | Adapter API facade. | No new lifecycle. | Own modules only. | Implement commands or duplicate SQL. |
| crates/config/src/jobs.rs | R4 | JobsOperatorConfig uses the current PostgreSQL section for a jobs-only entry. | Public snapshot; crate-private validation. | loader -> typed projection. | Immutable config and validation only. | Existing config types/serde. | Queue behavior, Clap command syntax, I/O resources. |
| crates/config/src/load.rs | R4 | Reuse source precedence and redaction for new typed projection. | Public load_jobs_operator; explicit-env counterpart crate-private for existing test style. | Files/secrets-dir/env -> merge -> projection validate. | Existing loader Error. | Existing deps. | New source namespace or command dispatch. |
| crates/config/src/lib.rs | R4 | Jobs-retained exports. | Export JobsOperatorConfig/load_jobs_operator inside jobs markers. | Config facade. | None added. | Own modules only. | Provider or worker dependency. |
| crates/jobs-worker/src/cli.rs (adopted) | R5, R7 | Worker loader grammar and jobs-optional commands have a distinct parse-only responsibility. | pub(crate) WorkerArgs/OperatorCommand; commands behind jobs markers. | argv -> admitted command shape before dependency I/O. | lib.rs owns usage exit; no resources here. | Existing Clap and service-config LoadOptions. | SQL, pool lifecycle, output JSON, business handlers. |
| crates/jobs-worker/src/operator.rs (adopted) | R5 | Adopt one-shot process mode and caller transaction; finish D3–D5 without normal-worker resources. | pub(crate) Request and run; internal execute/report/output types; command grammar stays in cli.rs. | run -> operator -> admitted PostgreSQL -> in_tx adapter action -> acknowledged receipt. | Own one-shot cancellation/close, transaction finality, safe JSON/schema/handled-set echo; lib owns exit mapping. | Existing service-config/infra-postgres/migrate/infra-jobs/Tokio; declared workspace Clap and serde_json. | Queue SQL, NATS/handler construction, background loops, config precedence. |
| crates/jobs-worker/src/lib.rs | R5 | Select command before full worker load/registration and keep process result mapping. | Preserve public run/register contract; operator module marker and internal result mapping. | No command -> existing start; command -> operator entry. | Existing worker exits 0/1/3 unchanged; operator outcomes 0/1, parse 2. | Existing and jobs-marked operator module. | Duplicate startup or external recovery retry. |
| crates/jobs-worker/src/bootstrap.rs | R7 | Documentation/call-site adjustment only if check-startup delegate or sampler assumptions require it; existing build-before-start composition is preserved. | No new public surface. | All registries/peers built before engines start. | Existing lifecycle. | Existing deps. | A second operator implementation. |
| test/tests/jobs/operator.rs (adopted) | R2, R7 | First black-box DB proof of public operator API and bounded paging. | Private test module under existing jobs integration target. | Existing admitted DB fixture -> public operator API. | Existing bounded DB test lifetime. | Existing integration test deps. | New runner or production test-only seam. |
| test/tests/jobs/main.rs | R7 | Include recovery proof in current target. | mod operator. | Existing jobs suite assembly. | Existing fixtures. | Existing modules. | Alternate validation carrier. |
| test/tests/jobs/execution.rs | R1, R2, R3, R6 | Extend existing persistence/cancellation/retention/metrics proof where missing. | Existing black-box tests and local support only. | Current engine fixtures. | Existing bounded coordination. | Existing test deps. | Mirror implementation or expose internal controls only for tests. |
| test/tests/jobs/process.rs | R3, R5, R7 | PostgreSQL-only commands and shared observation at process boundary. | Existing process fixture tests. | Built worker/fixture invocation. | Test child cleanup and current process contract. | Existing test deps. | A new service or validation environment. |
| crates/jobs-worker/tests/process.rs | R5, R7 | Existing built-binary CLI/help/refusal proof; extend if its boundary proves the case. | Existing black-box tests. | Actual jobs-worker binary. | Existing process-test owner. | Existing dev deps. | Duplicate DB integration scenarios. |
| test/tests/webhooks/inbound.rs (mechanical foundation repair) | R7 | Preserve historical-row comparison after the additive archive column. | Existing private integration test; no public surface. | Migration fixture -> old/new row comparison. | Existing database test owner. | Existing test dependencies. | New webhook behavior or runtime compatibility path. |
| test/tests/messaging_outbox.rs | R2, R3, R7 | Preserved publication identity/bytes and one-slot/shared observation after recovery. | Existing outbox test target. | Stored outbox -> operator -> publisher -> current broker fixture. | Existing integration owner. | Existing test deps. | New live broker/consumer deployment proof. |

Tests are placed here by behavioral owner; Implementation reuses adequate
coverage and chooses exact new cases. Adding all listed test files/edits is not
a checklist independent of the proving surface.

Non-Rust inverse map:

| Path | Responsibility and source/generated authority |
| --- | --- |
| migrations/20261002150001_add_background_job_recovery_history.sql | R2: adopt immutable recovery_history column migration; its version follows baseline newest 20261002150000. If another accepted migration lands first, allocate the next free strictly increasing UTC-style version and update inventory before implementation freeze. |
| migrations/20261002150002_index_failed_background_jobs.sql | R2/R3: adopt one `-- no-transaction` CREATE INDEX CONCURRENTLY IF NOT EXISTS statement, under current migrator recovery. Same version-allocation rule. |
| crates/jobs-worker/Cargo.toml and Cargo.lock | R5: retained-worker Clap and jobs-marked serde_json edges at their already resolved versions; deliberately regenerate any affected package lists, without version upgrade. |
| .sqlx/query-*.json | R2/R3/R6: generated by make sqlx-prepare for all changed/new fixed queries, remove obsolete failed-retention statement metadata. Never hand-edit. |
| scripts/lib/template_profiles.json | R7: canonical removal/marker inventory for new jobs files, migrations, config exports/loader markers, command selection and dependencies. |
| scripts/tests/template-profile-projections.py | R7: existing pruning invariants; adjust only where new source footprint needs expectation coverage. |
| scripts/ci/template-init-check.sh | R7: use current factored representatives; change only a concrete missing retained/pruned command proof, not a new duplicate matrix. |
| docs/background-jobs.md | R1-R7: operator syntax/receipt/outcomes, failed storage, cancellation bound, retained limits and runbook; discard loss warning before example. |
| docs/architecture/async.md | R1-R3/R6: replace contradicted early-slot, failed-retention and per-engine sampling descriptions with selected owners. |
| docs/architecture/runtime-lifecycle.md and docs/configuration-source-policy.md | R4/R5: one-shot entry, explicit command exception/projection and unchanged normal worker lifecycle. |
| docs/postgres-transactional-outbox.md | R2/R3/R7: recovery identity, indefinite replay/finite dedup distinction, publisher kind and retained one-slot/failure domain. |
| docs/architecture/boundaries.md and docs/architecture/persistence.md | R2/R5/R6/R7: retain foundation ownership/history documentation and reconcile transaction finality, canonical admission and the actual 12s operation backstop. |
| scripts/ci/sqlx-prepare.sh | R7: retain foundation script change as source-generation infrastructure; any further alteration requires an observed metadata-generation defect. |
| migrations/README.md | R2/R7: new schema history and forward-only compatibility sequence. |

The new migrations and operator file removals are jobs-owned, not messaging-
owned; PostgreSQL outbox already requires jobs. Messaging-only builds retain cli.rs/Clap with flattened LoadOptions after jobs
commands and the operator module are pruned.
No API/OpenAPI/gRPC source changes are required. Existing CI surface selection
already covers the changed crates/test/migration/scripts paths; a new classifier
or workflow edit needs an observed gap, not a speculative gate.
