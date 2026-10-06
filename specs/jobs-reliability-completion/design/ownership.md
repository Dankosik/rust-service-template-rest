# Ownership map

Status: ready. Independent [Technical Design Review](../technical-design-review.md): PASS.
Owner: Technical Design. Consumed with [system design](system.md).
This is an extension of current owners, not a new runtime subsystem. Placement
is mechanically closed after choosing the existing integration-fixture carrier;
there is no new production crate, cross-crate responsibility transfer or public
runtime interface. `rust-coder`'s earliest owner and `rust-structural-quality`'s
deletion test select the map below. Independent Technical Design Review covers
the combined decision; a separate Rust placement panel is untriggered.

## Responsibilities

| Responsibility | Affected path / current evidence | Semantic owner and exact action | Dependency/composition/generated boundary | Cleanup | Proof owner | Reopen condition |
| --- | --- | --- | --- | --- | --- | --- |
| Handler lifetime | `attempt::drive`, `run_attempt`, current Pending-drop gap | `infra-jobs`: private handler owner and destruction observation in `src/attempt.rs` | std unwind + existing futures-util; unchanged Handler API, no unsafe/new crate | Destroy once within existing attempt lifetime, then normal fenced persistence or uncertainty | Owner-local proof and existing `test/tests/jobs/execution.rs` | Needed behavior changes deadline/result precedence |
| Supervisor custody | `Engine::start`, claim dispatch, existing engine guard/Started.failed and worker readers | `infra-jobs`: carry the per-start stop/failure capability through `src/claim.rs`, arm retirement at submission, implement guard beside existing lifecycle ownership in `src/engine.rs` or `src/attempt.rs` according to which owns its held state | Private plumbing only; no new public failure type, task registry or borrowed capability escaping start | Report unexpected termination and stop claim admission; tracker still owns join | `infra-jobs` and existing jobs process proof; worker source repair only for a demonstrated reader gap | Existing worker reader cannot preserve primary/degraded distinction |
| Ownership observation | Existing queue metrics omit actual `AttemptSlots` / completion membership | `infra-jobs`: two label-free process gauges beside current attempt metrics in `src/attempt.rs`, updated by slot and completion ownership transitions | Existing metrics facade only; no public state API or separate sampler | Final retirement/unwind decrements ownership; scrape is observation, not enforcement | Owner-local structural assertions plus reference process samples | Instrumentation would require a new registry or change release ordering |
| Reference business | Existing `test/src/jobs.rs` shares kinds with a real fixture binary | `integration-tests`: `test/src/reading_counter.rs` owns operation, acceptance, marker/aggregate recipe and job registration | Test/recipe owner may use existing provider APIs; no production feature crate imports a provider; retained-profile markers protect compilation | Its callers own admitted pool and commit fate | Existing jobs integration area | Reference requires an unavailable public mechanism or production application contract |
| Independent receiver | Existing outbox consumer and outbound webhook fixtures | `integration-tests`: `test/src/reading_counter_receiver.rs` owns real NATS/HTTP receipt and marker/counter readback against a separate DB | Existing messaging and test-only webhook transport, no transport mocks claiming durable effects | Receiver process owns listener, consumer, pool and joined shutdown; driver retains process custody | Combined reference proof | External truth cannot survive producer restore |
| Reference composition | Existing `test/src/bin/jobs_worker_fixture.rs` is a non-shipped worker binary | `integration-tests`: `test/src/bin/reading_counter_fixture.rs` owns local command dispatch and invokes actual `jobs_worker::run` in worker mode | Finite accept/read/worker/receiver modes; no public template endpoint or image addition | One-shot commands close their pools; process modes retain native lifecycle ownership | Black-box scenario driver | Need for another process/runtime framework |
| Business schema | Existing `test/fixtures/migrations/` authority | `test/fixtures/migrations/reading_counter/`: first fixture migration creates requests, separate effects and article aggregates, including receiver channel scope | Not embedded by template `migrate::MIGRATOR`; derived service owns installed reference fixture/migration history | Disposable DB only; no runtime auto-migration | Actual Tx/backup/restore proof | Schema must become canonical shipped business state |
| Runtime proof | Existing jobs suites, commit/autocommit proxy, outbox ACK relay and lifecycle helpers | `test/tests/jobs/reliability.rs` holds combined scenario integration; `main.rs` wires its retained-profile module; share existing seams only if their second caller needs it | Reuse exact library behavior and existing real PG/NATS profiles; fixture fault control never enters shipped worker | Every child/proxy/task and held connection has a release and join owner | Implementation final validation | Fixture shortcut substitutes for real crash, restore or effect |
| Derived adoption and observation | `scripts/tests/template-sync-canary.py`, `scripts/ci/test-integration-db.sh`, Compose helper | `scripts/tests/jobs-reliability-reference.py` owns deterministic reference installation, exact revisions, source patch adoption, backup command orchestration, process sampling and receipt assembly | Existing initializer/sync/Compose/lock; additive service-owned fixture material only; no generic runner | One disposable resource manifest and teardown; failed result retained before cleanup | Source-only reference run and initialized representative | Requires uncontrolled runtime copy, unavailable tool or authority |
| Documentation and selected execution | Current jobs/async/outbox/webhook guides, test README, integration CI owner | Amend relevant guides and source-only integration invocation; carry actual commands/receipts into final Completion | Existing classifier/gates; source-confirmed HeaderValue prose fix only | No extra release or per-task gate | Static link consistency; selected command/CI result | CI would run full exercise once per matrix/harness |

