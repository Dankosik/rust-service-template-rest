# Service-to-service authentication: research synthesis and decisions

Recorded 2026-09-29 against `origin/main` 3c220db, before implementation.
[Intent](../intent.md) owns requester meaning. This file owns the accepted
decisions, the crate comparison, rejected alternatives and reopen conditions
while the work is open; on completion the durable parts move into
[Outbound machine authentication decisions](../../../docs/outbound-machine-authentication-decisions.md),
[Authentication](../../../docs/authentication.md) and the new service-to-service
guide, and this bundle is deleted.

Labels: **F** fact from a cited primary source, **I** inference, **A** labeled
assumption, **U** unknown. Quantities are external limits unless marked.

## 1. Verdict on the working model

The model is confirmed with one outdated rule and two recorded deviations.

| Element | Verdict | Primary basis |
| --- | --- | --- |
| Short-lived JWT access token by client credentials, `aud` = receiver | Confirmed. Refresh tokens are not used: client credentials is re-run. | **F** RFC 9700 §2.3 (audience restriction SHOULD, RS MUST refuse a wrong audience); RFC 6749 §4.4.3 (no refresh token); RFC 9068 §2.2, §4 |
| `private_key_jwt` to the AS instead of a shared secret | Confirmed. **Outdated rule:** the assertion `aud` is the AS *issuer identifier as its sole string value*, not the token endpoint. | **F** RFC 9700 §2.5 (asymmetric client authentication RECOMMENDED); draft-ietf-oauth-rfc7523bis-11 §4 (2026-03-26, RFC Editor queue); OIDF disclosure 2025-01-24, CVE-2025-27370/27371; OAuth 2.1 draft-16 §2.4; FAPI 2.0 Security Profile |
| User context through RFC 8693 token exchange, never an unsigned header | Confirmed. The receiver decides on the top-level claims and the current actor only. `may_act` is evaluated by the AS, not the receiver. | **F** RFC 8693 §4.1, §4.4; RFC 9700 §4.13 (proxy-injected headers must be sanitized and the hop protected) |
| Scopes checked per route | Confirmed. | **F** RFC 9700 §2.3; RFC 9068 §4 |
| Bearer tokens without sender constraint | **Deviation from a SHOULD.** Recorded below with a reopen condition. | **F** RFC 9700 §2.2.1, §4.10; OAuth 2.1 draft-16 §1.4.3; only FAPI 2.0 makes it a MUST |
| Railway private network as the transport | **Deviation.** WireGuard encrypts but authenticates no service; tokens carry identity. | **F** Railway private-networking reference (fetched 2026-09-29); OAuth 2.1 draft-16 §1.5 |

