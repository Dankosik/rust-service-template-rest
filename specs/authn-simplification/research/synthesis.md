# Authentication simplification evidence

Status: ready. Checked 2026-09-25 against base `4edd184` and resolved dependency
sources. Scope is the decisions in [the specification](../spec.md), not an
independent performance or security certification.

## Current behavior and decision effects

| Fact and primary locator | Decision effect |
| --- | --- |
| `crates/infra-http/src/authn.rs::protect` accepts one method tuple and authenticates only explicitly wrapped routes; `crates/service/src/bootstrap/mod.rs::Prepared` passes authentication into the idempotency composer rather than retaining a full-router verifier. | Use the assembled contract to enforce authentication on every registered operation, including a service without idempotency. |
| `crates/infra-bearerauthn/src/jwt.rs::parse_key_set` deserializes all raw keys together and propagates individual key failures. | One malformed/ineligible key must not invalidate independent usable keys. |
| `jwt.rs::verify_signature` and `claims.rs` split library signature validation from custom JSON visitors and claim validation. | Use typed serde claims and the library's verified decode/Validation boundary, retaining only application/profile rules the library does not implement. |
| `refresh.rs::Reservation` takes the triggering request's remaining deadline; `SharedRefresh::keys` awaits a mutex. `introspection.rs::verify` sleeps until the outer deadline when reserve is exhausted. | Independent process-owned refresh, snapshot reads, explicit deadline failure, per-waiter cancellation. |
| `provider.rs` disables pooling, forces HTTP/1, and installs custom DNS via `infra-egress-dns`; the same crate remains required by independent outbound HTTP. | Simplify only trusted authentication transport; preserve independent Stage 10.2 connection-address admission. |
| `config/src/authn.rs` is a mode plus foreign fields and duplicate URL checks; bearer parser fixes 32 KiB while default inbound headers are 16 KiB. | Tagged config variants, one validated URL grammar, explicitly related token/header bounds. |

## Primary external contracts

- [RFC 6750 section 3.1](https://www.rfc-editor.org/rfc/rfc6750.html#section-3.1)
  supports bare Bearer for absent/unsupported authentication, invalid_request
  for malformed bearer requests, invalid_token for rejected tokens, and
  insufficient_scope for authorization failure. These are separate outcomes.
- [RFC 7517 section 5](https://www.rfc-editor.org/rfc/rfc7517.html#section-5)
  recommends skipping unsupported/missing/out-of-range individual JWKs. Its
  key-id recommendation does not justify selecting an arbitrary duplicate key.
- [RFC 9068 sections 2 and 4](https://www.rfc-editor.org/rfc/rfc9068.html#section-4)
  requires signed access tokens, explicit type and its mandatory claims. The
  optional profile remains stricter than the general resource-server dialect;
  these normative requirements are not removed by the simplification request.
- [RFC 7662 section 2.2](https://www.rfc-editor.org/rfc/rfc7662.html#section-2.2)
  makes aud/iss/exp optional at the generic protocol level. Their mandatory use
  here is accepted service policy to prevent wrong-resource/wrong-issuer trust
  and unbounded token validity; missing evidence is a rejected token, not an
  IdP outage. Active-response syntax/type corruption remains provider failure.
- [RFC 8725 sections 3.1, 3.8, 3.9 and 3.11](https://www.rfc-editor.org/rfc/rfc8725.html#section-3)
  grounds algorithm allowlisting, issuer/audience validation and type isolation.
  Configuration trust does not make bearer headers or JWT claims trusted.
- [RFC 7515 section 4.1.11](https://www.rfc-editor.org/rfc/rfc7515.html#section-4.1.11)
  requires understanding critical extensions; unsupported critical headers
  remain rejected. No generic visitor is necessary to implement this rule.

## Resolved library evidence and alternatives

Local Cargo registry source is authoritative for these resolved versions:
`jsonwebtoken-11.1.0`, `reqwest-0.13.5`, `axum-0.8.9` under
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`.

`jsonwebtoken/src/decoding.rs::decode` verifies signature before returning
typed claims and runs Validation. `decode_header` is unverified and may only
select/check local policy. `DecodingKey::from_jwk` supports the requested key
families. `validation.rs` defaults to 60s leeway and nbf validation off, so both
must be explicitly set. Required-claim handling recognizes exp/sub/iss/aud/nbf,
not client_id/jti/iat; those remain typed application checks. `iss` typed as a
string closes the library validation layer's more permissive issuer shape.
The current 30s temporal equality boundary agrees with Validation's comparisons.
`ClaimsForValidation`'s `numeric_type` deserializer treats present nbf:null as
failed parsing; with validate_nbf enabled, validation.rs rejects it. Omitted nbf
remains allowed. The original optional-null assumption was therefore narrowed
to preserve this library-native rejection; introspection's present numeric null
remains malformed provider evidence. No payload preprocessing is required.
Its `from_jwk` conversion preserves family/material rather than a one-algorithm
binding. RFC 8725 section 3.1 therefore needs a local eligibility rule: assign
one algorithm before token selection and refuse ambiguous absent-alg RSA keys
when both RS256 and PS256 are configured. This is a normative rule, not optional
extra strictness.
`crypto/aws_lc/mod.rs` supplies RS256, PS256, ES256 and EdDSA verification.
`crypto/mod.rs` installs panic factories when neither or both feature providers
are selected automatically: choose aws-lc explicitly before first crypto use
and retain feature discipline. No dependency upgrade is justified.

[Reqwest 0.13.5 ClientBuilder](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html)
provides pooling, HTTPS-only, redirect policy, proxy disabling, timeout and retry
policy. Body size still requires bounded consumption. Connection reuse is
compatible with an operator-trusted destination; credentials must remain
per-request and redirects/proxies disabled. This is not evidence that general
untrusted outbound address controls are redundant.

Axum source `routing/method_routing.rs::route_layer` wraps registered method
endpoints while preserving fallback. Implicit HEAD dispatches GET when HEAD is
absent. Contract lookup must account for that inheritance; otherwise a global
method lookup would incorrectly classify normal HEAD as an undocumented route.

| Candidate | Fit and disposition |
| --- | --- |
| Existing jsonwebtoken + serde + reqwest + Tokio watch | Reuses pinned dependencies, signature/registered-claim validation, typed parsing, pooled HTTP and latest-value snapshots. Selected building blocks; application contract and refresh scheduling remain local. |
| Existing hand-written visitors/signature-only checks/custom auth DNS | Duplicates supported library mechanisms; remove the superseded paths within scope. |
| [tower-oauth2-resource-server 0.12.3](https://docs.rs/tower-oauth2-resource-server/0.12.3/tower_oauth2_resource_server/) | Same-level alternative for JWT HTTP middleware. Published behavior maps validation failures to 401 and does not establish this service's introspection, Problem/deadline, contract-derived route or joined-refresh semantics. User explicitly excludes wholesale adoption; no claim of a vulnerability or proven incompatibility is needed. A separate experiment can assess those seams if requested. |
| Full OIDC interactive client | Different boundary: this service consumes access tokens and does not perform login, grants or browser sessions. |

No live IdP was contacted. No latency, throughput, revocation freshness beyond
the existing refresh policy, or provider interoperability certification is
claimed. Refresh this evidence if pinned versions, trust boundary, algorithm
families, or the requested all-in-one experiment change. Technical Design owns
placement and exact mechanisms consistent with the closed behavior below.
