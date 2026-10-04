# Ownership map V1

Status: ready, part of the reviewed [Technical Design](design.md).
[Dependency custody](dependency-custody.md) owns the portable source choice.
The application facade, custom cleanup tracking and public type migration from
the superseded option are removed. Existing crate boundaries remain.

## Responsibilities

| Responsibility | Current evidence and affected path | Semantic owner / exact action | Dependency/composition/generated boundary | Cleanup | Proof owner | Reopen condition |
| --- | --- | --- | --- | --- | --- | --- |
| **Bounded native return** | SQLx return retains its Floating size guard across unbounded callback/ping/close; published source and causal probe are recorded | SQLx library mechanism, with template dependency owner carrying one isolated backport in vendor/sqlx-core/src/pool/connection.rs | Same PgPool/PoolConnection/Executor/Tx driver; no app return task or wrapper | Whole-return timeout drops native owned future; native refill remains | Existing real-DB pool owner test/tests/postgres.rs | Changed source/drop semantics or failed release/reuse proof |
| **Dependency source custody** | Published sqlx-core 0.9.0 archive/checksum, upstream-aligned patch and current locked graph | Root Cargo.toml/Cargo.lock; vendor/sqlx-core/PATCHES.md; checksum verification and bounded source-only lock projection | Excluded dependency, not workspace application crate; unchanged versions/features/transitive graph | Remove vendor/path patch when a suitable official release proves equivalent behavior | Locked metadata/graph/build and dependency gate; source identity proof | Any change beyond two lock source fields and isolated source patch |
| **Portable delivery and profile containment** | .dockerignore allowlist, cooked stage has only recipe; chef0.1.78 skeletonizes workspace packages | .dockerignore, build/docker/Dockerfile, scripts/lib/template_profiles.json, scripts/ci/changed-surfaces.sh | Real excluded vendor copied before cook; PostgreSQL selection owns all references; existing CI gates selected | Absent profile removes vendor/patch/exclude/COPY/allowlist; upstream retirement removes exceptions | Existing profile projection and runtime-image owners, classifier self-test | Dummy vendor, missing cook source, absent-profile residue or ungated source family |
| **Acquisition observation** | observe.rs transaction histogram; named raw/implicit acquire callers in design table | infra-postgres observe.rs adds shared private future observer and public acquire helper; lib.rs reexports helper; pool.rs owns slow threshold/configures native acquire logs Off | Native futures/results/connection ownership unchanged; no new metric, Pool or Executor type | One named-path event owner, no duplicate attempt accounting | Focused event proof and saturation observation | Missing named caller, leaked field, duplicate counting or altered error/cancellation meaning |
| **Callsite coverage** | connect, session verification, in_tx_with, readiness, migration history, jobs claim/completion/startup, idempotency startup | Their existing files replace only acquisition calls or move implicit acquisition inside the existing observed future | Existing errors, query strings, deadlines, Tx finality and operation durations preserved | No duplicate acquire/observer inside an already acquired connection or Tx | Existing crate tests plus primary PostgreSQL observation owner | Any callsite needs a new semantic/lifecycle owner |
| **Operational guidance** | Persistence budgets/readiness/transactions/supported deployments and existing validation owners | docs/architecture/persistence.md and docs/validation/postgres.md record bound, source retirement, diagnostic coverage and complete sizing/recovery method | Current config/defaults and optional profile markers remain authoritative | Remove superseded unbounded-return statements | Static arithmetic/consistency, docs-check; real-DB R4 evidence | Behavior, capacity or readiness change -> Specification |
| **Acceptance evidence** | Existing postgres.rs silence/commit fixtures and health policy proof | test/tests/postgres.rs and existing support/commit_proxy.rs for material additions; focused observer tests beside infra-postgres owner | No test-only production constructor/hook; no fixture type migration needed | Disposable design target already removed; retained probe is historical feasibility only | Implementation final-validation owner | Wrong negative control, missing real server or unjoined fixture work |

Selected reuse rung for bounded return: use the existing maintained SQLx
mechanism and backport the narrow upstream fix where ownership already lives.
The strongest rejected source is supported PoolOptions hooks: they cannot
bound the final native ping or all early close branches. The next viable
library alternative, generic Deadpool, is rejected on total lasting lifecycle
integration cost, not because it requires replacing SQLx. Source/version,
archive identity, parity and retirement are fixed in dependency-custody.md.

