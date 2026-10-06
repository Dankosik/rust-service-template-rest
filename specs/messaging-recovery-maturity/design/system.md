# Technical design: messaging recovery maturity

Status: ready, candidate T3, 2026-10-06. Inputs are the ready
[specification](../spec.md), [intent](../intent.md),
[Definition transition](../definition-transition.md) and
[provider evidence](../research/design-evidence.md). Base is
`699887b18594088a59bcc23a049d290d089f6da1`. This artifact selects mechanisms;
Implementation selects concrete cases and commands at its final-validation boundary.

## Drivers and selected reuse

The invariant is custody of immutable event meaning across two independently
durable stores and an uncertain network. Logical ID owns the business effect;
publication ID owns one transport attempt across retries. Native JetStream,
the existing typed consumer, `infra_postgres::in_tx_with`, the existing jobs
engine and native backup tools remain the mechanisms. No universal inbox,
delivery engine, new provider, or workspace crate is introduced.

| Choice | Decisive constraint and alternative disposition | Accepted cost / reopen |
| --- | --- | --- |
| Extend current stream admission | async-nats 0.50 already exposes every required field; server administration at startup would cross ownership | Earlier refusal requires operator correction; reopen on provider field semantics changing |
| Example-owned receipt plus unique-key arbitration | Native PostgreSQL uniqueness settles racing transactions; an immediate SELECT cannot establish absence after uncertain COMMIT; generic inbox libraries would still need example policy and Tx integration | One receipt per logical event over its replay horizon; reopen for an adopter's actual receipt-retirement policy |
| Thin native DLQ operator workflow | Existing restore helper owns Go-compatible bytes/identity; native delete has no CAS; read/check/delete and a KV lease cannot fence stream replacement | Retirement requires lifecycle custody described below; no new broker coordinator |
| Native NATS snapshots and pg_dump/pg_restore | These preserve actual store state and consumer positions; row rewriting or mock snapshots cannot prove restore | Quiescing is needed for the coherent cross-store boundary; mismatches remain explicit scenarios |
| Reuse installed async-nats, SQLx, sha2, serde, rcgen and tracing tools | Current dependency graph supplies transport, arbitration, hashing, typed artifacts, fixture TLS and capture | No version/toolchain upgrade; a newly discovered missing general mechanism reopens dependencies |
| Preserve one publisher slot, broker default MaxAckPending, shared worker | No representative evidence yet supports new concurrency or lifecycle policy | Measurements below can reopen one named choice before its runtime edit |

## Stream admission and supported transfer size (B1)

`infra-messaging` remains the sole admission owner. During native startup,
read source stream, then consumer DLQ stream after resolving its subject.
Reject `no_ack`, non-file storage and non-default persistence on every stream
this role publishes into. Publisher-only startup stops after source admission;
disabled messaging performs no network work. Integrate PR #239 by semantic
changes, retaining main's `MessagingStartup`, native publication ownership and
shared deadlines; do not replace the older whole file.

Source consumer admission retains the existing positive `max_message_size`
requirement and upper bound `S <= M + 8192`, where S counts encoded headers
and payload together and M is the adapter payload bound. At `Consumer::admit`,
the complete registry is available. Compute L as the largest byte length of
a configured route subject selected by this consumer filter, including a
selected route without a handler. No unbounded wildcard expansion enters L.
Any selected message outside these registered routes retains today's permanent
failure handling but is outside the normal-transfer guarantee.

For a header name n and value length v, `line(n,v) = len(n) + 2 + v + 2`.
Use checked arithmetic and these deliberately conservative bounds:

```text
A = line("Nats-Msg-Id", 68)            # "dlq-" plus 64 hexadecimal digits
  + line("Original-Subject", L)
  + line("Dead-Letter-Reason", 9)       # longest closed current reason
  + line("Nats-Expected-Stream", len(actual_dlq_stream_name))
D = S + A                             # payload plus encoded headers
H = min(S, 8192) + A                   # encoded headers alone
```

The source S already includes header framing, original identity and admitted
trace bytes. Transfer copies only the first retained identity/trace values,
then adds the four lines above; counting the replacement publication header
again avoids depending on a minimum original-header size. Thus D is an upper
bound, with small acknowledged slack, for every supported normal envelope.
This preserves current wire acceptance and avoids inventing a subject/header
limit. Reuse the actual wire header constants and closed reason strings so a
future transfer-field addition cannot silently escape this arithmetic.

