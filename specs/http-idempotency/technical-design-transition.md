# Technical Design Transition Result V1

```text
status: ready
owner: Technical Design
result: specs/http-idempotency/design/system.md; design/ownership.md
review: specs/http-idempotency/technical-design-review.md (PASS after one
  bounded delta recheck); design/ownership-review.md (all triggered lenses PASS)
movement_evidence: every one of the ten Definition decision groups is closed
  without downstream invention, and no observable spec rule changes. Closed:
  the authentication-split placement with an opaque transaction handle; the
  writer-gated, non-waiting advisory try-lock under read committed; canonical
  encoding 1 with pinned vectors; one-transaction execution with mapped
  commit outcomes and a bounded readback; the schema, SQL, stored format, and
  bounds; activation, cleanup, and lifecycle; profile, lock, and projection
  machinery with a 16-graph proof plan; proof locations and test seams;
  documentation owners; adopter release closure. Probes measured the API
  shape and the opaque handle, arbitration semantics, lost acknowledgements,
  marker formatting, the SQL text, and the pinned vectors. An independent ownership panel and an independent
  Technical Design Review permit movement.
reopen_owner: none
next_owner: Planning
```

Planning consumes the [system design](design/system.md), the
[ownership map](design/ownership.md), the ready [Specification](spec.md), the
[ownership panel receipt](design/ownership-review.md), and the
[review](technical-design-review.md). Planning sequences and bounds this
work. It must not re-decide any of the following; changing one reopens its
owner below:

1. **Placement.** Follow the authentication split. The seam is
   `infra_http::idempotency`, and `infra-idempotency-store` is the only crate
   that names the profile table. No feature depends on the store, `sqlx`,
   `infra-postgres`, or any provider crate. `Tx` stays opaque, and only the
   store's `connection` reaches the connection; `infra-http` never
   re-exports it. The operation's SQL lives in a `crates/infra-<provider>`
   adapter behind the feature's `async-trait` port.
2. **Public API.** Keep the signatures in system design section 4, with
   `Activation::Active` `#[non_exhaustive]` and `execute` taking
   `Result<Fingerprint, FingerprintError>`.
3. **Arbitration.** The first statement after an explicit `READ COMMITTED`
   `BEGIN` is a writer-gated `pg_try_advisory_xact_lock`, followed by the
   read. A live record decides before the lock result. No isolation choice
   is offered, and nothing is retried. The lock key is the scope digest's
   first 8 bytes, read big-endian.
4. **Schema and SQL.** Keep the section 6 migration and statements. Expiry
   uses the database clock. The upsert requires exactly one row. Readback
   runs on a fresh connection, bounded by `RequestDeadline` minus 100 ms.
   Cleanup runs 500-row batches with a 1 s statement timeout, every 60 s.
   `Store::new` keeps whole microseconds of the retention, after
   configuration's range check.
5. **Bytes.** Keep canonical encoding 1, the digest domains, and the pinned
   vectors. Keep stored success format 1 with its bounds and five headers.
   The first response is answered from the captured stored form.
6. **Outcomes.** Keep the section 9 mapping and the order of the key layer
   inside `protect`. 504 precedence uses a pending future. The counter
   records exactly one outcome once the fingerprint is valid, with a drop
   guard for `abandoned`. The 2xx wiring guard stays.
7. **Contract.** `Composer::route` and `Composer::agree` enforce the
   declaration rules. `Composer::components` is the family's only
   registration path. `openapi.rs` owns the expected-value constants.
8. **Configuration and lifecycle.** `http_idempotency.retention` accepts
   1 min to 30 days, is vacant when empty, and is required only when the
   boundary is active. The contract is built before admission, and
   activation runs before readiness. The cleanup task is spawned on the
   existing tracker and joined in the existing stage.
