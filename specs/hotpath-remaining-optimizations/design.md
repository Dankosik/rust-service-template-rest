# Technical Design: remaining hot-path optimizations

Status: ready. Independent [Technical Design Review](technical-design-review.md): PASS.

Authority: [ready specification](spec.md), blob
`82c39b3adfa32073c719dc70513c39b171b2dea8`, and [Definition transition](definition-transition.md).
This design selects mechanisms and proof boundaries for all four areas. It
does not claim a measured optimization or complete Implementation. Root owns
the approved machine and immutable evidence custody in [operations](operations.md).

## Decisions and coverage

| Area | Selected candidate or bounded disposition | Adoption evidence |
| --- | --- | --- |
| Payload serialization | Stream the three Base64 fields of `Incoming` through existing Serde/Base64 APIs; retain generic jobs preparation and duplicate bypass. | Total allocated bytes per newly admitted delivery, allocation count and CPU separately; exact pre-JSONB byte parity and ordinary-release controls. |
| Span creation | Reuse static method/protocol values and evaluate display formatting of the existing span name without its eager temporary String. | Separate `make_span` allocation attribution and exact span/log parity; SQL-free ordinary HTTP CPU, latency and throughput. Name formatting is independently removable if it fails. |
| HTTP metric recording | Obtain numeric status strings from a fixed static table of the existing `StatusCode` type. | Exact removal of per-record status ownership plus metric attribution and ordinary HTTP controls; every legal numeric status retains its exact label. |
| SQL exchanges | Preserve the transaction, receipt, enqueue and wake sequence. Reject the concrete fusion/bypass candidates below on invariant or supported-API evidence; finish with a bounded baseline protocol observation before the area's final no-supported-optimization disposition. | Actual warm new-delivery, debounced and duplicate exchange counts, new-delivery latency, pool wait and connection occupancy; disclose unchanged SQL and absence of a SQL speedup. An unexplained removable exchange reopens this design. |
| Pool sizing | Compare budget-admissible 4, 8 and 16 connections in the existing synthetic topology, using the existing configuration key. Select the smallest measured useful value. | Explicit connection-budget arithmetic, actual settings/topology, retained mixed 2,000 RPS pool wait, p95 and useful throughput, database/service CPU and RSS. |

The two HTTP rows are independently dispositioned subparts of one required
observability area. The SQL row is deliberately an investigated area with
rejected concrete mechanisms, not an optimization credited to another row.
Every candidate still uses the specification's adopted/rejected/no-supported/
blocked definitions. A blocked evidence row cannot close the task.

## Current source and version custody

Source inspection on 2026-10-04 used the current dirty checkout. The accepted
body transfer is already present and remains the starting behavior.

| Source | Git blob inspected |
| --- | --- |
| `crates/infra-webhooks/src/inbound.rs` | `ad981d30e1590dcb6ea0b31842f633b288626965` |
| `crates/infra-jobs/src/enqueue.rs` | `a1da3f1b40051dfc4c3c0347d9622a69905467ef` |
| `crates/infra-http/src/observe.rs` | `4cf33813623f368e62aaad0424d00719af62f7b8` |
| `crates/infra-postgres/src/transaction.rs` | `63350438ed9e4ce314ad24ec7b456f40e82ef8d7` |
| `crates/infra-postgres/src/pool.rs` | `37ceb0443f1d083fec732b142f942769a22ca4a7` |
| `Cargo.lock` | `981eb9ec797d1d23d6d6effabc21d5a100ede398` |

These individual identities support source decisions. They do not replace
root's complete frozen source archive, configuration/feature identities and
binary hashes for comparisons. No implementation writer starts until that
baseline custody is complete. A source change in one of these mechanisms
requires its affected design assumptions to be checked again.

