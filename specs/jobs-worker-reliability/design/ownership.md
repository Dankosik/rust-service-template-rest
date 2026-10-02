# Rust ownership map V1: jobs reliability

Status: ready

The mechanism authority is [system design](system.md); behavior is
[Specification](../spec.md). Every new declaration below exists for this
accepted correction. This map adds no crate, service port, trait, runtime
framework or generic administration layer. Existing source paths are evidence;
named new files are planned artifacts, not current implementation proof.

## Responsibilities

| Responsibility | Affected path and current evidence | Semantic owner and exact action | Dependency/composition/generated boundary | Cleanup | Proof owner | Reopen condition |
| --- | --- | --- | --- | --- | --- | --- |
| R1 — admitted attempt custody | `infra-jobs/attempt.rs::run_attempt`, `Completions`, `complete_batched`, `write_batch`; `claim.rs::dispatch_known` transfers permits | Extend attempt owner with private shared admission and cancellation/retirement guards; global/per-kind permit remains until last completion bookkeeping drop | Entirely internal; existing claim dispatch signature may retain its tuple and wrap it on receipt; no public Engine or Handler signature change | Remove early `drop(slots)` and superseded ownership comments; every guard and batch has synchronous teardown | Unit coordination in `attempt.rs`; real locking/deadline behavior in existing `test/tests/jobs/execution.rs` | Material cancellation/order cannot preserve B1 under this owner |
| R2 — retained custody and registered observation | `maintenance.rs::{remove_expired,sample_once,publish_sample}`, `engine.rs::{start,Shared.peers}` | Maintenance removes failed retention, adds failed gauge and one complete union sample; Engine's existing process-duty owner starts that sampler | No new process/task type; process peers remain engine-owned; worker builds all peers before starts | Delete failed duration/branch and each beside-engine sampling spawn; preserve completed cleanup and last-good data | Existing jobs execution/process tests; unit decode/freshness assertions beside maintenance | New fleet-wide SLO or measured observer cost; timestamp semantics cannot cover registered union |
| R3 — operator storage and query contract | All queue SQL is already `infra-jobs`; `kind.rs` privately owns JobId and kind grammar; shared Tx owns commit | Add `crates/infra-jobs/src/operator.rs`; expose `pub mod operator` in `lib.rs`; reuse kind validation and UUID internals from inside this crate | Adapter SQL/DTO/error owner only; no service-config, clap, handler registry, broker, listener or transaction-control SQL | No legacy recovery API exists; each call has one borrowed Tx and no spawned work | New `test/tests/jobs/operator.rs` module under existing jobs target; input/format units beside operator | Behavior delta, changed durable representation, or required public API cannot stay provider-owned |
| R4 — operator configuration projection | `config/load.rs::merge<T>` and existing migration projection; `jobs.rs` owns retained jobs config | Add public `JobsOperatorConfig` in `jobs.rs`, public `load_jobs_operator` plus crate-private explicit-environment variant in load.rs, export through existing jobs block in lib.rs | Same generic loader and secret pre-scans; only postgres field decoded, no new key/default; jobs-profile removable | Existing Config and migration projection remain their own APIs; no duplicate source parser | Unit config tests in jobs.rs/load.rs, worker process proof | Projection requires unrelated provider policy or loses current secret safety |
| R5 — worker command and one-shot lifecycle | `jobs-worker/lib.rs::run/start/exit_code`, existing Signals and provider close APIs | Add private `cli.rs` for optional commands and private `operator.rs` for projection/admission/transaction/output/close; dispatch in lib.rs before start; centralize operator exit mapping with ordinary mapping | Worker root alone depends on clap and composes config/infra-jobs/infra-postgres/migrate; existing Registration and ordinary start remain untouched | No operation constructs registry or ordinary bootstrap; no task survives close/runtime shutdown; do not add standalone binary | `crates/jobs-worker/tests/process.rs` for shipped CLI; jobs operator integration module for admitted database command path | Command needs independent deployable/transport or a new shutdown budget rather than existing ceilings |
| R6 — persisted history/index and generated projections | `migrations/`, `.sqlx/`, `scripts/lib/template_profiles.json`, initializer projection tests | Add the two ordered migrations in rollout; regenerate changed SQLx metadata; register whole-file/marker removals for every added jobs surface | Migrations own SQL schema, `.sqlx/` is derived by pinned tool; profile manifest owns removal, never generated snapshots as source | No old migration rewrite, no startup DDL, no runtime schema probing/repair | Existing migration-check, SQLx preparation/check, jobs real-database proof and selected source initializer CI | Base/schema drift or initializer graph cannot remove the new operator surface cleanly |
| R7 — current adopter and operator contract | jobs/outbox guides, async/runtime/config/persistence architecture, migrations README | Replace active early-slot/seven-day-failed/multiple-sampler/all-positional-refusal claims, document commands and retained limits; preserve profile markers | These describe actual owners; task-local design does not silently override stale shipping runbook | Remove superseded claims rather than append contradictory text | Static consistency and existing docs-check; final delivery review | Any document asks for a behavior different from the Specification |