Require `H <= 65_535`, NATS 2.15's independent header-byte ceiling, in addition
to current advertised server `max_payload >= D` and either nonpositive
DLQ `max_message_size` or a positive value at least D. Header overflow is a
distinct sanitized consumer-admission refusal carrying required header bytes
and the native ceiling. Source headers are bounded by both S and 8192, and
the transfer retains a subset plus A, so H bounds the actual transfer header
encoding. This closes long registered subjects that are valid under an
operator-increased control-line limit; no new source subject grammar is added.
A missing calculable
bound refuses consumer admission before creating/updating or pulling its
durable consumer. Retain the resolved source/DLQ limits in the startup resource
until registry-aware consumer admission; this is private adapter data, not a
new configuration key or a second topology reader. Diagnostics name role,
closed refusal, required bytes and effective limit, never raw provider errors.
Normal source publication keeps its existing M/header bounds. Runtime DLQ
failure/ambiguity still retains source custody. Admission is a startup
observation; subsequent operator drift is not silently certified.

## Durable-effect example and uncertain COMMIT (B3)

Put the opt-in, compilable example in `test/examples/messaging_recovery.rs`
with its reusable example behavior in
`test/examples/messaging_recovery/effect.rs`. This package already owns
executable recipes and imports the actual worker, PostgreSQL and messaging
adapters. The entry point composes `jobs_worker::run` for consumer/worker mode;
its producer mode uses the existing outbox transaction. There is no shipped
business kind or new default migration. Example SQL lives in
`test/fixtures/messaging_recovery.sql` and is applied only to an owned example
database. Fixed example SQL may use bound dynamic SQL as existing recipe
fixtures do: the schema is deliberately absent from production migration
history and therefore from production `.sqlx` metadata.

The sample event is an immutable counter increment, with `counter_id`, signed
integer `delta` and no ignored payload fields. Its receipt key is
`(consumer_scope, logical_id)`, with a fixed example scope. Receipt meaning
compares event type, schema, UTC occurred-at, counter ID and delta exactly;
publication/stream sequence/trace context are excluded. JSON whitespace and key
order are not business meaning. Use timestamp text normalized by the same
event representation if PostgreSQL timestamp precision would lose nanoseconds.
A conflicting key is a permanent, observable conflict, never an equivalent
duplicate. No automatic receipt expiry is provided.

One explicit READ COMMITTED `in_tx_with` attempt first inserts the receipt with
`ON CONFLICT DO NOTHING RETURNING`. PostgreSQL's unique-index arbitration waits
for a competing unresolved original transaction. If inserted, apply the
counter mutation in that same caller-owned Tx. If not inserted, a subsequent
statement reads the now-committed receipt and compares meaning; only an equal
receipt resolves as duplicate success. Statement/lock timeout, cancellation,
missing row after conflict or database loss is unresolved, with no successful
handler result. Any failed mutation rolls the new receipt back with it.

`CommitUnknown` is reported as unresolved and yields the existing retryable
handler disposition. There is no automatic closure replay after the error.
A later delivery or explicit same-ID reconciliation performs the same unique-key
arbitration first: equal committed receipt establishes effect; insertion after
the old transaction resolves establishes that this key had no committed effect
and permits the same-identity attempt. It does not infer absence from a racing
plain SELECT. The existing finite retry/DLQ custody policy remains intact;
unknown effect is never ordinary success. External provider effects are absent
from this example and cannot inherit its atomicity claim.

The shared-process capacity run uses one configured PostgreSQL pool and the
real ordinary jobs/outbox/consumer composition. Current worker registration
runs before pool admission and exposes no pool. Close that concrete missing
edge in `jobs-worker`, not with an example-owned second pool: add an opt-in
deferred message-registry factory to `Registration`, under the existing outbox
profile that guarantees PostgreSQL and messaging. Public
`Registration::with_postgres_messages` accepts a single
`FnOnce(PgPool, &mut MessagingRegistry) -> Result<(), BuildError>`;
it declares consumer intent during ordinary registration and rejects a second
factory. The existing `Registration.messages` is the only registry: early
registration supplies routes, then the factory adds database-backed handlers.
Existing Registry duplicate checks reject competing handlers; no registry is
silently replaced and no new generic dependency container is needed.

Bootstrap validates consumer configuration before dependency I/O when that
intent is declared, then opens/adopts its existing one pool and verifies
migration history. It invokes the factory once with a clone of that admitted
native pool and that same registry before `connect_messaging`, validates its
routes/handlers and
then follows the existing consumer admission/engine/listener order. The factory
performs synchronous bounded composition only: no provider I/O, pool creation,
task spawn or internal retry. Returned error, empty handlers, conflict or panic
uses existing startup refusal and retained-resource cleanup. Normal callback
users and messaging-only graphs keep their existing path; profile pruning
removes the deferred PostgreSQL API with outbox. The factory does not own close,
readiness or lifecycle policy; the worker still closes its native pool
after draining tasks. Ordinary jobs, outbox and effect handlers share the same
pool ceiling, probes and metrics. This small composition extension is needed
by the executable accepted example and does not alter role separation or
publisher concurrency.