Parity means native pool maximum and reuse, ordinary query/stream results,
transaction finality, admitted session budgets, password refresh, lifetime/idle
policy, close outcome and optional-profile behavior. The only accepted
runtime deltas are bounded return and specified acquisition diagnostics.
There is no new general library/dependency feature or operator configuration.

## Files

| Rust path | Responsibility | Present reason / declarations and visibility | Call-path and lifecycle/error ownership | Allowed dependencies / forbidden responsibilities |
| --- | --- | --- | --- | --- |
| vendor/sqlx-core/src/pool/connection.rs | Bounded native return | Upstream dependency implementation; internal timeout constant and existing return branch only; no new public API | SQLx owns Floating, semaphore, ping/close and refill; expiry drops owned future | Existing upstream runtime/tracing; no template or business dependency |
| crates/infra-postgres/src/observe.rs | Acquisition observation | Public acquire returning native PoolConnection; private shared observer for acquire/connect; existing histogram constants unchanged | Measures native future and logs fixed outcome; result/cancellation unchanged | Existing sqlx/Tokio/tracing; no connection-state policy or metrics duplication |
| crates/infra-postgres/src/pool.rs | Acquisition observation; Callsite coverage | Existing constructor/session owner; code-owned one-second threshold and configured native log levels | connect_with observed in place; verify_session explicitly acquires inside existing observed duration | Existing provider deps; no lazy public factory, pool wrapper or new cleanup task |
| crates/infra-postgres/src/lib.rs | Acquisition observation | Reexport acquire helper; PgPool remains native SQLx alias | Existing crate entry points and documentation | No replacement pool/Executor contract |
| crates/infra-postgres/src/transaction.rs | Callsite coverage | Existing in_tx_with calls acquire observer; existing Tx, guard and TxError declarations remain | Keep pending-BEGIN close_on_drop and finality logic untouched | Native pool and observer; no custom checkout lifetime |
| crates/infra-postgres/src/probe.rs | Callsite coverage | Existing PostgresProbe uses observed acquire | Same shared pool, ping and ProbeError under health deadline | Existing health/sqlx/adapter; no readiness policy or private connection |
| crates/migrate/src/lib.rs | Callsite coverage | Existing verify_history uses observer; migration runner unchanged | Existing history deadline and error mapping | Existing infra-postgres; no schema/migration source changes |
| crates/infra-jobs/src/claim.rs | Callsite coverage | send_claim uses observer | Existing engine backstop and Acquire error class; private LISTEN policy untouched | Existing provider deps; no claim/retry/concurrency change |
| crates/infra-jobs/src/attempt.rs | Callsite coverage | write_batch and send_outcome use observer | Existing acknowledgement/finality/deadline owners remain | Existing provider deps; no new completion/reconciliation policy |
| crates/infra-jobs/src/maintenance.rs | Callsite coverage | check_startup uses observer | Existing startup validation; maintenance transactions inherit in_tx observation | Existing provider deps; no repeated observer around Tx work |
| crates/infra-idempotency-store/src/maintenance.rs | Callsite coverage | check_startup explicitly acquires within its existing observed future | Preserve direct SQL, timeout, writable check and error mapping; retention already uses in_tx | Existing provider deps; no identity/replay/finality changes |
| test/tests/postgres.rs; test/tests/support/commit_proxy.rs | Acceptance evidence | Primary permanent pool/server proof and existing transport fixture if needed | Negative control, finite fixture lifetime and observable recovery/finality | Test dependencies/harness only; no production API for tests |

Focused observer tests may stay in observe.rs's existing cfg(test) module.
No consumer fixture signatures or raw SQLx test-administrator aliases change.
No source edit is needed in credentials.rs, idempotency attempt.rs, inbound
webhooks, service/worker bootstrap or health merely because their runtime
uses the patched library; existing types and boundaries already route them.

The exact non-Rust owner map is in dependency-custody.md. It includes the
vendor payload/provenance, Cargo.toml/Cargo.lock, .dockerignore/Dockerfile,
profile inventory, classifier and documentation. The published vendor's other
Rust files are copied unchanged third-party source, not application-owned
reimplementations. No template-owned generic vendor framework is created.

Root deletion check: removing the upstream return deadline restores the
reproduced library defect. Removing source/Docker/profile custody makes the
same patch nonportable or unreviewable. Removing the ordinary acquire observer
loses the required non-transaction outcome evidence. A new Pool wrapper,
streaming dependency, TaskTracker or consumer type migration has no present
responsibility and is excluded. Several existing provider crates receive a
mechanical helper call; the ownership review is limited to this map and the
new dependency-source containment, without reopening their business designs.