R3 and R5 contain the only non-mechanical module placement decisions. Their
reuse rung is the existing adapter/composition crate over installed dependencies
and the standard library, as evidenced in
[mechanism research](../research/mechanisms.md). R3 rejects a separate admin
crate/table abstraction because all queue truth and SQL already belong to
infra-jobs. R5 rejects embedding CLI dispatch in provider/config crates because
it owns a binary mode and resource lifetime; it rejects a second executable
because the required deliverable is jobs-worker. Parity is the no-subcommand
worker path plus projected-profile compile/process proof. Upgrade/extraction
requires a second actual lifecycle consumer or inability to preserve that
dependency direction. R1's shared admission/Drop guards are private ownership
details, not a reusable service framework.

## Exported contract

Keep the new public provider surface in `infra_jobs::operator`, leaving the
existing root exports and JobId constructor unchanged. Required types and
capabilities are:

- `RecoveryTarget`: private fields id/kind/version; validated constructor from
  the three CLI strings, read-only accessors for safe receipts. It uses the
  current `JobId` and schema-supported nonnegative generation domain.
- `Inspection`: validated one-id, failed-page or unhandled-page request;
  cursor/limit/handled-set constructors enforce the bounds in system design.
  No caller can create a malformed mutation target or bypass request bounds by
  direct public field assignment.
- `JobSnapshot`: safe fields enumerated in system design; state and failure
  are closed enums, version has lossless decimal rendering, times are UTC text.
  It never holds payload/unique key/trace/error text/history bodies.
- `InspectionResult`: one/missing/page with observed_at; page includes scanned,
  items, complete and next_cursor. This is data, with no process exit behavior.
- `Redriven`: old target identity and new version returned by the statement;
  `Discarded`: exact old target identity. Both document provisional finality
  when used inside a caller-owned Tx.
- `InputError`: safe closed field/reason without rejected values.
  `OperatorError`: `Missing`, `Stale`, `Conflict`, SQL failure and shared
  `TxError` propagation, plus bounded startup/timeout categories as needed by
  the existing provider methods. Public safe classification exposes
  CommitUnknown and bounded SQLSTATE/cause; Display/Debug do not leak database
  error detail. No automatic retry classifier is exported.
- Async `inspect(&mut Tx, &Inspection)`, `redrive(&mut Tx, &RecoveryTarget)`,
  `discard(&mut Tx, &RecoveryTarget)`; inspection itself sets the two-second
  LOCAL statement limit through observed SQL, so worker needs no direct SQLx
  dependency. The command selects read-only `in_tx_with` for inspection and
  ordinary `in_tx` for mutation.
- `check_startup(&PgPool, require_writable: bool)` delegates to the internal
  shared session-check owner; `OPERATION_TIMEOUT` is the public operator name
  for the existing job-operation 12-second backstop, and `STARTUP_TIMEOUT`
  aliases the existing five-second jobs startup-check ceiling used around
  complete pool admission. Neither is a new configuration knob.

Implementation uses `JobState` and `FailureReason` with `as_str()` for the
closed snapshot values. `RecoveryTarget::version()` returns the admitted i64;
receipts render it as a string, and snapshot/new versions already use strings.
`OperatorError::sqlstate()` reuses `Option<Cow<'_, str>>` from infra-postgres,
which validates the bounded SQLSTATE; `cause()` and `is_commit_unknown()` carry
safe classification without raw driver formatting.