The independently-restored
scenario uses a separate consumer process and effect database, with the same
example handler. This is fixture composition through current supported roles,
not a production role-separation change.

## One-record DLQ workflow (B4)

The wire owner remains `restore_dead_letter`. Add a thin opt-in operator example
`crates/infra-messaging/examples/dlq_recovery.rs` for selection, reconstruction,
manifest and publication. Native async-nats read APIs and the admitted existing
`Messaging` publisher supply the operations; retain and close both native
resources under one finite command deadline. Use supported TLS/credentials
options; do not add a general administration API or worker loop. Native
retirement belongs to the owned-session runner below, not a new runtime module
or the jobs SQL recovery commands.

`inspect` reads the exact native stream sequence (not a moving consumer cursor)
and reports missing, malformed/unrestorable, or restorable. An explicit payload
option emits inspected bytes; default output carries metadata and digest only.
The saved selection contains stream name, sequence, stored timestamp, subject,
all source header values and payload digest, and the actual bytes needed for
reconstruction. Its versioned local manifest is written before possible
publication, with exclusive creation/atomic replacement and restrictive file
permissions. Do not print credentials or manifest payload in routine logs.

Inside an acquired owned maintenance session, `redrive` rereads and compares
the selected record before dispatch, reconstructs
through `restore_dead_letter`, and publishes that immutable PreparedEvent to
its original destination with expected source stream. The logical ID, type,
schema, event time and raw payload stay fixed; publication ID derives from
actual DLQ stream/sequence/time/publication ID. A retry after crash reuses the
same manifest and preparation. Each run reports publication and retirement
separately: rejected, ambiguous, confirmed; retained, absent, stale, refused,
unknown or retired. Only a positive expected-stream PubAck permits retirement.
A manifest carrying earlier confirmation is evidence to reconcile, not license
to skip current identity/precondition checks or mint another identity.

**Native limitation and required maintenance boundary.** NATS 2.15 delete sends
only sequence and `no_erase`; it has no record/incarnation CAS. A second fetch,
stream creation timestamp, `Nats-Stream-Identity`, process mutex or KV lease
does not close the restore-between-read-and-delete race. Stream creation time
may itself survive restore. Therefore the tool must refuse retirement when
the selected stream's lifecycle cannot be held stable until every outstanding
mutation is resolved or its entire broker lifetime has ended.

The delivered executable demonstration has this authority because its runner
creates the original authoritative broker cluster, storage, private network
and credentials, exposes no external administrative route, and is the sole
topology owner. One controller serializes its one-record recovery operations
and all topology operations. Only this owned-session path can invoke native
retirement; an arbitrary remote URL or an `--assume-exclusive` flag cannot
enable it. The session identifies its exact containers/volumes/network and
rejects a manifest from another generation. Preserve the native cluster and
fence after a lost deletion response; read the exact record again to report
retired, retained or still unknown. A controller/command crash does not
authorize restore/replacement. Recovery of an abandoned session first stops
all its owned broker processes and clients; only a fresh isolated lifetime
with fresh endpoint/credential identity may then restore. Old requests cannot
cross that boundary, and old manifests cannot act on it without inspection.

For an adopter-operated broker, equivalent exclusive lifecycle/credential
custody belongs to its deployment operator and must cover delayed server
requests as well as command execution. The runbook names that prerequisite
and native one-record deletion sequence; the generic example does not claim
to verify unknown administrators or automatically supply that platform fence.
Without it, inspection remains available while the controlled workflow refuses
mutation. No account settings are changed by application startup.
Within the fence, compare full selected identity immediately before publish
and delete; absent/replaced data causes no action based on a stale selection.
Even if a response is lost after deletion, its result remains separately
unknown until inspection resolves it. A snapshot copy is not evidence that
the original live DLQ record was retired.

## Native recovery and capacity fixture (B2, B5)

One opt-in runner, `scripts/ci/messaging-recovery.sh`, owns a unique compose
project and manifest under a caller-selected output directory. A companion
`test/fixtures/messaging-recovery-compose.yml` extends the canonical pinned
images, adding three named file-storage NATS nodes, private routes and TLS
client connections, separate volumes, and PostgreSQL. It derives image pins
from the existing compose authority rather than adding an independent pin.
Generate a short-lived fixture CA/server certificate for explicit loopback and
container names using existing rcgen or installed OpenSSL; verify trust normally,
never disable certificate verification. Client and cluster-route TLS settings
are recorded separately; a claim of encrypted routes needs route TLS enabled.
Admin access and monitoring stay inside the owned fixture boundary.

