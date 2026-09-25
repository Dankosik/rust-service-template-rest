# HTTP idempotency

Select `HTTP_IDEMPOTENCY=postgres` (or `--http-idempotency postgres`) during
initialization to retain this pack. It requires `DATABASE=postgres` and an
authentication engine: a key is scoped to a verified caller. The default
`none` removes the boundary, configuration, migration, tests, and this guide.
Retaining the pack performs no query and starts no task until a protected
operation opts in through `Composer::route`.

An opted-in operation gives each verified caller and key at most one committed
participating database effect during the configured retention period. The
stored success is replayed only for the same complete HTTP request; an operation
name is not part of that durable identity.

## Compose a protected operation

`Composer::route(routes!(handler))` is the sole opt-in. It adds the required
`Idempotency-Key` header and generated Problem responses to the route value
that is served. Adopters retain their normal protected-operation security,
success, and business-response declarations; they do not hand-declare
idempotency metadata or validators. The composed operation must be POST, PUT,
PATCH, or DELETE, bearer-only protected, declare at least one 2xx response and
no 1xx/3xx response. `Composer::agree` rejects disagreement between composition
and the assembled OpenAPI document before readiness.

At request time the hardened body limit and deadline apply, authentication runs
before key processing, and ordinary extraction, validation, and authorization
run before `execute` on every attempt. Authorization therefore remains in the
handler outside the work closure: replay never executes that closure. A replay
may be refused by the current authorization policy just as a first attempt may.

```rust,ignore
async fn create_widget(
    idempotency: Idempotency,
    principal: VerifiedPrincipal,
    Extension(widgets): Extension<Arc<dyn CreateWidgets>>,
    Json(input): Json<NewWidget>,
) -> Response {
    if !may_create(&principal) { return forbidden(); }
    idempotency.execute(async |tx: &mut infra_http::idempotency::Tx<'_>| {
        widgets.create(tx, &input).await.into_response()
    }).await
}
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

Only a storable 2xx commits work and the record. A non-2xx work result rolls
back and keeps its ordinary response. A concurrent undecided attempt receives
409 `idempotency_request_in_progress` with `Retry-After: 1`; once a committed
record is visible, equality or mismatch decides the result. A transient
connection/availability fault or uncertain commit returns 503
`idempotency_unavailable` with `Retry-After: 1`. Retry that same request/key:
it can replay, receive 409 while the attempt is live, or execute after rollback.
There is no automatic work retry or post-commit readback. Known non-transient
database/programming/data faults, including SQLSTATE `25P02`, are sanitized 500,
not 503. Logs retain only bounded failure class and SQLSTATE/cause category.
Cancellation drops the transaction; cancellation during COMMIT remains
durability-uncertain, and the outer 504 makes no rollback guarantee. In both
cases an identical same-key retry returns to normal arbitration.

The outcome metric keeps one outcome per attempt and its abandoned-attempt
semantics. It folds the former `outcome_unknown` into `unavailable`, removes
`reconciled`, uses `not_stored` for known internal faults, and retains
`integrity` for corrupt stored records. It adds no high-cardinality labels.

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
security, and framing headers are generated normally.

## Retention, diagnostics, and maintenance

`http_idempotency.retention` is a non-secret duration from one minute through
30 days. It is required only when composition activates the boundary; active
startup also requires PostgreSQL, the admitted schema, and a writable session.
Publish this duration as the retry promise. Expired keys may execute again.

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

The legacy digest-only rows cannot be reinterpreted. The maintenance-only
transition is: quiesce idempotent traffic and cleanup; drain/stop every old
replica; count legacy rows and observe their maximum expiry; wait until database
time passes every live expiry; apply the guarded forward migration; start only
new replicas; verify schema admission and readiness for every replica; then
reopen traffic. The migration takes its lock before checking rows, refuses while
any are live, may replace expired/empty legacy state, and leaves applied
migrations unchanged. Its existing 15 s lock and 5 min total bounds apply.
Do not run old and new implementations together. Before a successful migration,
old binary/schema remain the rollback path; after it, roll forward or use a
separately authorized database recovery, never restart old code. See
[Migrations](../migrations/README.md) and [PostgreSQL validation](validation/postgres.md).