Names may receive idiomatic mechanical refinements only when the type/visibility,
dependency, behavior and proving boundaries above remain identical; record
those refinements in this map. Do not publish a generic repository trait,
transaction constructor, raw SQL handle, arbitrary filter, force operation,
batch mutation or unredacted DTO.

The private worker CLI uses `WorkerArgs { flattened LoadOptions, optional
OperatorCommand }`. clap is a direct workspace dependency with the same
features already selected by config, unconditionally because a messaging-only
worker still uses the wrapper parser for loader flags. The optional field,
command enum and command-specific tests are removed by jobs profile markers;
the loader-only parser remains valid. `serde_json` is a direct dependency
inside the existing jobs dependency marker. Only worker operator formatting
uses it; no serde dependency or serialization framework is added to the public
DTO solely for tests. The command runner consumes its `OperatorCommand` once,
calls `service_config::load_jobs_operator`, invokes the adapter once and
returns a typed operator outcome to the central worker exit mapper.

## Files

Paths below are repository-relative. The responsibility reference supplies its
constraints, source evidence, proof owner and reopen condition above.

| Rust path | Responsibilities and present reason | Declarations/visibility | Call path and lifecycle/error owner | Allowed dependencies | Forbidden responsibilities |
| --- | --- | --- | --- | --- | --- |
| `crates/infra-jobs/src/attempt.rs` | R1, full attempt/bookkeeping custody | Private AttemptSlots, queue/batch guards and existing functions | Claim dispatch → supervisor → persist; owning future/drop retires work | Current std/Tokio/SQLx/infra-postgres only | CLI, config, new background lifecycle, independent permit pool |
| `crates/infra-jobs/src/engine.rs` | R2, one process sampler and peer name snapshot | Existing private Shared/Peer/start; no new public Engine API | Engine::new owns process duties; beside shares; task failure uses existing channel | Current maintenance/claim/Tokio only | Fleet discovery, operator config, table-specific recovery |
| `crates/infra-jobs/src/maintenance.rs` | R2 and R3, completed-only retention, sample and reusable session check | Existing private maintenance functions; crate-private pool-based session check | Engine and operator call fact check; sampler publishes only after full decode | Current SQLx/infra-postgres/metrics/Tokio | CLI formatting, transaction commit policy, unknown-kind metric labels |
| `crates/infra-jobs/src/operator.rs` (new) | R3, supported safe query and recovery seam | Public validated request/result/error types and functions described above; private SQL/decoders | Worker → shared Tx → adapter; no tasks, no commit/retry | Existing infra-postgres, SQLx macros, UUID, std; serde_json only for internally needed representations | Credentials/config, clap/exit codes, direct broker/business effects |
| `crates/infra-jobs/src/lib.rs` | R3, expose the operator namespace | `pub mod operator` and crate docs | Provider API only | Existing module graph | Runtime composition |
| `crates/config/src/jobs.rs` | R4, narrow operator snapshot beside jobs capability | Public JobsOperatorConfig, crate-private validate; existing JobsConfig unchanged | Loader → typed postgres-only config | Existing PostgresConfig/serde/validation | Provider connections, CLI command types, queue SQL |
| `crates/config/src/load.rs` | R4, shared source load with narrow decode | Public load_jobs_operator and crate-private testable environment variant | Existing merge/pre-scans → projection validate | Existing config/serde/std | New source parser or bypass of secret scan |
| `crates/config/src/lib.rs` | R4, profile-owned exports | Add exports to existing jobs marker | Consumers see typed projection and loader | Existing module graph | Worker mode dispatch |
| `crates/jobs-worker/src/cli.rs` (new) | R5, binary syntax/help/usage | Private or pub(crate) WorkerArgs/OperatorCommand | lib::run parses once before config; jobs markers remove commands | Existing workspace clap, LoadOptions, operator request constructors where retained | SQL, provider connection, copying secret values into diagnostics |
| `crates/jobs-worker/src/operator.rs` (new) | R5, one-shot resource composition and safe receipt | Private runner/outcome/formatting; no public administration API | lib dispatch → config → signals → pool/history/session → shared Tx → close/runtime; result maps centrally | service-config, infra-jobs, infra-postgres, migrate, Tokio, existing Signals, serde_json/secrecy if required for Dsn | Handler registration, normal worker bootstrap, NATS/listeners/exporters, raw queue SQL |
| `crates/jobs-worker/src/lib.rs` | R5, dispatch and exit-code ownership | Private modules, existing public run/Registration unchanged, central mode result mapping | Existing run branches before start; ordinary start/exit semantics preserved | Current composition deps plus cli/operator | Queue policy, duplicate parser, process::exit |
| `crates/jobs-worker/tests/process.rs` | R5 proof of actual shipped parser/mode and unchanged default | Black-box tests under existing jobs markers | Built binary public surface; no production helper | Existing dev deps; add a direct existing workspace dev edge only for a used proving capability | Queue internals or a fake worker production path |
| `test/tests/jobs/operator.rs` (new) | R3/R5 proof through real persistence and command boundary | Private module within existing jobs integration target | Existing per-test database, Tx, shipped/fixture process harness as appropriate; every task/process joined | Existing integration-tests dependencies and fixtures | New fault-proxy/server framework, test-only production API |
| `test/tests/jobs/main.rs` | R3 proof module registration | Private `mod operator` | Existing jobs test target | Existing target dependencies | New validation target or duplicated suite |
| `test/tests/jobs/execution.rs` | R1/R2 proof of custody/retention and stale completion interaction | Existing tests and shared fixture calls | Existing Engine+real PostgreSQL suite | Existing test dependencies | Mirroring private choreography without observable invariant |
| `test/tests/jobs/process.rs` | R2/R5 proof of sampler/freshness/process integration | Existing process proofs | Existing jobs fixture; no second worker implementation | Existing test dependencies | New runtime profile |
| `crates/infra-messaging/src/outbox.rs` (tests only, if its current colocated proof is the smallest missing boundary) | B4 parity proof under R3, immutable prepared identity | Existing private test module only; no production change expected | Existing prepared intent/publisher contract | Existing dev dependencies | Recovery SQL or alternate publication route |