Resource admission precedes build/start: retain free disk, existing artifact
availability, host/container CPU and memory limits, and competing load. This
host currently has only about 4.8 GiB free and 16 GiB RAM; a fresh large target
directory is not admissible by assumption. The fixture budget is at most
1 GiB additional retained data, 3 GiB container memory, one bounded scenario
at a time, and a 15-minute run deadline. Keep at least 2 GiB host free disk;
preflight and periodic resource checks stop the owned run if the floor cannot
be preserved. Build space is additional and must be established from available
artifacts or the existing CI runner before build. Do not prune shared caches,
install sccache or stop other users' containers. If no authorized runner meets
the budget, retain the unverified claims; do not substitute an R1 result.

Create source and DLQ with file/default persistence, R3, ACKs, finite message
and total storage limits and explicit retention. Read back effective config,
leader and two current replicas before the observation. Use real durable
consumer state and effect receipt tables. The runner's synthetic event manifest
is the known input oracle; it records exact logical/publication IDs and hashes
of canonical wire meaning, with bytes retained for authorized replay. It is
measurement evidence, never an invented recovery source when testing missing
durable authority. Classify replay availability from actual outbox/source/DLQ
storage, not from the oracle's copy.

For coherent backup, stop ingress and drain/stop workers, record the common
quiescent ID boundary, take native `nats backup stream --consumers` of source
and DLQ and `pg_dump` of producer/effect databases. Validate archives with the
installed native CLI. Restore through `nats backup restore stream` into an
absent stream and `pg_restore` into fresh fixture databases; compare exact
record bytes and consumer state before permitting replay. These are separate
snapshots made coherent by quiescence, not an atomic distributed snapshot.

The same runner deliberately selects older/newer archives for mismatched
producer/broker and consumer/effect cases. An older effect database requires
an explicitly named replay consumer/position over retained source bytes;
ordinary resumption from a newer ACK floor is not recovery. An older broker
needs publishable retained outbox bytes or another declared retained durable
authority. Terminal/deleted intent without bytes produces exact missing IDs
and reconciliation stop. Do not reset every completed job or inject the oracle
payloads to hide that loss.

R3 fault chooses the actual leader, kills only its owned node and retains the
other two, then observes quorum recovery and exact pre-fault ACKed bytes.
Publication attempts around the fault retain confirmed/rejected/ambiguous
sets. Capacity exhaustion uses native stream `max_bytes` plus `DiscardNew`
or an account storage quota in the isolated fixture: retain native refusal
and usage evidence proving that specific boundary was reached. This is not a
host disk-full or physical fsync claim. Release owned capacity and observe
same-identity outbox/source recovery. Retention pressure uses finite `max_age`
and a stopped consumer; distinguish retained catch-up from explicitly expired
IDs. No busy poll, infinite producer, faulting shared disk or unowned cleanup.

Measurement builds once and reuses the release artifacts. Run the current
one-slot/shared-worker baseline with 1 KiB events and a bounded 64 KiB slice,
offered rates that move from stable to backlog growth, a bounded broker outage,
and catch-up with the ordinary jobs probe present. A sample carries offered,
admitted, PubAck-confirmed and effect-applied rates; oldest queue age/backlog;
outage/catch-up timing; retry/ambiguity/failed custody; pool waits/occupancy,
query latency, database CPU/locks/WAL/transaction age; broker bytes, CPU and
replication lag; worker/durable concurrency and effective `MaxAckPending`.
Retain a time series and exact ID reconciliation, not just aggregate means.
Keep message count and duration below the resource ceiling. For repeated
whole-command comparisons use `hyperfine --warmup 3`, preserving variance and
resetting the fixture workload between runs; diagnostics do not become an
unbounded benchmark campaign.

Three dispositions are initially **retain**: publisher concurrency 1,
broker-default effective `MaxAckPending`, shared worker roles. Reopen only
with attributable observations: (a) publisher is saturated while database and
broker have headroom and oldest age grows; (b) aggregate replicas are prevented
from using free handler slots by the observed broker pending limit; (c) native
shared admission/critical failure demonstrably interrupts ordinary jobs or
their latency beyond the measured baseline envelope. A comparison may use
already-supported process counts and fixture-only broker limits. Any production
knob, new default, or role change first gets a narrow reviewed design delta
with finite bounds, pool allocation, failure/drain policy and comparable proof.
Do not hold independent implementation tasks for that empirical choice or
report a throughput gain from the knob itself. Delivery requires actual bounded
findings and a final retain/change disposition for all three.

