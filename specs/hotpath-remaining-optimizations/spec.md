# Specification: remaining hot-path optimizations

Status: ready. Independent [Specification review](definition-review.md): PASS.
Authority: [Intent](intent.md). Evidence: [profiling](../hotpath-profiling/report.md)
and the accepted [body-transfer result](../hotpath-optimizations/report.md).

## Outcome and area coverage

Reduce avoidable service cost across all four remaining areas. Technical Design
owns mechanisms, API shape, placement and comparison details. Each area must
receive a bounded investigation, a concrete candidate or an evidence-backed
reason no candidate is viable, and a final disposition under the rules below.
An optimization in one row cannot silently substitute for another row.

| Area | Required outcome and primary observable |
| --- | --- |
| Deployment-specific pool sizing | Produce a usable sizing rule and a concrete, measured configuration for the synthetic deployment. Account for the database's available application connections, all replicas/process pools, worker requirements, dedicated connections, rollout overlap, and explicit operational reserve. Report pool wait, p95 latency and useful throughput at the retained mixed 2,000 RPS point. The prior 4-versus-16 result is a starting hypothesis; 16 is not universally prescribed. |
| SQL round trips | Reduce sequential database exchanges on new webhook admission where a safe measured candidate exists. Count actual exchanges rather than treating the partial hotpath SQL count as the full protocol. Report new-delivery latency and connection occupancy separately from acquire wait, along with database/service cost. Receipt, job, commit and worker wake semantics remain governed below. |
| Payload serialization | Reduce measured allocated bytes per new delivery during payload serialization/preparation, reporting allocation count and CPU separately. Preserve the accepted body-transfer optimization. Large payloads are the primary discriminator; small and duplicate requests constrain regressions. |
| Span and metric overhead | Reduce measured allocations or CPU in request span creation and HTTP metric recording while preserving operator-visible meaning. Disposition both span creation and metric recording even if only one yields a useful change. Ordinary HTTP CPU per useful response is distinct from instrumented function costs. |

For pool sizing, let the deployment's usable connection budget exclude reserved
database/operator slots and other workloads. The sum of maximum concurrent
application pools and dedicated connections, including permitted deployment
overlap, must fit the remainder. Worker minimums and shared-pool readiness
behavior stay in force. Unknown deployment inputs are labeled unknown; a
synthetic example may not be advertised as a ready production value. Keep
`postgres.max_connections` default 4 and its existing supported range and
configuration precedence. Current Railway authority is read-only and does not
authorize applying the result there. A measured synthetic configuration and
an actionable budget-based sizing rule close this area within current scope;
production application is explicitly excluded, not silently marked done.

## Preserved behavior and truth

Current source fixed in the pre-edit baseline and its contracts are canonical:
[HTTP admission](../../crates/infra-http/src/webhooks.rs),
[inbound webhook](../../crates/infra-webhooks/src/inbound.rs),
[enqueue](../../crates/infra-jobs/src/enqueue.rs),
[HTTP observation](../../crates/infra-http/src/observe.rs),
[inbound contract](../../docs/inbound-webhooks.md),
[persistence](../../docs/architecture/persistence.md), and
[jobs](../../docs/architecture/async.md).

| Trigger / surface | Required result and forbidden divergence |
| --- | --- |
| New valid webhook | Same status, headers/body, identity, receipt and job metadata, kind, consumer-visible body and content type. Acknowledged acceptance is `204` only after the atomic receipt+job commit. |
| Authentication or admission rejection | Verify the same original body bytes, with the same endpoint, key, timestamp, signature and parsing rules; preserve error identity and precedence. Rejected requests acquire no durable effect. Body, header, identity, serialized-payload, admission and concurrency limits remain effective. |
| Duplicate / simultaneous same identity | Authenticate every delivery; preserve exact endpoint/raw-ID deduplication, first winner, retention and `204` behavior. A later different body cannot replace the winner or create another job. Do not add payload serialization to the duplicate path that currently bypasses enqueue preparation. |
| Durable transaction | Keep receipt and job in the same caller-owned transaction, isolation and commit fate. No early success, weaker `fsync`/`synchronous_commit`, hidden retry, partial durable effect or automatic replay of the business closure after commit uncertainty. Known failures roll back; unavailable or unknown acknowledgement remains `503` and reconciles through the same delivery identity on the sender's retry. Preserve cancellation and connection reuse safety. |
| Enqueue and worker wake | Jobs remains the owner of its durable insert and validation. Preserve caller transaction participation, typed validation failures and their precedence, unique-key behavior, trace carrier and all other job kinds. Preserve per-kind bounded/debounced wake behavior, commit visibility and polling recovery; fewer exchanges must not create notification storms or delay otherwise eligible work by removing wakes. Worker execution, leases, completion and retries are unchanged. |
| Serialization and resource bounds | Preserve serialized JSON bytes before PostgreSQL normalization for current payloads, including Base64 alphabet/padding and binary bodies, and the existing JSON-value contract for stored rows. Keep decoded-NUL and serialized-size validation, rejection ordering and generic job serialization semantics. Any retained buffer/cache capacity stays bounded under body size and concurrency; no unbounded payload retention or per-request growth that lives for the process. |
| Logs / traces / metrics | Preserve field values and meaning, request correlation, parent/trace propagation, sampling/export decisions, log filtering and health-log defaults, metric series/units/buckets, label cardinality bounds and refusal/error observations. Savings cannot come from deleting spans or metrics, disabling telemetry, dropping failure visibility or exporting sensitive values. If SQL statements are combined, statement observation may describe the new real statement; retain transaction/outcome signals and truthful bounded summaries rather than fabricating removed statements. |
| Other profiles and routes | Optional profiles remain inert until selected. Preserve the hardened HTTP chain, public response contracts, readiness, deadlines, overload, shutdown and other jobs' behavior. Changes in shared helpers must satisfy these same obligations for their existing callers. |

