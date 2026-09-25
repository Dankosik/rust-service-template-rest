# T1 implementation notes

Implementation is in `rust-service-template-rest.codex-authn-contract-and-libraries`
on `codex/authn-contract-and-libraries`, from reviewed phase snapshot `2935249`
and production base `4edd184`. The root owns the canonical task ledger and
publication. These notes record implementation inputs, not acceptance.

## Dependency integration

The direct JWT material-admission edge uses the already resolved
`aws-lc-rs 1.18.1`, declared once with default features disabled. The adapter
selects `alloc`, `aws-lc-sys` and `prebuilt-nasm`; all were present in the
pre-change feature graph. The JWT profile removes this direct edge together
with jsonwebtoken. Authentication no longer depends on `infra-egress-dns`;
independent outbound HTTP retains that owner. Config no longer depends on
`url`, because the adapter owns the one provider URL grammar. Existing tracing
supplies closed adapter diagnostics.

The root reconciled the user-supplied requirement that every Cargo invocation
remain locked with the dependency skill's lockfile-edit guideline: this
intentional change updates only the local packages' manifest-derived dependency
lists. No registry package, version or checksum changes. Both
`cargo metadata --locked --offline --format-version 1 --no-deps` and
`cargo tree --locked --offline -p infra-bearerauthn --depth 1` accepted that
graph. This is dependency-resolution feedback, not runtime proof.

The shared `tls-fixtures` projection retains tokio-rustls for either selected
authentication or outbound HTTP; auth-only TLS fixtures do not retain DNS
admission dependencies.

## Compile-only feedback during coding

`scripts/ci/validation-lock.sh -- cargo check --locked --offline
-p infra-bearerauthn -p service-config --all-targets` completed successfully
after fixing private cross-module claim-policy accesses and one malformed test
literal. It compiled production and test code; it ran no tests. Two adapter
dead-code warnings were returned to their writer for cleanup. The shared lock
serialized this work with the sibling checkout's build and OpenAPI generation.

Pinned serde source showed that internally tagged unit variants ignore
remaining fields even with the container's `deny_unknown_fields`. Config uses
an empty struct `None {}` variant and an explicit default implementation so
the accepted no-dormant-fields rule is enforced by serde itself. Exact audience
normalization retains every nonblank string unchanged, including surrounding
whitespace; only blank values reject.

## Assembled validation handoff

After HTTP/composition and canonical generation join, select the mixed-surface
route with `make plan` and run that route once under the delivery owner. The
matching build/workspace tests, generated OpenAPI agreement, documentation and
instruction checks, and profile projection checks remain required at that
boundary; a fresh independent assembled authorization/lifecycle review also
remains required. Keep any CI-owned profile/runtime work in its selected CI
route rather than multiplying full builds by harness and database dimensions.

Adapter inline proof now includes signed JWT validation and mixed-key material
admission, typed claims and scopes, private TLS exchanges with body/deadline
bounds, immediate introspection capacity, and refresh publication with distinct
waiter deadlines. Config proof covers mode shape, exact audiences and defaults.
These tests have been written and compiled where noted above; they have not
been executed as implementation feedback.

The focused consumer diagnostic also completed successfully:
`cargo check --locked --offline -p infra-http -p service -p jobs-worker
-p integration-tests --all-targets --features integration-tests/integration`.
It compiled the existing mounted database-fixture code without starting a
database or running any test. The initial pass identified two mechanical
cross-module/closure errors, which the HTTP owner repaired. Subsequent source
authoring removes an obsolete idempotency rule, supplies public Debug
implementations, and closes no-auth bootstrap marker coverage.

All four implementation writers joined. `make fmt` produced the final source
formatting. The final combined compile-only command covered all six changed
production/test packages and the existing integration feature successfully;
the remaining no-auth shadow-binding warning was repaired without changing
the source-marker layout. Canonical `make openapi-generate` then completed
under the common validation lock and produced no diff in
`api/openapi/service.yaml`. This completes artifact production, not final
validation or acceptance. No runtime tests, lint, aggregate validation or
independent review have run for this implementation candidate.