9. **Profile machinery.** `HTTP_IDEMPOTENCY=none|postgres` keeps the refusal
   messages and one combination predicate. The lock has five fields plus
   the historical shapes. Keep the two marker profiles with their ids and
   paths, the 128 projections, and the 16 graphs (13-16 appended, with a
   database step). CI gains a fourth initializer part, which restores the
   `database-postgres` cache and saves none. Receipts carry the digests.
   Where P9 is retained, the classifier selects the database suite for
   `infra-http` and `infra-bearerauthn` changes. `template-init-check` keeps
   `requires_heavy=false` and adds `requires_docker=true`. The one-shot
   `none` equality runs against `d24d173`.
10. **Proof placement.** Follow system design section 11 and the ownership
    map: inline owner tests, P1-P8 at the store boundary, and P9 mounted in
    the introspection graphs. The one-shot commit proxy stays. No
    `test-support` feature and no JWT fixture are added.
11. **Deferrals.** `query!`, offline metadata, `sqlx-cli`, and per-query
    spans wait for the first feature-owned repository. There is no
    migration-history exemption.
12. **Documentation.** Keep the documentation owners and marker ids in the
    ownership map. No portable instruction changes. Unmarked edits in
    service-owned files name no pack item. Template-owned files carry no
    markers.

Bounded assumptions carried forward, each with its reopen owner:

- Only `READ COMMITTED` is offered. Reopen Technical Design when an operation
  needs a stricter level.
- The lock key is a 64-bit prefix. Reopen Technical Design on an observed
  false 409 that no holder explains.
- Encoding 1, the digest domains, and stored format 1 are fixed. Reopen the
  Specification, with a compatibility plan, to change them.
- The readback reserve is 100 ms. Reopen Technical Design if measured
  readback latency on a healthy writer exceeds it.
- Cleanup runs 500-row batches every 60 s with a 1 s statement bound. Reopen
  Technical Design for a measured backlog one tick cannot drain, or for lock
  waits caused by cleanup.
- The portable-instruction reading in system design section 3 holds. Reopen
  the Specification's non-goal if an instruction is read to forbid features'
  existing `infra-http` edge.
- The persistence deferrals follow the triggers in section 6.4.
- Definition's D1-D5 and D7-D9 remain as the synthesis records them.

Reopen Research for changed sqlx drop or commit semantics, a PostgreSQL
major-version change in the proof image, or RFC publication of the draft.
Reopen the Specification for any observable change. Reopen Technical Design
if implementation disproves a pinned API, statement, lock behavior,
cancellation path, marker layout, or proof mechanism.

Carried notes, not decisions: Implementation may adopt them without
reopening a decision.

- The guide's readback for converting an existing operation should identify
  the release by `app.version` in `service_starting` or by the platform's
  rollout status, not only by the operation count (review note).
- Not adopted: the ownership panel's optional `compile_fail` guard for the
  opaque `Tx`. The `attempt.rs` row's forbidden responsibilities carry that
  rule.

Current identities (SHA256):

- `design/system.md`: `47cf5fe908bc790a0dae2425a05e3e1fd3cae9ef0c1aa81bce547b8b9964510f`
- `design/ownership.md`: `80818a74412e3ff55be24086d41df3d22d6e3a4f353d06c7ce8f17e7ade7414e`
- `design/ownership-review.md`: `09843798c46b7b9feb67b1345d0d86f9dad8c6acdf54c11466fc48f81a2408a3`
- `technical-design-review.md`: `023a019d6f094e8ae14bcf679774845709fec84babc90e639843e17f5b73c078`

Evidence boundary: static design, measured probes (system design section 14),
and independent source-backed review. No product code, configuration, CI, or
documentation outside `specs/http-idempotency/` changed, and nothing was
compiled in the repository. The probes ran only in the session scratchpad,
and their container and processes were stopped and removed. No runtime,
database suite, initializer matrix, CI, or deployment result is claimed.

Authority stays with local stage-10.3 delivery. Planning is the next fresh
actor, and this actor stops here. Other stage-10 capabilities, PR, push,
merge, publication, and deployment remain out of scope. No user-owned
question survives.