No change to `claim.rs` is required by the selected shape: the permit tuple is
wrapped at the supervisor boundary. If Rust's actual caller ownership forces a
signature-only transfer, `claim.rs` may receive that mechanical R1 edit without
a new mechanism; record it here before final review. No change to `kind.rs`,
bootstrap.rs, shutdown.rs, migrate Rust, infra-postgres Rust or production
outbox logic is anticipated. Add one only with a concrete discovered need and
the smallest owner reopen; avoid widening APIs to manufacture proof seams.

## Non-Rust and generated file authority

- `crates/jobs-worker/Cargo.toml`: the existing clap and serde_json edges above;
  Cargo.lock is regenerated only if those package-edge changes require it.
- `migrations/<new UTC version>_add_background_job_recovery_history.sql` and
  `<next UTC version>_index_failed_background_jobs.sql`: ordered naming rule
  from rollout, source schema authority; migrations/README.md documents them.
- `.sqlx/query-*.json`: generated metadata for changed/additional checked
  statements, never hand-edited; remove superseded query metadata through the
  existing prepare owner.
- `scripts/lib/template_profiles.json`: remove both migrations, operator.rs
  and operator-only proof when jobs is unselected; register new config/lib/CLI
  markers. `cli.rs` and its clap dependency remain for messaging-only workers.
  Existing removal of jobs-worker as a whole when no retained worker capability
  still applies. Outbox retains jobs, so its recovery path stays present.
- `scripts/tests/template-profile-projections.py`: extend existing structural
  expectations only where new removal facts are otherwise unproved. Existing
  initializer matrix owns compilation of affected source graphs; do not add
  another repeated whole-workspace matrix.
- `docs/background-jobs.md`, `docs/postgres-transactional-outbox.md`,
  `docs/architecture/{async,runtime-lifecycle,persistence,boundaries}.md`,
  `docs/configuration-source-policy.md`, and affected crate rustdoc comments:
  R7 exact semantic corrections. Add the operator subsection under jobs-owned
  markers; the default service/migrate CLI remains loader-only.

Any additional Rust test file must follow the evidence-bounded rule: extend an
existing owner first; a new file is justified only for a separate public
process/provider surface already named above. Implementation chooses concrete
tests and commands; this map assigns proving ownership without a test-plan
approval gate.
