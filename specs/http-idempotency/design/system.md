# HTTP idempotency: system design

Status: ready. Owner: stage-10.3 Technical Design. Behavior authority:
[Specification](../spec.md) (ready), [Intent](../intent.md),
[research synthesis](../research/synthesis.md), and the Definition
[transition](../definition-transition.md) checklist. Baseline:
`d24d1737d1647d9bfdf2b15ecb258630ec9cd326`. `main` has since moved to
`ac6a6ffe248cf6dac5232438876c564030316ed1`, which only makes
`Dsn::admit_with_environment` crate-private; no design input changes.
Placement, markers, and file-level owners are in the
[ownership map](ownership.md).

Nothing here changes an observable rule of the specification. Where a rule
needs a mechanical form, the form is named with the rule it enforces. Paths
that do not exist yet are written as code, not links.

## 1. Drivers

- One committed business effect per verified caller, `operationId`, and key
  inside the retention window, across replicas, with the effect and its replay
  record in one PostgreSQL transaction.
- Non-waiting arbitration (D1): a duplicate never waits while holding a pooled
  connection; concurrent replays never get 409.
- Fail closed on writer uncertainty (D6); an unknown commit is its own outcome
  and is never re-executed inside one request.
- Replay bytes equal the first success; only five headers survive.
- Inert until an operation opts in; `HTTP_IDEMPOTENCY=none` output equals the
  baseline contract and `Cargo.lock` for the same selection.
- Existing owners stay unchanged: the hardened chain, `protect`, `in_tx`, commit
  classification, budgets, readiness, and the migration rules.

## 2. Decisions and alternatives