## Development feedback (B6)

The tracing failure remains an investigation, not an assumed production defect.
The earliest owners are `infra-object-storage/src/tests.rs` for capture and
`observe.rs` for real span fields. Reproduce using the built focused test
binary, varying only test concurrency/dispatcher lifetime; record span enabled
state, callsite registration and captured new-span/record callbacks. Compare
the exact selected tracing-core source before attributing a cache race. A fix
must preserve one operation span, Amazon region only, and the no-network
presign behavior. Prefer the supported tracing-subscriber registry/layer if
the homemade capture subscriber is causal; no production seam or blanket test
serialization without evidence. Root cause, failed-before/passed-after causal
proof or an already-landed repair is mandatory. The diagnostic's ordinary
function-level repair choice remains Implementation work; changed telemetry
semantics reopens this design.

`validation-lock.sh` retains one Git-common mutex and inherited lock ownership.
Add acquisition timestamp, checkout, candidate, PID and a sanitized command
identity (executable/make target, not arbitrary potentially secret arguments)
to the owner receipt. Wait emits immediately, then at bounded 10-second
intervals with elapsed/configured timeout and readable/missing/stale owner
state. Acquisition, timeout and cancellation have distinct terminal output.
Install wait signal handlers before sleeping; cancel/timeout cannot fall
through into the requested command. Only an acquired owner cleans its lock.
Preserve the command's exact exit status and hold custody until its child has
terminated; signals do not remove a live child's lock.

Do not retain the current unsafe stale-PID automatic reclamation: a dead
wrapper PID does not prove its child is gone, and competing reclaimers can
race a new owner. Report stale owner and refuse automatic reclaim; deliberate
operator cleanup follows process confirmation. This small behavior change is
the conservative way to uphold B6 without introducing a lock manager. PID
reuse may cause a conservative wait, never parallel validation. The existing
self-test owns serialization; extend it for wait/terminal diagnostics and
command-not-run after cancel/timeout. No host/cache configuration changes.

## Delivery, compatibility and proof

First preserve main #254 custody and #255 build owner while transplanting the
bounded #239 storage/ACK-loss changes. The resulting main-based recovery PR
supersedes duplicated #239 code; root delivery may retain #239 as historical
context or close it after the superseding PR is reviewable. The independent
feedback repair is a coherent second PR if it can be extracted without
cross-candidate validation duplication. No merge or deployment is authorized.

Affected deployment edges are existing producer/outbox → source → consumer →
effect DB and consumer → DLQ → operator → source. Event bytes, publication IDs
and receipt meaning stay stable across mixed versions; old binaries lack
admission protection. Operators first correct ACK/storage/size topology,
then adopt the new binary. Unsafe settings cause a sanitized startup stop,
not mutation. Rollback to an old binary removes those checks and is not a
durability guarantee. Example schema/fixtures never migrate a live database.

Retained/removed profile markers and initializer inventory must cover example
sources, manifests and fixture targets as well as adapter code: messaging-only
keeps DLQ tooling; messaging+PostgreSQL+outbox keeps the combined recovery
example; absent capability leaves no broken imports/targets. Reuse existing
messaging, PostgreSQL and source-only projection gates. Add the opt-in recovery
runner to the existing manual/selected CI execution path, once per assembled
candidate, not to every initializer/harness combination. Canonical classifier
and make owners must select any newly changed path family. No new mandatory
per-commit R3 performance matrix is selected.

Final proof follows current validation routing: matching build/relevant tests,
real PostgreSQL and broker evidence for their claims, actual Go wire parity,
changed shell self-tests/ShellCheck and workflow checks when changed,
profile removal and final independent delivery review. The fixture runner
reuses the shared heavy-validation lock; one release build feeds all scenarios.
Current phase runs static docs validation only. Existing CI observes the exact
published candidate; historical PR #239 passes prove only its historical scope.

## Ownership and phase exit

The reconciled [ownership map](ownership.md) fixes source placement and proof
owners. This design is ready only after the required ownership lenses and
independent Technical Design Review resolve. Narrow reopen: provider facts →
Research; changed behavior → Specification; unsupported platform fence or
resources → named external owner/incomplete observation; empirical runtime
choice → this design before its runtime edit. No requester-owned decision is
currently needed. The root then dispatches fresh Planning; this actor changes
only the task's design/evidence/review/transition artifacts.
