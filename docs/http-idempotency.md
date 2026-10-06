# HTTP idempotency

Select `HTTP_IDEMPOTENCY=postgres` (or `--http-idempotency postgres`) during
initialization to retain this pack. It requires `DATABASE=postgres` and an
authentication engine: a key is scoped to a verified caller. The default
`none` removes the boundary, configuration, migration, tests, and this guide.
Retaining the pack performs no request-time query or task until a protected
operation opts in through `Composer::route`. Enabled PostgreSQL startup first
verifies, read-only through `migrate`, that every embedded migration is
applied with its checksum; versions from a later release are admitted, so a
rollback still starts.

An opted-in operation gives each verified caller and key at most one committed
participating database effect during the configured retention period. The
stored success is replayed only for the same complete HTTP request; an operation
name is not part of that durable identity.

## Compose a protected operation

`Composer::route(routes)?` is the sole opt-in, using an annotated route tuple
bound as shown below. It returns
`Result<UtoipaMethodRouter<_>, CompositionError>` after it adds the required
`Idempotency-Key` header and generated Problem responses to the route value
that is served. The service assembly calls `finish(self) -> Activation` after
all routes are composed; activation counts those routes and does not inspect
the generated document. Invalid local composition therefore fails assembly
before serving. Adopters retain their normal protected-operation
security, success, and business-response declarations; they do not
hand-declare idempotency metadata or validators. The composed operation must
be POST, PUT, PATCH, or DELETE, effectively protected by the assembled OpenAPI
policy (normally the retained profile's root bearer requirement; an optional
`x-security-decision` must agree, and an anonymous alternative or public
override cannot scope a caller key), declare at least one 2xx response and no
1xx/3xx response. Referenced success Response Objects are unsupported at this
seam and fail composition; an idempotent operation declares its success
response inline so the composer can enforce the stored-header allowlist.
Existing OpenAPI gates continue to own generic reference resolution,
operation-ID uniqueness, and generated-contract drift.

At request time the hardened body limit and deadline apply, authentication runs
before key processing, and ordinary extraction, validation, and authorization
run before `execute` on every attempt. Authorization therefore remains in the
handler outside the work closure: replay never executes that closure. A replay
may be refused by the current authorization policy just as a first attempt may.

```rust,ignore
async fn create_widget(
    idempotency: Idempotency,
    principal: VerifiedPrincipal,
    State(widgets): State<Arc<dyn CreateWidgets>>,
    Json(input): Json<NewWidget>,
) -> Response {
    if !may_create(&principal) { return forbidden(); }
    idempotency.execute(async |tx: &mut infra_http::idempotency::Tx<'_>| {
        widgets.create(tx, &input).await.into_response()
    }).await
}
```

`Json` is `infra_http::extract::Json`, so a body that does not fit answers a
Problem before `execute`.

The handler must return the response from `execute`, or preserve its response
extensions. A private extension marks a successful response as executed or
replayed; a handler-created 2xx response without that marker becomes the
existing sanitized wiring 500. This detects a response that is disconnected
from the atomic seam. It cannot roll back an effect a handler performed outside
that seam. Ordinary non-2xx validation and authorization responses remain
valid without the extension.

The handler keeps its ordinary protected-operation annotation (its success
response and `ProtectedOperationProblemResponses`; security normally inherits
the root bearer default) with no idempotency metadata. Compose it inside
`service::api::contract`, which receives the composer. The final
authentication layer wraps the fully assembled contract, so authentication
precedes key handling without per-operation wrapping:

```rust,ignore
// crates/service/src/api.rs, inside contract(idempotency: &mut Composer):
.routes(idempotency.route(utoipa_axum::routes!(widgets::http::create_widget))?)
```

Regenerate and review the OpenAPI document after composition changes. The
generated key description covers both wire encodings and decoded length; it
does not advertise the retired token-only pattern or a quoted wire-length limit.

## Request and key identity

The scope is the verified issuer, subject (when present) or client ID, and the
decoded key. Subject and client identities remain distinct; keys are
case-sensitive. `operationId` is deliberately absent, so renaming it does not
reopen a live key.