For handler/supervisor changes, reuse rung is current owner + standard library
and resolved Tokio/futures utilities. The strongest rejected source is a new
Tokio JoinSet/per-handler task owner: it adds scheduling and result custody
without a missing capability. Parity proof is unchanged precedence, admission,
fenced finality and process failure policy plus the narrow gap regressions.
Upgrade condition is a changed independent task-lifetime requirement. The
reference uses already-installed APIs and PostgreSQL's own dump/restore tools;
no maintained external application framework solves its service-specific
operation identity, receipt and revision-adoption policy more simply.

## Files

The added paths below are selected artifact destinations; they do not yet exist.
Implementation may split a cohesive fixture only when a real independent owner
emerges and updates this map, not for a file-size target. Production ownership
must not change through that mechanical refinement.

| Rust path | Responsibilities / present reason | Declarations and call-path role | Lifecycle/error owner | Allowed dependencies | Forbidden responsibility |
| --- | --- | --- | --- | --- | --- |
| `crates/infra-jobs/src/attempt.rs` | Handler lifetime; ownership observation; supervisor retirement if its guard holds attempt-local state | Private safe handler holder, attempt outcome/persistence flow; existing exports unchanged | Attempt and shared completion registration | Existing infra-jobs graph | New budgets, DB readback replay, generic executor |
| `crates/infra-jobs/src/engine.rs` | Supervisor custody: existing start owns failure latch and guard vocabulary | Private guard/capability plumbing; `Started.failed` doc includes attempt supervisor | Engine start, native worker failure consumers | Existing graph | Global shared failure policy, unbounded results |
| `crates/infra-jobs/src/claim.rs` | Supervisor custody: acknowledged claim handoff arms retirement | Private dispatch plumbing, no SQL/schema change | Issued claim then admitted supervisor | Existing graph | Releasing acknowledged claims outside attempt owner |
| `test/src/reading_counter.rs` | Reference business | Fixture-visible payload, acceptance/readback, Tx recipe, registration | Caller Tx and job fencing | Existing integration-test provider/serialization deps | Template runtime business migration, general dedup framework |
| `test/src/reading_counter_receiver.rs` | Independent receiver | Fixture-visible receiver start/readback, fixed channel handlers | Receiver owns committed marker vs ACK; bounded joined runtime | Existing integration-test messaging, HTTP fixture, PG deps | Producer snapshot truth, ACK-before-commit, remote destination policy |
| `test/src/bin/reading_counter_fixture.rs` | Reference composition | Private entrypoint, finite CLI dispatch; actual worker calls existing runner | Mode-specific caller or existing worker lifetime | `integration-tests`, existing jobs-worker/config/runtime crates | Shipped public API, replacement lifecycle abstraction |
| `test/src/lib.rs` | Reference business/receiver inclusion | Profile-gated fixture modules only | No task spawn or I/O | Existing graph | Production registration |
| `test/tests/jobs/reliability.rs` | Runtime proof | Black-box scenario/recipe proof selected under retained requirements | Driver's child/proxy/pool owners | Existing fixture and real provider dependencies | New business implementation duplicate, production fault knobs |
| `test/tests/jobs/main.rs` | Runtime proof inclusion | Correct profile-gated module inclusion | None beyond suite | Existing graph | Full matrix orchestration |
| `test/tests/jobs/execution.rs`, `test/tests/jobs/process.rs` | Narrow existing proof extension if necessary for R1 | Executor chooses sufficient regressions at existing surface | Existing engine/process fixture owners | Existing graph | Repeated copy of adequate baseline tests |

The only conditionally touched production worker files are
`crates/jobs-worker/src/bootstrap.rs` or `src/shutdown.rs`, and only if proof
exposes a concrete existing failure-reader gap. Their established responsibility
remains process failure custody; no proactive refactor is selected.

Non-Rust carriers follow the same responsibilities: `test/Cargo.toml` declares
the test-only binary and exact retained-profile closure; fixture SQL owns only
educational tables; the reference Python driver owns its orchestration; existing
CI/command/classifier files may add one source-only invocation. No version bump
or dependency upgrade is implied by moving an already-required fixture
dependency into the binary's appropriate manifest section. Any genuine new
crate/feature dependency reopens the dependency owner before admission.

In the initialized service the installed `test/` reference and schema, local
architecture/skill customization, Cargo declarations, preserved DB data and
adopted runtime source are service-owned. The exact baseline-to-candidate patch
updates only the selected runtime files after portable sync. The driver records
installed and preserved hashes so ownership is independently reviewable.