Evidence: [standards](#evidence-custody) `standards.md` §0–§10.

## 2. Accepted decisions

### D1. One outbound acquisition path: signed client assertions, no client secrets

The outbound profile authenticates to the authorization server only with a
private key. `client_secret_basic` is removed, not kept beside it.

- **F** RFC 9700 §2.5 recommends asymmetric client authentication because the
  AS then stores no shared secret. Every shortlisted server supports a
  key-based method (§3).
- **I** A shared client secret is the same class of credential as the static
  bearer and HS256 schemes this work replaces. Keeping it as a second mode
  would leave the template with two ways, which the intent forbids.
- Cost: Amazon Cognito and other secret-only providers can no longer be used
  by this profile. **Reopen** when a required provider supports only shared
  secrets.

One request form: `grant_type=client_credentials`, `client_id`,
`client_assertion_type=urn:ietf:params:oauth:client-assertion-type:jwt-bearer`,
`client_assertion`, plus `scope` and `audience` when configured (RFC 7523 §2.2,
RFC 7521 §4.2). The same client assertion authenticates the token exchange
(D3a), so one registered client and one key per service cover both.

- **F** Keycloak, Hydra, Authelia, Spring Authorization Server, Duende, Okta and
  Auth0 (Enterprise) accept this form. Zitadel v4.19.2 does not: its service
  users log in with a key only through the RFC 7523 §2.1 JWT-bearer grant, and
  only an OIDC application, a second registration with its own key, may call
  token exchange (`internal/api/oidc/client_credentials.go`, `client.go`
  `VerifyClient`). Supporting the JWT-bearer grant would therefore still not
  give Zitadel a one-identity path, so the template does not add it.
  **Reopen** if Zitadel is chosen: the gap is the JWT-bearer grant form plus a
  separate exchange identity.

### D2. Assertion contents

- Claims: `iss` = `sub` = `client_id`; `aud` = the configured
  `assertion_audience`, one JSON string; `iat` = `nbf` = now; `exp` = now + 60 s;
  `jti` = a fresh random UUID v4. A new assertion is signed for every token
  request and never cached.
  - **F** rfc7523bis §4 and the OIDF notice require one string audience; Keycloak
    rejects several audiences by default and caps assertion age at 60 s from `iat`;
    Zitadel requires `iat`; Keycloak, Hydra, Authelia, Duende, Auth0 and Entra
    require a single-use `jti`. Spring, Duende and Authlib sign per request;
    Nimbus defaults to 60 s.
- `assertion_audience` is required configuration, not derived. The guide
  says to use the issuer identifier; a server that accepts only its token
  endpoint (Hydra v26, Okta, Entra) gets that URL. One literal covers every
  server without a mode switch.
  - **I** The audience-injection attack needs a client that talks to more than
    one AS (security-topics-update-03 §2.1). A service configured with one AS
    per integration is not exposed even with the token-endpoint value, which is
    why the literal is acceptable.
- Header: `alg`, `kid` (required configuration; Keycloak and Hydra select the
  key by it) and `typ: client-authentication+jwt` (rfc7523bis §4 SHOULD).
  - **F** Keycloak, Hydra, Authelia and Spring Security 7 do not check `typ`; Duende switches to strict issuer-audience
    mode on it, which D2 already satisfies. Spring Authorization Server 1.5.x
    (Spring Security 6.5) rejects it. **Reopen** if a required server rejects
    the typed header.
- Algorithms: `RS256` (default; the only algorithm every shortlisted server
  accepts), `PS256`, `ES256`. One algorithm per key (rfc8725bis §3.1).
  Ed25519 is omitted because Hydra and Duende do not accept it; if it is added
  later, use the fully specified `Ed25519` identifier (RFC 9864), not `EdDSA`.
- Key material: one PKCS#8 or PKCS#1 PEM private key per integration, supplied
  only through the environment (`APP__INTEGRATIONS__<NAME>__OAUTH__PRIVATE_KEY`);
  a file value is refused by the secret guard. Construction signs one probe
  assertion so a key that does not match its algorithm fails at startup.
- Rotation: register the new public key at the AS beside the old one, deploy
  the new `private_key` and `key_id`, then delete the old public key. No
  `jwks_uri` endpoint is served by the service: it would add a public route and
  an AS-to-service dependency for a restart-rotated key.
- No `x5t#S256` header. Entra needs it, but Entra offers no token exchange that
  emits `act` and is not shortlisted. **Reopen** if Entra becomes a required AS.

### D3. User context: RFC 8693 token exchange, one exchanged token per integration

A caller attaches `OnBehalfOf` with the verified inbound access token to the
outbound request; the HTTP and gRPC bindings then exchange it for a token
addressed to that integration and send it instead of the service token.

- Request: `grant_type=urn:ietf:params:oauth:grant-type:token-exchange`,
  `subject_token`, `subject_token_type=urn:ietf:params:oauth:token-type:access_token`,
  `requested_token_type=urn:ietf:params:oauth:token-type:access_token`, the
  integration's `audience` and `scope` when configured, and the service's own
  authentication (D3a). No `resource`: Zitadel, Keycloak and authentik reject it.
- **F** RFC 8693 does not itself require client authentication; the template
  always authenticates the exchange, otherwise anyone holding a user token
  could mint downstream tokens (`standards.md` §4).
- The raw inbound token reaches the exchange through
  `infra_bearerauthn::Principal::access_token() -> &SecretString`, retained
  from the verified bearer bytes. Handlers never forward it as an
  `Authorization` header; the guide forbids token pass-through (RFC 9700 §2.3
  audience restriction).
- Exchanged tokens are cached per credential owner, keyed by the SHA-256
  digest of the subject token, bounded to 1024 entries, each until its reuse
  cutoff (the D1 ten-second margin), with concurrent misses for one subject
  coalesced into one exchange. A resource 401 evicts only that entry. 1024 is
  a memory bound, not a tuning key: it covers the distinct users one service
  calls one integration for within a token lifetime at the template's scale,
  and at about 1–2 KiB per token it caps the cache near 2 MiB; a miss beyond it
  costs one exchange, never correctness. This reuses the Moka pattern the
  introspection cache already carries; the earlier single-token decision
  rejected Moka only for a one-key cache.
  - A revoked user session leaves an already exchanged token usable until its
    own expiry, as with any bearer token; the guide says so, and short
    exchanged-token lifetimes at the AS bound it.
  - **F** Spring's `TokenExchangeOAuth2AuthorizedClientProvider` also reuses an
    exchanged token until it expires.
- Transaction Tokens are not adopted: draft-ietf-oauth-transaction-tokens-11
  (2026-07-30) is not an RFC, no shortlisted AS issues them, and a Txn-Token is
  not an access token, so each hop would still need its own audience-bound
  token. **Reopen** when it is published and a chosen AS issues it.

#### D3a. How the exchange is authenticated

The exchange carries the same fresh client assertion as D1 and no
`actor_token`. The token the receiver gets names the user as `sub` and the
calling service as `client_id`/`azp`; an AS that expresses delegation also adds
`act`.

- **F** Keycloak 26.7.4 standard token exchange V2 (GA since 26.2) authenticates
  the requester as a confidential client, requires it to be in the subject
  token's `aud`, and silently ignores `actor_token`
  (`StandardTokenExchangeProvider`); it emits no `act`.
- **F** Zitadel emits `act` only for an `actor_token` under its impersonation
  permission and returns an opaque token unless
  `requested_token_type=...:jwt` is sent; authentik authenticates the exchange
  only with a client secret. Neither fits D1, so `actor_token` and a
  configurable requested type are not added.
- `requested_token_type` is `urn:ietf:params:oauth:token-type:access_token`, and
  the response must report that `issued_token_type` and a Bearer token type.

### D4. Inbound: the current actor and the verified token

- `Principal::actor() -> Option<&Actor>` exposes the outermost `act` object's
  `sub` and `client_id` (RFC 8693 §4.1). Nested actors are not exposed: they
  are informational and must not drive access control. A present `act` that is
  not an object, or whose `sub` is not a string, is malformed evidence (invalid
  JWT, unavailable introspection result, as for other claims). `may_act` is
  ignored by the receiver (§4.4: an AS input).
- The calling service is always `client_id()`. Whether `subject()` names an end
  user depends on the AS: RFC 9068 §2.2 makes `sub` the client ID for client
  credentials, but Keycloak and Zitadel use a service-account user ID, and
  Keycloak's exchanged tokens carry no `act` (**F**,
  `authorization-servers.md`). No standard claim separates a service token
  from an exchanged user token across the shortlist, so a template accessor
  such as "end user only when `act` is present" would be always empty on
  Keycloak, and "`sub != client_id`" would misclassify every Keycloak service
  token. The template therefore adds no discriminant.
  - Why that is safe (RFC 9700 §4.15): the confusion is exploitable when a
    client can choose an identifier that equals a user's `sub`. The shortlisted
    servers generate service-account subjects themselves in the user
    namespace, so a service token used where a user is expected names an
    account that owns no user data. The guide's receiver rules: authorize the
    calling service by `client_id()` and scopes; authorize user data by
    ownership of `subject()`; never compare `subject()` with client
    identifiers.
  - **Reopen** when the chosen AS emits a standard discriminant (`act` on
    exchanged tokens, or RFC 9068 `sub` = client ID for service tokens); then
    add an accessor that relies on it.
