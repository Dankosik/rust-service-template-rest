# Research synthesis: PostgreSQL-backed HTTP idempotency

Valid as of 2026-09-23. Decisions affected: the stage-10.3 behavior contract
([spec](../spec.md)) and the later Technical Design. This is primary-source and
current-checkout evidence, not implementation proof. Labels: **fact** (read from
a primary source), **inference** (derived here), **decision** (a Definition
choice the spec adopts), **assumption** (bounded, with a reopen condition),
**unknown**. Refresh on an sqlx upgrade, a PostgreSQL major-version change for
the proof image, publication of the IETF draft as an RFC, a new authentication
mechanism, or a new initializer selection.

## Baseline

- **Fact.** Rust `main` was clean and equal to `origin/main` at
  `d24d1737d1647d9bfdf2b15ecb258630ec9cd326`; stage 10.2 merged as
  `43b7588edbdb1ebfbc478fb28e0e3d2e77417960` (PR #42).
- **Fact.** `crates/infra-postgres/src/transaction.rs`: `in_tx`/`in_tx_with`
  hand the closure a connection, commit on `Ok`, roll back on `Err` within 3 s.
  A commit error with SQLSTATE class `23` or `40` except `40003` is
  `TxError::CommitFailed` (nothing written); every other commit error,
  including I/O, is `TxError::CommitUnknown`; `retryable` is `40001`/`40P01`.
  Budgets in [Persistence](../../../docs/architecture/persistence.md#budgets):
  acquire 3 s, `statement_timeout` 8 s, `idle_in_transaction_session_timeout`
  8 s, default `postgres.max_connections` 4. `http.request_timeout` defaults to
  8 s (`crates/config/src/http.rs`).
- **Fact.** `migrations/` is empty. The persistence leaf defers `query!` with
  offline metadata and `sqlx-cli`, per-query spans, and the template's own
  migration-history exemption "to the first repository (stage 10)"; this stage
  adds the first profile-owned schema, so those deferrals reach their trigger.
- **Fact.** Protected operations compose through `infra_http::authn::protect`,
  which refuses a route whose OpenAPI metadata lacks `x-security-decision`
  `protected`, bearer-only security, or the 400/401/403/431/503/504 family
  (`crates/infra-http/src/authn.rs`); handlers receive a sealed
  `VerifiedPrincipal` with an exact issuer and a subject and/or client ID
  ([Authentication](../../../docs/authentication.md)). `AUTHN=none` removes all
  of this. The chain stamps `RequestDeadline` before the tower timeout, which
  answers 504 `request_timeout` and remains the final authority
  (`crates/infra-http/src/harden.rs`).
- **Fact.** The closed catalog (`crates/infra-http/src/problem.rs`) already has
  `bad_request`, `conflict` (409, RFC 9110 §15.5.10), `unprocessable_content`
  (422, §15.5.21), `service_unavailable` (503, §15.6.4); profile codes are
  marker-scoped, as authentication's are.
- **Fact.** [Validation boundary](../../../docs/template-sync.md#validation-boundary):
  96 canonical DATABASE × AUTHN × OUTBOUND_HTTP × harness projections and 12
  runtime graphs; the public initializer keeps full locked metadata,
  formatting and OpenAPI preflight before target writes. Lock schema 1 writes
  the current four-field shape and admits two historical profile shapes.

## Go template evidence

Read with `git show HEAD:<path>` at committed HEAD
`749473366bd4544c96cd640d09cb4b6ff7ff6473`; the checkout had unrelated dirt
(docs, `scripts/init-module.sh`, an untracked spec), none in the idempotency
files. Paths: `docs/postgres-http-idempotency.md`, `internal/httpidempotency/`,
`internal/infra/postgresidempotency/` (including
`store_commit_unknown_integration_test.go`), `internal/infra/http/idempotency.go`,
`internal/config/http_idempotency_config.go`,
`cmd/service/internal/bootstrap/startup_idempotency.go`,
`internal/infra/postgres/queries/postgres_http_idempotency.sql`, migrations 3
and 9, `test/postgres_http_idempotency_*_test.go`, `internal/problem`,
`internal/reqctx/principal.go`, and the two skill references.

| Go decision (fact) | Reason that carries over | Go-only mechanism | Rust disposition |
| --- | --- | --- | --- |
| Claim, business work, and result in one transaction; migration 9 dropped the two-phase `reserved`/`completed` schema of migration 3 | A rollback leaves nothing that blocks a retry; replay evidence shares the effect's fate | pgx, sqlc, Goose up/down | Carry the behavior; the schema starts at the one-transaction shape (no reservation history) |
| Scope = verified caller (`subject:` else `client:`, issuer and value length-prefixed), operation, optional resource; identity stored as a SHA-256 digest | A global key lookup is cross-caller disclosure; subject vs client must not collide | `reqctx.Principal` exists without an auth profile | Carry caller + operation; drop the resource refinement (D2) |
| Fingerprint = versioned SHA-256 of operation-owned semantic input; version mismatch is a mismatch; the version and representation stay stable across rolling deployments and the retention window, and an encoding-only change waits for live rows to expire or ships a compatibility reader first | Compare meaning, not bytes; honest retries replay across deployments; a deliberate meaning change is a new version | `encoding/json` | Carry the rule, including that stability rule. The canonical encoding is design |
| Key = exactly one field line of 1..255 RFC 9110 `tchar`; missing/invalid is 400 `bad_request`; grammar applied after authentication | A client cannot look up an expired draft; auth failures precede key errors | `CaptureKey` wrapper | Carry (D4) |
| Mismatch 422 `idempotency_key_mismatch`; 503 `idempotency_unavailable`; 503 `idempotency_outcome_unknown` | Stable, distinct recovery per outcome | — | Carry, add 409 in-progress (D1) |
| A concurrent same-key request blocks on the unique index until the winner ends, then replays (`TestPostgresHTTPIdempotencySerializesConcurrentReplicas`) | Arbitration at the writer across replicas | pgx context cancellation sends a server cancel request | Deviate to non-waiting 409 (D1) |
| Claim guarded by `NOT pg_is_in_recovery()` and `transaction_read_only = off`; read path refuses a non-writer even when a row exists | A replica's absence never authorizes execution | — | Carry (D6) |
| `CommitUnknown` → readback on a fresh writer connection: found → stored result; otherwise `outcome_unknown`, never re-executed | Unknown is its own outcome | Test injects an unknown result around a real commit | Carry |
| Only a 2xx codec result is stored; encoded result ≤ 1 MiB; oversize rolls back | Failure rolls back the effect, so there is nothing to replay | JSON codec re-renders generated types | Carry success-only (D5) and a body ceiling (D8) |
| Declared success headers limited to `Content-Disposition`, `Content-Encoding`, `Content-Language`, `Content-Type`, `Location`; operation must be protected with no anonymous alternative and declare 400/401/403/422/500/503/504 | Replay must not promise what it cannot reproduce | kin-openapi document walk at startup | Carry; add 409 and the Rust protected 431; decide runtime handling of other headers |
| Declared operations activate the store: requires `postgres.enabled`, positive `http_idempotency.retention` (no default; `0s` placeholder), startup cleanup pass; otherwise inert | Retention is the published client retry window, owned by the deployment | koanf | Carry the observable rule (D7) |
| Cleanup every minute, 500-row `FOR UPDATE SKIP LOCKED` batches draining the backlog; failure is degraded maintenance | Bounded, non-blocking maintenance | — | Carry as fixed component policy |
| `HTTP_IDEMPOTENCY=none\|postgres`; `postgres` requires `DATABASE=postgres` | Pack depends on the database | Bash initializer | Carry and add `AUTHN≠none` (D3) |

The Go skill reference `go-api-contract/references/idempotency-and-replay.md`
adds: bind the key to the authenticated caller; 409 for an in-flight retry and
422 for different input; reserve the key only from the durable boundary, so a
request rejected earlier leaves it usable; prove equivalent replay, mismatch,
in-flight conflict, and cross-caller isolation. Its 409 guidance and the Go
store's blocking wait disagree; the Go store never emits 409.

## Current external contracts

| Claim | Primary locator | Decision effect and limit |
| --- | --- | --- |
| **Fact.** `draft-ietf-httpapi-idempotency-key-header` is at revision 07 (2025-10-15), state *Expired* (2026-04-18), WG document, not an RFC. It defines the value as an RFC 8941 sf-string, recommends UUID-like keys, lets the resource define uniqueness and publish its expiry policy, and says a missing key SHOULD be 400, different payload 422, a retry before completion 409, and a retry after completion SHOULD receive the earlier result, "success or an error". | [Datatracker record](https://datatracker.ietf.org/doc/draft-ietf-httpapi-idempotency-key-header/), [rev 07 text](https://www.ietf.org/archive/id/draft-ietf-httpapi-idempotency-key-header-07.txt) §2.1–2.7 | Adopt its 400/409/422 split as this API's published rule, not as a standard. The unquoted grammar (D4) and success-only replay (D5) are recorded deviations. The adopter guide publishes the retention. |
| **Fact.** `INSERT ... ON CONFLICT DO UPDATE` guarantees an atomic insert-or-update under concurrency; the `WHERE` condition is evaluated last and conflicting rows are locked even when it is false. In Read Committed, a conflict with another transaction's not-yet-visible row is resolved against that row. | [PostgreSQL 18 INSERT](https://www.postgresql.org/docs/18/sql-insert.html#SQL-ON-CONFLICT), [Transaction isolation §13.2.1](https://www.postgresql.org/docs/18/transaction-iso.html#XACT-READ-COMMITTED) | Writer-side unique arbitration is feasible, and a duplicate insert waits for the other transaction to end. |
| **Fact.** In Repeatable Read, modifying or locking a row that a concurrent transaction changed and committed fails with "could not serialize access due to concurrent update"; the application retries. | [§13.2.2](https://www.postgresql.org/docs/18/transaction-iso.html#XACT-REPEATABLE-READ) | Arbitration under stricter isolation can surface as a retryable failure; it must never become a second execution. |
| **Fact.** Transaction-level advisory locks are released at transaction end; `pg_try_advisory_xact_lock` returns false without waiting. `lock_timeout` applies to each lock acquisition attempt, explicit or implicit. | [Advisory locks §13.3.5](https://www.postgresql.org/docs/18/explicit-locking.html#ADVISORY-LOCKS), [functions §9.28.10](https://www.postgresql.org/docs/18/functions-admin.html#FUNCTIONS-ADVISORY-LOCKS), [lock_timeout](https://www.postgresql.org/docs/18/runtime-config-client.html#GUC-LOCK-TIMEOUT) | A non-waiting in-progress signal is feasible (candidate mechanisms: a transaction-scoped try-lock, or a very short `lock_timeout` on the conflicting insert). Selection is design. |
| **Fact.** `statement_timeout` and `idle_in_transaction_session_timeout` bound a running statement and an idle open transaction; `client_connection_check_interval` defaults to 0, so the server notices a lost client only at its next socket interaction. | [Client defaults](https://www.postgresql.org/docs/18/runtime-config-client.html), [connection settings](https://www.postgresql.org/docs/18/runtime-config-connection.html) | An abandoned attempt releases its transaction within the template's 8 s + 8 s session budgets at worst. |
| **Fact.** Hot standby sessions are strictly read-only and `transaction_read_only` is always on; `default_transaction_read_only` makes new transactions read-only; `pg_is_in_recovery()` reports recovery. | [Hot standby](https://www.postgresql.org/docs/18/hot-standby.html), [functions §9.28.4](https://www.postgresql.org/docs/18/functions-admin.html#FUNCTIONS-RECOVERY-CONTROL) | The writer check is per transaction; a read-only session is refused. |
| **Fact.** With `synchronous_commit = on`, success is reported after the local WAL flush; synchronous replication waits for standby confirmation after the commit record is written locally; with `off`, a reported commit can be lost on a crash as a whole transaction. | [WAL settings](https://www.postgresql.org/docs/18/runtime-config-wal.html#GUC-SYNCHRONOUS-COMMIT), [synchronous replication](https://www.postgresql.org/docs/18/warm-standby.html#SYNCHRONOUS-REPLICATION), [async commit](https://www.postgresql.org/docs/18/wal-async-commit.html) | A lost acknowledgement can hide a durable commit. Because effect and record commit together, they share one durability fate. |
| **Fact (version-exact, `sqlx` 0.9.0 per `Cargo.lock`).** `Transaction::commit` clears `open` only after `COMMIT` returns (`sqlx-core-0.9.0/src/transaction.rs:120`); dropping an open transaction queues `ROLLBACK` into the write buffer (`:265`, `sqlx-postgres-0.9.0/src/transaction.rs:62`). Dropping a `PoolConnection` spawns `return_to_pool`, which pings: it flushes the buffer and waits for every pending ReadyForQuery, with no client-side timeout (`sqlx-core-0.9.0/src/pool/connection.rs:199,275,314`; `sqlx-postgres-0.9.0/src/connection/mod.rs:94,136,180`). | local Cargo registry source | A request future dropped during commit may still commit (the `COMMIT` precedes the queued `ROLLBACK`), so cancellation or timeout after commit starts is an unknown outcome. A dropped request that was waiting on a lock keeps its pooled connection busy until the server statement ends. |
| **Fact.** `tower` 0.5.3's timeout future returns `Elapsed` when its sleep fires; the inner handler future is dropped with it (`tower-0.5.3/src/timeout/future.rs`). | local registry source | The 504 path drops in-flight idempotent work mid-transaction. |

## Solution discovery

Neutral terms: at-most-once execution per scoped key; replay of a stored
response; writer arbitration across replicas; atomic commit with the caller's
business transaction; fail-closed store.

| Candidate (crates.io, 2026-09-23) | Class | Evidence | Disposition |
| --- | --- | --- | --- |
| `axum-idempotent` 0.4.0 (MIT, released 2026-09-23, 6,923 downloads) | Substitute | README: response cache in a `ruts` cookie session; best-effort; "two identical requests that arrive concurrently can both reach the handler"; forwards without handling when the session or store is unavailable | Eliminated: no single execution, fails open, no PostgreSQL or transaction coupling |
| `idempotent` 2.0.0 (MPL-2.0, 2026-09-16, 150 downloads, MSRV 1.98) | Substitute | Memory and Valkey stores; lease plus fencing token in a separate store; its `Fenced` outcome is an effect that ran but was not cached | Eliminated: cannot commit inside the business transaction; MPL-2.0 is outside `deny.toml`'s allow list |
| `actix-idempotent`, `sova-idempotency`, `minco-plugin-idempotency`, `kcode-kweb-idempotency`, `cratestack-*`, `reliar-inbox` 0.0.0 | Not substitutes | Other frameworks, framework-bound, or a messaging inbox | Not comparable |

Queries: `idempotency`, `idempotent`, `idempotency-key`, `idempotency axum`,
`idempotency tower`, `idempotency postgres`, `idempotency sqlx`,
`idempotency middleware`, and exact names (`tower-idempotency`,
`http-idempotency`, `axum-idempotency`, `sqlx-idempotency`: absent). Later
queries added no new substitutes. **Inference:** no maintained crate commits a
replay record inside the caller's PostgreSQL transaction, so a template-owned
boundary over the existing `sqlx`, `infra-postgres`, problem catalog, protected
composition, and `RequestDeadline` is justified. Digest crates are already
resolved transitively (`sha2` 0.10.9 and 0.11.0, `aws-lc-rs` 1.18.1). Technical
Design selects the mechanism and any direct dependency under `rust-dependencies`.

## Decisions and their evidence

- **D1, concurrent attempts: immediate 409, no waiting (decision; deviates from
  the Go store).** The draft and the Go skill reference answer an in-flight
  retry with 409. With sqlx 0.9, a waiting request whose future is dropped
  still occupies its pooled connection until the server statement ends. The
  default pool has four connections, so a few duplicate retries of a slow
  operation could starve unrelated requests for up to the 8 s statement budget.
  Counter-evidence: Go's wait gives a transparent replay. The cost is one more
  client round trip, guided by `Retry-After: 1`. Reopen if sqlx cancels server
  statements on drop, or a measured workload shows 409 churn is harmful.
- **D2, scope = verified caller + `operationId`, no resource refinement
  (decision).** A caller-controlled refinement lets a changed retry escape
  mismatch detection and execute twice. Without it, reusing a key for a
  different target is 422, which is safe. That holds because the semantic input
  fingerprints every typed value that selects the target, whether it comes
  from the path, query, a header parameter, or the body. Counter-evidence: Go's
  optional resource serves multi-tenant machine callers. Reopen if a real
  caller needs per-tenant key namespaces.
- **D3, `HTTP_IDEMPOTENCY=postgres` requires `DATABASE=postgres` and an
  authentication profile (decision; deviates from Go).** Rust `AUTHN=none`
  removes the principal type and protected composition, so a retained pack
  there could never serve an operation. That is a partial capability, which the
  roadmap forbids. A caller-less key would be a global namespace, which is a
  disclosure. Reopen if the template gains another verified-caller mechanism.
- **D4, key grammar: one unquoted field of 1..255 `tchar` (decision).** The
  draft's quoted sf-string form is refused with 400. The draft expired without
  an RFC; the Go template and common practice use unquoted tokens; one grammar
  avoids `"k"` versus `k` ambiguity. Reopen on RFC publication or a consumer
  that requires sf-string.
- **D5, only 2xx successes are stored and replayed (decision; deviates from
  the draft's "success or an error").** Under the one-transaction requirement,
  a failure rolls the effect back. Storing the failure would need a second
  commit and would block a legitimate retry after a transient error. Reopen if
  a product needs deterministic business rejections replayed.
- **D6, fail closed on writer uncertainty (decision, Go parity).** Arbitration,
  replay reads, and commit readback use the writable primary only. A read-only
  session, a replica, or an unavailable database never authorizes execution or
  replay.
- **D7, retention (decision and assumption).** Adopter-supplied, with no
  template default, as in Go: the draft makes the expiry policy the resource's
  published decision. Bounds are 1 minute to 30 days. The lower bound is below
  any plausible retry backoff; the upper bound keeps storage bounded and leaves
  permanent duplicate guards to business uniqueness. Both are assumptions;
  reopen on a real requirement outside them. Expiry is fixed when the record is
  written, on the database clock. The cleanup cadence (one minute, Go parity)
  and the batch size are fixed component policy, not configuration.
- **D8, stored response bounds (decision).** The body ceiling is 1 MiB
  (1,048,576 bytes): Go's precedent is 1 MiB for the encoded result, and the
  ceiling also equals the default `http.max_body_bytes`. The replayable header
  fields are bounded at 8 KiB in aggregate. That is half the default 16 KiB
  inbound head limit (`http.max_header_bytes`) and far more than the five
  allowed fields need (assumption; reopen if a real operation needs more).
  Both are fixed component limits, not knobs.
- **D9, observability (decision).** One bounded outcome counter, following the
  authentication verification metric, plus warn logs for cleanup failures.
  HTTP metrics cannot tell a replay from an execution.
- **Matrix arithmetic (inference).** 12 graphs with `HTTP_IDEMPOTENCY=none`
  plus 4 with `postgres` (`DATABASE=postgres` × 2 auth engines × 2 outbound)
  gives 16 runtime graphs and 16 × 8 = 128 canonical projections. Eight
  non-harness selections are refused: 6 with `DATABASE=none`, 2 with
  `DATABASE=postgres` and `AUTHN=none`. Implementation proves the counts
  against the resulting source.

## Falsifiers and downstream proof

The strongest wrong implementations:

- claim-then-check outside the business transaction: the rollback falsifier
  catches a record that survives;
- a read-then-insert race: two independent pools, one executor held open,
  more than one effect;
- a lock wait for duplicates: a held winner, and a duplicate that does not
  answer 409 promptly;
- re-execution after a lost acknowledgement: count effects after a real commit
  whose acknowledgement is suppressed;
- a caller-less scope: two callers, one key, one replayed body;
- replay from a read-only session;
- an expired record that is still replayed;
- a declared `x-idempotent` operation that is not served through the boundary,
  or the reverse.

All but the last need a real PostgreSQL (`ALLOW_HEAVY=1 make test-integration-db`).
Two independent pools against one server are the valid cross-replica proof
boundary, because arbitration happens at the database.

**Unknown:** no local PostgreSQL probe ran during Definition. The documented
semantics suffice for the contract, and implementation proof must observe
them. Whether hyper drops a handler future on client disconnect is not
investigated; the contract holds either way.
