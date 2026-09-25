# HTTP idempotency

Select `HTTP_IDEMPOTENCY=postgres` (or `--http-idempotency postgres`) during
initialization to retain the idempotency pack. It also requires
`DATABASE=postgres` and an authentication engine (`AUTHN=oidc-jwt` or
`oidc-introspection`): a key is scoped to a verified caller, and
`AUTHN=none` has no verified caller to scope it to. The default `none`
removes the boundary, its configuration section, problem codes, migration,
tests, and this guide. Retaining the pack changes nothing by itself: it makes
no database query, starts no task, and serves every route exactly as before
until an operation opts in by declaring `x-idempotent: true`. After that,
each verified caller gets at most one committed business effect per
operation and key inside the retention window, and a later retry with the
same input gets that effect's success response back.

This assumes you can already add a plain operation — see
[First Production Feature](first-production-feature.md) for that base
workflow. Everything below is what changes once an operation also opts into
this boundary.

## Declare the operation

An operation is idempotent exactly when its generated OpenAPI operation
carries `x-idempotent: true`, and agreement is two-way: it is served through
the boundary if and only if it also carries that extension. No route may be
silently non-idempotent or silently idempotent, and declaring the
`Idempotency-Key` header parameter without `x-idempotent: true` fails
startup the same way, so the contract never advertises a key the service
would ignore. Every idempotent operation must:

1. Set `x-idempotent` to the JSON boolean `true` — nothing else is valid.
2. Use `POST`, `PUT`, `PATCH`, or `DELETE`.
3. Be effectively protected by the assembled OpenAPI policy: normally it
   inherits the retained profile's root bearer requirement. An optional
   `x-security-decision` must agree with that policy; an anonymous alternative
   or public override cannot scope a caller key.
4. Declare the header parameter `Idempotency-Key` exactly once, `required:
   true`, a string schema with `minLength: 1`, `maxLength: 255`, and this
   exact `pattern`:

   ```text
   ^[!#$%&'*+.^_`|~0-9A-Za-z-]+$
   ```

   Other header parameters are still allowed; if the work reads one to pick
   the response, it belongs in the semantic input below. After standard
   HTTP field parsing, which drops surrounding whitespace, a key is compared
   as an exact, case-sensitive byte string: it is never normalized or
   case-folded.
5. Declare at least one 2xx success response and no 1xx or 3xx response, and
   let its 2xx responses declare only the five replayable headers (see "What
   a replay returns").
6. Declare Problem responses at 400, 401, 403, 409, 422, 431, 500, 503, and
   504. In practice, reference
   `infra_http::idempotency::IdempotentOperationProblemResponses` once in
   `responses(...)`, beside your own success response — it expands into
   400, 401, 409, 422, 431, 500, 503, and 504, with the 409 and 503
   components documenting an optional `Retry-After` header. 403 alone may be
   any Problem-shaped response, so the default it includes is enough unless
   you replace it with your own. 413 needs no new work: every operation
   already declares it.

**`operationId` stability.** A key's scope includes the verified issuer, the
verified subject or client ID, and the operation's `operationId`. Renaming an
`operationId` — already a breaking change under
[Compatibility](architecture/http.md#compatibility) — also starts a new key
namespace: a retry keyed under the old name finds no record under the new
one and executes again rather than replaying.

The guide's shape, with the persistence port from "Do the work inside the
transaction" below:

```rust,ignore
// crates/widgets/src/http.rs
#[utoipa::path(post, path = "/widgets", operation_id = "createWidget", tag = "widgets",
    params(infra_http::idempotency::IdempotencyKey),
    request_body = NewWidget,
    extensions(("x-idempotent" = json!(true))),
    responses((status = 201, description = "created", body = Widget),
              infra_http::idempotency::IdempotentOperationProblemResponses))]