- `Principal::access_token()` (D3) is the only way the raw token leaves the
  verifier; its Debug stays redacted.
- Header `typ`: unchanged. The `resource-server` profile accepts `JWT` and
  absent `typ`, which Zitadel, Keycloak (default) and Hydra emit; `rfc9068`
  keeps requiring `at+jwt`.

### D5. Scopes declared in the OpenAPI contract and enforced by the route layer

An operation's OpenAPI security requirement may now list scopes:
`security: [{bearerAuth: [billing.write]}]`. The final HTTP layer grants the
request when any requirement object's scopes are all present in the verified
principal (OpenAPI 3.1 §4.8.30: alternatives are OR, scopes within one are AND)
and otherwise returns `403 forbidden` with `Bearer error="insufficient_scope"`
before idempotency admission and handler extraction.

- This replaces the handler helper `infra_http::require_scope`, which is
  deleted: one declared, reviewable policy per route instead of a check each
  handler must remember. Unsupported security shapes (another scheme,
  anonymous `{}`) still fail startup.
- gRPC has no OpenAPI document; its handlers keep checking
  `Principal::scopes()` themselves. **Reopen** when a derived service serves
  protected gRPC methods (no current GonkaGate or Bitrina service uses gRPC).

### D6. The service-to-service guide

A new guide, `docs/service-to-service-authentication.md`, declares this path
the only supported way services call each other and forbids static bearer
tokens, HS256 or any shared JWT secret, caller-minted JWTs, custom HMAC
request signatures and unsigned identity headers. It explains choosing and
wiring an AS (§3), per-AS registration notes, and migration with a temporary
dual-acceptance window.

