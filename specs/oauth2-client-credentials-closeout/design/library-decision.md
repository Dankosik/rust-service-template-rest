# Library decision

Status: ready; consumed by the reviewed Technical Design. Evidence date: 2026-10-02.

## Decision and drivers

Retain the existing two-grant adapter over `infra-outbound-http`, jsonwebtoken,
aws-lc-rs, Tokio and Moka. Add no OAuth library, crypto backend, crate or
toolchain version. The current repair concerns the template's cancellation,
deadline, reuse, suppression and retention policy; none of the compared grant
libraries removes those responsibilities while preserving the accepted contract.

This is a local-fit decision, not an absence claim. Huskarl is a credible
maintained client with token exchange, private-key JWT authentication and DPoP.
Its optional platform crypto can be disabled and its supported HTTP and signer
interfaces can use the existing admitted transport/backend. That option was
evaluated, not dismissed because its default crypto has additional dependencies.

The common drivers are both accepted grants; exact assertion policy; fixed
bounded HTTPS transport; no internal retry or resource replay; closed errors;
acquisition-start monotonic lifetime; request-owned foreground cancellation;
one service-token acquisition at a time; the accepted cache and refresh policy;
and removal of enough current machinery to justify migration and proof cost.

## Compared mechanisms

| Mechanism | Removed responsibility | Remaining or added responsibility | Disposition |
| --- | --- | --- | --- |
| Current adapter with targeted repairs | Superseded detached refresh path and incorrect expiry/failure/retention behavior | The existing small forms, response admission, assertion policy, and all per-owner policy remain local | Selected: directly repairs the failing owners without introducing a second protocol/error/clock representation |
| oauth2 5.0.0 with custom HTTP | Client-credentials request building and standard response parsing | Private-key assertions, token-exchange fields/response policy, bounded transport bridge, closed errors, and all cache/lifecycle policy remain; extension parameters do not themselves implement those semantics | Rejected for this closeout: partial replacement, no decisive reduction of the changed mechanism |
| Huskarl 0.11.4 / core 0.10.5 using built-in grants/auth/cache | Standard grant forms, JWT auth construction and some caching | Built-in defaults do not preserve the template's assertion clock, client-credentials audience field, monotonic admission, error projection, no-retry rule or owner/drop behavior | Rejected as a direct replacement; differences are concrete compatibility costs, not proof the library is unsuitable generally |
| Huskarl with default features disabled, custom HTTP, signer/auth and supported grant extension points, retaining template policy | Standard exchange form building/parsing can move upstream | HTTP deadline/limit/error bridge, custom authentication policy, custom client-credentials audience handling, no-retry adaptation, response re-admission and current cache/lifecycle policy remain | Viable but rejected here: added adapters span the same boundaries as the removed form/parse code, while the defects still require the same local repairs |

No numeric code-size or performance win is claimed. The candidate replacement
would remove the `request_client_credentials`/`request_token_exchange` form
assembly and parts of `post_form` parsing. It would not remove `Signer` policy,
`into_token` admission, service-token state, Moka subject isolation, 401 identity
checks, transport wrappers, metrics or the new owned refresh lifecycle. A
supported custom `ClientAuthentication` can reuse the current signer directly;
it need not add a raw-signature adapter merely to preserve private_key_jwt.

## Decision-changing published-source checks

The [Definition research](../research/baseline-and-libraries.md) establishes
versions, publication/maintenance signals and supported seams. Technical Design
independently read the exact [Huskarl client archive](https://crates.io/api/v1/crates/huskarl/0.11.4/download)
and [core archive](https://crates.io/api/v1/crates/huskarl-core/0.10.5/download):

- Client `src/grant/client_credentials.rs` accepts scope, resource and
  authorization details, but its built-in client-credentials form has no
  `audience` member. The template sends that accepted configured field.
  `OAuth2ExchangeGrant` supports a custom form; the gap is extra adapter work,
  not an impossible integration.
- Core `src/client_auth/jwt_bearer.rs` exposes custom audience, expiry duration
  and explicit typ, but constructs its JWT with `issued_now_expires_after` and
  no template backdated nbf. A custom `ClientAuthentication` implementation
  can supply the exact existing assertion/form fields. `JwsSigner` and
  `JwsSignerSelector` remain supported alternatives using admitted crypto.
- Client `src/grant/core/grant.rs` calls `with_dpop_nonce_retry!`; the macro in
  `src/grant/core/form.rs` retries a `use_dpop_nonce` error once. The inspected
  condition is the response verdict, with no NoDPoP guard. A no-retry integration
  must account for this supported default-method behavior even when DPoP is
  not selected; blindly adopting `exchange()` does not preserve the template.
- `src/grant/core/token_response.rs` keeps raw lifetime accessible, but the
  ordinary conversion anchors expiry to response-time wall clock. The template
  must continue its acquisition-start monotonic admission, omitted-expiry and
  overflow policy. Its strict issued-token-type and content-type admission also
  remain local requirements.
- Core `src/http/mod.rs` supplies `HttpClient`, and
  `src/client_auth/mod.rs` supplies the custom authentication seam. These permit
  a bounded transport and no optional native-crypto admission. Upstream errors
  are richer than the template's closed enum, so sanitization still belongs at
  this adapter boundary.

The [oauth2 5.0.0 API](https://docs.rs/oauth2/5.0.0/oauth2/) supports custom HTTP
and client credentials. Its absence of a native same-level private_key_jwt plus
RFC 8693 flow leaves the local profile policy with this crate, as in the existing
decision record.

## Cost, dependencies and reopen conditions

Accepted cost: the template continues owning its deliberately narrow protocol
adapter and must maintain its protocol/negative-path proof. No claim is made
that a larger application's OAuth needs should use the same approach.

No Huskarl graph is admitted, installed or resolved; therefore no speculative
transitive count, vulnerability clearance or compile-compatibility claim is
made. The optional native backend's prerelease RSA dependency is not the reason
for rejection. The young pre-1.0 release history increases migration/proof cost
but is not a security finding. No DPoP rollout is selected.

The existing locked graph resolves Tokio 1.53.1, Moka 0.12.16 and jsonwebtoken
11.1.0. A locked, no-default-features normal feature-tree inspection confirmed
those package identities. Implementation should make Tokio `sync` and `macros`
explicit normal features if the selected driver uses channels and `select!`;
both are already resolved in the workspace, and no version/crypto change is
intended. Reinspect the normal no-gRPC feature route after that declaration.

Reopen if a published version removes the named adaptation costs, new required
grants/DPoP make the upstream mechanism materially more valuable, an advisory
changes the admitted backend decision, or Implementation finds that retained
code requires more machinery than the supported upstream integration. A
different version/graph reopens dependency Research before admission.