Representative composition: two authenticated concurrent requests with one
endpoint/ID and different bodies produce one matching receipt/job, with the
first accepted body authoritative. If commit acknowledgement is lost, the
sender receives uncertainty and retries the same identity; reconciliation
cannot duplicate the job. A combined SQL or serialization change must preserve
that sequence while traces and metrics still distinguish durable outcomes.
Increasing the synthetic pool cannot escape this requirement or exceed the
deployment connection budget.

## Comparison and disposition

Freeze a complete immutable source baseline including accepted body transfer,
opt-in instrumentation and unrelated dirty work; HEAD alone is insufficient.
Record each compared candidate's exact source/configuration identity. Historical
results justify the questions but do not serve as the new baseline. Match
toolchain, resolved dependencies, release profile, allocator, features,
fixtures, database durability, CPU placement and generator within comparisons.
Vary pool size only in the labeled sizing comparison; hold it equal for code
attribution. Use a final assembled comparison against the original frozen
baseline to expose interactions among accepted changes.

Use ordinary release results for service latency/CPU/throughput claims and
matched instrumented results for hotpath attribution and allocations. Keep
hotpath 0.28.4 feature-only with jemalloc and MCP over SSH. Primary workloads
are retained new small/large webhook deliveries, duplicate-only and mixed
webhook load, plus the SQL-free HTTP mixture for observability. Reuse adequate
scenarios and behavior tests; do not create a Cartesian matrix of every feature
or deployment. Implementation selects concrete cases, commands and sufficient
repeated samples under the repository's final-validation owner.

Record raw observations, normalization denominators, repeated spread, execution
order, achieved useful throughput, errors, dropped work, service CPU, latency,
peak RSS and relevant database/pool signals. Fast rejections are not successful
work. Distinguish allocation turnover, live memory, RSS, async wall time and
CPU; do not add nested scopes or component percentiles. Keep unfavorable valid
runs; exclusions need an independently observable invalidity reason.

| Final disposition | Meaning |
| --- | --- |
| Adopted measured optimization | Relevant behavior proof passes, the area's primary metric improves reproducibly beyond run variation (or shows an exact causal allocation/exchange reduction), and ordinary release comparison shows no reproducible regression beyond observed variation in useful throughput, errors/drops, CPU, latency or peak RSS across affected workloads. An exact exchange reduction without a useful measured performance benefit is reported as a mechanism result, not automatically adopted as an optimization. |
| Rejected candidate | A tested candidate fails correctness, bounded resources, benefit or regression criteria. Remove its production delta; retain the evidence and explicit failed criterion. This closes that candidate, not a claim that the area was optimized. |
| No supported optimization | Bounded evidence finds no viable useful candidate, or measurements remain inconclusive. Preserve the simpler baseline and explain the discriminating evidence, limitation and reopen condition. This is an explicit area disposition, not an optimization success. |
| Blocked | Required evidence cannot be obtained within available authorized infrastructure or authority. Retain resumable work and name the missing input/owner; do not use this as a completed area or silently skip it. |

Report all four areas even if several end with rejection. No arbitrary global
percentage is promised. Final completion means every area has a non-blocked
disposition, every retained change meets its adoption rule, and the assembled
candidate has the required behavior proof and independent final review.
If none qualifies, report that no new optimization was established rather than
claiming the desired performance improvement. Mandatory real-database proof
applies to changed durable behavior under the persistence validation owner;
reuse existing coverage and extend only material gaps. All execution, including
docs/instruction checks and measurement reduction, stays on the approved
DigitalOcean host. Definition itself uses static reads only.

## Boundaries and reopen conditions

No Railway writes, production tuning or data, deployment, push/PR, paid provider
calls, schema/retention changes, durability relaxation, allocator/release-profile
change, or unrelated cleanup. Any new paid host must be authorized through the
root's [operations record](operations.md) before execution. Design may proceed
while host approval is pending.

Reopen Definition for a changed outcome, compatibility/invariant or success
meaning; Technical Design owns mechanisms and ownership conflicts. Technical
Design must check installed/resolved library capabilities before custom
mechanisms; it records any task-relevant dependency decision. Missing external
authority or infrastructure returns to the root. Production SLO, Railway
capacity, long-run leak freedom and external-provider performance remain
unmeasured unless separately authorized and established.