The seam privately hashes one length-framed tuple: request method, original
URI path, query presence and bytes, received `Content-Type` field values in
their received order, and exact bounded body bytes. It uses the URI before
route rewrite, preserving percent encoding, query order, and absent-versus-empty
query. It does not normalize media types or JSON, and it excludes other headers
(including `Accept`), credentials, request IDs, tracing, and `operationId`.
The raw body is restored unchanged for normal handler extraction. Consequently,
different JSON whitespace/order, a changed path/query/content type/body, or
numerically distinct large `u64` values are different requests. There is no
caller-supplied fingerprint, version, canonical JSON, or alternate digest.

The private framing is fixed so compatible replicas calculate the same values:
`F(b) = u64_be(len(b)) || b`; both domains end in a literal zero byte. Scope is
`SHA256("http-idempotency/scope/v2\0" || F(issuer) || caller_tag ||
F(caller_value) || F(decoded_key))`, using tag `01` for subject and `02` for
client. Request is `SHA256("http-idempotency/request/v1\0" || F(method) ||
F(path) || query_presence || [F(query)] || u64_be(content_type_count) ||
F(each content-type value) || F(body))`, where query presence is `00`/`01`.
For the non-secret fixture issuer `https://issuer.example`, subject
`fixture-subject`, key `k-123`, the scope digest is
`98df4043b9e795011fe4f84c3844bcfe9ade67032cc0044471d5097f7a30ad94` and its
signed big-endian first eight bytes are advisory key `-7431150200512080639`.
`POST /widgets/%2F?a=1&b=2` with `Content-Type: application/json` and body
`{"n":1}` has request digest
`2e4fb57a46095c45e886d290707bf645412366966bf10009314001fd087eb30b`.

Exactly one `Idempotency-Key` field is required. After HTTP surrounding OWS is
handled, a value beginning with `"` must be one complete IETF Structured Field
string parsed by `sfv`, with no parameters or trailing item/non-OWS input. Its
decoded value may contain valid spaces and escapes. All other values must be
visible ASCII bytes `0x21..0x7e`, including `/` and `=`. Both forms decode to
1..255 bytes, and quoted `"abc"` is the same key as unquoted `abc`. Empty,
repeated, non-ASCII, control-containing, malformed, or over-limit values return
sanitized 400 `bad_request` before arbitration. A malformed quoted value never
falls back to unquoted parsing.

The grammar uses workspace `sfv` 0.15.0 with default features disabled and
`parsed-types` only in `infra-http`. Its owned `Item` parser handles complete
Structured Field string syntax and escapes; the boundary still owns field
cardinality, OWS, unquoted compatibility, and decoded bounds. The former
token-only parser and a handwritten escape parser cannot meet those rules.

For a live key, an equal request replays and a different request returns 422
`idempotency_key_mismatch` without work. A 422 is not a reason to retry under a
new key before reconciling the original effect. A changed excluded header alone
replays the stored representation; use a new key for a new operation or
representation.

## Transaction, outcomes, and replay

`execute(work)` owns one explicit READ COMMITTED transaction. `work` receives
the opaque shared `infra_http::idempotency::Tx`; only `infra-postgres` owns its
lifecycle and exposes its connection to provider adapters. Adapters do not
commit, roll back, or name the idempotency table.
<!-- template:begin jobs:docs-http-idempotency-jobs-enqueue -->
With jobs retained,
`infra_jobs::enqueue(tx, ...)` joins this same transaction: a success record,
business effect, and job commit together; replay, refusal, and rollback enqueue
nothing. External effects are outside this guarantee.
<!-- template:end jobs:docs-http-idempotency-jobs-enqueue -->

Only statements issued through that `Tx` share the record's fate. An outbound
HTTP call, a message, a file, or a write to another datastore is not covered:
it can run on an attempt that later rolls back and run again on a retry, so
such an effect needs its own idempotency key derived from the same request or
its own durable design.

Only a storable 2xx commits work and the record. A non-2xx work result rolls
back and keeps its ordinary response. A concurrent undecided attempt receives
409 `idempotency_request_in_progress` with `Retry-After: 1`; once a committed
record is visible, equality or mismatch decides the result. A transient
connection/availability fault or uncertain commit returns 503
`idempotency_unavailable` with `Retry-After: 1`. Retry that same request/key:
it can replay, receive 409 while the attempt is live, or execute after rollback.
There is no automatic work retry or post-commit readback. Known non-transient
database/programming/data faults, including SQLSTATE `25P02`, are sanitized 500,
not 503 when the transaction boundary establishes that no commit occurred.
Every unknown COMMIT outcome is 503, including a malformed protocol response:
the fault's permanence does not establish rollback. Retry with the identical
request and key; never substitute a new key to resolve an uncertain attempt.
Logs retain only bounded failure class and SQLSTATE/cause category.

