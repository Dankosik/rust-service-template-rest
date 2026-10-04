# Mechanism comparison after user steering

Status: historical supporting comparison and reopen request, now consumed by
the reviewed Definition in definition-transition.md. Current design/design.md
and design/dependency-custody.md own the active replacement decisions; this
is not an implementation acceptance record. On 2026-10-04 the user challenged
application-level pool management and asked why a ready-made solution was not
used. Maintenance simplicity and library ownership are now explicit selection
drivers. The previous facade option described in [source research](sqlx-release.md)
is historical evidence, not the selected implementation direction. The
[current design](../design/design.md) and [ownership map](../design/ownership.md)
replace that draft. Its ownership panel stopped without verdicts after this
steering; no implementation had begun.

## Recommendation

Do not implement the application-owned pool facade. Keep pool return and
connection accounting in the pool library. For this existing template, the
smallest total change is a narrowly backported, upstream-aligned **whole-return
timeout in SQLx core**, plus ordinary acquisition observation at the template's
existing operation boundaries. Preserve the native PgPool/Executor/query!/
transaction/migration API and existing session, lifetime, password and shutdown
policy. The previous proposal turns one upstream pool defect into a permanent
application-owned cleanup lifecycle; its demonstrated correctness does not
make that maintenance tradeoff preferable.

This recommendation has a real cost: **there is no released SQLx fix today**.
It requires temporary custody of one vetted dependency backport, not merely a
configuration change or an upgrade. Do not describe it as already supplied by
stable SQLx. Use the exact published sqlx-core 0.9.0 source plus only the
whole-return patch, rather than pinning an unreviewed PR branch. A local
vendored sqlx-core package is the reproducible carrier within current local
authority: the original crate has 110 files, about 649 kB unpacked, while the
upstream production correction is 10–11 added lines and one changed await.
Keep its license/provenance and a small visible patch record; patch crates.io
resolution once and keep the rest of SQLx at its current release. The
PostgreSQL profile must remove the vendor/patch when absent. Retire the
backport when a released SQLx version supplies the equivalent proven bound.
No fork publication or remote write is authorized or needed for local work.

This is a recommendation to the responsible phase owners, not acceptance of
the patch. Dependency custody, exact source delta, profile containment and
proof must be fixed in the replacement Technical Design before Planning.

## Why an ordinary supported configuration does not fix native SQLx

The [local source and causal probe](sqlx-release.md) establish that SQLx 0.9.0
retains the permit while an unbounded return ping waits. A bounded
after_release callback is followed by another unbounded ping; closed-pool and
max-lifetime close branches bypass the callback. Increasing capacity, lifetime,
idle timeout or caller timeout cannot remove this retention. The selected
library-layer correction wraps the whole owned return, matching the successful
probe mechanism without requiring an application to invoke hidden APIs.

Live upstream reads on 2026-10-04 found:

