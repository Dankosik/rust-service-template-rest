# T1 — PostgreSQL destination identity and candidate progress

Outcome:
Replace bracketed admitted IPv6 host bytes and serial Tokio TCP starvation with
bare IPv6 options and concurrently owned native TCP attempts, preserving the
existing caller deadline and pool/transaction finality.

Consumes:
- [Specification](../spec.md#postgresql-literal-ipv6) and [fallback decision](../spec.md#conditional-address-fallback-decisions).
- [Design](../design/transport.md#postgresql-admission-and-candidate-selection) — exact normalization, winner cleanup and resolver-order error policy.
- [Execution boundary](execution-boundary.md) — assembly and final proof owner.

Provides:
- Correct admitted options and SQLx Tokio socket selection, regression coverage and PostgreSQL operator guidance.

Boundary:
Keep Dsn as sole admission owner; only parsed IPv6 changes host representation.
Keep original TLS identity, password replacement and all pool/migrator/LISTEN
consumers. Drop losing TCP futures/streams before the existing continuation.
Preserve empty-resolution and last-resolver-address errors, whole-return patch,
async-io behavior, pool/lifetime/statement/LISTEN budgets and uncertain effects.

Mutable owners:
- `crates/infra-postgres/src/dsn.rs`, existing PostgreSQL tests and provider-owned fixtures.
- `vendor/sqlx-core/src/net/socket/mod.rs`, its native tests and `vendor/sqlx-core/PATCHES.md`; retain separate TCP and whole-return retirement.
- `docs/architecture/persistence.md` for affected DNS/socket/password/CA/pool/LISTEN assertions.

Exclusive locks:
- none; reserve a shared test fixture with its current writer before an overlapping change.

Final validation:
- Claim: IPv6 reaches the intended dial/TLS identity; later healthy candidates make progress and failure/finality semantics remain intact.
- Checks: Matching assembled build/tests and documentation route under execution-boundary.md; existing real-database owner for observed database claims; no additional checks.
- Observable: Admitted options and selected native connection reject the old bracket/starvation defects without SQL/effect on losing candidates; no claim beyond the exercised boundary.

Reopen if:
The selected SQLx shape cannot preserve the accepted identity/error/cleanup
contract (Technical Design), or behavior must be excluded/changed (Definition).