Resolved mechanisms were checked against installed crate source and primary
library documentation: `base64` 0.23.1 for this webhook path, `serde_with`
3.23.0, `serde` 1.0.229, `serde_json` 1.0.151, `http` 1.5.0, `axum` 0.8.9,
`metrics` 0.24.6, `opentelemetry` 0.33.0, `tracing-opentelemetry` 0.34.0,
`tracing-opentelemetry-instrumentation-sdk` 0.42.1 and `sqlx` 0.9.0.
No dependency, feature, toolchain, allocator or release-profile change is
selected. The other base64 version in the lockfile is not this adapter's API.

## Payload mechanism and flow

Current `serde_with::Base64` serialization materializes an encoded String
for each Base64 field. Replace only the serialization side of `Incoming`'s
`message_id`, `body` and optional `content_type` with one private adapter in
`inbound.rs`, implementing `SerializeAs<T>` for `T: AsRef<[u8]>`. Its mechanism
is `Serializer::collect_str` over `base64::display::Base64Display` using
`STANDARD`. Keep existing `Base64` deserialization through separate
`serialize_as`/`deserialize_as` annotations, derived struct serialization,
field order and the current version field.

The supported APIs are [Base64Display](https://docs.rs/base64/0.23.1/base64/display/struct.Base64Display.html),
[Serde collect_str](https://docs.rs/serde/1.0.229/serde/trait.Serializer.html#method.collect_str),
[serde_json serializer](https://docs.rs/serde_json/1.0.151/src/serde_json/ser.rs.html),
and [serde_as split annotations](https://docs.rs/serde_with/3.23.0/serde_with/attr.serde_as.html).
The JSON serializer overrides `collect_str` and escapes formatted fragments
into its existing output buffer. This is library reuse, not a custom Base64
encoder or hand-written JSON representation.

The material path remains HTTP bounded body -> owned `Bytes` -> authentication
over the original bytes -> READ COMMITTED receipt insert -> only the winner
constructs `Incoming` -> generic jobs preparation -> job insert and applicable
wake -> caller transaction commit -> 204. A duplicate is authenticated and
returns 204 through the receipt branch without invoking `Incoming` serialization.
Borrowed callers keep their current body-transfer behavior. The signature
vector and protocol implementation remain those already owned by the webhook
tests and `standard-webhooks`; no signing algorithm or input changes.

For each encoded nonempty field of length `n`, current `Engine::encode`
requests `4 * ceil(n / 3)` temporary bytes. The candidate removes that one
temporary allocation per field. This is an exact component-level expectation,
not total saved bytes: `Base64Display` writes 1,024-byte stack chunks, and
`serde_json::to_string` starts its output Vec at capacity 128. Changed buffer
growth and formatting can offset turnover savings or increase count/CPU.
Only a matched remote measurement can establish the net result.

There is no retained scratch pool, thread-local buffer, second serialization
pass, or public `JobKind` sizing API. All storage is request-owned and bounded
by existing body/header/identity/admission limits. `infra_jobs::prepare` keeps
kind -> unique key -> delay -> serialize -> serialized size -> decoded-NUL
ordering, the exact reported size, typed errors and generic serialization
semantics. Stored JSON-value meaning and exact serialized bytes before JSONB
normalization both remain obligations, including binary bytes, padded Base64,
and absent versus empty optional fields.

The strongest alternative is an advisory `JobKind` capacity hint plus a
pre-sized `serde_json::to_writer` output. It would address geometric growth,
but changes a shared public responsibility before growth is established as
the limiting cause. Reserving the generic maximum inflates small-job memory;
serializing twice changes custom serializer invocation semantics. Defer this
alternative. Reopen only if matched total allocation evidence defeats the
selected local adapter because output growth dominates; do not implement the
hint without that design reopen. If net benefit or parity fails, remove the
adapter and retain the baseline serializer.

## HTTP span and metric mechanisms

All production changes stay in `infra-http/src/observe.rs`; the existing
subscriber, tracing layer, hardened chain and metrics recorder stay their
owners. There are three independently attributable span changes:

1. Reuse the nine standard method names as borrowed `Cow<'static, str>` values.
   An extension method retains its original owned text in the span; `_OTHER`
   remains only the existing bounded metric-method label.
2. Pass `otel_http::http_flavor(request.version())` directly to `set_attribute`.
   It already returns `Cow<'static, str>` with static standard versions and
   an owned fallback. OpenTelemetry's `Value` conversion preserves that form.
3. Evaluate `%format_args!` for `otel.name` inside the existing span macro.
   Use the original method, the route with trailing whitespace trimmed, and
   a separator only when the resulting route is nonempty. This preserves
   the original whole-name `.trim()` result for valid method tokens and all
   matched/unmatched route strings. Keep the static span callsite name,
   `otel.kind`, field names and request ID. Every consuming subscriber may
   still allocate while formatting; application-temporary removal is not
   proof of lower total allocation or CPU. Restore this subchange separately
   if it fails parity or adoption, retaining other qualifying span changes.

For metrics, introduce a private static `[StatusCode; 900]` containing values
100 through 999, constructed at compile time through public
`StatusCode::from_u16`. A private status-label function indexes by
`status.as_u16() - 100` and returns `as_str()` from that static element.
The type already guarantees the index range. This uses the library's numeric
rendering with a valid static lifetime; it creates no digit-rendering copy,
unsafe lifetime conversion, heap initialization or request-driven cache.
Pass the returned static string to the existing `Label::new` instead of
allocating `Arc<str>` for the status. Keep the vector of labels, owned route
label, metric registration and histogram recording unchanged.

The [resolved status API](https://docs.rs/http/1.5.0/http/status/struct.StatusCode.html)
supports all 100..999 values and `const from_u16`; `as_str` is otherwise tied
to the receiver. The table is a bounded 900-value static cost (currently
1,800 value bytes, not a portable ABI promise). The selected static strings
remove the method/protocol temporary allocations for standard values and
the status allocation per metric record. The table does not change series
cardinality. No router catalog, intern pool, handle cache, lock or new layer
is needed.

Primary capability checks:
[OpenTelemetry Value](https://docs.rs/opentelemetry/0.33.0/opentelemetry/enum.Value.html),
[span extension API](https://docs.rs/tracing-opentelemetry/0.34.0/tracing_opentelemetry/trait.OpenTelemetrySpanExt.html),
[metrics source](https://docs.rs/crate/metrics/0.24.6/source/src/label.rs),
and [MatchedPath](https://docs.rs/axum/0.8.9/axum/extract/struct.MatchedPath.html).
The extension exposes individual `set_attribute`, not a supported bulk setter;
`metrics::IntoLabels` still materializes a Vec for arrays/slices; Axum does
not expose its underlying route `Arc`. Thus an array substitution is not an
allocation-elimination claim. A histogram-handle cache would introduce
capacity, synchronization and recorder-lifetime policy without necessary
evidence; reject it for this candidate. Reopen only if the remaining route/key
cost is material after measured static-value changes.

The observation path remains request span and extracted parent -> handler ->
Problem completion -> response-head duration and status -> access log and
span status -> active gauge retained until body drop. Preserve attribute
presence/absence, exact extension-method values, query redaction, request and
trace correlation, disabled-span fallback, sampling/export/filter decisions,
health-log default and refusal/error visibility. Exact exported values and
JSON log field values must survive the string-to-display visitor change.
Metric names, three labels, method normalization, units, histogram buckets,
response-head timing and active-body lifetime do not change.

## SQL alternatives and evidence closure

The decisive constraint is two pieces of information available only after
acknowledgement: receipt insertion determines whether serialization may run;
job insertion determines whether the per-kind wake debounce may be consumed.
The database owns durability, jobs owns insertion/validation/wake policy,
webhooks owns receipt identity, and `infra-postgres::in_tx_with` owns commit
classification and connection reuse. No extra retry is selected.

| Concrete candidate | Expected saving | Decisive contradiction or unsupported dependency |
| --- | --- | --- |
| Receipt + job in one checked data-modifying CTE | One new-delivery exchange | The client must serialize the bound job payload before it learns whether the receipt won. This adds preparation to duplicates, including simultaneous different-body duplicates. Prechecking receipt existence races; server-side reconstruction would replace the exact-byte generic jobs serialization owner. |
| Job INSERT + conditional `pg_notify` in one CTE | One exchange on eligible notifying enqueues | Choosing `wake_due` before INSERT lets a duplicate or failed insert consume the interval and suppress a later eligible wake. Checking without reservation lets concurrent requests notify together. A no-unique-key branch still has failed/in-flight insert reservation races. |
| Use a due-only per-kind async gate through combined insert/notify; Created advances time, fallback inserts then awaits the gate | Same eligible exchange | SQLx supports the statement, and the gate avoids false duplicate/error reservation. But it can remove B's otherwise eligible wake behind A's unrelated stalled insert, as the schedule below shows. Allowing a fallback to bypass the pending gate instead can permit competing notifications. This is a scheduling-contract rejection, not a syntax or measured-performance rejection. |
| Send NOTIFY + COMMIT in raw multi-statement SQL | One notifying exchange | This bypasses supported SQLx transaction commit bookkeeping and the adapter's commit-failed/unknown policy. SQLx checked prepared statements do not provide a parameterized multi-statement transaction-finalization API. Custom protocol/state coupling is not justified by this rare optional exchange. |
| Remove the pool-return ping through a SQLx hook | One post-transaction exchange | SQLx 0.9.0 runs `after_release`, then unconditional connection `ping`; `false` or error closes the connection. It is not a supported skip-ping control. `test_before_acquire(false)` is already selected and controls another point. A reconnect-per-request replacement is not a preserved pool/reuse mechanism. |
| Remove BEGIN/COMMIT, weaken commit durability, or omit wakes | Several exchanges or synchronous work | Violates caller-owned atomic receipt/job fate, acknowledged acceptance, or eligible wake/polling contract. Schema-side procedures/triggers and debounce tables would add excluded schema and new authority. |

Relevant primary contracts are SQLx
[PoolOptions hooks](https://docs.rs/sqlx/0.9.0/sqlx/pool/struct.PoolOptions.html),
[Transaction](https://docs.rs/sqlx/0.9.0/sqlx/struct.Transaction.html),
[pool release source](https://docs.rs/crate/sqlx-core/0.9.0/source/src/pool/connection.rs),
[PostgreSQL connection source](https://docs.rs/crate/sqlx-postgres/0.9.0/source/src/connection/mod.rs),
and PostgreSQL [NOTIFY transaction semantics](https://www.postgresql.org/docs/18/sql-notify.html)
and [data-modifying CTE visibility](https://www.postgresql.org/docs/18/queries-with.html#QUERIES-WITH-MODIFYING).
The installed SQLx source shows `return_to_pool` running the release hook then
`raw.ping()`, while PostgreSQL's `ping` sends Sync and waits for ReadyForQuery.
No dependency upgrade or fork is selected to reach private driver state.

The strongest surviving SQL-level alternative was the due-only gate. A
deterministic schedule falsifies its wake compatibility: let the last wake be
older than 25 ms. A takes the gate and stalls in its combined INSERT. B's
independent ordinary INSERT succeeds. In the baseline B can consume the due
wake immediately, notify and commit; in the candidate B waits for A's gate.
A eventually acknowledges Created and advances the timestamp, but its caller
has not committed yet. B resumes within 25 ms, suppresses its own notification
and commits. B's otherwise eligible committed work now waits for A's later
commit or polling. A's eventual rollback makes the loss even clearer.
Holding B's transaction through the gate also adds a dependency before B's
commit. Caller deadlines bound how long a request can wait; they do not make
this newly suppressed wake equivalent. This is a source-supported
counterexample under ordinary concurrency, not an observed throughput or
latency regression and not a prohibition on SQL fusion in general.

Supported unique-key callers expose an additional cycle: B begins its ordinary
insert while the interval is not due and owns key X; the interval expires
before B resumes. A takes the now-due gate and inserts X, waiting on B's
uncommitted row. B then waits for A's gate before returning to its caller's
commit. Deadlines can break that cycle only through a failure absent from
the original dependency graph. This also rules out treating the new gate as
merely a harmless bounded timing cost.

The candidate and fallback share a choice that cannot be repaired merely by
a timeout: waiting retains this dependency; letting B notify while A's already
submitted statement may notify risks multiple wakes in the interval; consuming
the slot before A's acknowledged creation restores the duplicate/error failure
above. A future supported way to cancel/withdraw the pending notification
without losing transaction outcome, or an explicitly changed wake contract,
would reopen this decision. No such mechanism is in the selected APIs.

Therefore no SQL production delta is admitted by this design. This conclusion
is bounded to the named admission path, preserved invariants, resolved driver
and these concrete candidate families, not a claim that databases cannot be
optimized. It cannot be reported as a measured SQL improvement.

Close the empirical uncertainty with a short protocol observation on root's
frozen baseline, after pool/statement warming: a new immediate delivery with
wake due, a new delivery inside debounce, and an authenticated duplicate.
Separate the request's BEGIN, receipt INSERT, job INSERT, optional NOTIFY and
COMMIT acknowledgement from the pool's return Sync/ping and any acquire-idle
ping or prepared-statement setup. The source expectation is `4 + W` sequential
request exchanges for new delivery (`W` is 0 or 1) plus one return ping, and
three request exchanges plus one return ping for a duplicate. These are
predictions to check, not measurements and not hotpath span counts.

Root chooses the supported capture on the disposable local PostgreSQL link;
retain a protocol/event trace with clear request correlation, synthetic input
and raw evidence, avoiding an extra production instrumentation layer. If the
protocol shows an unexplained redundant exchange or disproves a capability
assumption, reopen SQL design with that discriminator. Otherwise report
`No supported optimization` with these bounded rejection reasons and measured
baseline counts/costs. If required protocol evidence is unavailable, keep the
area blocked and return the exact missing observation to root, not a success.

Measure new-delivery latency separately from acquire wait. The existing
transaction timer starts before acquire, so it is not pure occupancy.
For means only, matched transaction-duration and acquire-wait sums/counts can
derive an acquire-adjusted transaction interval when their event populations
agree. That interval excludes the driver's asynchronous return ping: actual
slot reuse occupancy also includes return/release. Use protocol timings or
an explicitly bounded diagnostic to separate these; never subtract p95s,
add nested spans, or label transaction wall time CPU. Keep database CPU and
commit/wait observations alongside service cost.

All failure flows stay with the existing owners: validation refuses before
enqueue SQL; a failed closure rolls back; an insert conflict returns duplicate
only through the established identity; commit-failed/unknown retains its typed
classification, uncertainty maps to 503 and same-identity retry reconciles;
cancellation retains transaction/connection cleanup. The worker sees committed
jobs and commit-delivered notifications, with existing per-kind 25 ms debounce
and polling recovery. No transaction closure is automatically replayed.

## Budgeted synthetic pool selection

Let `M` be server `max_connections`, `Rdb` its reserved database/operator
slots unavailable to ordinary application traffic, `O` the peak allocated
connections of other workloads, and `Rop` an additional explicit operational
reserve. The available application budget is `A = M - Rdb - O - Rop`.
Do not subtract the same reservation twice. Budget maxima, not current idle
counts. For every allowed steady or overlapping rollout state require:

`sum(process_count_i * pool_max_i) + sum(dedicated_connections_i) <= A`.

Count old and new replicas during permitted overlap. A worker's direct LISTEN
connection is outside its pool and must be included; engines built beside
one another share that one listener. Ordinary worker pool minimum is
`jobs.max_workers + 2`; the retained combined jobs+outbox case needs `N + 5`,
and outbox-only needs three. The migrator's dedicated connection and any
simultaneous observer/admin workload also need an explicit budget location.
Readiness uses the shared pool; it is not an extra dedicated connection.
No current reserve or worker minimum is spent twice.

Root must record the actual synthetic server settings and process topology
before labeling any chosen pool admissible. Select an explicit synthetic
operational reserve of ten ordinary slots, in addition to PostgreSQL's
reserved slots; it is a conservative experiment allowance, not a universal
production default. Benchmark admission uses one service, no processing worker,
no rollout overlap and no concurrent build/test/migration. Count the bounded
monitor sessions under `O` (or a stated part of that reserve, once only).
Root can reserve one future/occasional migrator slot outside the active pool.
For example, **if** the observed server is `M=100`, `Rdb=3`, observers `O=2`,
`Rop=10`, the usable budget is 85 and `16 + 1 <= 85`; this is arithmetic on
assumed inputs until readback, not a report of current settings.

The candidates are 4, 8 and 16, filtered by the actual budget and current
configuration range 1..500. Preserve global default 4. Compare the retained
mixed new/duplicate/readiness 2,000 RPS workload with equal source, fixtures,
durability, CPU layout and observation settings. Choose the smallest candidate
that gives a reproducible useful improvement under the specification and does
not exceed the budget. If 8 and 16 are indistinguishable, choose 8. If none
qualifies, report the measured admissible baseline and the bottleneck; do not
recommend 16 from historical evidence. A source comparison uses equal pool
size on both sides, never baseline 4 versus code candidate 16.

This area produces task-local configuration and a report/sizing rule, not
changes to `crates/config`, default TOML, Railway settings or deployment code.
An actual production recommendation remains unknown until its server budget,
replicas, worker mix, overlap and other workloads are known. Reopen for changed
topology, pooler multiplexing, worker workload or measured database saturation;
more pool slots do not remedy a CPU/WAL/storage bottleneck by themselves.

## Ownership map

### Responsibilities

| Responsibility | Affected path and current evidence | Semantic owner and exact action | Boundaries and cleanup | Proof owner and reopen |
| --- | --- | --- | --- | --- |
| Incoming Base64 serialization | `Incoming` serde annotations and current temporary strings in `inbound.rs` | `infra-webhooks`: private SerializeAs adapter beside Incoming, serialization annotations only | Existing base64/serde_with/serde_json APIs; no jobs or schema ownership transfer; remove adapter if rejected | Existing webhook unit and `test/tests/webhooks/inbound.rs` proof; remote matched preparation allocations; reopen on byte/error difference or output growth defeating benefit |
| HTTP observation representation | `make_span`, `record`, `method_label`, existing telemetry tests in `observe.rs` | `infra-http`: private static method conversion/status table helper and local span construction changes | Existing tracing/OTel/metrics APIs; no subscriber/router/config/lifecycle ownership transfer; independently revert failing name formatting, remove rejected candidate code | Existing observe/harden/context/log/metric proof and remote HTTP comparison; reopen on parity or resource/benefit failure |
| SQL exchange investigation | Receipt winner branch; jobs enqueue/wake_due; Tx and SQLx release paths | Existing webhooks/jobs/postgres owners retained; task-local trace and decision only | No source delta, new insert API, SQL metadata, driver fork or schema; no legacy/parallel path added | Existing real PostgreSQL validation plus root protocol observation; reopen on unexplained removable exchange or new supported library capability |
| Synthetic connection budget and comparison | Existing postgres key and worker minimum/listener architecture | Task-local measurement/report owner, with root-only remote operations | Existing config input; no global key/default/range change; teardown through root operations record | Actual settings/topology, matched mixed-load measurements; reopen when budget/topology or comparison validity changes |

Reuse choice for payload is the declared serialization libraries' supported
display/annotation extension rather than a capacity API or custom encoder;
parity is exact JSON bytes and generic error order. Reuse choice for HTTP is
the standard library and existing public typed APIs rather than a cache or
new middleware; parity is the complete emitted signal meaning. Upgrade only
when a later accepted requirement or measured remaining cost justifies it.
Each responsibility has one present owner and one row above.

### Files

| Path | Responsibilities and present reason | Declarations/visibility and call path | Lifecycle/error owner; allowed dependencies; forbidden responsibility |
| --- | --- | --- | --- |
| `crates/infra-webhooks/src/inbound.rs` | Incoming Base64 serialization; this already owns the wire payload | One private adapter and serialization annotations; receive winner -> Incoming -> jobs enqueue | Request-owned temporary state; jobs retains preparation errors; existing Serde/Base64 only; no generic jobs policy or durable SQL change |
| `crates/infra-http/src/observe.rs` | HTTP observation representation; this owns HTTP span/metric emission | Private status table/label helper and method value helper as necessary; make_span/record remain existing private entry points | Existing request/body lifetime and failure observation; existing http/metrics/OTel/std only; no recorder installation, telemetry filtering or router catalog |

No new Rust file, module, crate, exported API or cross-crate responsibility is
selected. Existing in-file unit-test modules own any material parity gap;
existing `test/tests/webhooks/inbound.rs` owns a needed black-box database
gap. Implementation chooses concrete cases and reuses adequate coverage.
The ownership review uses the repository's complementary panel because two
crates are changed: responsibility/execution paths, package/dependency/visibility
and generated boundaries, and file cohesion/proof placement. The broader
Technical Design reviewer consumes those bounded receipts and checks the
remaining mechanisms and cross-flow coherence.

Comparison fixture/script adaptations belong under this task's `scripts/`
only when first needed, reusing the prior profiling/optimization tools. Do
not mutate historical evidence or executables to reinterpret old runs.
Task-local machine inputs and raw evidence belong with root's operations
custody. These artifact locations are not production abstractions.

## Claim-scoped execution and release boundary

Planning can order bounded candidates and comparisons from this design; it
must retain each area's explicit disposition. Required proof is factored,
not a product of every profile/database/load configuration. Reuse the ordinary
baseline/candidate release builds, matched feature-only hotpath builds and
existing load fixtures. Attribution can compare candidate deltas independently;
the assembled retained candidate is then compared with the original frozen
baseline to reveal interactions. Record exact source, build and config identity
for every such comparison. A rejected candidate is removed from the final
production delta, with its unfavorable evidence retained.

Use the existing retained new small/large, duplicate-only and mixed webhook
workloads, plus SQL-free HTTP. Large new deliveries distinguish serialization;
duplicates/small inputs constrain regressions. Span and metric scopes each
receive their own allocation/CPU disposition; ordinary process CPU per useful
response stays distinct. Repeated order/spread, useful response denominator,
errors/drops, CPU, p95, peak RSS, database cost and pool wait accompany primary
metrics. Preserve unsuitable runs with their externally observable exclusion
reason; keep valid unfavorable runs. No sum of nested allocations/latencies
or percentile subtraction supports a claim.

All execution, including executable reduction and documentation checks, occurs
only on root's approved DigitalOcean host. No build/test/load runs concurrently.
The existing [Rust validation](../../docs/validation/rust.md) owner selects the
matching build and tests; [PostgreSQL validation](../../docs/validation/postgres.md)
owns real-server proof for the claimed atomicity/locking/commit/wake path.
Reuse existing uncertainty/cancellation/first-winner cases, adding only a
material uncovered behavior if implementation exposes one. This design does
not create a separate test-planning phase or authorize new test infrastructure.
No changed SQL means no speculative SQLx metadata regeneration; if SQL design
reopens and admits a checked statement change, its generated metadata and
real-server proof join that changed surface.

The public contract, schema, JSON representation, worker semantics and
configuration default stay compatible; no migration, dual-format rollout or
legacy fallback is needed. Disabling retained profile code still leaves it
inert. Final independent Implementation Review considers the assembled change
and coverage of all four areas. CI, release, production SLO, production pool
application, long-term leak freedom, push/PR and deployment are outside this
result. Root retains cleanup and cost/time limits without delegation here.

Reopen Definition for changed meaning/invariants/adoption rules; Technical
Design for mechanism, interface or ownership changes; root for missing remote
evidence/authority. Stop this actor after reviewed Technical Design transition,
before Planning or source/test changes.