pub async fn create_widget(idempotency: Idempotency, principal: VerifiedPrincipal,
                           Extension(widgets): Extension<Arc<dyn CreateWidgets>>,
                           Json(input): Json<NewWidget>) -> Response {
    if !may_create(&principal) { return forbidden(); }          // every attempt, before the seam
    // `CreateWidgets` is the feature's port; its `infra-<provider>` adapter runs the SQL
    // on `infra_idempotency_store::connection(tx)`.
    idempotency.execute(Fingerprint::new(CREATE_WIDGET_V1, &input), async |tx: &mut Tx<'_>| {
        match widgets.create(tx, &input).await {
            Ok(widget) => (StatusCode::CREATED, [(LOCATION, widget.location())], Json(widget)).into_response(),
            Err(err) => err.into_problem().into_response(),  // non-2xx: rolled back, returned unchanged
        }
    }).await
}
```

Run `make openapi-generate` and review the YAML diff as usual
([HTTP Architecture](architecture/http.md#adding-an-operation)). A violated
rule fails startup and `make openapi-check`'s contract tests independently,
so the mistake is never only a runtime diagnostic.

## Choose the semantic input

`execute` takes `Result<Fingerprint, FingerprintError>` directly, so build it
from `Fingerprint::new(version, &input)`: a `NonZeroU32` version constant
declared beside the handler (for example
`const CREATE_WIDGET_V1: NonZeroU32 = NonZeroU32::MIN;` for version 1) and
one `Serialize` value —
usually an operation-owned struct. A serialization failure there is a code
fault, not a client error: `execute` answers a sanitized 500 with no
recorded outcome, never a Problem the caller could act on.

That value must contain every validated, typed request value that
determines the business effect or the response: path, query, and header
parameters and the body, after decoding and default normalization, plus any
request header the work reads to choose the representation (`Accept`,
`Accept-Language`) as a normalized typed value. It must never contain the
`Idempotency-Key` itself, the caller's credentials (the verified caller is
already the scope), `X-Request-ID`, or trace context.

Two attempts are the same request exactly when their versions match and
their inputs encode to the same canonical bytes. Representation-only
differences — JSON member order, whitespace, header order or casing,
request id, trace headers, the credential value — never change that
encoding; a changed included value always does. An unordered collection
serializes in iteration order, which is not deterministic for `HashSet`, so
use `BTreeSet` or a sorted `Vec` for any set-shaped field — never `HashSet`.

**Keep the encoding stable while a record from it can still be live.** Within
one version, the same request must canonicalize identically on every
replica and across restarts and deployments, for as long as the retention
window keeps a record from it live, or an honest retry stops replaying
across a deploy. A change that would alter the encoding of an *unchanged*
request — renaming an included field, encoding a newly added default —
ships only after every record written under the old encoding has expired,
or together with a comparison that still accepts the old encoding:

1. Ship a release that writes the current shape and accepts the new one
   too, via `Fingerprint::new(version, &input).and_then(|f|
   f.also_matching(&new_shape))` — still a `Result<Fingerprint,
   FingerprintError>` you can pass straight to `execute`.
2. Once that release runs everywhere, ship the release that writes the new
   shape and accepts the old one.
3. After one retention period, drop the old acceptance.

Reserve a version bump for a *deliberate* change of meaning instead — a
change where an older request should no longer be treated as the same
request at all. Bumping produces a different fingerprint digest, so a live
record written under the previous version becomes a mismatch: an in-window
retry of an older request gets 422 `idempotency_key_mismatch` until that
record expires, rather than replaying under the new meaning.

## Authorize every attempt before the seam

For an idempotent operation, in order: the existing transport chain, then
authentication, then key validation, then the operation's own request
decoding, validation, and authorization, then arbitration and execution —
the seam, entered only by calling `idempotency.execute(..)`. Decoding,
validation, and authorization run on **every** attempt, including one that
turns out to replay a stored success; a replay never bypasses current
authentication or authorization. Compose them, as the snippet above does,
before the call to `execute`, not only inside the work closure. Extra checks
placed inside the closure are allowed too, and a non-2xx result there rolls
back like any other non-2xx work result, but they run only on an attempt
that actually executes, never on one that replays.

This ordering has a consequence worth designing for: a check that reads
state the committed effect itself changes — a `DELETE` whose authorization
loads the resource being deleted, say — runs before arbitration on every
retry. Once the effect has committed, that same check can now fail (the
resource is gone) and reject the retry with its own response, instead of
ever reaching the record that would otherwise have replayed the original
success. That is by design, not a bug to route around: decide, per
operation, whether a retry should see that rejection or should be authorized
independently of the effect it is retrying.

## Do the work inside the transaction

The closure passed to `execute` receives only `&mut Tx<'_>`, an opaque
handle re-exported as `infra_http::idempotency::Tx`. It has no methods, no
`Deref`, and no conversion to a connection; the only way to reach
`&mut sqlx::PgConnection` is the free function
`infra_idempotency_store::connection(tx)`, which `infra-http` does not
re-export. That shape is deliberate: it keeps a feature crate from reaching
any table through the handle at all, matching the rule that
[a feature crate depends on no transport or provider crate](repo-architecture.md#global-invariants) —
only on `infra-http`'s inbound contract surfaces.

Structure the write the way any other persistence port would
([Persistence Architecture](architecture/persistence.md)): a feature owns
the port, and an adapter maps rows and joins the caller's transaction — here
that transaction arrives as `Tx`, not as `in_tx` directly. The port is an
`async-trait` trait in the feature's own HTTP module (`CreateWidgets`
above), with methods that take `&mut Tx<'_>` and return feature-owned
results; the adapter lives in its own `crates/infra-<provider>` crate, which
depends on the feature (to implement its port) and on
`infra-idempotency-store` (for `Tx` and `connection`) — nothing else new.
Two rules bind that adapter by contract, the same way an `in_tx` closure is
bound today, because `Tx`'s own type cannot enforce them:

- it never ends the transaction with transaction-control SQL (`COMMIT`,
  `ROLLBACK`, `SAVEPOINT`, ...) — the boundary alone decides that, from
  whether the closure's response is 2xx;
- it never names `http_idempotency_records`, the profile's own table; it
  only reads and writes the feature's own tables through the shared
  connection.
  <!-- template:begin jobs:docs-http-idempotency-jobs-enqueue -->

  With the [background jobs](background-jobs.md) pack retained, the adapter
  may also enqueue a job with `infra_jobs::enqueue(connection(tx), ...)`:
  the one template-owned write this rule admits. Such an adapter also
  depends on `infra-jobs`, besides the feature and `infra-idempotency-store`
  (so the "nothing else new" above describes an adapter that enqueues no job).
  The job commits with the success record or not at all (a replayed, refused,
  in-progress, or rolled-back attempt enqueues nothing). The adapter still
  issues no transaction-control SQL and names neither
  `http_idempotency_records` nor `background_jobs`, because the enqueue seam
  owns its statement.
  <!-- template:end jobs:docs-http-idempotency-jobs-enqueue -->

```rust,ignore
// crates/widgets/src/http.rs — the port, feature-owned
#[async_trait::async_trait]
pub trait CreateWidgets: Send + Sync {
    async fn create(&self, tx: &mut Tx<'_>, input: &NewWidget) -> Result<Widget, RepositoryError>;
}

// crates/infra-widgets/src/lib.rs — the adapter, provider-owned
use infra_idempotency_store::connection;

#[async_trait::async_trait]
impl CreateWidgets for WidgetsRepository {
    async fn create(&self, tx: &mut Tx<'_>, input: &NewWidget) -> Result<Widget, RepositoryError> {
        sqlx::query_as::<_, WidgetRow>(
            "INSERT INTO widgets (name, count) VALUES ($1, $2) RETURNING id, name, count",
        )
        .bind(&input.name)
        .bind(input.count)
        .fetch_one(connection(tx))
        .await
        .map(WidgetRow::into_widget)
        .map_err(RepositoryError::from)
    }
}
```