- Migration order (**F** rfc7523bis §6; RFC 8725 bis §3.11; OIDC Core §10.1.1):
  receivers first accept both schemes, selecting the scheme by an unambiguous
  signal and never by fallback after a failure, and count use per scheme;
  callers move one at a time; each receiver removes the old scheme once its
  count stays at zero, then its code and secrets are deleted. The legacy
  acceptor is service-local code in the derived service, never template code.
- Replacements it names: request integrity beyond TLS uses RFC 9421 HTTP
  Message Signatures, not a custom HMAC; business replay protection (payments'
  replay store, billing's single-use `jti`) uses idempotency keys
  ([HTTP idempotency](../../../docs/http-idempotency.md)), not authentication.
  **F** No standard requires a per-request replay store for internal calls; the
  only single-use MUST is the assertion `jti` at the AS (OIDC Core §9).

### D7. Deferred with reopen conditions

| Item | Decision | Reopen |
| --- | --- | --- |
| DPoP (RFC 9449) | Not adopted. Audience-bound tokens of minutes lifetime on a private network; per-request proof signing, nonce state and one retry on both token and resource calls; no maintained Rust client. Zitadel, Hydra and authentik do not offer it. | Tokens leave the private network, a compliance regime requires sender constraint, or the chosen AS and a maintained Rust client support DPoP for client credentials. |
| mTLS-bound tokens (RFC 8705), SPIFFE, WIMSE | Not adopted. No per-service certificates or mesh on Railway; WIMSE drafts split in late 2025 and have no mainstream implementation. | The platform issues workload identity. |
| Transaction Tokens | See D3. | See D3. |
| Private JWK key input | PEM only. | An AS hands out keys only as JWK. |
| Introspection client secret (inbound opaque-token mode) | Unchanged; the service-to-service guide requires JWT access tokens and `oidc-jwt`. | Introspection becomes part of a service-to-service path. |

## 3. Authorization server shortlist (for the guide; the choice is deferred)

The user deferred which AS runs and who pays; the guide records the evidence.
Versions checked: Zitadel v4.19.2, Keycloak 26.7.4, Hydra v26.2.0, authentik
2026.8.3, Authelia v4.39.28, Spring Authorization Server 7.1.1, Duende 8.0.9,
plus Auth0, Okta, Entra and Cognito documentation (2026-09-29).

1. **Keycloak 26.7** — JVM container plus PostgreSQL, Apache-2.0. The only
   self-hostable candidate that covers the whole path with one client per
   service: "Signed JWT" client authentication (issuer or endpoint `aud`,
   single-use `jti`, 60 s cap), client credentials, GA standard token exchange
   V2, per-service audience through client scopes, optional `at+jwt` header,
   DPoP GA. Costs: no `act` (the receiver sees the calling service as `azp`),
   `sub` of a service token is the service-account user ID, no `audience`
   parameter on client credentials, and roughly 0.75–1.5 GB of memory (**I**).
2. **Spring Authorization Server 7** — covers the protocol, but it is a Java
   framework the team would build and operate; `act`, audience mapping and
   assertion replay protection are custom code.
3. **Zitadel ≥ 4.19.2** — one Go container plus PostgreSQL, AGPL-3.0, GA token
   exchange with nested `act`. It does not fit D1 (see D1 and D3a): service
   users need the JWT-bearer grant and a separate OIDC application exchanges
   tokens. Pin ≥ 4.19.2 for GHSA-vrh8-c9cm-wh8v and GHSA-w4gv-rcwj-w6r5.

Not shortlisted: authentik 2026.8 (token exchange accepts only a client secret;
assertion verification skips `aud` and `jti`), Hydra (no RFC 8693, issue open
since 2018), Authelia (no token exchange yet), Duende (licence cost), Auth0 (`private_key_jwt` on Enterprise),
Okta (per-service audiences need a paid add-on), Entra, Cognito and Google
(no exchange that emits `act`).

## 4. Crate comparison and implementation choices

| Need | Options examined | Decision |
| --- | --- | --- |
| Token requests (two grant forms) | `oauth2` 5.0.0 (latest, 2025-01-21): `AuthType` only `BasicAuth`/`RequestBody`; `private_key_jwt` is a FIXME at `src/endpoint.rs:110`; every builder hard-codes `grant_type`; no token-exchange or JWT-bearer request and no generic grant. The maintainer keeps JWT signing and DPoP out of the crate (#211, #265) and points to `add_extra_param` for assertions (#217), which works: with no secret it sends `client_id` in the body (RFC 7521 §4.2). `openidconnect` 4.0.1: neither feature, and its signing uses RustCrypto `rsa` (RUSTSEC-2023-0071). | **Remove `oauth2`** (see the note below the table). |
| Assertion signing | `jsonwebtoken` 11.1.0 (already locked, aws-lc backend): `encode` with `Header { typ, kid }`, RS/PS/ES; `use_pem` adds `simple_asn1` (the only new package; `pem`, `num-bigint`, `time` already locked) and reads PKCS#1/PKCS#8 RSA and PKCS#8 EC. `josekit` (OpenSSL), `jwt-simple` (second crypto family), `biscuit`/`aliri`/`openid` (`ring`), `jose-jws`/`jwt-compact` (stale) rejected. Hand-written JWS over aws-lc: ~30 more template-owned lines. | `jsonwebtoken::encode` with `use_pem`. |
| Token exchange client | No maintained Rust crate implements an RFC 8693 client (**F** crates.io/docs.rs survey). Go exposes it only in `google/internal/stsexchange`; Nimbus `TokenExchangeGrant`, Spring `TokenExchangeOAuth2AuthorizedClientProvider` and Duende IdentityModel are the precedents. | Template-owned request on the shared form POST. |
| Exchanged-token cache | Moka 0.12.16 (locked; used by the introspection cache with SHA-256 keys, per-entry expiry and coalescing). | Moka `future::Cache` with `Expiry`, 1024 entries. |
| Assertion `jti` | `uuid` (workspace), aws-lc random. | `uuid` v4. |
| Inbound `act` | Extends the existing borrowed-claims parser. | Template-owned (a claim model, not a mechanism). |
| DPoP | No maintained Rust client crate. | Deferred (D7). |

**Why `oauth2` goes although it could carry the assertion.** The crate lane
recommended keeping it for `client_credentials` with `add_extra_param`
(`rust-crates.md` §F.1), and stage 10.8 adopted it because hand-written code
would duplicate "Basic encoding, form construction, and standard response
handling". That rationale no longer holds for this path. Basic encoding
disappears with the secret. Form construction and response parsing must be
template-owned anyway for token exchange, which `oauth2` cannot send. Keeping
the crate would therefore not remove any template code; it would add a second
request path (its `AsyncHttpClient` adapter plus its error mapping) beside the
exchange path, and keep the `thiserror` 1.x duplicate. One form POST over the
existing bounded client, with `url::form_urlencoded` and one serde response
type, is the fewer-mechanism design. **Reopen** if a maintained crate offers
both a client-assertion hook and RFC 8693 requests.

Template-owned gaps, named: the assertion claims and signing call; the two
grant forms and their response type; the exchanged-token cache wiring; the
`act` claim model; the OpenAPI scope table; configuration, initializer and
guide integration. No protocol parser beyond one serde struct, no retry loop,
no custom JOSE code.

## 5. Configuration delta (`integrations.<name>.oauth`)

| Key | Change |
| --- | --- |
| `token_url`, `client_id`, `scopes`, `audience` | Unchanged. |
| `client_secret` | Removed. |
| `private_key` | New, environment-only secret (PEM). |
| `key_id` | New, required. |
| `algorithm` | New, `RS256` (default), `PS256`, `ES256`. |
| `assertion_audience` | New, required; the AS issuer unless the AS accepts only its token endpoint. |

The profile keeps its initializer value `OUTBOUND_AUTH=oauth2-client-credentials`
so existing locks and sync keep working; token exchange needs no new profile
marker because its API takes a `SecretString` and does not depend on the
inbound crate.

## 6. Sibling Go template

Noted only, no change: `go-service-template-rest` would face the same
decisions. Go's `x/oauth2` has no `private_key_jwt` (golang/go#57186, #73431
open) and no public RFC 8693 client, so the Go port would also own the
assertion and exchange requests.

## Evidence custody

Full claim-level findings with locators live outside the tracked tree under
`.git/claude/s2s-auth/research/`: `standards.md`, `authorization-servers.md`,
`rust-crates.md`, `ecosystem.md`, `baseline.md`.