The complete store attempt ends at the hardened chain's
`RequestDeadline.at() - 100 ms`. Acquisition, BEGIN, all work statements,
bounded response-body capture and COMMIT share that one
absolute cutoff; body reads and earlier handler work already spent part of it.
The remaining 100 ms reserves bounded in-memory terminal response mapping,
not delivery to a slow client. With the default eight-second request and a
full three-second acquire wait, at most 4.9 seconds remain for the rest of the
foreground attempt; this is not a fresh per-statement budget. Native cleanup
can retain its local slot for up to five seconds afterward, outside the response
reserve.

If the cutoff is exhausted before `execute` starts its store operation, it
returns 503 `idempotency_unavailable` with `Retry-After: 1` without database
dispatch or work invocation. A configured 100 ms request, or one with 100 ms
or less remaining at this boundary, therefore cannot start a database attempt.
Expiry during the attempt returns that same 503; the outer request deadline
still maps to `gateway_timeout` 504. Each has the normal same-key recovery
path and adds no internal retry.

Cancellation drops the native transaction and pooled connection. SQLx owns
subsequent cleanup with a five-second whole-return bound; pending BEGIN retains
its close-on-drop guard. Cleanup cannot replace an already computed result and
does not establish non-execution or prevent buffered dispatch. Cancellation
during COMMIT remains durability-uncertain; cancellation after acknowledgement
while response delivery is pending can also hide a committed result.
Neither 503 nor the outer 504 guarantees rollback. An identical same-key retry
returns to normal arbitration. Background cleanup, jobs and migration retain
their existing budgets; the HTTP cutoff lowers no global session limit. See
[Persistence Architecture](architecture/persistence.md#query-pool-checkout-and-cancellation)
for the query-pool guarantee and SQLx upgrade conditions.


`http_idempotency_outcomes_total` counts exactly one `outcome` per attempt
and has no other label. An attempt is a request the boundary refused for its
key or body before the handler, or one whose handler reached `execute`; a
request the handler refuses before `execute` records none:

| `outcome` | What happened | Answer |
| --- | --- | --- |
| `executed` | The work committed together with its record. | The 2xx |
| `replayed` | A live record for the same request decided. | The stored 2xx |
| `not_stored` | The operation answered a non-2xx and rolled back, or the request body could not be read or exceeded the limit before arbitration. | That response, 400, or 413 |
| `in_progress` | Another attempt holds the key. | 409 |
| `key_mismatch` | A live record belongs to a different request. | 422 |
| `invalid_key` | The key is missing, repeated, or malformed. | 400 |
| `unavailable` | A transient database fault, an uncertain commit, or an exhausted attempt cutoff. | 503 |
| `internal` | A known non-transient database, query, or record-write fault. | 500 |
| `unstorable` | The operation returned a 2xx the boundary cannot store: a body over 1 MiB, headers over 8 KiB, or a failed body stream. The work rolled back. | 500 |
| `integrity` | A stored record cannot be decoded. | 500 |
| `abandoned` | The attempt was dropped before it answered: the client left or the request timeout fired. | None, or the outer 504 |

`not_stored` is ordinary traffic. `internal`, `unstorable`, and `integrity`
each need an operator or a code change, so alert on them separately from it.
Their log events are `http_idempotency_store_failed` (with `phase`,
`failure_class`, SQLSTATE, and bounded cause),
`http_idempotency_success_not_stored` (with `operation` and `failure`), and
`http_idempotency_integrity_failed`.

The first stored success and replay have byte-exact status/body and preserve
only these response headers, including repeated values and per-name order:
`Content-Type`, `Content-Encoding`, `Content-Language`, `Content-Disposition`,
`Location`, `ETag`, and `Last-Modified`. Their database-native
`http_idempotency_header_pair[]` representation (`name text`, `value bytea`) is
byte-safe. The body is bounded at 1 MiB; the 8 KiB header budget sums
each lowercase name and exact value bytes, including repeated fields. Invalid
or null stored pairs, non-2xx status, and oversized records are integrity
faults and never replay or execute work. Other handler headers are stripped
before storage while current request/trace,
security, and framing headers are generated normally. A replay adds exactly
`Idempotent-Replayed: true`; a fresh success, mismatch, in-progress response,
or failure has no boundary-generated replay header. The header is generated
after the handler returns from private response provenance and is never stored.
Handler-supplied values are stripped, so they cannot impersonate or suppress
the marker.

## Retention, diagnostics, and maintenance

`http_idempotency.retention` is a non-secret duration from one minute through
30 days. It is required only when composition activates the boundary; active
startup also requires PostgreSQL, the admitted schema, and a writable session.
Publish this duration as the retry promise. Expired keys may execute again.

A record keeps the whole success body, up to 1 MiB, as the handler returned
it, for the full retention and without application-level encryption. An
operation whose success carries personal or otherwise sensitive data keeps
that data in `http_idempotency_records` for that long: choose the retention,
database access, and backup policy with that in mind.

The table holds one row for every stored success until it expires: about the
rate of stored successes multiplied by the retention, each row with its body.
Nothing in the boundary limits one caller's share. Every new key from an
authenticated caller keeps up to 1 MiB until it expires, so the bounds on
that growth are the retention, the size of the operation's success body, and
a per-caller rate limit at the gateway, which this template does not provide.
Return an identifier or a small
representation from an idempotent operation, not a large document, and watch
the size with
`SELECT pg_size_pretty(pg_total_relation_size('http_idempotency_records'))`,
which includes the TOAST bodies and both indexes.

Each store attempt uses at most one pooled connection. Replay, mismatch,
and in-progress arbitration use three transaction statements
(`BEGIN`, one lock-and-read, `ROLLBACK`); a stored success uses five plus the
work's statements. The record write is the last statement, so the commit needs
no pre-commit probe, and a connection idle for one second or less is handed
out without a ping. These counts exclude statement preparation. A duplicate never
waits for the holder, but distinct keys executing together compete for the pool; once it is exhausted,
a request waits up to the 3 s acquire budget or its earlier absolute attempt
cutoff and then gets 503 `idempotency_unavailable`. Size
`postgres.max_connections` for concurrent work, replays, the readiness probe,
and one cleanup connection, and keep the work inside `execute` short.

The transaction also buffers the successful response before committing, so
serialization and body production hold its connection and locks. Return a
small bounded response; a long-running operation can commit a durable job
and return its identifier. Moving response capture after COMMIT would lose
the atomic replay guarantee.

While the boundary is active, a background task deletes expired records once
a minute in batches of 500 rows, each its own `READ COMMITTED` transaction
under a 5 s statement timeout, skipping rows a live attempt holds. The level
is named because under a stricter server default a batch would fail with a
serialization error whenever another replica's cleanup or an attempt changed
one of its rows first. A failed run retries on the next tick; it changes
neither readiness nor serving. `http_idempotency_cleanup_runs_total` counts
every run by `outcome` (`completed`, `failed`), and
`http_idempotency_cleanup_removed_records_total` counts the records each
committed batch deleted, so a cleanup that stopped completing or stopped
deleting shows without reading logs. A failed batch logs
`http_idempotency_cleanup_failed` with its phase (`failure`), SQLSTATE, and
bounded cause; a startup check that could not reach a verdict logs
`http_idempotency_startup_check_failed` with the same fields, or
`cause = "timeout"` for its 5 s bound. None of them carries driver text.

Deleted records become dead rows at the rate successes are stored. The table
therefore starts its autovacuum at 5,000 dead rows plus 1% of the table, not
at the server's default 20%, and its TOAST table follows the same values
(`20261002150000_tune_http_idempotency_records_autovacuum.sql`). The values
are table storage parameters: they change no server setting, and an operator
may override them with `ALTER TABLE ... SET` without touching a migration.

Each new record retains verified issuer, caller kind/value, non-secret scope
digest, and expiry, never raw keys, credentials, or request bodies for
diagnosis. A trusted database operator may use bound SQL scoped by all three
caller fields, for example:

```sql
SELECT expires_at, encode(scope_key, 'hex') AS scope_digest
FROM http_idempotency_records
WHERE issuer = $1 AND caller_kind = $2 AND caller_value = $3;
SELECT count(*) FROM http_idempotency_records
WHERE issuer = $1 AND caller_kind = $2 AND caller_value = $3;
DELETE FROM http_idempotency_records
WHERE issuer = $1 AND caller_kind = $2 AND caller_value = $3;
```

Before the delete, block new requests for that caller, drain its in-flight work,
then confirm the caller is quiescent. Deletion deliberately removes replay
protection, so deleted keys must not be retried. There is no HTTP purge API and
no claim of safe concurrent purge.

A 409 diagnostic may expose only scope digest and operation/correlation ID.
Derive the signed advisory bigint from the first eight scope-digest bytes;
`pg_locks` exposes it as unsigned high/low 32-bit `classid`/`objid` with
`objsubid = 1`. Join `pg_locks.pid` to `pg_stat_activity` for application name
and transaction age, then inspect a committed row by full scope digest. A 409
proves an in-flight attempt, not a durable in-progress row, and never exposes
raw key or caller value.

## Roll it out

Apply the migrations with the `migrate` job before the release that composes
the first operation, set `APP__HTTP_IDEMPOTENCY__RETENTION`, publish that
window to clients, then deploy.

Composing an operation that already serves requests protects retries only
once every replica runs the new release: a replica on the old release ignores
`Idempotency-Key`, so a same-key retry that lands there executes again.
Promise the key only after the rollout finishes (compare `app.version` in
each replica's `service_starting` record). A rollback, or removing the route
from `Composer::route`, withdraws the protection at once, even for keys still
inside a published window.

## Mechanism and reopen conditions

The 2026-10-02 library comparison keeps the boundary template-owned:
[`axum-idempotent` 0.4.0](https://crates.io/crates/axum-idempotent/0.4.0)
caches responses in a session store and lets concurrent duplicates reach the
handler, and [`idempotent` 2.0.0](https://docs.rs/idempotent/2.0.0/idempotent/)
keeps leases in a separate store. Neither shares the business write's commit.
[`naidempotency-pgsql` 2.0.0](https://docs.rs/naidempotency-pgsql/2.0.0/naidempotency_pgsql/)
does commit records with business writes through its `natx-pgsql` ambient
transaction. Adopting it would replace this service's explicit borrowed `Tx`
and pool wiring, and its lease/state schema and caller/route identity would
need a compatibility assessment against this replay and retention contract.
Reassess when a maintained adapter supports the caller-owned transaction and
the current contract, or the service adopts that runtime for another reason.

Each attempt is one explicit `READ COMMITTED` transaction on the writer. Its
first statement refuses a recovering or read-only session, takes
`pg_try_advisory_xact_lock` on the scope digest's first eight bytes without
waiting, and reads the record under a snapshot taken before the lock. A live
record decides regardless of the lock, and no record without the lock answers
409. Only with the lock and no record does a second statement read again,
so its snapshot follows the lock and sees a holder that committed in between.
A holder that commits between the first snapshot and a failed lock yields 409,
and the retry replays. The record body is returned only for an equal
fingerprint, so a mismatch neither detoasts nor transfers it. The primary key and
an upsert that replaces only an expired row are the backstop. A duplicate
gets 409 instead of holding a pooled connection while another attempt owns
the key; `REPEATABLE READ` would hide the committed record from it. The shared
pending-BEGIN guard protects SQLx 0.9.0's cancelled-BEGIN defect; native SQLx
owns later transaction drop and bounded connection return. The narrow library
backport is dependency-sensitive;
replace it only with a selected released driver mechanism that preserves the
same cancellation and capacity guarantees and their regression proof.
Reopen for measured harmful 409 churn or an operation that needs stricter
isolation.

The work itself also runs at `READ COMMITTED`. Arbitration protects one key;
different keys can still contend on the same business state. The feature's
adapter must preserve its invariants with atomic conditional statements,
constraints, or row locks. An operation requiring a stronger isolation level
reopens the transaction design before composition.

The fingerprint hashes the received request rather than a typed or canonical
form, so it cannot omit a path, query, or body value; RFC 8785 serializes
numbers as IEEE 754 doubles, so integers above 2^53 would collide. A client
that re-serializes an equal body differently gets 422, a safe refusal.
Reopen when a real consumer needs representation-tolerant retries. Before
composition, check whether an excluded header, such as `If-Match` or a tenant
selector, distinguishes operations the service must refuse to replay. If it
does, request identity must cover that input. Extending the framing changes a
persistent format and needs a compatibility plan for live records.
Keys accept the expired IETF draft's Structured Field string through `sfv` and
Stripe-compatible unquoted visible ASCII. An uncertain commit answers 503 and
is resolved by the client's same-key retry; a post-commit readback would only
turn that rare 503 into a 2xx. Headers are `http_idempotency_header_pair[]`
rather than `jsonb` because their values are bytes.

Deliberate deviations from the draft: only 2xx successes are stored, because
a failed attempt rolls its effect back; the key is scoped to the verified
caller, so an authentication engine is required; retention has no template
default. Cleanup deletes 500-row batches every 60 s under a 5 s statement
timeout; reopen for a backlog one tick cannot drain or for lock waits cleanup
causes. The five-second limit applies to each batch, not the whole run; a run
keeps draining until a batch removes fewer than 500 rows and cancellation can
stop it. Add pacing or a total run budget when measured backlog or maintenance
load competes with requests. Autovacuum thresholds trigger cleanup; they are
not a hard size bound while vacuum is blocked or cannot keep up.

Expiry stays a batched `DELETE` followed by vacuum. Range partitioning on
`expires_at` with partition drops would remove both, but a partitioned
table's primary key must include the partition column, so `scope_key` alone
would stop being unique and the advisory lock would become the only
arbiter; the cleanup's `ctid` is also local to one partition. Reopen for
partitioning when autovacuum cannot keep the table's dead space bounded at
the measured write rate, or when the deletes' WAL volume matters to
replication.


## Performance evidence

Arbitration decodes the stored fingerprint, status, and headers before deciding
a fingerprint mismatch, so their decoding errors still return `Integrity`; the
statement returns the body only for an equal fingerprint. Cleanup consumes each selected
`ctid` under its row lock within the same statement; it retains the 500-row bound,
expiry recheck, `SKIP LOCKED`, transaction boundary, and current five-second
statement hang guard. The historical measurements below used the earlier
one-second guard.

DigitalOcean measurements on 2026-09-28 used c-4 hosts, locked Rust 1.98.1 release
builds, the pinned PostgreSQL 18 image, a four-connection pool, client CPU 0 and
PostgreSQL CPUs 1–3. The work closure was empty to isolate the store's cost.
Each timing cell used three warmup pairs, six baseline-null pairs and twelve
alternating baseline/candidate pairs. A speed claim required at most 5% null
noise, improvement above max(5%, twice that noise), and at least 10/12 positive
pairs. Allocation profiling was separate, using three paired 128/256-operation
slopes. These are synthetic store observations, not endpoint capacity or SLOs.

The original isolated comparisons against `9631b002` qualified both mechanisms:

| Mechanism and workload | Qualified result |
| --- | --- |
| Borrow body for a 1 MiB mismatch | 10.94% lower time; exactly one 1 MiB body copy removed; median peak RSS 14,200 → 12,280 KiB |
| Drain 100,000 expired records beside 100,000 live records | 30.13% lower drain time and 37.94% lower PostgreSQL-container CPU |

The assembled implementation was also compared against `51c3b9e` on a second
host. The store/provider/dependency/toolchain surfaces were unchanged between
these bases. Large mismatch qualified again: 11.28% lower time, 12/12 positive
pairs, 1.73% null noise; the replay and fresh-write controls were neutral.
Allocation slopes saved 1.21–1.31 million requested bytes per large mismatch,
above twice the 260,482-byte null noise. This whole-process range includes SQLx
buffer growth; it is not an assertion of exactly 1 MiB net process savings.

The second host did **not** establish a fresh cleanup speed or CPU claim:

| Expired rows (with 100,000 live rows) | Paired median time reduction | Null noise | Disposition |
| --- | ---: | ---: | --- |
| 0 | +1.11% | 6.78% | Inconclusive |
| 500 | -3.51% | 6.70% | Inconclusive |
| 5,000 | -8.90% | 8.75% | Inconclusive |
| 100,000 | +18.87% | 5.52% | Inconclusive |

Negative reductions mean longer observed times; the noisy controls are not
claimed regression-free. PostgreSQL CPU fell 26.35% in the paired median with
12/12 positive pairs, but 6.61% null noise prevented qualification. One
prospective baseline-only recovery grouped eight independent drains; it failed
its declared noise criteria and admitted no further candidate comparisons.
Cleanup is retained on its earlier qualified, source-identical evidence, with
these later limitations disclosed; isolated gains are never added together.

Cleanup's benefit depends on plan choice and cardinality. The large-backlog
custom plan replaced repeated primary-key probes with a `Tid Scan` and reduced
buffer hits from 3012 to 1512 in one diagnostic batch. Auto mode selected custom
plans in the diagnostic; forced-generic plans used hash joins and did not support
a speedup. Small-backlog isolated controls were neutral. Reopen the performance
assessment for forced-generic plans, materially different statistics, partitioning
(`ctid` is relation-local), or PostgreSQL upgrades. No schema or planner setting
is changed by this optimization.

### Round trips, body copies, and TOAST compression

A second pass on 2026-09-28 measured each hypothesis against `4824ffc` on a
DigitalOcean c-4 pair in lon1 (client and PostgreSQL 18.6 over the VPC,
0.4–0.7 ms RTT), with a store-level load generator and an empty work closure.
Every round ran all variants in a fresh random order; a result is the median
of 6–8 paired rounds, and "n/6" counts rounds that improved. Client CPU is
the process's on-CPU time per attempt, server CPU the database host's busy
time per attempt.

| Change | Workload | ops/s | p50 | Client CPU | Server CPU |
| --- | --- | ---: | ---: | ---: | ---: |
| All code changes together, including the idle-only ping | stored success, 1 KiB, 1 caller | +20.3% (6/6) | −16.7% | −26.6% | −5.9% |
| | stored success, 1 KiB, 32 callers | +12.5% (6/6) | −11.3% | −21.3% | −13.5% |
| | replay, 1 KiB, 1 caller | +54.6% (6/6) | −35.8% | −27.8% | −17.3% |
| | replay, 1 KiB, 32 callers | +37.6% (6/6) | −27.2% | −28.3% | −25.8% |
| | mismatch, 1 KiB | +57.2% (6/6) | −36.7% | −27.8% | −24.2% |
| | in progress | +63.2% (6/6) | −38.6% | −27.0% | −29.9% |
| | mismatch, 1 MiB | ×8.3 (6/6) | −88% | −89% | −94% |
| lz4 instead of pglz | stored success, 64 KiB JSON | +41.9% (6/6) | −31.0% | −0.9% | −61.4% |
| | stored success, 256 KiB JSON | +96.9% (6/6) | −51.8% | −5.9% | −69.1% |
| | stored success, 1 MiB JSON, 4 callers | +126.4% (6/6) | −58.7% | −11.3% | −73.6% |
| | replay, 256 KiB JSON | +23.7% (6/6) | −19.4% | +0.7% | −42.1% |
| | stored success, 256 KiB random bytes | −6.4% (0/6) | +5.0% | −1.9% | +5.9% |

The code changes, each first measured alone:

- **One lock-and-read statement.** Replay, mismatch, and in-progress lose a
  round trip (+15–26% ops/s alone); a stored success keeps two reads.
- **No pre-commit probe after the record write**: the write's success already
  proves the transaction is not aborted. A stored success loses a round trip
  (+8% ops/s, −25% allocations, −8–12% server CPU). Measured with an explicit
  `statement_succeeded` call; the transaction handle now tracks it itself.
- **Ping only idle connections.** Measured here with a 500 ms window; the same
  change landed separately as #110 with pgx's one-second threshold, which is
  equivalent under this load. Every attempt loses a round trip (+6–7% stored
  success, +13–25% decisions).
- **Body only for an equal fingerprint.** A 1 MiB mismatch no longer detoasts,
  sends, or copies the body.
- **`Record::body` is `Bytes`**, shared with the captured response instead of
  copied: −25% allocated bytes per stored success with 256 KiB and 1 MiB
  bodies, with time neutral.

Rejected after measurement: a BRIN index on `expires_at` instead of the btree
(neutral), `STORAGE EXTERNAL` without compression (+57% at 256 KiB JSON, well
behind lz4's +97%, and +6% on random bytes), and a `VOLATILE` SQL function
that re-reads after the lock inside the first statement (+6–8% for a stored
success, but −7% replay and −4% in-progress, and it adds a schema function
whose correctness rests on the per-query snapshot rule). Reopen lz4 if stored
bodies are mostly already compressed (for example `Content-Encoding: br`),
where it cost 6%.