Build that adapter in `service::api::contract()` itself when it holds no
runtime resource of its own — the common case here, since its connection
arrives per call through `Tx` rather than from a pool it opens itself.
Build it in bootstrap instead, and thread it into `contract()`, only when it
does hold one (a second pool, an outbound client, anything bootstrap opens
before readiness); [Integration Boundaries](architecture/integration.md#adding-a-dependency)
owns that split for any adapter, idempotent or not.

The boundary's own transaction is always explicit `READ COMMITTED`; an
operation cannot choose a stricter level in this stage. Reach for row locks
or constraints inside the provided transaction if you need a stronger
invariant than that gives you.

**Side-effect limits.** Only writes made through that connection share the
record's commit fate, including under `synchronous_commit = off`. An
outbound HTTP call (even through the bounded outbound profile, when it is
retained), another datastore, a message, a file, or a spawned task is not
covered: none of it is blocked or wrapped by the boundary, it may run on an
attempt that then rolls back, and it may run again on a retry. An operation
with such a side effect needs its own provider idempotency key derived from
the same request identity, or a separate durable design of its own.

## Compose the route

`idempotency.route(infra_http::routes!(widgets::http::create_widget))`, added inside
`service::api::contract()`, is the one composition call an idempotent
operation needs:

```rust,ignore
// crates/service/src/api.rs, added by the adopter inside contract():
//     .routes(idempotency.route(infra_http::routes!(widgets::http::create_widget)))
```

It retains the documented route carrier while it adds key handling. The final
authentication layer wraps the fully assembled contract, so authentication
precedes key validation without per-operation wrapping. With the pack retained,
`contract()` already takes the `Composer` as a parameter and merges
`idempotency.components()`, so this one line, plus the handler and its port, is
the whole adopter-owned change.

At startup, `Composer::agree` re-checks every declaration rule against the
assembled document in both directions: a composed operation without
`x-idempotent: true`, or a declared `x-idempotent: true` operation never
composed through `route`, fails startup with a sanitized diagnostic before
readiness, and the independent contract test in
`crates/service/tests/openapi.rs` fails the same mismatch under `make test`.
The boundary is active exactly when at least one operation is composed —
`agree` is the only place that decides it.

## Configure retention and watch activation

`http_idempotency.retention` (environment `APP__HTTP_IDEMPOTENCY__RETENTION`)
is a non-secret human-readable duration between 1 minute and 30 days
inclusive; a set value outside that range fails startup naming the key,
whether or not the boundary is active. It has no usable default and is
required only once at least one idempotent operation is composed. Publish
whatever you set as the client-facing retry window — it is the only promise
callers can act on (see "Guide clients through retries").

Once active, startup also requires `postgres.enabled = true`, a present
profile schema (the migration applied — see "Roll it out"), and a writable
session; failing any of these is a startup failure (exit 1, sanitized
diagnostic) before readiness admission, exactly like a missing retention. An
inactive boundary (no composed operation) requires none of this and does no
idempotency-related work at all.

## What a replay returns

A replay, and the first success of the executing attempt, both carry the
stored status, the stored body bytes exactly, and exactly these headers when
the work set them, with the stored values: `Content-Type`,
`Content-Encoding`, `Content-Language`, `Content-Disposition`, `Location`
(the bound constants are `infra_http::idempotency::MAX_STORED_BODY_BYTES`,
1,048,576 bytes, and `MAX_STORED_HEADER_BYTES`, 8,192 bytes, counting
`name.len + value.len + 4` per field). Any other header the work sets on a
2xx is stripped before storage, so the first response never promises
something a replay would omit; the chain still generates the current
request's own `X-Request-ID`, trace context, `nosniff`, and framing headers
for every response, replay or not, and a replay carries no marker header of
its own. The success response is fully buffered — streaming successes are
not supported.

**Response compatibility while records are live.** A replay returns exactly
the bytes captured when the record was written, not a freshly rendered
response. If you change the operation's response shape, a retry of an
in-window key from before the change can still get the old shape back for
as long as that record survives — up to one retention period after the
deploy. Treat a breaking response change on an idempotent operation with
the same care as the encoding rule above: ship it only once every record
written under the old shape has expired, or keep clients tolerant of both
shapes for one retention period.

## Size the pool and let cleanup run

Every request the boundary touches holds one pooled connection: four round
trips for a replay, a 409, a 422, an integrity failure, or an unavailable
answer, and the work's own duration plus five boundary round trips for an
execution. A duplicate with the same key never waits for
the one executing — it gets 409 at once — but distinct keys executing at the
same time do compete for the pool; once it is exhausted, a further request
waits up to the existing
[3 s acquire budget](architecture/persistence.md#budgets) and then gets 503
`idempotency_unavailable`. Size `postgres.max_connections` from your
expected concurrent work, replays, the readiness probe, and one connection
for cleanup below — and keep the work inside `execute` fast, since it
occupies its connection for as long as it runs.

While active, a background task removes expired records once a minute in
batches of 500 rows, repeating until a batch deletes fewer than 500; each
batch runs under its own 1 s statement timeout. It skips a row a live
attempt still holds and never deletes a live record. A failed run logs a
warning naming a failure class, with no data, and the next run retries —
cleanup failures change neither readiness nor serving. The task is spawned
on the existing tracker and is cancelled and joined inside the existing 5 s
background-task
[shutdown stage](architecture/runtime-lifecycle.md#shutdown), same as any
other background task; its own work stays bounded so the pool still closes
inside its own 5 s dependency-close budget.

## Metrics, the startup record, and data custody

`http_idempotency_outcomes_total` (the constant
`infra_http::idempotency::HTTP_IDEMPOTENCY_OUTCOMES_METRIC`) carries one
label, `outcome`, whose closed values are:

| `outcome` | Recorded when |
| --- | --- |
| `invalid_key` | the `Idempotency-Key` header failed validation |
| `unavailable` | the writable primary could not be reached for arbitration, the record write failed before commit, or the commit was rejected with a retryable class |
| `in_progress` | another attempt already holds this scope and key |
| `key_mismatch` | a live record exists under a different fingerprint |
| `replayed` | a live record exists under the same fingerprint; its stored response was returned |
| `integrity` | a live record exists but could not be decoded |
| `executed` | this attempt's work produced a storable success that committed |
| `not_stored` | the work's own non-2xx response, an unstorable 2xx, or a non-retryable commit rejection |
| `reconciled` | the commit's outcome was unknown and a readback found the record this attempt wrote |
| `outcome_unknown` | the commit's outcome was unknown and readback did not resolve it |
| `abandoned` | the request ended with no other outcome: budget expiry, disconnect, or a panic |

Once activation succeeds, bootstrap logs one `http_idempotency_active`
record naming the composed operation count and the configured retention —
the same startup-record style as `service_starting`
([Runtime Lifecycle](architecture/runtime-lifecycle.md)).

**Data custody.** The store keeps only fixed-length one-way digests — one
over the scope and key, one over the fingerprint — plus the stored success
response and its expiry. It never keeps the raw key, the caller identity, or
the request input; a stored success body is retained, as business data, for
the whole retention window. There is no per-caller deletion path: expiry and
the cleanup task above are the only way a record ever leaves the table.

## Guide clients through retries

| Response | Client action |
| --- | --- |
| 409 `idempotency_request_in_progress` (`Retry-After: 1`) | retry with the same key |
| 503 `idempotency_unavailable` (`Retry-After: 1`) | retry with the same key |
| 503 `idempotency_outcome_unknown` (`Retry-After: 1`) | retry with the same key |
| 504 `request_timeout`, or no response at all (a lost connection) | retry with the same key |
| 422 `idempotency_key_mismatch` | reconcile before you resend — see below |
| any other status, including an operation's own business 400, 409, or 422 | keep that operation's ordinary meaning; not an idempotency retry signal |

A 422 `idempotency_key_mismatch` means the key is bound to a different
recorded request: either it was reused for genuinely new input, or the retry
spans a deliberate meaning change on the server side (see "Choose the
semantic input"). The recorded request may already have committed its
effect, so a client must reconcile — check what actually happened — before
it resends under a new key; only once it has done that may it choose to
repeat the effect deliberately. A 400 for the key itself (missing, repeated,
over 255 characters, or containing a byte outside RFC 9110's token
characters, including the draft's quoted form) means the header is
malformed; retrying the same bytes will not help.

## Roll it out

Bring the pack up in this order:

1. **Migrate.** Run the `migrate` job (`/migrate` in the image) before the
   release that composes any operation, so `http_idempotency_records`
   exists first ([Persistence Architecture](architecture/persistence.md#migrations)).
2. **Set retention.** Configure `APP__HTTP_IDEMPOTENCY__RETENTION` (and keep
   `APP__POSTGRES__ENABLED=true`), and publish that value as the client
   retry window before traffic depends on it.
3. **Deploy.** Roll out the release that composes the operation through
   `idempotency.route(..)`. Older replicas mid-rollout answer 404/405 for a
   brand-new route, same as any other new operation.

**Converting an operation that already serves requests.** Rolling out the
release that newly composes an existing operation through `Composer::route`
protects retries only once *every* replica is serving it through the
boundary — an older replica in the same rollout still ignores the
`Idempotency-Key` header entirely, so a same-key retry that happens to land
there executes the effect again. Tell clients the key is honored only after
the rollout has actually finished, not before, because a later rollback, or
a later removal of `x-idempotent: true`, withdraws that protection
immediately — including for a retry still inside a previously published
retention window.

To confirm the rollout has actually finished, check `app.version` in every
replica's `service_starting` startup record (or your deployment platform's
own rollout-completion status) against the release that composes the
operation — not just the operation count `http_idempotency_active` reports,
since a matching count does not by itself prove which release produced it.

## Prove it

Inline tests beside each owning module cover contract-agreement rules,
catalog metadata, key grammar, and the fingerprint and outcome mappings
without a database, and run under `make test` like any other unit test. A
claim about arbitration, rollback, replay, expiry, cleanup, or a commit
outcome needs a real PostgreSQL and runs, for both the store-level and the
HTTP-mounted suites under `test/tests/http_idempotency/`, with:

```bash
ALLOW_HEAVY=1 make test-integration-db
```

[PostgreSQL Validation](validation/postgres.md) owns that command and its
Docker requirement. The HTTP-mounted suite runs only where the retained
authentication engine exports a real verifier fixture — today,
`AUTHN=oidc-introspection` — because it drives requests through the hardened
chain with that verifier. It is absent from a JWT-only output, which still
runs every engine-independent boundary test and the full store-level suite.

## Mechanism and reopen conditions

The boundary is template-owned because no maintained crate commits the
replay record inside the caller's PostgreSQL transaction: `axum-idempotent`
0.4.0 caches responses in a session store, lets concurrent duplicates reach
the handler, and fails open, while `idempotent` 2.0.0 keeps leases in a
separate store. Reassess when a maintained crate joins the caller's
transaction.

Each attempt is one explicit `READ COMMITTED` transaction on the writer. Its
first statement refuses a recovering or read-only session and takes
`pg_try_advisory_xact_lock` on the first eight bytes of the scope digest,
which never waits; a live record decides before the lock result. A duplicate
gets 409 instead of waiting, because with `sqlx` 0.9 a dropped waiting
request keeps its pooled connection busy until the server statement ends.
Stricter isolation is not offered: under `REPEATABLE READ` the committed
record stays invisible to a duplicate. Reopen for a `sqlx` release that
cancels server statements on drop, measured harmful 409 churn, a false 409
that no lock holder explains, or an operation that needs stricter isolation.

An unknown commit is read back on a fresh writer connection within the
request deadline minus a 100 ms reserve; reopen if healthy-writer readback
latency exceeds that reserve. Cleanup deletes 500-row batches every 60 s under
a 1 s statement timeout; reopen for a backlog one tick cannot drain or for
lock waits that cleanup causes. Canonical encoding 1, its digest domains, and
stored success format 1 (1 MiB body, 8 KiB replayable headers) are fixed while
records are live; changing one needs a contract change with a compatibility
plan.

The contract follows the expired IETF `Idempotency-Key` draft (revision 07)
where it is sound and deviates deliberately: keys are unquoted tokens (the
structured-field string form is refused), only 2xx successes are stored
because a failure rolls the effect back, the scope is the verified caller and
`operationId` with no resource or tenant refinement, an authentication engine
is required, and retention has no template default. Reopen on the draft's
publication as an RFC, a consumer that needs quoted keys or replayed business
rejections, a real per-tenant key namespace, or a new verified-caller
mechanism. Reassess the database-backed proof when `sqlx` or the proof image's
PostgreSQL major version changes. The Definition, design, and plan comparison
are preserved in the stage-10.3 implementation commit.
