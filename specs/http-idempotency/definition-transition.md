# Definition Transition Result V1

```text
status: ready
owner: Definition
result: specs/http-idempotency/spec.md; intent.md; research/synthesis.md
review: specs/http-idempotency/definition-review.md (CONCERNS after one bounded delta
  recheck; the C1 wording obligation was discharged by the Specification owner)
movement_evidence: requester intent is concrete, and each of these has one grounded
  disposition: selection and combination rules, declaration and two-way OpenAPI
  agreement, key grammar, scope and fingerprint (including encoding stability),
  one-transaction execution, replay contents, retention, non-waiting concurrency,
  failure and uncertain-commit outcomes, authentication and outbound interplay,
  profile/lock/sync, observability, and proof expectations. Current primary evidence
  (sqlx 0.9.0 source, PostgreSQL 18 docs, IETF draft rev 07, Go HEAD 749473366b,
  crates.io survey) closes the feasibility questions Definition needs. Independent
  Specification Review permits movement.
reopen_owner: none
next_owner: Technical Design (System / Integration Design, then Rust Code /
  Ownership Design as triggered)
```

Final identities (SHA256):

- `intent.md`: `f67e695511da81efd527fc9bf501fc0bcd88badfe9aeeca7b5305db3801f1236`
- `research/synthesis.md`: `bf48091c7af098a95af0ca59c9bdef0c1b54a5212e068487dc26a17c905932a3`
- `spec.md`: `a2cc06e21dc08a6c16b051e1515c7383505fdc5bc1d0fee55cf6ac92e5ddbd59`

Technical Design must fix the following, without changing any observable rule
in the spec:

1. **Crate placement and the Rust API.**
   - The executor seam a handler calls after authentication, validation, and
     authorization. It hides PostgreSQL types from feature crates and gives the
     work only the provided transaction. It must fit the dependency direction:
     [boundaries](../../docs/architecture/boundaries.md) says a feature
     depends on no infra crate, while
     [First Production Feature](../../docs/first-production-feature.md) lets a
     feature use `infra-http`'s problem catalog. Technical Design reconciles
     the two.
   - The declaration and composition helper, modeled on
     `infra_http::authn::protect`.
   - The two-way agreement check (startup refusal plus contract tests),
     including the refusal of an advertised key without `x-idempotent`.
   - Detection of an active boundary.
2. **The non-waiting arbitration mechanism.** Candidates are a
   transaction-scoped try-lock and a short `lock_timeout`. It reads first, so
   concurrent replays never get 409, and it stays correct under read committed
   and under any isolation Technical Design permits.
3. **Fingerprints.**
   - The canonical encoding and digest, and how an operation declares its
     semantic input, including header and negotiation values.
   - Pinned literal encodings for the stability rule.
   - A way for an operation to supply a comparison that also accepts its
     previous encoding. Waiting for old-encoding records to expire would
     require pausing the operation, so this is the usable path for an encoding
     change.
4. **Schema and SQL.**
   - One forward-only migration storing only digests.
   - The [persistence](../../docs/architecture/persistence.md) deferrals this
     first profile schema triggers: `query!` with offline metadata and
     `sqlx-cli`, per-query spans, and the template's migration-history
     exemption.
   - The versioned stored format, the 1 MiB body and 8 KiB header bounds, and
     header stripping at the boundary.
5. **Commit outcomes.**
   - The mapping for a retryable versus a definite `TxError::CommitFailed`.
   - The `CommitUnknown` writer readback inside `RequestDeadline`, and any
     response reserve derived from that owner.
   - The 504 precedence.
6. **Cleanup and startup.**
   - The cleanup task: a one-minute cadence, with batch and statement bounds
     inside the existing background-join and dependency-close budgets.
   - Startup activation: the optional `http_idempotency.retention` with its
     1 minute to 30 days bounds, `postgres.enabled`, and the schema and
     writable-session checks before readiness admission.
7. **Contract surface.**
   - The `http_idempotency_outcomes_total{outcome}` wiring, including
     `abandoned` on drop.
   - The marker-scoped problem codes, and the response components for
     idempotent operations (409 and 503 declaring optional `Retry-After`).
   - Unreferenced reusable components are allowed but add Redocly
     `no-unused-components` warnings.
8. **Profile machinery.**
   - `HTTP_IDEMPOTENCY` parsing and refusals (`DATABASE=none`, `AUTHN=none`,
     unknown values).
   - The lock field and the admitted shapes in `scripts/lib/template_state.py`.
   - The `scripts/lib/template_profiles.json` inventory, and the `Cargo.lock`
     feature-edge projection.
   - The `none`-output equality proof against `d24d173`.
   - 128 projections and 16 graphs: graph IDs and the CI split (today 1–6 and
     7–12) in `.github/workflows/ci.yml`, `scripts/ci/template-init-check.sh`,
     `scripts/ci/verify.sh`, and `make/source.mk`, and the counts in
     `docs/template-sync.md`, `docs/build-test-and-development-commands.md`,
     and `docs/ci-cd-production-ready.md`. A CI gate change routes through the
     [CI/CD owner](../../docs/ci-cd-production-ready.md).
9. **Test seams.**
   - P6's lost-acknowledgement injection around a real commit.
   - P9's fixture strategy for the JWT graphs, with no verification bypass or
     principal constructor.
   - How database tests obtain caller scopes.
10. **Documentation.**
    - The marker-scoped guide.
    - `docs/architecture/http.md` and `persistence.md`,
      `docs/template-sync.md`, `docs/configuration-source-policy.md` (the new
      key), and `migrations/README.md`.
    - The roadmap's stage-10 independence sentence.

    The roadmap status row still reads "10.2 locally accepted", although PR
    #42 merged. That stale text is for the delivery owner.

Bounded assumptions, each with its reopen condition in the synthesis:

- **D1:** a non-waiting 409 instead of Go's blocking wait.
- **D2:** no resource or tenant scope refinement.
- **D3:** an authentication engine is required.
- **D4:** only the unquoted token grammar.
- **D5:** success-only replay.
- **D7:** retention bounds of 1 minute to 30 days, with no template default,
  because the adopter publishes the window.
- **D8:** 1 MiB body and 8 KiB replayable-header bounds.
- **D9:** the outcome counter.

No user-owned question survives. Each adopting service chooses its retention
value.

Authority stays with local stage-10.3 delivery. Other stage-10 capabilities,
PR, push, merge, publication, and deployment remain out of scope. The request
stops before push and explicitly requires real-PostgreSQL proof and
profile/harness validation. So `ALLOW_HEAVY=1 make test-integration-db` and the
initializer matrix (`ALLOW_FULL=1`) are local requirements for this delivery,
not an expansion of acceptance.

Reopen Specification for a new selection or combination rule, waiting
semantics, error replay, a scope refinement, a grammar change, or a
stored-format change. Reopen Research for changed sqlx drop or cancel
semantics, RFC publication of the draft, a PostgreSQL major-version change for
the proof image, or a new verified-caller mechanism. The parent coordinator
continues the request.