| Decision | Selected | Strongest rejected alternative | Decisive constraint | Cost accepted | Reverses when |
| --- | --- | --- | --- | --- | --- |
| Placement | This follows the authentication split. `infra-http` gains a marker-scoped module `idempotency` holding the inbound composition: declaration checks, the composer, key handling, scope and fingerprint digests, the executor seam, stored-success encoding, Problem mapping, the metric, and the OpenAPI helpers. A new provider crate `infra-idempotency-store` owns the PostgreSQL record store: arbitration, the execution transaction, readback, startup check, and cleanup. It knows nothing about HTTP. `infra-http` also gains the four catalog codes. | One pack crate that feature handlers depend on directly | A feature would then depend directly on a crate that owns SQL. That contradicts the portable ownership rules (`rust-structural-quality`; the Rust Code / Ownership Design completion criterion) and triggers the spec's conditional reopen. The accepted precedent is `infra-http -> infra-bearerauthn`: `infra-http` owns composition, extractor, and Problem mapping, and the provider crate owns the mechanism. Features already consume `VerifiedPrincipal` this way. A store module inside `infra-postgres` is rejected because that crate owns no schema. | `infra-http` grows by one removable module directory and two marker-scoped dependencies; the store's interface is generic over the rollback payload. | A consumer needs a non-HTTP replay store (the schema's 2xx status and header blob are HTTP-shaped), or the portable ownership rules change. |
| Response components | The idempotent response family (four new components and `IdempotentOperationProblemResponses`) lives with the seam's other contract types in `idempotency/openapi.rs`, and `Composer::components` registers it through the pre-wired `contract` parameter. | `infra_http::problem::responses` plus `ProblemComponents`, as authentication did | This keeps every pack contract type in the pack's removable directory. It gives the `contract` parameter a real use before any operation exists; otherwise it would be unused and fail `make lint` in every retained output. Test routers of any state can merge the components, which they cannot do through the `pub(crate)` `ProblemComponents`. | A second registration path. The rule is documented in the `responses` module doc, `docs/architecture/http.md` step 3, and First Production Feature. | A second pack needs a shared family, or `ProblemComponents` becomes public. |
| Arbitration | A transaction-scoped advisory try-lock, taken in the first statement after `BEGIN ISOLATION LEVEL READ COMMITTED` and gated by a writer check, then a separate record read. A visible live record decides first; the lock result matters only when no live record is visible. | `INSERT` reservation with a short `lock_timeout` | `pg_try_advisory_xact_lock` reports held or free without waiting or a timer. A timer-based signal turns unrelated lock waits (cleanup batches, extension locks) into false 409s. Probe A shows the next statement sees a record committed before the lock was acquired. | A replay takes and releases one in-memory lock: four round trips for a replay, five plus the work's own statements for an execution. | Measured lock-table pressure, or a PostgreSQL change to advisory-lock release order. |
| Read order | The lock statement, then the read statement. | Read, try-lock, re-read | Equivalent outcomes: under read committed, a record committed between two statements is visible to the later one. Lock-first needs only one read. The Definition's "reads first" is kept as decision precedence: the read decides before the lock result. | None observable. | — |
| Isolation | The boundary transaction is always explicit `READ COMMITTED`; operations cannot choose another level in this stage. | Let an operation pick `REPEATABLE READ` or `SERIALIZABLE` | Probe B: under `REPEATABLE READ` the lock can be acquired after a holder committed while the committed record stays invisible to the older snapshot. The work would run and then fail with `23505` or `40001`. Under read committed the work never starts in that case. | An operation that needs stronger invariants uses row locks or constraints inside the provided transaction. | A real operation needs snapshot isolation. Then add an in-transaction reservation upsert right after the lock, which turns the race into a pre-work `40001`/`23505` → 503, and prove it per level. |
| Fingerprint encoding | A template-owned canonical encoding over `serde::Serialize` (section 8), digested with SHA-256 | `serde_json_canonicalizer` 0.3.2 or `serde_jcs` 0.2.0 (RFC 8785) | Both format every integer through `f64` (`value as f64` in their serializers), so distinct integers above 2^53 collide. That breaks "a change to any included value always changes it". `bcs` 0.2.1 has no floats and encodes enum variants by index. `postcard` 1.1.3, `ciborium` 0.2.2, and `bincode` keep map iteration order, which is nondeterministic for `HashMap`. | About 250 lines of serializer plus pinned vectors. | A maintained crate offers injective integers and floats, sorted maps, and a fixed specification. |
| Digest crate | `sha2` 0.11.0 with `default-features = false`, a new direct dependency of `infra-http` inside the profile markers | `aws-lc-rs` digest, or PostgreSQL `sha256()` | `sha2` 0.11.0 is already resolved with the same features through `sqlx-postgres` in every `DATABASE=postgres` graph, so no package or feature edge is added. The initializer's JWT lock projection guards `aws-lc-rs` features. Hashing in PostgreSQL would send raw keys and identities to the server and its statement logs. | One workspace declaration. | `sqlx-postgres` stops using `sha2`, which would add a package to postgres graphs. |
| Commit outcome | The store reuses `infra_postgres::in_tx_with` unchanged. `CommitFailed` with `retryable` becomes `CommitRejected { retryable: true }` and 503 `idempotency_unavailable`. Any other `CommitFailed` becomes a 500. `CommitUnknown` triggers a writer readback on a fresh pooled connection, bounded by `RequestDeadline` minus a 100 ms reserve. | A pack-owned transaction wrapper | The spec keeps `in_tx` and commit classification unchanged, and its variants already carry the distinction. | Replays and refusals roll back through the same 3 s rollback bound. | `in_tx` gains an outcome that cannot be mapped. |
| Deferred persistence tooling | `query!`, offline `.sqlx` metadata, and `sqlx-cli` stay deferred, retargeted to the first feature-owned repository. Per-query spans stay deferred. No migration-history exemption. | Adopt `query!` for the store's statements now | Section 6.4. | Statement types are checked by the real-database suite, not at compile time. | The first feature-owned repository, or a store statement the retained suite does not execute. |
| Test seams | Store tests use raw scope digests, because the store has no notion of callers; only `infra-http`'s key layer derives a scope, and only from a verified principal. Lost acknowledgements are injected by a test-only wire proxy around a real commit. No new `test-support` feature and no JWT fixture. | A scope or principal constructor behind a feature | Section 11.2. | HTTP-level database proof (P9) runs only where the introspection fixture exists. | The introspection engine is removed or a JWT-only mounted proof is required. |
| Graph proof | Runtime graphs 13–16 run their retained idempotency database suite after build and test. | Compile-only for the retained database suite | The spec requires P9 to run in each retained graph whose engine has a fixture, and the JWT graphs to run every engine-independent boundary test. P1–P9 are real-PostgreSQL claims. | Graphs 13–16 need Docker. | The spec narrows the graph claim. |

## 3. Dependency direction

A feature's HTTP module keeps exactly the dependency it has today. First
Production Feature lets `crates/<feature>/src/http.rs` use `infra-http`, and
the authentication profile already adds `VerifiedPrincipal` to what features
consume there. The idempotency seam arrives the same way, as
`infra_http::idempotency::{Idempotency, Fingerprint, Tx, IdempotencyKey,
IdempotentOperationProblemResponses}`. `Tx` is re-exported from the store
crate. No feature depends on `infra-idempotency-store`, `infra-postgres`,
`sqlx`, or any other provider crate.

The portable texts that say features depend on no transport or provider
(`.agents/skills/rust-structural-quality/SKILL.md` line 10,
`.agents/skills/rust-coder/SKILL.md` line 10, and the completion criterion of
Rust Code / Ownership Design) keep the reading that First Production Feature
and the authentication profile already rely on:
`infra-http`'s inbound contract surfaces are allowed, and the transport
mechanism (chain, server) and provider crates are not. Project Structure's
placement steps already state this reading: feature crates depend on `axum`,
`utoipa`, and `infra-http`, "but never on `infra_http::harden`,
`infra_http::Server`, or a provider crate". Today they name `infra-http`
"for the problem catalog and shared responses"; this stage widens that clause
to its inbound contract surfaces, and `VerifiedPrincipal` already falls under
it. The retained profile
adds no new feature-to-crate edge, so the spec's conditional reopen for
portable instructions is not triggered. The claim is scoped to the crate
graph. `Tx` is a provider-defined opaque handle re-exported unchanged,
whereas authentication wraps `Principal` in `VerifiedPrincipal`. The
re-export is the minimal choice: `Tx` gives its holder nothing but the right
to pass it on, so a wrapper would add only a `sqlx` edge to `infra-http`.

Where the operation's SQL lives:

- A feature's business module (`lib.rs`) names no transport, provider, or
  `infra-http` type. Its persistence port stays feature-owned (Persistence).
- The work receives `&mut Tx<'_>`, an opaque handle. It has no public
  methods or associated functions and implements no `Deref`, and no
  conversion trait (`From`, `Into`, `AsRef`, `AsMut`, `Borrow`, `BorrowMut`)
  leads from it to the connection. It derives only `Debug`, as the workspace
  lints require. Only the store's free function
  `infra_idempotency_store::connection(&mut Tx<'_>)` yields
  `&mut sqlx::PgConnection`, and `infra-http` does not re-export it. A crate
  therefore reaches the connection only by depending on
  `infra-idempotency-store`, as the repository adapter does and a feature
  crate never does. Anything reachable through `Tx`'s own path would not hold
  that line, because `infra-http` re-exports that path and `PgConnection`'s
  inherent `copy_in_raw` and `copy_out_raw` (`sqlx-postgres` 0.9.0
  `src/copy.rs`) need no import: a feature could read or write any table
  through them. Probe 1's fourth run confirms the opaque form.
- As Persistence and Integration Boundaries already record, that adapter
  implements the feature's port and maps rows into feature types. It
  therefore depends on the feature. It lives in a `crates/infra-<provider>`
  crate that depends on the feature and on `infra-idempotency-store` (for
  `Tx` and `connection`). `service::api` only injects it, as
  `Extension<Arc<dyn Port>>` on the route. The port is an `async-trait`
  trait in the feature's HTTP module whose methods take `&mut Tx<'_>` and
  return feature-owned results. `async-trait` is already a workspace
  dependency. Probe 1 compiled this shape with a `Send` handler. The
  adapter can run any statement on that connection. Like an `in_tx` closure
  today, it is held by contract rather than by type to two rules the guide
  states: it never ends the transaction with transaction-control SQL, and it
  never names the profile table.
- Reverse edges: a crate that a feature depends on never depends on that
  feature, and the compiler refuses the cycle. This covers `infra-http`, the
  store, and everything under them. Provider adapters that map into feature
  types may depend on the feature, as Persistence and Integration Boundaries
  already state.

Crate edges added:

- `infra-http -> infra-idempotency-store, sha2`, marker-scoped. `infra-http`
  already depends on `infra-bearerauthn` whenever authentication is retained,
  which the pack requires.
- `infra-idempotency-store -> infra-postgres, sqlx, tokio, tokio-util,
  tracing, thiserror`. No HTTP crate.
- `service -> infra-idempotency-store`, marker-scoped, for `Store::new`.
- `integration-tests` gains dev-dependencies on `infra-idempotency-store`,
  plus the P9-only set (ownership map).

No crate gains an edge to a feature, and no infra crate depends on
`service`.

## 4. Rust API

**`infra_http::idempotency`** (inbound composition and the handler seam):

```rust
pub const HTTP_IDEMPOTENCY_OUTCOMES_METRIC: &str = "http_idempotency_outcomes_total";
pub const MAX_STORED_BODY_BYTES: usize = 1_048_576;
pub const MAX_STORED_HEADER_BYTES: usize = 8_192;
pub use infra_idempotency_store::Tx;

/// Declaration checks and composition for idempotent route tuples.
pub struct Composer { /* store, verifier, composed operationIds, recorded failures */ }
impl Composer {
    pub fn new(store: infra_idempotency_store::Store, verifier: infra_bearerauthn::Verifier) -> Self;
    pub fn inert() -> Self; // Store::inert() + Verifier::disabled(): document rendering and tests
    pub fn route<S: Clone + Send + Sync + 'static>(&mut self, routes: UtoipaMethodRouter<S>) -> UtoipaMethodRouter<S>;
    pub fn components<S: Clone + Send + Sync + 'static>(&self) -> OpenApiRouter<S>;
    pub fn agree(self, document: &utoipa::openapi::OpenApi) -> Result<Activation, AgreementError>;
}
pub enum Activation { Inactive, #[non_exhaustive] Active { store: infra_idempotency_store::Store, operations: NonZeroUsize } }
pub struct AgreementError { /* operation label, static rule */ } // "idempotent operation contract is invalid: {operation}: {rule}"

/// Extractor, present only on routes composed by `Composer::route`.
pub struct Idempotency { /* attempt */ }
impl Idempotency {
    pub async fn execute<W, R>(self, fingerprint: Result<Fingerprint, FingerprintError>, work: W) -> Response
    where W: AsyncFnOnce(&mut Tx<'_>) -> R, R: IntoResponse;
}

pub struct Fingerprint { /* version, current digest, accepted equivalent digests */ }
impl Fingerprint {
    pub fn new<T: Serialize + ?Sized>(version: NonZeroU32, input: &T) -> Result<Self, FingerprintError>;
    pub fn also_matching<T: Serialize + ?Sized>(self, equivalent: &T) -> Result<Self, FingerprintError>;
}
pub struct FingerprintError; // static Display only; `execute` renders it as a sanitized 500 with the request id

#[derive(IntoParams)] pub struct IdempotencyKey { /* header Idempotency-Key, exact schema */ }
#[derive(IntoResponses)] pub enum IdempotentOperationProblemResponses { /* section 5 */ }
// response components: IdempotencyBadRequest, IdempotencyRequestInProgress,
// IdempotencyKeyMismatch, IdempotencyUnavailable
```

**`infra_idempotency_store`** (PostgreSQL record store; no HTTP types):

```rust
/// Cloning shares the pool.
#[derive(Clone, Debug)] pub struct Store { /* Option<Arc<{ PgPool, retention }>> */ }
impl Store {
    pub fn new(pool: PgPool, retention: Duration) -> Self;                 // no I/O; keeps whole microseconds (6.2)
    pub fn inert() -> Self;                                                  // see "Inert store" below
    pub async fn attempt<W, T>(&self, scope: &ScopeKey, accepted: &[Digest], work: W) -> Attempted<T>
    where W: AsyncFnOnce(&mut Tx<'_>) -> WorkOutput<T>;
    pub async fn read_back(&self, scope: &ScopeKey, accepted: &[Digest]) -> ReadBack;
    pub async fn check_startup(&self) -> Result<(), StartupError>;           // schema shape + writable session
    pub async fn remove_expired(&self) -> Result<u64, CleanupError>;        // drains in bounded batches
    pub async fn run_cleanup(self, cancel: CancellationToken);               // periodic task body
}
pub type Digest = [u8; 32];                    // seam files import the hashing trait as `sha2::Digest as _`
pub struct ScopeKey(Digest);                   // from_digest(Digest); the advisory lock key is derived inside
pub struct Record { pub fingerprint: Digest, pub format: i16, pub status: i16, pub headers: Vec<u8>, pub body: Vec<u8> }
pub enum WorkOutput<T> { Commit(Record), Rollback(T) }
pub enum Attempted<T> {
    Unavailable,                               // not writable; acquire, begin, or statement failure; inert
    Live { matched: bool, record: Record },    // a live record; matched = fingerprint ∈ accepted
    InProgress,                                // no live record; the key is held elsewhere
    RolledBack(T),                             // the work asked for rollback
    WriteFailed,                               // the record write failed before COMMIT
    Committed(Record),                         // commit acknowledged
    CommitRejected { retryable: bool },        // TxError::CommitFailed
    CommitUnknown,                             // TxError::CommitUnknown
}
pub enum ReadBack { Found { matched: bool, record: Record }, Absent, NotWritable, Failed }
pub struct Tx<'c> { /* &'c mut PgConnection; derives only Debug; no methods, associated functions, Deref, or conversions (section 3) */ }
pub fn connection<'a>(tx: &'a mut Tx<'_>) -> &'a mut sqlx::PgConnection;   // adapters only; not re-exported by infra-http
pub enum StartupError { SchemaMissing, NotWritable, Unavailable }                // static Display
pub enum CleanupError { Acquire, Begin, Statement, Commit }                     // class only
```

**Inert store.** `Store::inert()` holds no pool and never does I/O:
`attempt` returns `Unavailable`, `read_back` returns `Failed`,
`check_startup` returns `Err(StartupError::Unavailable)`, `remove_expired`
returns `Err(CleanupError::Acquire)`, and `run_cleanup` returns at once.
In production none of these runs: an inert store renders the document
(`Composer::inert()`), or stands in when the pool or the retention is absent,
and then the boundary is inactive or activation refuses at
`required_retention` before any store call (section 10).

Feasibility is measured (section 14, probe 1). An axum handler whose future
must be `Send` calls `execute` with an async closure over `&mut Tx<'_>`,
including through the port shape of section 3.
`execute` wraps that closure in a second async closure that buffers the
response and returns `WorkOutput`. The store runs it inside an
`in_tx_with`-shaped `AsyncFnOnce(&mut PgConnection)` helper and maps the
result to `Attempted<T>`.

The guide's feature path, which P9 composes exactly, including the
`async-trait` port and its adapter:

```rust
// crates/<feature>/src/http.rs
#[utoipa::path(post, path = "/widgets", operation_id = "createWidget", tag = "widgets",
    params(infra_http::idempotency::IdempotencyKey),
    request_body = NewWidget,
    security(("bearerAuth" = [])),
    extensions(("x-security-decision" = json!({"exposure": "protected", "rationale": "..."})),
               ("x-idempotent" = json!(true))),
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

// crates/service/src/api.rs, added by the adopter inside contract():
//     .routes(idempotency.route(routes!(widgets::http::create_widget)))
```

## 5. Declaration and agreement

`Composer::route` validates one `routes!` tuple (one path, one method) and
records the `operationId`. Rule 3 is `protect`'s own acceptance of that
tuple, so the pack never duplicates `protect`'s private rules. `Composer::agree`
re-checks rules 1, 2, 4, 5, and 6 for every composed operation on the
assembled document and adds the document-wide rules. `agree` reports both a
failure `route` recorded and a failure it finds itself as `AgreementError`,
naming the operation and one static rule.

A tuple that fails validation is still merged, but answers a sanitized 500
to every request, so it is never served silently; `agree` then refuses.
`protect` consumes its argument, so `route` keeps a clone of the tuple for
that fail-closed form. The contract test in `crates/service/tests/openapi.rs`
checks the same rules with an independent JSON walk over the generated
document.

Per idempotent operation (spec rules 1–6):

1. `x-idempotent` is the JSON boolean `true`.
2. The method is POST, PUT, PATCH, or DELETE.
3. `infra_http::protect` accepts the tuple: `x-security-decision` protected
   with a rationale, explicit bearer-only security, and the protected family.
4. There is exactly one header parameter named `Idempotency-Key`, compared
   case-insensitively. It has `required: true`, schema `type: string`,
   `minLength: 1`, `maxLength: 255`, and `pattern`
   ``^[!#$%&'*+.^_`|~0-9A-Za-z-]+$`` byte-for-byte.
5. At least one 2xx response, no 1xx or 3xx, and 2xx responses declare
   headers only from Content-Type, Content-Encoding, Content-Language,
   Content-Disposition, and Location, compared case-insensitively.
6. Problem responses at 400, 401, 409, 422, 431, 500, 503, and 504 are
   exactly the components `IdempotentOperationProblemResponses` declares
   (table below). 403 is any `application/problem+json` response with the
   `Problem` schema. The 409 and 503 components declare a `Retry-After`
   header that is not required.

   Requiring those components is how rule 6's description clause becomes
   checkable. Their descriptions are written to cover every Problem that
   authentication or the boundary returns at that status, and the operation's
   own outcomes at 400, 409, and 422.

| Status | Component in `IdempotentOperationProblemResponses` | Covers |
| --- | --- | --- |
| 400 | `IdempotencyBadRequest` (new) | `authentication_malformed`, `bad_request` for the key, the operation's own 400 |
| 401 | `AuthenticationUnauthorized` | authentication |
| 403 | `AuthenticationForbidden` (replaceable) | the operation's authorization |
| 409 | `IdempotencyRequestInProgress` (new, optional `Retry-After`) | `idempotency_request_in_progress`, the operation's own conflict |
| 413 | `RequestEntityTooLarge` | transport; every operation already declares it |
| 422 | `IdempotencyKeyMismatch` (new) | `idempotency_key_mismatch`, the operation's own 422 |
| 431 | `AuthenticationOversize` | authentication |
| 500 | `InternalServerError` | sanitized `internal_error` from the boundary, wiring faults, panics |
| 503 | `IdempotencyUnavailable` (new, optional `Retry-After`) | `authentication_unavailable`, `idempotency_unavailable`, `idempotency_outcome_unknown` |
| 504 | `AuthenticationTimeout` | `request_timeout` |

**Expected values.** `openapi.rs` owns the key's header name, pattern, and
length bounds and the four component names as `pub(super)` constants, and
`declaration.rs` checks against them. utoipa 5.5.0 accepts only literals for
`pattern`, `min_length`, and `max_length` (`utoipa-gen`
`component/features/validation.rs`), so the `IdempotencyKey` derive repeats
them and an inline test pins the generated schema to the constants. The
contract test keeps its own literals as an independent oracle.

Document-wide rules in `agree`:

- any `x-idempotent` value other than `true` is invalid;
- the set of operations declaring `x-idempotent: true` equals the set
  composed through `route`;
- no other operation declares an `Idempotency-Key` header parameter;
- the four new components are present.

`agree` returns `Active` with the store when at least one operation is
composed, and `Inactive` otherwise. This is the only activity detection.

`Composer::components` is the family's only registration path (section 2).
With the pack retained and no idempotent operation, the generated contract
gains exactly the four unreferenced response components, as the authentication
components already appear unreferenced. Redocly reports them under
`no-unused-components`, which `.redocly.yaml` sets to warn.

## 6. PostgreSQL

### 6.1 Schema

One forward-only migration,
`migrations/20260923000001_create_http_idempotency_records.sql`, retained
only with the pack:

```sql
CREATE TABLE http_idempotency_records (
    scope_key   bytea       NOT NULL PRIMARY KEY CHECK (octet_length(scope_key) = 32),
    fingerprint bytea       NOT NULL CHECK (octet_length(fingerprint) = 32),
    format      smallint    NOT NULL CHECK (format > 0),
    status      smallint    NOT NULL CHECK (status BETWEEN 200 AND 299),
    headers     bytea       NOT NULL,
    body        bytea       NOT NULL,
    expires_at  timestamptz NOT NULL
);
CREATE INDEX http_idempotency_records_expires_at ON http_idempotency_records (expires_at);
```

The table stores digests and the stored success only. `format` names the
stored-success encoding (1 in this stage). Size bounds are enforced by
`infra-http` before the write and re-checked on decode; they are not table
constraints. Only `infra-idempotency-store` names the table.

### 6.2 Statements

The store uses runtime `sqlx::query` with typed binds; there is no dynamic
SQL. `$lock` is the first eight bytes of the scope digest read as a
big-endian `i64`.

| Use | Statement |
| --- | --- |
| Writer check and lock | `SELECT NOT pg_is_in_recovery() AND current_setting('transaction_read_only') = 'off' AS writable, CASE WHEN pg_is_in_recovery() OR current_setting('transaction_read_only') = 'on' THEN false ELSE pg_try_advisory_xact_lock($lock) END AS acquired` |
| Read | `SELECT fingerprint, format, status, headers, body FROM http_idempotency_records WHERE scope_key = $1 AND expires_at > statement_timestamp()` |
| Write (after the work) | `INSERT INTO http_idempotency_records AS r (scope_key, fingerprint, format, status, headers, body, expires_at) VALUES ($1, $2, $3, $4, $5, $6, statement_timestamp() + $7) ON CONFLICT (scope_key) DO UPDATE SET fingerprint = EXCLUDED.fingerprint, format = EXCLUDED.format, status = EXCLUDED.status, headers = EXCLUDED.headers, body = EXCLUDED.body, expires_at = EXCLUDED.expires_at WHERE r.expires_at <= statement_timestamp()`; exactly one affected row is required |
| Readback (fresh connection) | `SELECT NOT pg_is_in_recovery() AND current_setting('transaction_read_only') = 'off' AS writable, r.fingerprint, r.format, r.status, r.headers, r.body FROM (SELECT 1) AS one LEFT JOIN http_idempotency_records AS r ON r.scope_key = $1 AND r.expires_at > statement_timestamp()` |
| Startup check (in a read-committed transaction) | `SELECT NOT pg_is_in_recovery() AND current_setting('transaction_read_only') = 'off' AS writable, (SELECT count(*) FROM (SELECT scope_key, fingerprint, format, status, headers, body, expires_at FROM http_idempotency_records LIMIT 0) AS shape) AS shape_rows` |
| Cleanup batch (own transaction, `SET LOCAL statement_timeout = '1000ms'`) | `DELETE FROM http_idempotency_records WHERE scope_key IN (SELECT scope_key FROM http_idempotency_records WHERE expires_at <= statement_timestamp() ORDER BY expires_at LIMIT 500 FOR UPDATE SKIP LOCKED) AND expires_at <= statement_timestamp()` |

`$7` binds `std::time::Duration` as an `interval`. `sqlx-postgres` 0.9.0
refuses to encode a `Duration` with a sub-microsecond part
(`types/interval.rs`), and the configuration's humantime parser accepts
nanosecond units. `Store::new` therefore drops any sub-microsecond part of the
retention once, after configuration has checked the value as written against
its range. The database clock resolves 1 µs, so no expiry changes
observably. The write statement's own time on the database clock fixes the
expiry. The startup check maps SQLSTATE `42P01` or `42703` to
`SchemaMissing` and `writable = false` to `NotWritable`. Anything else,
including its 5 s bound, maps to `Unavailable`.

### 6.3 Arbitration and execution sequence

**Store** (`Store::attempt`), inside
`in_tx_with(pool, TxOptions { isolation: ReadCommitted, read_only: false }, ..)`:

1. Writer check and lock. If not writable → `Unavailable`.
2. Read:
   - a live record → `Live { matched, record }`;
   - no live record, lock acquired → run the work;
   - no live record, lock not acquired → `InProgress`.

   Every outcome except the work run returns an error from the closure, so
   `in_tx_with` rolls back a transaction that wrote nothing.
3. The work returns `WorkOutput`. `Rollback(T)` rolls back →
   `RolledBack(T)`. `Commit(record)` writes the record. A write error, or an
   affected-row count other than one, → `WriteFailed` after rollback.
4. `in_tx_with` commits:
   - `Ok` → `Committed(record)`;
   - `CommitFailed` → `CommitRejected { retryable: retryable(err) }`;
   - `CommitUnknown` → `CommitUnknown`;
   - `Acquire`, `Begin`, or a failed statement → `Unavailable`.

The store never retries or re-runs the work.

**HTTP seam** (`Idempotency::execute`):

1. Sets the attempt's "seam used" flag. If the fingerprint is `Err`, it
   answers at once with a sanitized 500 `internal_error` that carries the
   request id. That is a code fault, like wiring faults: it records no outcome
   and skips the 504 check below. Otherwise it creates the outcome drop guard.
2. Calls `Store::attempt` with the scope, the fingerprint's accepted digests
   (current first), and a wrapper closure. The wrapper runs the handler's work.
   - A non-2xx response returns `Rollback(response)` and goes back unchanged.
   - For a 2xx response, `stored.rs` captures it. It buffers the body through
     `http_body_util::Limited` at `MAX_STORED_BODY_BYTES`, keeps the five
     replayable headers in `HeaderMap` order, and drops every other header.
   - A body stream error, a body over 1 MiB, or headers over 8 KiB (sum of
     `name.len + value.len + 4`) → `Rollback(Unstorable)`.
   - Otherwise it returns `Commit(format-1 record with the current fingerprint)`.
     The captured stored form (status, filtered headers, body) is kept in a
     local that `execute` owns and that the closure borrows.
3. A pure function maps `Attempted` to either a response plus an outcome, or
   "read back". See the table in section 9.
   - `Live { matched: true }` decodes the record into a replay; a decode
     failure is 500 integrity.
   - `Committed(_)` answers from the captured stored form, the same bytes that
     were written. No decode runs, and the result is byte-identical to later
     replays. The wrapper always fills that form before it returns `Commit`;
     if the form were absent, `execute` would answer 500 and record
     `integrity` instead of panicking.
4. On read back: `Store::read_back` runs under
   `tokio::time::timeout_at(RequestDeadline::at() - 100 ms, ..)`.
   - `Found { matched: true }` whose record decodes → that stored success,
     `reconciled`.
   - Anything else, including the bound → 503 `idempotency_outcome_unknown`.

**504 precedence.** Before returning any idempotency Problem, meaning any
Problem mapped from `Attempted` or `ReadBack`, if
`tokio::time::Instant::now() >= RequestDeadline::at()`, `execute` awaits
`std::future::pending()` so the chain's timer answers 504
`request_timeout`, as authentication already does. The outcome is recorded
only when `execute` actually returns, so the drop guard records `abandoned`
in that case. Successes, replays, and the work's own responses are returned
as computed. The 100 ms readback reserve is a constant in `execute.rs` with
the same value as authentication's private response reserve. It is derived
from `RequestDeadline`, not configured.

**Cancellation.** A dropped future (504, disconnect, panic) drops the
transaction. `sqlx` queues `ROLLBACK`, and the returning connection flushes
it, so the advisory lock is released when PostgreSQL ends the transaction.
A future dropped during `COMMIT` may still commit (synthesis, sqlx 0.9.0
source). The next retry resolves that outcome through the ordinary sequence.

**Cleanup interplay.** Cleanup never waits: it skips row-locked records. An
attempt replacing an expired record may wait for one in-flight cleanup batch,
which its 1 s statement timeout bounds. Probe G observed a 0.40 s wait against
a batch held open for 0.40 s.

### 6.4 Deferred persistence tooling

- **`query!` with offline metadata: deferred to the first feature-owned
  repository.** The store's statements are template constants over one
  template-owned table. Every statement runs against the migrated schema in
  the retained database suite, both in the source and in each of graphs 13–16.
  Adoption would bring three costs:
  - `sqlx-cli` would enter `tools/versions.env`, which is portable, so it
    would appear in every output.
  - `cargo sqlx prepare --check` would need a migrated database.
  - `scripts/ci/test-integration-db.sh` exports `DATABASE_URL` for the
    unmigrated compose database. `sqlx-macros-core` 0.9.0 prefers a live
    `DATABASE_URL` over `.sqlx` unless `SQLX_OFFLINE=true`
    (`src/query/mod.rs:80-88`), so that script's compilation would fail
    without new environment plumbing.

  Update the persistence decision and the "First persistence repository"
  wording in `docs/backend-library-selection.md` to name the first
  feature-owned repository as the trigger.
- **Per-query spans: deferred to the first feature-owned repository.** The
  outcome counter, the request span, and the 1 s slow-statement warning
  answer the operational questions this stage has. Reopen for a question
  about arbitration, work, or commit latency that they cannot answer.
- **Migration-history exemption: not adopted.** `migration-history-check.sh`
  already treats a file added in the change range as an addition, including
  amendments before merge. A merged profile migration has been copied into
  derived services, so its correction is a new forward migration there too.
  Reopen if the template must rewrite a merged profile migration.

## 7. Stored success format 1

The seam's `stored.rs` captures, encodes, and decodes this format; the store
only moves bytes.

- `status` is the 2xx status.
- `body` holds the exact body bytes, at most 1,048,576.
- `headers` concatenates entries of the form
  `name_id: u8 || value_len: u16 BE || value bytes`, in `HeaderMap` iteration
  order, keeping every value of a name. `name_id` is 1 for Content-Type,
  2 Content-Encoding, 3 Content-Language, 4 Content-Disposition, and
  5 Location.
- Encoding refuses a total above 8,192 bytes under the spec's accounting.

Decoding requires `format = 1`, a 2xx status, known name ids, exact lengths,
`HeaderValue::from_bytes` success for every value, and both bounds. A decode
failure is an integrity failure (500), or an unresolved outcome (503) during
readback.

Replay builds a response with the stored status and a full body of the stored
bytes, then appends the headers in stored order. The chain adds the request
id, trace context, `nosniff`, and framing.

## 8. Canonical encoding, digests, and vectors

**Canonical input encoding 1** of a `Serialize` value. Lengths and counts are
`u32` big-endian; `text(s)` is `u32 len || UTF-8 bytes`.

| Serde data model | Bytes |
| --- | --- |
| bool | `01` then `00` or `01` |
| every integer width, and `u128` up to `i128::MAX` | `02` then the value as a 16-byte big-endian `i128` |
| `u128` above `i128::MAX` | `03` then 16-byte big-endian `u128` |
| f32, f64 | `04` then the 8-byte big-endian bits of the value as `f64`; every NaN becomes `0x7ff8000000000000` |
| char, str | `05` then `text` |
| bytes | `06` then `u32 len` and the bytes |
| none / some(x) | `07` / `08` then `enc(x)` |
| unit, unit struct | `09` |
| unit variant | `0a` then `text(variant name)` |
| newtype struct | `enc(inner)` |
| newtype variant | `0b` then `text(name)` then `enc(value)` |
| seq, tuple, tuple struct | `0c` then `u32 count` then the elements in order |
| tuple variant | `0d` then `text(name)` then `u32 count` then the elements |
| map | `0e` then `u32 count` then entries sorted by the bytes of `enc(key)`, each `enc(key) || enc(value)`; duplicate encoded keys fail |
| struct | `0f` then `u32 count` then the serialized fields sorted by name bytes, each `text(name) || enc(value)`; skipped fields are absent |
| struct variant | `10` then `text(name)` then the struct payload |

Names are serde names, after `rename`. Sorting makes field order and
`HashMap` order irrelevant. An unordered sequence type such as `HashSet`
serializes as a sequence in iteration order, so the guide and the API
documentation require `BTreeSet` or a sorted `Vec`. Integer widths share one
form, so widening a field keeps the encoding. A newly added field with
`skip_serializing_if = "Option::is_none"` keeps the encoding of requests that
omit it.

**Declaring the semantic input.** An operation passes one `Serialize`
value, usually an operation-owned struct. That value contains:

- every validated, typed value that determines the effect or the response:
  path, query, and header parameters and the body, after decoding and default
  normalization;
- any request header the work reads to choose the representation, such as
  `Accept` or `Accept-Language`, as a normalized typed value.

It never contains the key, credentials, `X-Request-ID`, or trace context. The
version is a `NonZeroU32` constant beside the handler.

**Changing an encoding without a meaning change.** During a rollout, replicas
of two releases serve at once, and each replica accepts only the encodings it
knows. The guide therefore prescribes three steps:

1. Ship a release that writes the current shape and accepts the new one
   (`also_matching(&new_shape)`).
2. Once that release is everywhere, ship the release that writes the new shape
   and accepts the old one.
3. After one retention period, drop the old acceptance.

A deliberate meaning change bumps the version instead, and in-window retries
of older requests get 422.

**Fingerprint digest.** SHA-256 over
`"http-idempotency/fingerprint/v1" || 0x00 || u32 BE version || encoding`.
A different version gives a different digest, so it is a mismatch.
`also_matching` adds the digest of another input shape under the same
version, and replay accepts either. The write stores the current digest.

**Scope digest.** SHA-256 over `"http-idempotency/scope/v1" || 0x00 ||
text(issuer) || tag || text(caller) || text(operationId) || text(key)`:

- The tag is `01` when a verified subject is present, and the caller is the
  subject.
- Otherwise the tag is `02`, and the caller is the client ID.
- A principal with neither is a wiring fault (sanitized 500).

The store derives the advisory lock key from the digest's first eight bytes,
read as a big-endian `i64`. A false 409 would need a 64-bit prefix collision
between keys held at the same time.

**Pinned vector.** An independent reference implementation computed these
values, and a second one reproduced them (section 14, probe 7). The seam's
tests reproduce the encoding, fingerprint, and scope literals. The store's
`attempt.rs` test reproduces the lock key from the scope digest, because only
the store derives it.
Input: `struct { name: "widget", count: 3u32, tags: vec!["a", "b"], note: None::<&str> }`.

- encoding: `0f0000000400000005636f756e740200000000000000000000000000000003000000046e616d650500000006776964676574000000046e6f74650700000004746167730c00000002050000000161050000000162`
- version 1 digest: `b4d06026e23a6de8d9fabe96199ec1713cebc5807fa7d09ace149402f559ff64`
- version 2 digest: `cadb7034bde8b7b3ed8c5caaf0bc4638bd956b2fd1f466be921d2a7a203ed419`
- scope (issuer `https://issuer.example`, subject `fixture-subject`,
  `createWidget`, key `k-123`): `e48d23569e751551fdcb4683dd3e6026c6f3f177b5b79e2c7ae9f4e4e36187d4`,
  lock key `-1977885806413146799`
- the same text as a client ID: `19fa9cef85be61dd5fe3a519736fd76940cbbddec761038bd5a918e07ba1a7b9`

## 9. HTTP boundary

**Composition.** `Composer::route` applies a key-handling `route_layer`
first, then `infra_http::authn::protect(routes, verifier)`. Authentication
therefore runs before key handling, and both run before any handler
extractor. Method fallbacks stay 404/405. `route` also registers
`describe_counter!(HTTP_IDEMPOTENCY_OUTCOMES_METRIC, ..)`.

**Key layer.** In order:

1. Read `RequestDeadline` and the verified principal through the
   `VerifiedPrincipal` extractor. If either is missing, answer a sanitized 500
   and never fall back to an anonymous scope.
2. Validate the key: exactly one `Idempotency-Key` field value of 1..=255
   bytes, every byte a `tchar`. The check applies to the value as hyper and
   httparse deliver it, with OWS trimmed. A failure answers 400 `bad_request`
   with the detail `Idempotency-Key is missing or invalid` and one
   `invalid_params` entry: name `header.Idempotency-Key`, reason `must be one
   Idempotency-Key field of 1 to 255 RFC 9110 token characters`. It records
   `outcome=invalid_key`.
3. Insert the attempt for the `Idempotency` extractor. The attempt carries
   the `ScopeKey`, operation, deadline, request id, store, and a shared "seam
   used" flag. The extractor removes it from the extensions and answers a
   sanitized 500 if it is missing. Only `execute` sets the flag.
4. After the handler returns, if the status is 2xx and the flag is unset, log
   a wiring error and replace the response with a sanitized 500. A handler
   that never enters the seam cannot return a success.

**Outcome mapping.** Problems use the fixed detail sentences below,
`Retry-After: 1` where marked, and the request id. No log, trace, metric, or
Problem carries the key, identity, digest, input, or body. The seam and the
store log failure classes only, never `sqlx` error text.

| `Attempted` or step | Response | `outcome` |
| --- | --- | --- |
| Invalid key (key layer) | 400 `bad_request` | `invalid_key` |
| `Err(FingerprintError)` given to `execute` | 500 `internal_error` "request failed" | none (code fault) |
| `Unavailable` | 503 `idempotency_unavailable` "idempotent request processing is unavailable", `Retry-After: 1` | `unavailable` |
| `Live { matched: false }` | 422 `idempotency_key_mismatch` "Idempotency-Key is bound to a different request" | `key_mismatch` |
| `Live { matched: true }`, decodes | the stored success | `replayed` |
| `Live { matched: true }`, undecodable | 500 `internal_error` "request failed" | `integrity` |
| `InProgress` | 409 `idempotency_request_in_progress` "a request with this Idempotency-Key is in progress", `Retry-After: 1` | `in_progress` |
| `Committed` | the stored success | `executed` |
| `RolledBack(response)` for a non-2xx | the work's response, unchanged | `not_stored` |
| `RolledBack(Unstorable)` | 500 `internal_error` | `not_stored` |
| `WriteFailed` | 503 `idempotency_unavailable`, `Retry-After: 1` | `unavailable` |
| `CommitRejected { retryable: true }` | 503 `idempotency_unavailable`, `Retry-After: 1` | `unavailable` |
| `CommitRejected { retryable: false }` | 500 `internal_error` | `not_stored` |
| `CommitUnknown`, then `Found { matched: true }` that decodes | the stored success | `reconciled` |
| `CommitUnknown`, anything else | 503 `idempotency_outcome_unknown` "the outcome of this request is unknown", `Retry-After: 1` | `outcome_unknown` |
| Dropped by timeout, disconnect, or panic | 504, no response, or the panic 500 | `abandoned` (drop guard) |

The closed outcome set is one module-private (`pub(super)`) enum in the
seam, used by the key
layer for `invalid_key` and by `execute` for everything else. Once the
fingerprint is `Ok`, `execute` records exactly one outcome: the mapped one
when it returns, or `abandoned` from the drop guard. `Err(FingerprintError)`
and wiring faults record none. Step-4 rejections (decoding, validation,
authorization) happen before `execute` and record none.

## 10. Configuration, startup, lifecycle

**Configuration.** New section file `crates/config/src/http_idempotency.rs`:

- `HttpIdempotencyConfig { retention: Option<Duration> }`, a human-form
  duration with `deny_unknown_fields` and default `None`.
- An empty or whitespace-only value is vacant. The deserializer builds on
  `app::occupied_string`, like `app.instance_id`, then parses the trimmed
  text with `humantime`.
- A set value outside 1 min..=30 days fails load, naming
  `http_idempotency.retention`.
- `required_retention(&self, postgres: &PostgresConfig) -> Result<Duration,
  ValidationError>` names `postgres.enabled` ("must be true when an idempotent
  operation is served") or `http_idempotency.retention` ("is required when an
  idempotent operation is served").

No other key or knob is added.

**Bootstrap.** All changes are marker-scoped except one neutral reorder and
one neutral test edit.

1. `serve()` binds the verifier returned by `prepare_auth`, which today is
   discarded. After the pools open, `prepare_http_idempotency` builds
   `Composer::new(store, verifier)`. The store is `Store::new(pool, retention)`
   when both the pool and the retention are present, and `Store::inert()`
   otherwise. `prepare_http_idempotency` never calls `required_retention`. The
   composer is carried to `admit_and_serve` through `Prepared`.
2. `admit_and_serve` builds `service::api::contract(&mut composer)` before
   readiness admission. The call moves above `readiness.refresh` in every
   graph, which changes no observable behavior because contract assembly is
   pure. Its comment, "only the routes are needed here", becomes pack-neutral:
   "The routes and the committed OpenAPI document are the two halves of one
   contract. Assembly is pure, so it runs before readiness admission."
   `activate_http_idempotency` then runs
   `composer.agree(contract.get_openapi())`:
   - `AgreementError` fails startup with the sanitized composition diagnostic.
   - `Inactive` does nothing more: no query, no task, no required value.
   - `Active { store, operations, .. }` passes both to the private
     `start_http_idempotency`. It calls `required_retention(..)`, then
     `store.check_startup()`, then spawns
     `store.run_cleanup(cancel.child_token())` on the existing tracker and
     logs `http_idempotency_active` with the operation count and retention.
     `#[non_exhaustive]` leaves `agree` the only producer of `Active`, so the
     bootstrap tests call `start_http_idempotency` directly with
     `Store::inert()`.

   Any failure exits 1 before admission, and the existing partial-startup path
   closes the pool.
3. `service::api::document()` renders with `Composer::inert()`. The
   `openapi` binary is unchanged. The authentication test at `api.rs:157`
   changes from `contract().into_openapi()` to `document()`, an unmarked
   edit that is valid in every output.

**Cleanup.** `Store::run_cleanup` sits beside `remove_expired`, following the
`record_metrics_periodically` precedent. It runs a `tokio::time::interval` of
60 s (first tick immediate, `MissedTickBehavior::Delay`) inside
`cancel.run_until_cancelled`.

- Each tick runs batches of 500 until one deletes fewer than 500 rows. Each
  batch is its own `in_tx` with a 1 s statement timeout.
- A failed run logs `http_idempotency_cleanup_failed` with `failure =
  acquire | begin | statement | commit` and waits for the next tick. It
  changes neither readiness nor serving.
- Cancellation ends the task at once. An interrupted batch finishes
  server-side within 1 s, inside the 5 s background-join and dependency-close
  budgets.

**Unchanged:** readiness probes, shutdown stages, `in_tx`, pool budgets, the
chain, and `protect`.

## 11. Proof architecture

### 11.1 Proving surfaces

| Claim | Owner and location | Runs in |
| --- | --- | --- |
| Key grammar, including one `identity.rs` test that for every byte the runtime `tchar` check agrees with the character class of the key pattern constant in `openapi.rs` (the test expands the bracket expression itself, with no regex crate); the section 8 encoding, fingerprint, and scope literals; canonical encoding table cases; stored format 1 round trip and bounds; header filtering; the pure `Attempted`/`ReadBack` → response and outcome mapping with fabricated store results (every `Attempted` and `ReadBack` row of section 9, `Retry-After`, detail sentences, 504 precedence under a paused clock); declaration rules and agreement (each rule, both directions, the reverse key rule, non-`true` values); the generated key schema pinned to the `openapi.rs` constants; catalog metadata of the four codes; the key-rejection Problem and the wiring guard as factored functions; DB-free mounted checks with `Verifier::disabled()` and an inert store (authentication failures precede key handling; 404/405 preserved) | inline `#[cfg(test)]` beside each owner in `crates/infra-http/src/idempotency/` | `make test` in the source and graphs 13–16 |
| Retention forms, bounds, vacancy, unknown keys, activation errors, and a value with a sub-microsecond part, which loads | `crates/config/src/http_idempotency.rs`; loader test in `load.rs` | `make test` |
| Independent JSON walk of rules 1–6 and the reverse rule; agreement of the assembled contract; one test-only idempotent route through the production composer | `crates/service/tests/openapi.rs`; `crates/service/src/api.rs` tests | `make test` |
| Activation glue: `activate_http_idempotency` with `Inactive` touches no store and spawns no task; `start_http_idempotency` with `Store::inert()` refuses disabled PostgreSQL and unset retention, naming the key | `crates/service/src/bootstrap` tests | `make test` |
| The lock-key literal derived from the pinned scope digest; the pure `TxError`-to-`Attempted` classification | inline `#[cfg(test)]` in `crates/infra-idempotency-store/src/attempt.rs` | `make test` in the source and graphs 13–16 |
| `Store::new` keeping whole microseconds of the retention | inline `#[cfg(test)]` in `crates/infra-idempotency-store/src/lib.rs` | `make test` in the source and graphs 13–16 |
| P1–P8 on real PostgreSQL at the store boundary, with two independent pools as replicas | `test/tests/http_idempotency/main.rs` (plus `commit_proxy.rs`) | `ALLOW_HEAVY=1 make test-integration-db`; graphs 13–16 |
| P9: the mounted router under the hardened chain with the real introspection verifier, a real PostgreSQL, every declared status end to end (including a proxied lost acknowledgement, and a budget expiry counted as `abandoned`), and the outcome counter | `test/tests/http_idempotency/mounted.rs` | the same, graphs 15–16 only |
| Refusals, lock shapes and replay, 128 projections, `none` purity, sync | `scripts/tests/*` through the runner | `make template-init-check` |
| 16 runtime graphs | `scripts/ci/template-init-check.sh` | CI parts; locally with `ALLOW_FULL=1` |
| `none` equality against `d24d173` | one-shot delivery comparison (section 12) | final validation only |

At the store boundary the database outcomes are the claims: one effect,
duplicates `InProgress` without running work, replays `Live` without running
work, rollback leaving nothing, expiry, cleanup, `Unavailable` on a read-only
session even with a live record, `CommitUnknown` with a committed or absent
record found by `read_back`, abandonment, `check_startup`,
`run_cleanup` returning promptly on cancel (a bounded spawn, cancel, and
join), and a record written under a retention with a sub-microsecond part. The HTTP status and outcome for each result are proven by the pure
mapping and, end to end, by P9.

P8's refusals are proven where they are decided:

- configuration refusals in the config and bootstrap tests;
- schema and writer refusals by `check_startup` against real PostgreSQL. A
  missing schema uses a per-test database without migrations. A read-only
  session sets `default_transaction_read_only` on the per-test database before
  the pool opens.

Hot-standby recovery is covered by the same predicate but is not reproduced.
Expiry is exercised by moving `expires_at` into the past with SQL, so no
test waits on the wall clock.

### 11.2 Test seams

- **Caller scopes.** The store takes a `ScopeKey` built from any 32-byte
  digest, and store tests use fixed digests. Only `infra-http`'s key layer
  derives a scope, and only from a verified principal. No constructor for a
  principal, a verifier, or a seam attempt is added, and neither the store
  nor `infra-http` gains a `test-support` feature. HTTP-level database proof
  therefore needs the real
  introspection fixture, which is the P9 boundary the spec names.
- **Lost acknowledgement (P6).** A tokio TCP proxy inside the database suite
  sits between one pool and the server. When armed, it does one of two
  things with the first simple-query `COMMIT`:
  - forward it, wait for the server's `ReadyForQuery`, and close both sockets
    without relaying it (a real commit whose acknowledgement is lost);
  - close both sockets before forwarding it (nothing committed).

  It fires only once, so the readback's fresh connection passes through.
  `sqlx-postgres` 0.9.0 sends `COMMIT` as a simple `Query` message
  (`transaction.rs:40-44`, `connection/executor.rs:283-284`). A closed socket
  yields a non-database error that `classify_commit` maps to `CommitUnknown`.
  Probe 3 measured both modes. `main.rs` (store level) and `mounted.rs` (HTTP
  level) both use it. No production seam is added.
- **JWT fixture: not exposed.** P9 proves the composition order: authentication,
  then key, then the operation's validation and authorization, then
  arbitration. That order is engine-independent code in `protect` and the key
  layer, and the introspection fixture already mounts the real verifier. A JWT
  fixture would add discovery, JWKS, and signing material to
  `infra-bearerauthn`'s test surface without new boundary coverage. JWT
  graphs run the unit tests and P1–P8.
- **Outcome counter.** P9 installs a thread-local recorder:
  `metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder()`
  (not feature-gated in 0.18.3) with `metrics::set_default_local_recorder`,
  and reads `handle().render()`. `#[sqlx::test]` runs a current-thread
  runtime, so every emission lands on the test thread. P9 drives requests with
  the existing `axum-test` dev-dependency, so no request-driver dependency is
  added.

### 11.3 Graph matrix and CI

Graph numbering:

- Graphs 1–12 stay as they are today, all with `HTTP_IDEMPOTENCY=none`.
- Graphs 13–16 are appended, all with `HTTP_IDEMPOTENCY=postgres`:
  13 `postgres/oidc-jwt/none`, 14 `postgres/oidc-jwt/bounded`,
  15 `postgres/oidc-introspection/none`,
  16 `postgres/oidc-introspection/bounded`.

Each graph runs public initialization, `make build`, and `make test` once,
sharing one `CARGO_TARGET_DIR`. Graphs 13–16 then run the retained suite once
in the initialized tree:
`REQUIRE_DOCKER=1 bash scripts/ci/test-integration-db.sh --test http_idempotency`.
That compiles and runs only the idempotency test target. P9 is present only
in graphs 15–16.

The runner refuses before its snapshot, with exit 2, when a selected graph in
13–16 has no usable Docker. Each graph's receipt line gains
`openapi_sha256` and `cargo_lock_sha256` of the initialized output.

In CI:

- A fourth initializer part, `http-idempotency`, runs graphs `13,14,15,16`.
  The parts `source`, `database-none` (1–6), and `database-postgres` (7–12)
  stay, each with its own warm cache. The new part restores the
  `database-postgres` cache and saves none, so the repository's 10 GB cache
  budget (`docs/ci-cd-production-ready.md`) gains no fourth initializer
  cache.
- `required` still reads one aggregate.
- The source's P1–P9 also run in the existing `integration` job.
  `changed-surfaces.sh` selects it for store paths and, where P9 is retained,
  for `infra-http` and `infra-bearerauthn` changes (ownership map).

No aggregate runs twice: the projections run once, each graph builds once,
and the idempotency suite runs once per retained graph plus once in the
source.

## 12. Profile machinery (summary; exact owners in the ownership map)

- `HTTP_IDEMPOTENCY=none|postgres` (`--http-idempotency`), default `none`.
  Refusals, all during input parsing and before a snapshot or target write:
  - with `DATABASE=none`: "HTTP_IDEMPOTENCY=postgres requires
    DATABASE=postgres";
  - with `AUTHN=none`: "HTTP_IDEMPOTENCY=postgres requires AUTHN=oidc-jwt or
    oidc-introspection";
  - unknown values.
- Lock: new schema-1 records hold exactly `database`, `authn`,
  `outbound_http`, `http_idempotency`, and `agent_harness`.
  - Admitted historical shapes are the two existing ones plus the four-field
    shape without `http_idempotency`. A missing field means `none`, and each
    shape maps to its own inventory generation.
  - A lock whose `http_idempotency=postgres` violates the combination rule is
    refused.
- Marker profiles:
  - `http-idempotency`;
  - the derived `http-idempotency-mounted`, selected when
    `HTTP_IDEMPOTENCY=postgres` and `AUTHN=oidc-introspection`, which owns P9
    and every dependency only P9 uses.

  Neither is a CLI or lock value.
- Cargo.lock: the pack enables no feature on a shared crate beyond what the
  same postgres-and-authentication graph already enables. If the source lock
  gains an edge that only the pack needs, add a guarded feature-edge rule keyed
  on `http_idempotency == none`. The equality proof is the oracle.
- `none` equality is one-shot and delivery-only. Initialize a `d24d173` clone
  through its own public initializer for the 12 `none` selections with the
  runner's identity values (initialize only, shared target). Compare the
  `api/openapi/service.yaml` and `Cargo.lock` digests with the runner's
  receipts for graphs 1–12. Separately, the projection checker permanently
  asserts that no registered path of either profile, and no marker line or id
  of either profile, remains in any `none` projection.
- Sync never restores the pack. `template_sync.py` validates the new field
  like `outbound_http`.

## 13. Release closure and capacity

**This stage.** No deployment happens and no deployed node changes: the
template is not a running service, and the delivery stops before push. In the
source template, the existing CI image rehearsal (`migration-validate`,
selected by the `migrations` surface) does three things:

- applies the pack's migration with `/migrate`;
- requires `no_change` on replay;
- runs the lifecycle check with the pool open while the boundary stays
  inactive.

No `rollout.md` is persisted, because this stage has no operational sequence
of its own. The adopter sequence belongs in the guide.

**A derived service that selects the pack and serves its first idempotent
operation.** The affected graph is the PostgreSQL writer, the `migrate` job
(the image's `/migrate` entrypoint), the service replicas, and retrying
clients.

| Owner/node | Prerequisite | Action | Success signal | Distinct safe-failure signal | Duration or horizon | Rollback / roll-forward | Proof / readback |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `migrate` job → writer | The image contains the pack migration; `postgres.enabled` and DSN | Run `/migrate` before the new release | `migration_run` with `outcome` `success` or `no_change` | `migration_run` `outcome=error` with a `stage`; replicas of the new release still refuse startup at the schema check | One run inside the 5 min deadline | Forward-only; the empty table is inert for older releases and stays | `check_startup` passes on the new release |
| Deployment configuration | The retention the service publishes as its client retry window | Set `APP__HTTP_IDEMPOTENCY__RETENTION` (1 min..=30 days) and keep `APP__POSTGRES__ENABLED=true` | `http_idempotency_active` startup record with the operation count and retention | Exit 1 before readiness naming `postgres.enabled` or `http_idempotency.retention` | At startup | Restore the previous value; a changed retention affects only later records | Startup record |
| Service replicas | Migration applied, configuration set | Roll out the release that composes the operation | Readiness passes; `http_idempotency_outcomes_total` counts outcomes | A replica exits 1 before readiness (agreement, configuration, schema, or read-only writer), and traffic stays on existing replicas | Rolling; older replicas answer 404/405 for the new route until replaced | Roll back the release; records stay inert. With no idempotent operation left the boundary is inactive and runs no cleanup, so expired rows remain until an active release cleans them. | Outcome counter, access-log statuses |
| Converting an existing operation (later) | The operation already serves requests without the boundary; the migration and retention are in place | Roll out the release that composes it through `Composer::route`, then tell clients the key is honored | Every replica reports the new operation count in `http_idempotency_active` | As for service replicas: a replica exits 1 before readiness | Until the last older replica is replaced. Meanwhile an older replica ignores the key, so a same-key retry that reaches it executes again. The guarantee starts only after the rollout. | Rolling back, or later removing the annotation, withdraws the guarantee at once, including for retries still inside the retention window, so the service tells clients first; records stay inert | Startup records of every replica; outcome counter |
| Fingerprint encoding change (later) | Live records under the old encoding | The three-release `also_matching` sequence (section 8) | Honest retries replay across the rollout | `key_mismatch` rises on retries during the rollout | At least one retention period | Keep the acceptance until old records expire | Outcome counter |

Mixed versions: the digest domains and stored format 1 are fixed in this
stage. A later release that changes them reopens the Specification and needs
its own compatibility plan. An operation without the boundary ignores the key
(specification), so across replicas the guarantee holds only once all of them
serve the operation through the boundary.

**Capacity.** No throughput claim is made. For each request, the boundary
holds one pooled connection:

- replay, 409, 422, integrity, or unavailable: four round trips;
- execution: the work's duration plus five boundary round trips.

Duplicates never wait for the executing attempt. With the default four
connections, long work bounds concurrent executions. Further requests wait at
most the 3 s acquire budget and then answer 503. The guide sizes
`postgres.max_connections` from concurrent work, replays, the readiness probe,
and one cleanup connection.

Storage is roughly (successful idempotent requests per retention period) ×
(stored body + headers + about 100 bytes). Bodies are capped at 1 MiB and
headers at 8 KiB. Cleanup drains 500 rows per indexed batch and repeats
batches until the backlog is gone.

## 14. Measured evidence

All probes ran in the session scratchpad on 2026-09-23 and 2026-09-24. They
are design evidence, not completion proof. A session restart on 2026-09-24
cleared the scratchpad after they had run, so their scripts (`apiprobe/`,
`dbprobe/semantics.py`, `lostack_proxy.py`, `vectors.py`, `fmt/probe.rs`) are
no longer available; each item records the observation made at the time. Two
results were re-established afterwards: probe 1's fourth run (`visprobe/`) was
recreated from its recorded source and re-run with the same results, and
probe 7 recomputed the section 8 vectors with a fresh implementation.

1. **API shape.** Every run used `cargo check --offline` with rustc 1.98.1
   and sqlx 0.9.0 without TLS; the first three also used axum 0.8.9. Every
   positive check passed with no warnings. Scope: type checking only.
   - First run: a `post` handler whose future must be `Send` called a generic
     `execute<W: AsyncFnOnce(&mut Tx<'_>) -> R>`. `execute` ran the closure
     inside an `in_tx_with`-shaped helper.
   - Second run, the split shape: a store `attempt<W, T>` with
     `W: AsyncFnOnce(&mut Tx<'_>) -> WorkOutput<T>` ran inside the same helper
     and returned `Attempted<T>`. The seam's `execute` wrapped the handler's
     closure, buffered the response with `Limited`, and mapped the outcome.
     The handler bound sqlx statements on `tx.connection()`.
   - Third run, the guide's port shape: an `async-trait` port in the feature's
     HTTP module, with a method taking `&mut Tx<'_>`. An adapter implemented it
     with sqlx on `tx.connection()` and mapped `23505` to a feature error. The
     handler held `Extension<Arc<dyn Port>>` and called the port inside
     `execute`'s closure.
   - Fourth run, the opaque handle that replaces the inherent accessor of the
     earlier runs (`visprobe/`, four crates standing in for the store, the
     seam, a feature, and an adapter). The store's `Tx` had a private field
     and no methods, and the free function `connection` was not re-exported
     by the seam. The adapter ran `sqlx::query(..).execute(
     connection(tx))` inside an `async-trait` port method, and the workspace
     checked with no warnings. From the feature crate, which depends on the
     seam only, `tx.connection()` failed with E0599, `store::connection(tx)`
     with E0433, `tx.as_mut()` with E0599, and `&mut **tx` with E0614.
2. **Arbitration semantics**, `postgres:18.4-alpine`, psycopg:
   - A: a duplicate's try-lock is `false` while the key is held. After the
     holder commits, a new attempt acquires the lock and the next statement
     sees the record (`(True, True)`).
   - B: under `REPEATABLE READ`, the lock is acquired after release while the
     committed record stays invisible (`(True, False)`). A plain insert fails
     `23505`; an upsert over a row changed after the snapshot fails `40001`.
   - C: in a read-only transaction, raw `pg_try_advisory_xact_lock` returns
     `true`. The gated statement returns `(recovering False, read_only True,
     acquired False)`, and a write fails `25006`.
   - D: an upsert over a live record affects 0 rows.
   - E: the key is released by rollback and by `pg_terminate_backend`.
   - F: the table held 1,200 expired rows and 10 live ones, and an attempt
     held a row lock on one expired row. Cleanup batches deleted
     `[500, 500, 199]` in 0.011 s. After the attempt committed, 11 live and 0
     expired rows remained.
   - G: an attempt's upsert during an open cleanup batch waited 0.404 s, the
     time the batch was held, then affected 1 row.
3. **Lost acknowledgement**, `lostack_proxy.py` and `psql` against the
   same server. In both modes the client reported "server closed the
   connection unexpectedly".
   - Forward-then-drop: the row was committed (count 1).
   - Drop-before-forward: the row was absent (count 0).
4. **Marker formatting.** rustfmt 1.98.1 preserved markers around a
   function parameter, a method-chain line, and a split `let verifier =`
   binding. Removing the marked lines left `contract()`, `base().merge(..)`,
   and `prepare_auth()?;`.
5. **Source reads.**
   - `sqlx-macros-core` 0.9.0 prefers a live `DATABASE_URL`.
   - `sqlx-postgres` 0.9.0 encodes `std::time::Duration` as an interval only
     when it has no sub-microsecond part (`types/interval.rs`), and depends
     on `sha2` 0.11.0 with default features off.
   - httparse 1.10.1 trims trailing whitespace from values.
   - `metrics` 0.24.6 exposes `set_default_local_recorder`.
   - `metrics-exporter-prometheus` 0.18.3 `build_recorder` is not
     feature-gated.
   - `utoipa-axum` 0.2.0 exposes `OpenApiRouter::get_openapi`.
   - utoipa 5.5.0 `ToResponse` accepts `headers(..)`.
6. **SQL text.** The section 6.1 migration and every section 6.2 statement
   ran against `postgres:18.4-alpine` through `psql`, with binds via `PREPARE`
   and `$7` as `interval`.
   - The read and readback returned the expected empty and stored rows.
   - The upsert inserted one row.
   - The startup check returned `(t, 0)`.
   - The cleanup batch ran under its local timeout.
   - A missing column failed `42703` and a missing table `42P01`.
7. **Vectors.** A fresh implementation written from the section 8 text alone
   (`vectors_recheck.py`, after the restart) reproduced all six pinned
   literals: the encoding, both fingerprint digests, both scope digests, and
   the lock key.

## 15. Bounded assumptions and reopen

- `READ COMMITTED` only: reopen Technical Design when an operation needs a
  stricter level.
- A 64-bit lock-key prefix: reopen on an observed false 409 not explained by a
  holder.
- Encoding 1 and the digest domains are fixed. Any change to them reopens the
  Specification (stored format) and needs a compatibility plan for live records.
- The 100 ms readback reserve: reopen if measured readback latency on a
  healthy writer exceeds it.
- Cleanup batch 500, cadence 60 s, 1 s statement bound: reopen on a measured
  backlog that one tick cannot drain, or on lock waits attributable to cleanup.
- The portable-instruction reading in section 3: reopen the Specification's
  portable-instruction non-goal if an instruction is read to forbid features'
  existing `infra-http` dependency.
- Deferred tooling triggers are in section 6.4.
- Reopen Research for changed sqlx drop or commit semantics, a PostgreSQL
  major-version change of the proof image, or RFC publication of the draft.
  Reopen the Specification for any observable change.