- [Issue #4349](https://github.com/transact-rs/sqlx/issues/4349) describes this
  exact permanent permit-retention failure.
- [PR #4350](https://github.com/transact-rs/sqlx/pull/4350), head
  `60772eb39474136adb15c6ffd68ded307649bc35`, is open and unmerged. Its current
  pool implementation adds one five-second timeout around the whole return
  future, including callback, ping and close branches.
- [PR #4407](https://github.com/transact-rs/sqlx/pull/4407), head
  `c3735abf034689bb1fab9d7bf9f040e492de28cf`, is likewise open and unmerged and
  implements the same five-second ownership boundary, with more reuse and
  refill tests. These PRs are evidence/reference only, not task-owned PRs.
- Current main `8b65c2fb42a3a523b77000f41b648986d1d49ba0` still has a
  byte-identical pool/connection.rs to the installed/published 0.9.0 crate.
- [PR #4394](https://github.com/transact-rs/sqlx/pull/4394) separately fixed
  pending BEGIN on main, but the released 0.9.0 source still lacks that fix.
  The template's existing guard therefore remains necessary for this narrow
  backport. Do not pull unrelated transaction changes into it.

The published sqlx-core 0.9.0 archive checksum is
`05b44e85bf579a8eeb4ceaa77a3a523baf2bf0e9bac7e40f405d537b5d2d5ccb`, matching
Cargo.lock. Its `.cargo_vcs_info.json` names
`003b698e99e024f3621b8043a2426fde5b741171`. The inspected published return/inner
sources match the local hashes in the earlier research. PR heads contain
unrelated changes relative to that source lineage (network/TLS, migrations,
pool internals and other files); simply choosing their git revisions would
not be a one-patch upgrade. The scope is the isolated correction applied to
the published source, with no raw Cargo cache modification.

## Ready-made alternatives, traced through actual ownership

| Candidate | What it really handles | Remaining integration and why it is not the smallest change here |
| --- | --- | --- |
| Generic Deadpool 0.13.1 with SQLx PgConnection | Object drop returns the idle object and semaphore permit synchronously. A subsequent checkout owns bounded recycle/create work; cancellation or recycle failure drops its UnreadyObject and raw connection. Supported Object::take can discard pending BEGIN. | This can retain SQLx query!, borrowed transactions and dedicated migrations. It still needs a Manager, measured acquire entry, current password-options snapshot, idle/lifetime retirement, shutdown completion accounting and pending-BEGIN guard. Native &Pool Executor is not supplied; explicit acquisition can replace it. It is the best fallback if dependency-backport custody is unacceptable, not a turnkey elimination of integration code. |
| mobc-sqlx 3.0.0 with Mobc 0.9.0 | A real released SQLx 0.9 integration. Dropped checkout return performs no network I/O; get_timeout bounds checkout/create/check futures and their ownership. | Its SQLx check is ping without a smaller recycle deadline: one silent socket can consume an acquire window. Immutable manager options do not supply current password refresh. It lacks native SQLx &Pool Executor and complete acquisition diagnostics. Pool-specific policy and compatibility still require adaptation. |
| deadpool-postgres 0.14.2 with tokio-postgres 0.7.18 | Synchronous object/permit return; Verified recycle timeout discards the unready object, and ClientWrapper drop aborts its owned connection driver. Current tokio-postgres handles pending-BEGIN rollback more directly. | Strongest whole-driver replacement, but SQLx query!/offline checking, rows/errors, Tx Executor, migration/testing contracts and TLS/rotation policy must move or split between two drivers. Commit finality still belongs to the template. No reason from this bounded pool defect justifies that broader migration. |
| bb8-postgres 0.9.0 with bb8 0.9.1 | Connection checkout has a total timeout, and validation calls tokio-postgres. | On silent validation timeout, PooledConnection can remain Present and has_broken only sees is_closed=false, allowing repeated reuse of the same silent client. The spawned driver handle is detached. Configuring a timeout does not establish the required resource disposal. Reject for this outcome. |

The earlier draft's blanket statement that Deadpool requires replacing SQLx
was too broad. **Generic Deadpool can pool SQLx connections.** Retaining SQLx
was also an assistant-selected assumption, not an immutable user demand.

Important Deadpool details from its released source:

- Native wait/create/recycle timeouts are separate. Recycle runs at next
  checkout and can visit several stale idle objects. Three one-second recycle
  failures can use a three-second caller window before a replacement dial.
  This is finite checkout recovery rather than an indefinitely retained
  return task; it should not be rejected solely by an artificially strict
  first-replacement requirement. Whole-acquire bounds still need one owner.
- Async pre/post-recycle hooks lie outside the native recycle timeout; adding
  network work there would reopen the bound.
- Metrics.last_used starts at successful checkout, not at object return.
  Exact current ten-minute idle-since-return retirement needs a return
  timestamp; otherwise its policy changes. Thirty-minute lifetime can use
  age in recycle and retain. Retain must run on an existing supervised tick,
  and its returned removed objects must be dropped.
- Manager-held synchronized PgConnectOptions can preserve password refresh
  without locks across I/O. Deadpool does not supply SQLx's current options
  update policy automatically.
- Pool::close stops admission and returns synchronously. Completion requires
  bounded accounting for both active objects and acquisitions/creation;
  size alone misses a connection still being created. This is another
  adapter lifecycle obligation, even without a new background task.

These costs survive after dropping the facade's self-imposed Executor
compatibility and three-second-first-replacement requirements. They are why a
library-layer correction is the smaller change for this existing template;
for a greenfield service, Deadpool would be a reasonable pool selection.

Primary sources: [Deadpool pool](https://docs.rs/deadpool/0.13.1/src/deadpool/managed/pool.rs.html),
[object](https://docs.rs/deadpool/0.13.1/src/deadpool/managed/object.rs.html),
[metrics](https://docs.rs/deadpool/0.13.1/src/deadpool/managed/metrics.rs.html),
[manager](https://docs.rs/deadpool/0.13.1/src/deadpool/managed/manager.rs.html),
[mobc-sqlx manager](https://docs.rs/mobc-sqlx/3.0.0/src/mobc_sqlx/lib.rs.html),
[Mobc](https://docs.rs/mobc/0.9.0/src/mobc/lib.rs.html),
[deadpool-postgres client ownership](https://docs.rs/deadpool-postgres/0.14.2/src/deadpool_postgres/lib.rs.html),
[recycling methods](https://docs.rs/deadpool-postgres/0.14.2/src/deadpool_postgres/config.rs.html),
[bb8 return/checkout](https://docs.rs/bb8/0.9.1/src/bb8/inner.rs.html), and
[bb8-postgres driver task](https://docs.rs/bb8-postgres/0.9.0/src/bb8_postgres/lib.rs.html).

Registry checks on 2026-10-04: Deadpool 0.13.1 and deadpool-postgres 0.14.2
were released 2026-08-26, mobc-sqlx 3.0.0 on 2026-08-15, Mobc 0.9.0 on
2025-06-25, bb8 0.9.1 on 2025-11-24, bb8-postgres 0.9.0 on 2024-12-09,
and tokio-postgres 0.7.18 on 2026-06-12. Upstream repositories were
unarchived. Source/maintenance checks are not clean advisory results; a
selected dependency change still follows the repository dependency gate.

## What PgBouncer can and cannot own

PgBouncer is useful infrastructure for controlling backend sessions and
queueing, and its configured query/idle/transaction timeouts can terminate
work or sessions that it still controls. They do not release an application
process's semaphore when the client-to-pooler link silently drops replies.
That last network leg remains between the client driver and the pooler.
The [official timeout documentation](https://www.pgbouncer.org/config.html#dangerous-timeouts)
also distinguishes query_wait_timeout from query_timeout; these are not a
client-object return timeout. Adding a mandatory pooler or new server tuning
would add deployment authority and operational policy without closing R1 by
itself. Preserve supported PgBouncer deployments and document its separate
client/server budgets rather than use it as a substitute for local ownership.

## Smallest Specification reopen

Return this to Definition/Specification; do not ask the user to choose a
library implementation.

1. Replace the assistant-selected blanket retain-SQLx/replacement exclusion
   with the accepted driver: prefer maintained library ownership and the
   simplest total migration/maintenance cost. The chosen technical direction
   still retains SQLx because of the evidence above, not because alternatives
   were forbidden.
2. Change R1's technical cleanup bound from three seconds to **five seconds**,
   matching SQLx's existing close-on-drop bound and the upstream whole-return
   correction. Keep ordinary acquisition at three seconds. A request arriving
   during cleanup may time out; subsequent work can recover after cleanup
   without restart, capacity growth or automatic SQL retry. Distinguish local
   slot recovery, replacement connectivity and lingering physical server
   sessions. Do not promise the first post-cancellation request succeeds.
3. Keep useful acquisition diagnosis for named template-owned transaction,
   direct-query, startup/history, readiness, jobs and maintenance paths, with
   acquisition wait separate from execution. Permit an ordinary explicit
   acquire observer and supported native logging. Do not require a universal
   PgPool facade, changed public Executor contract, new metric or unique pool
   API solely to enforce diagnosis for arbitrary future external callers.
   Preserve transaction histogram meaning, cancellation classification and
   bounded secret-free fields.
4. Preserve R3/R4, PostgreSQL 14+, supported poolers, current capacities,
   readiness/session/acquire budgets, finality and no automatic tuning. Clarify
   that configured session allocation is not a hard instantaneous cap on
   lingering PostgreSQL backends after an unreachable client is abandoned.

The cleanup interval changes an agent-selected technical assumption, not a
user SLA. The dependency backport is a local technical effect under existing
change authority; any future upstream message or remote publication still
requires its own authorization. Definition owns the reviewed behavior change;
Technical Design then fixes the exact dependency carrier, observer placement,
ownership map and proof before Planning.

## Evidence and review state

This comparison used exact released sources and live upstream metadata, with
two fresh read-only specialists: `/root/pool_design/ready_pool_sqlx` and
`/root/pool_design/ready_pool_postgres`, assigned gpt-6-astra/high. No new
library build or database comparison was run. The earlier real-server SQLx
probe remains valid only for its recorded native and bounded-return seam.

The three facade ownership reviewers verified their fixed hashes and then
stopped on the changed acceptance boundary. None issued PASS or established
a concrete defect before stopping. The outer Technical Design review did not
start. No implementation or remote effects occurred. The next required result
is the reviewed Specification delta; the previous facade draft must not be
routed to Planning.
