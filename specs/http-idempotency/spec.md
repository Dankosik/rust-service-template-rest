# Specification: optional PostgreSQL-backed HTTP idempotency

Status: ready. Definition owner: stage-10.3 Definition. Baseline:
`d24d1737d1647d9bfdf2b15ecb258630ec9cd326`. Requester meaning:
[Intent](intent.md). Decision-changing evidence and deviations:
[research synthesis](research/synthesis.md). This contract owns observable
behavior. Technical Design owns crate placement, the Rust API, SQL, and
mechanism.

## Outcome and selection

`HTTP_IDEMPOTENCY=none|postgres` (direct entry `--http-idempotency`) is an
independent initializer selection, and `none` is the default. `postgres`
requires `DATABASE=postgres` and an authentication engine (`AUTHN=oidc-jwt` or
`oidc-introspection`). Rust `AUTHN=none` has no verified caller to scope keys
to, so a retained pack could never serve an operation
([D3](research/synthesis.md#decisions-and-their-evidence)). An unsupported
combination or unknown value is refused before any target write, with a
diagnostic naming the requirement. The lock records `http_idempotency`.

- `none` output contains no idempotency runtime code, configuration section,
  problem codes, migration, tests, or guide. Its generated contract and runtime
  dependency graph equal those the same selection produced before this stage.
- `postgres` output retains a complete, usable pack: the idempotency boundary,
  configuration, problem codes, one migration, tests, and an adopter guide. The
  pack is inert until an operation opts in. It makes no database query, starts
  no task, requires no configuration value, and changes no served behavior.
  The generated contract may gain only reusable problem-response components
  that no operation references. Running `migrate` creates the empty profile
  table even before any operation opts in.
- Runtime `postgres.enabled` and `authn.mode` stay runtime switches, and
  selecting the pack enables neither.

Outcome: a derived service adds an idempotent operation by following the guide.
It composes the operation through the pack's documented seam, without editing
the hardened chain or the pack's own code. After that, each verified
caller gets at most one committed business effect per operation and key within
the retention window. A later retry with the same input gets that effect's
success response back, with two exceptions:

- the retry no longer passes authentication, the operation's validation, or
  its authorization;
- the operation has since made a deliberate change of meaning under a new
  fingerprint version. The retry then gets 422 until the record expires.

## Declaring an idempotent operation

An operation is idempotent when its generated OpenAPI operation carries
`x-idempotent: true`. Every such operation meets all of these rules:

1. The extension value is the boolean `true`; any other value is invalid.
2. The method is `POST`, `PUT`, `PATCH`, or `DELETE`.
3. It satisfies the existing protected-operation contract: `x-security-decision`
   is `protected`, and security is bearer-only with no anonymous alternative.
4. It declares the header parameter `Idempotency-Key` exactly once. Other
   header parameters are allowed; see "Same request" for how they count. The
   key parameter has `required: true` and a string schema with `minLength: 1`,
   `maxLength: 255`, and this exact `pattern`:

   ```text
   ^[!#$%&'*+.^_`|~0-9A-Za-z-]+$
   ```

5. It declares at least one 2xx success response and no 1xx or 3xx response.
   Its 2xx responses declare only replayable headers (see Replay).
6. It declares Problem responses for 400, 401, 403, 409, 422, 431, 500, 503,
   and 504. Each description covers every Problem that authentication or the
   boundary can return at that status. The declared 409 and 503 responses
   document an optional `Retry-After` header.

Agreement is two-way: an operation is served through the idempotency boundary
if and only if its generated operation declares `x-idempotent: true`. A
violation of either direction or of any rule above does two things. The
service refuses to start with a sanitized composition diagnostic, and the
contract tests fail under `make test`. No route may be silently non-idempotent
or silently idempotent. With the pack retained, an operation that declares an
`Idempotency-Key` header parameter without `x-idempotent: true` is refused the
same way, so the contract never advertises a key the service would ignore. The
committed `api/openapi/service.yaml` remains the byte-compared generated
authority. Other operations are unchanged, and an `Idempotency-Key` header
sent to them is ignored.

## Request identity

**Key.** A request carries exactly one `Idempotency-Key` field line. After
standard HTTP field parsing, its value is 1 to 255 RFC 9110 `tchar` characters.
Keys are exact, case-sensitive byte strings. The following are all invalid:

- a missing or empty key, or a repeated field;
- more than 255 characters;
- any other byte, including quotes (so the draft's quoted sf-string form is
  invalid), spaces, commas, and non-ASCII.

An invalid key gets 400 `bad_request` with a fixed detail and one
`invalid_params` entry: `name` is `header.Idempotency-Key`, and `reason` is a
fixed sentence that names the rule and never echoes the value.

**Scope.** A key is unique within:

- the verified issuer;
- the verified subject when one is present, otherwise the verified client ID
  (a subject and a client ID with equal text are different callers);
- the operation's `operationId`, which the existing lint keeps unique.

Values are exact, as in the authentication contract. The same key from another
caller or on another operation is independent. There is no resource or tenant
refinement ([D2](research/synthesis.md#decisions-and-their-evidence)).
Renaming an `operationId` starts a new key namespace.

**Same request.** Each idempotent operation declares a positive fingerprint
version and a semantic input. The semantic input is every validated, typed
request value that determines the business effect or the response, wherever
the request carries it: path, query, header parameters, or body. Values are
taken after decoding and default normalization. It excludes only three
things:

- the `Idempotency-Key` itself;
- the credentials that authenticate the caller, because the verified caller is
  already the scope;
- transport correlation headers (`X-Request-ID` and trace context).

Every value that selects the target is therefore fingerprinted. Request
headers that the work uses to choose the response representation, such as
`Accept` or `Accept-Language`, also count, even though OpenAPI does not declare
`Accept` as a parameter. A retry that differs only in them gets 422. Two
attempts are the same request exactly when their versions are equal and their
semantic inputs have the same canonical encoding.

- Representation-only differences never change the fingerprint: JSON member
  order, whitespace, header order or casing, request id, trace headers, and
  the caller's credential value.
- A change to any included value always changes it.
- Within one version, an unchanged request has the same canonical encoding on
  every replica and across restarts and deployments, for as long as a record
  written under that version can be live, so an honest retry keeps replaying
  across a deployment. Some changes would alter the encoding of an unchanged
  request, for example renaming an included field or encoding a newly added
  default. Such a change ships only after those records have expired, or
  together with a comparison that also accepts the previous encoding.
- A deliberate change of meaning uses a new version. A different version is a
  mismatch, so a retry of a request recorded under the previous version gets
  422 `idempotency_key_mismatch` until that record expires.

**Data custody.** The store keeps these:

- fixed-length one-way digests of the scope with the key and of the
  fingerprint;
- the stored success response and its expiry.

It never keeps the raw key, the caller identity, or the request input. Stored
success bodies are business data retained for the retention window. There is
no per-caller deletion path.

## Processing order

For an idempotent operation, in order:

1. The existing transport chain and connection policy, unchanged: shedding,
   the request timeout, header limits and 431, a declared `Content-Length`
   over the body limit (413 before routing), and the 404/405 fallbacks.
2. Authentication under the existing protected contract, unchanged, including
   its mapping when runtime `authn.mode = "none"`. There, credential syntax is
   still checked first (401 `authentication_required`, 400
   `authentication_malformed`, 431 `authentication_oversize`), and only a
   well-formed bearer credential gets 503 `authentication_unavailable`.
   Authentication failures return before any key handling. If the boundary
   finds no verified principal (a wiring fault), it answers a sanitized 500
   and never falls back to an anonymous scope.
3. Key validation (400, as above). The boundary validates only the key.
   General extractor-rejection mapping stays deferred to the first product
   operation with parameters or a body, as
   [HTTP Architecture](../../docs/architecture/http.md#adding-an-operation)
   already records.
4. The operation's own request decoding and validation, then its
   authorization (403), if any. A streamed body over the limit still answers
   the existing 413 when this step reads it. These run on every attempt,
   including one that will replay. A replay never bypasses current
   authentication or authorization. An operation therefore authorizes every
   attempt before it enters the idempotency seam, not only inside the work.
   Extra checks inside the work are allowed; a non-2xx result there rolls
   back. A check that reads state the committed effect changed, for example a
   DELETE whose authorization reads the deleted resource, rejects the retry
   with its own response instead of replaying.
5. Arbitration and execution at the writable PostgreSQL primary (tables
   below).

A request rejected in steps 1–4 leaves no idempotency state behind, and the key
stays usable. Every idempotency database action happens inside the existing
request budget. The boundary never retries automatically, never extends the
budget, and adds no budget configuration. When the budget expires before an
outcome is sent, the answer is the existing 504 `request_timeout`, never an
idempotency Problem racing it.

## Arbitration and execution outcomes

A **live record** is a committed success for the same scope and key whose
expiry, on the database server clock, is still in the future. An expired record
counts as absent: it is never replayed and never blocks execution, whether or
not cleanup has deleted it.

| Condition after step 4 | Response | Operation's work | Record | `outcome` |
| --- | --- | --- | --- | --- |
| The writable primary cannot be used for arbitration: acquire or connect failure, read-only or recovering session, or a failed arbitration statement | 503 `idempotency_unavailable`, `Retry-After: 1` | Not run | Unchanged | `unavailable` |
| A live record exists with a different fingerprint | 422 `idempotency_key_mismatch` | Not run | Unchanged | `key_mismatch` |
| A live record exists with the same fingerprint | The stored success (Replay) | Not run | Unchanged | `replayed` |
| A live record exists but cannot be decoded as a valid stored success | 500 `internal_error`, sanitized, on every retry until expiry | Not run | Unchanged | `integrity` |
| No live record is visible, and another attempt holds the scope and key for execution and its transaction has not ended | 409 `idempotency_request_in_progress`, `Retry-After: 1`, without waiting for that attempt to end | Not run | Unchanged | `in_progress` |
| No live record, key free | This attempt executes (next table) | Run once, in one transaction | Next table | Next table |

Precedence follows the table. A visible live record always decides the
outcome, so concurrent retries that each find it all replay (or all mismatch).
None of them gets 409 because of another replay. While the executing
attempt's input is not yet committed, a concurrent attempt with the same key
and different input gets 409; a later retry gets 422.

| Execution result | Response | Business effect | Record | `outcome` |
| --- | --- | --- | --- | --- |
| The work produces a storable 2xx and the commit is acknowledged | That success, in its replay form | Committed | Created, or replaces an expired one | `executed` |
| The work produces any non-2xx response | That response, unchanged | Rolled back | None | `not_stored` |
| The work produces a 2xx that cannot be stored: its body fails while buffering or exceeds 1 MiB, or its replayable headers exceed 8 KiB | 500 `internal_error`, sanitized | Rolled back | None | `not_stored` |
| Writing the success record fails before `COMMIT` is sent (statement or connection error) | 503 `idempotency_unavailable`, `Retry-After: 1` | Rolled back | None | `unavailable` |
| The work panics | The existing 500 panic Problem | Rolled back | None | `abandoned` |
| The server rejects the commit with a retryable class (`40001`, `40P01`) | 503 `idempotency_unavailable`, `Retry-After: 1` | Not committed | None | `unavailable` |
| The server rejects the commit with any other definite class (`23xxx`, other `40xxx` except `40003`) | 500 `internal_error`, sanitized | Not committed | None | `not_stored` |
| Commit outcome unknown, and a writer readback within the budget finds a live record with the same fingerprint | That stored success | Committed once for this key (by this or an equivalent attempt) | That success | `reconciled` |
| Commit outcome unknown and not resolved as above | 503 `idempotency_outcome_unknown`, `Retry-After: 1` | Unknown | Unknown | `outcome_unknown` |
| The request budget expires, or the request is abandoned (for example a client disconnect) | The existing 504 `request_timeout`, or no response | Committed or not, atomically with the record | Atomically with the effect | `abandoned` |

The success response is sent only after the commit is acknowledged or read
back. The boundary never re-runs the operation's work within one request. An
unknown or abandoned outcome is resolved by the next same-key retry, which gets
a replay, a 409, a fresh execution (only if nothing committed), or a 422.

## Concurrency and one transaction

Arbitration happens at the shared PostgreSQL writer. Across all replicas, for
each scope and key:

- at most one attempt holds the key at a time, and only the holder can commit;
- at most one success commits until that record expires;
- after an attempt ends without committing, the next attempt may execute.

An attempt abandoned by timeout or disconnect holds its key only until
PostgreSQL ends that transaction. The existing `statement_timeout` and
`idle_in_transaction_session_timeout` session budgets bound that time (8 s each
by default). Until then, same-key requests get 409. A duplicate never waits for
the executing attempt while it holds a pooled connection
([D1](research/synthesis.md#decisions-and-their-evidence)).

An attempt's PostgreSQL transaction can also end early: a server restart or
failover, backend termination, or the idle-in-transaction timeout while the
work waits outside the database. That attempt then loses the key and can no
longer commit. Its non-transactional work may still be running when a later
attempt takes the key.

The operation's PostgreSQL writes through the transaction the boundary
provides commit exactly when its success record commits, and they share that
record's durability fate, including under `synchronous_commit = off`. Nothing
else is covered:

- writes made outside that transaction;
- outbound HTTP calls, including through the bounded outbound profile;
- other datastores, messages, files, and spawned tasks.

Those effects may happen on an attempt that rolls back, and may repeat on a
retry. The pack neither blocks nor wraps them. The guide requires such an
operation to use provider idempotency keyed from the same request identity, or
a separate durable design. The outbound HTTP contract is unchanged. These
outcomes hold under read committed and under any stricter isolation that
Technical Design lets an operation choose. An arbitration conflict that the
database reports as a serialization failure is the retryable 503, never a
second execution.

## Replay

A replay, and the first success of the executing attempt, carry:

- the stored status;
- the stored body bytes, exactly;
- exactly these replayable headers when the work set them, with the stored
  values: `Content-Type`, `Content-Encoding`, `Content-Language`,
  `Content-Disposition`, `Location`.

The boundary removes any other header that the work sets on a 2xx, so the first
response never promises something a replay would omit. The transport generates
headers for each exchange, so each response carries its own values for these:
the current request's `X-Request-ID`, trace context, `nosniff`, and framing such
as `Content-Length`. Replays carry no marker header. Bounds: the stored body is
at most 1 MiB (1,048,576 bytes). The replayable header fields total at most
8 KiB, counting `name.len + value.len + 4` per field. Both are fixed component
limits ([D8](research/synthesis.md#decisions-and-their-evidence)). The success
response is fully buffered; streaming successes are not supported.

**Retention.** A record is live for `http_idempotency.retention` after the
executing attempt writes its success, measured on the database clock. The
expiry is fixed at write time, so changing `retention` affects only later
records. After expiry, a request with that key executes afresh, whatever its
input, and replaces the record. The guide tells adopters to publish the
retention as the client retry window.

## Problem catalog

The profile adds these codes to the closed catalog, scoped by marker as
authentication's codes are:

| Code | Status | Type URI | Title | Header |
| --- | --- | --- | --- | --- |
| `idempotency_request_in_progress` | 409 | RFC 9110 §15.5.10 | `conflict` | `Retry-After: 1` |
| `idempotency_key_mismatch` | 422 | RFC 9110 §15.5.21 | `unprocessable content` | — |
| `idempotency_unavailable` | 503 | RFC 9110 §15.6.4 | `service unavailable` | `Retry-After: 1` |
| `idempotency_outcome_unknown` | 503 | RFC 9110 §15.6.4 | `service unavailable` | `Retry-After: 1` |

Key errors use `bad_request`. Integrity failures, unstorable successes, and
non-retryable commit rejections use `internal_error` with the sanitized detail.
Details are fixed sentences, and Problems carry the current request id. No
Problem, log, trace, or metric contains the key, caller identity, fingerprint,
request input, or stored body.

## Configuration, startup, and lifecycle

`http_idempotency.retention` (environment `APP__HTTP_IDEMPOTENCY__RETENTION`)
is a non-secret human-readable duration. It exists only with the pack and has
no usable default ([D7](research/synthesis.md#decisions-and-their-evidence)).
A set value outside the inclusive range of 1 minute to 30 days fails startup
and names the key, whether or not the pack is active. Unknown keys in the
section fail, as today.

The boundary is **active** when at least one idempotent operation is served.
Active startup has these requirements; failing any is a startup failure
(exit 1) with a sanitized diagnostic, before readiness admission:

- `postgres.enabled = true`;
- a set `retention`;
- a present idempotency schema;
- a writable session.

An inactive pack does none of this.

While active, a background task removes expired records every minute. It works
in bounded batches until the backlog drains. It skips records that live
attempts hold, and never deletes a live record. A failed run logs a warning
with a failure class and no data, and the next run retries. Cleanup failures
change neither readiness nor serving. The task is cancelled and joined within
the existing background-task stage, and its database work is bounded so that
closing the pool keeps the existing dependency-close budget.

The profile adds no configuration knob other than retention, and no readiness
probe or shutdown stage. At runtime, a read-only or recovering writer produces
per-request 503s, while readiness keeps its existing PostgreSQL probe.
Technical Design derives any response reserve from the existing request-budget
owner (`RequestDeadline`), not from new configuration.

**Observability.** The counter `http_idempotency_outcomes_total` has one label,
`outcome`. Its closed values, as used in the tables above, are:

- `invalid_key`, `unavailable`, `in_progress`, `key_mismatch`;
- `replayed`, `integrity`, `executed`, `not_stored`;
- `reconciled`, `outcome_unknown`, `abandoned`.

`invalid_key` counts step-3 rejections. `abandoned` counts every request that
ends without another recorded outcome: budget expiry, disconnect, or panic,
whether during arbitration, a replay read, or execution.

## Schema, profile, sync, and guidance

- **Schema.** One forward-only migration in `migrations/`, retained only with
  the pack and applied by the existing `migrate` binary. The service never
  creates or alters schema at runtime. The source template's migration set
  becomes non-empty, and `DATABASE=postgres` without the pack keeps it empty.
  Features never read or write the profile table directly.
- **Markers.** Every selected-only Cargo, Rust, configuration, migration,
  test, initializer, validation, and local-documentation owner carries markers
  or a registered removal path. Default initialization physically removes the
  pack. Selected initialization retains every owner, valid links, and a
  buildable, testable graph.
- **Lock.** New schema-1 records contain exactly `database`, `authn`,
  `outbound_http`, `http_idempotency`, and `agent_harness`. The admitted
  historical shapes are the two existing ones and
  `database`+`authn`+`outbound_http`+`agent_harness`, where a missing
  selection means `none`. Matching historical replay preserves the original
  lock bytes, and a different selection is a refused profile migration.
- **Sync.** Portable sync never restores a pruned idempotency pack, schema,
  configuration section, or guide. The target lock stays authoritative, and
  migrations and application code stay service-owned.
- **Preflight.** The public initializer keeps its complete locked metadata,
  formatting, and OpenAPI preflight before any target write. A refusal leaves
  target bytes and Git state unchanged.
- **Guide.** A marker-scoped adopter guide under `docs/` covers:
  - declaring an operation;
  - semantic input and fingerprint rules: keep the encoding of unchanged
    requests stable while records can be live, and bump the version only for
    a deliberate change of meaning, which turns in-window retries of older
    requests into 422;
  - `operationId` stability;
  - authorizing every attempt before the seam, and the retry consequence of
    checks that read state the effect changes;
  - retention publication;
  - replay contents and response compatibility while records are live;
  - side-effect limits and pool sizing for transactions held during work;
  - client guidance: retry with the same key after 409
    `idempotency_request_in_progress`, 503 `idempotency_unavailable` or
    `idempotency_outcome_unknown`, a 504, or a lost connection. 422
    `idempotency_key_mismatch` means the key is bound to a different recorded
    request: either the key was reused for new input, or the retry spans a
    deliberate change of meaning. The recorded request may already have
    committed, so a client must reconcile before it resends under a new key,
    or it can repeat the effect. Other statuses, including an operation's own
    business 409, keep their operation meaning;
  - configuration, cleanup, metrics, and data custody.

  Shared documents that describe selections and packs are updated to
  include it. That includes the roadmap's statement that stage-10 items are
  independent, because this profile requires an authentication engine.

Deliberately unchanged:

- the authentication contract (codes, statuses, budgets, principal, `protect`
  rules);
- the outbound HTTP contract;
- the PostgreSQL admission, pool, budgets, `in_tx`, and commit
  classification;
- the hardened chain order, the existing Problem codes, readiness, and the
  migration rules.

## Proof expectations

Real PostgreSQL claims (`ALLOW_HEAVY=1 make test-integration-db`). Two
independent pools against one server form the cross-replica boundary.

- **P1, concurrency.** Hold one executing attempt open. Same-key duplicates get
  409 before release and never run work. After release, retries replay.
  Exactly one effect exists. Concurrent retries after commit all replay.
- **P2, rollback.** A non-2xx result, a panic, and an unstorable success each
  leave zero effects and no record, and the retry executes.
- **P3, replay and scope.** A replay has an identical status, body, and
  replayable headers without running work. Representation-only differences
  replay; different input gets 422. Another caller and another operation are
  independent.
- **P4, expiry and cleanup.** An expired record is not replayed and the request
  re-executes. Cleanup drains a backlog larger than one batch, keeps live
  records, and does not disturb an executing attempt.
- **P5, writer.** A read-only session gets 503 and no work runs, including when
  a live record exists.
- **P6, unknown commit.** After a real commit whose acknowledgement is lost,
  the request gets the stored success with no second run. After a lost
  acknowledgement with no commit, it gets 503 `idempotency_outcome_unknown`
  with no second run, and a later retry executes once.
- **P7, abandonment.** An attempt dropped during its work leaves no effect,
  and the key becomes usable once its transaction ends.
- **P8, activation.** When active, a missing schema, a read-only session,
  disabled PostgreSQL, or unset retention refuses startup. When inactive,
  no idempotency database work happens.
- **P9, mounted router.** A test-only operation, composed exactly as the
  guide's feature path, runs with a real verifier fixture. It shows that
  authentication failures precede key validation, and that an authorization
  rejection precedes replay. It covers the key grammar cases (including a
  repeated field and the quoted form), `Retry-After`, and a replay with a fresh
  request id and stripped headers. It asserts status, content type, Problem
  code, headers, and the `outcome` counter value for every declared status the
  boundary produces.

  P9 runs in the source template and in every retained graph whose
  authentication engine exports a test-support fixture (today introspection
  only). The JWT graphs still build and run every engine-independent boundary
  test. Technical Design decides whether to expose a JWT fixture, and adds no
  verification bypass or principal constructor either way.

Proof without a database:

- contract-agreement tests in both directions and for each declaration rule;
- catalog coverage and configuration loader and validation tests;
- fingerprint canonicalization tables that pin literal expected encodings or
  digests, so an encoder change between builds fails them;
- key grammar tables;
- `make openapi-check`.

Initializer proof:

- the `DATABASE=none`, `AUTHN=none`, and unknown-value refusals leave the
  target byte-identical;
- the lock and replay cases;
- all 128 canonical DATABASE × AUTHN × OUTBOUND_HTTP × HTTP_IDEMPOTENCY ×
  harness projections;
- 16 runtime graphs, each with public initialization, build, and test once,
  not per harness: 12 with `none` plus 4 with `postgres`; eight non-harness
  selections are refused;
- for each of the 12 `HTTP_IDEMPOTENCY=none` selections: the generated
  contract and the projected `Cargo.lock` equal what the baseline `d24d173`
  source produces for the same selection, and no registered idempotency path
  or marker identifier remains. If a shared change would break this equality,
  Specification reopens;
- the purity and sync suites.

Also run `make docs-check`. Existing authentication, outbound, and PostgreSQL
receipts are reused only where the exact inputs and claims are unchanged.
Local checks claim no CI, publication, or deployment result.

## Representative scenario

Caller A sends `POST` with key `K` to replica 1, which executes. A's client
times out, and its retry reaches replica 2 while replica 1 is still working.
The retry gets 409 with `Retry-After: 1`, and replica 2 runs no work. The next
retry arrives after the commit and gets the identical 201 body with
`Location`, plus its own request id. A later request from A with `K` and a
changed body gets 422, and the record is unchanged. Caller B's `K` executes
independently. Once retention has passed, A's `K` executes afresh. If replica
1's commit acknowledgement had been lost, it would have answered either the
stored 201 (after readback) or 503 `idempotency_outcome_unknown`, never a
second effect.

## Non-goals and reopen

Not in scope:

- replaying non-2xx outcomes, a per-operation retention, or a
  resource/tenant scope refinement;
- a replay marker header, waiting duplicates, streaming successes, or
  anonymous idempotent operations;
- the draft's sf-string key form, or per-caller erasure;
- readiness participation, a product operation, or profile migration of
  an initialized service;
- idempotency for effects outside PostgreSQL;
- a new skill or portable instruction. Reopen that one only if a portable
  instruction would contradict the retained profile.

Reopen owners:

- **Intake:** a changed outcome or authority.
- **Research:** changed sqlx drop or cancel semantics, RFC publication of the
  draft, a PostgreSQL major-version change for the proof image, or a new
  verified-caller mechanism.
- **Specification:** a new selection or combination rule, waiting semantics,
  error replay, a scope refinement, a grammar change, or a stored-format
  change.

Technical Design next chooses:

- crate placement and the Rust API (the executor seam, the declaration and
  composition helper, and the agreement check);
- the non-waiting arbitration mechanism;
- the canonical fingerprint encoding and digest;
- the SQL, and the `query!`/offline-metadata, per-query-span, and
  migration-history-exemption deferrals that this first profile schema
  triggers;
- the stored format, and the cleanup batch and statement bounds;
- the startup schema and writer check, and the metric wiring;
- the marker inventory, initializer and validation transformations, and the
  CI split for 16 graphs.

None of these choices may change the observable rules above.
