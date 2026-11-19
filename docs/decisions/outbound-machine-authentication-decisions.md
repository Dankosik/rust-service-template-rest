# Outbound machine authentication decisions

<!-- template:begin outbound-auth:docs-outbound-machine-authentication-decisions -->
`crates/infra-oauth2-client-credentials` and `Cargo.toml` own
the grant forms, cache bounds, refresh driver, gRPC status mapping, and
dependency pins. This record keeps only rejected alternatives, RFC/provider
constraints, and reopen conditions.

## Protocol and library choices

| Choice / Rejected alternative | Decisive non-code reason | Reopen condition |
| --- | --- | --- |
| Grant-specific sources on one client registration over a combined credentials handle | A subject token is supplied by the request, so an exchange-only integration has no token to refresh in the background. A service-only integration has no subjects to cache. Different resource audiences can still spend one provider quota. The integration owner must be able to retire all bound clients even when callers supply their own deadlines. | The provider requires a separately registered client or a separate quota for each grant or resource. |
| Optional recipient scopes over a client-credentials `audience` parameter | Keycloak 26.8.0 supports `audience` for token exchange. Client credentials select an audience by requesting an optional client scope with the recipient's Audience mapper. Register one optional scope per service recipient, request only that recipient's scope, and keep other audience mappers out of the default scopes. Recipients must verify the token audience. Exchange can use `scope` to enable a mapper and `audience` to narrow the resulting audiences. [Keycloak server guide](https://www.keycloak.org/docs/26.8.0/server_admin/#_audience), [Keycloak token exchange guide](https://www.keycloak.org/securing-apps/token-exchange#_standard-token-exchange-scope). | A required provider needs a documented client-credentials resource selector beyond `scope`. |
| `private_key_jwt` (RFC 7523) only for both `client_credentials` and RFC 8693 token exchange; no `client_secret` or `actor_token` | Asymmetric assertions avoid shared secrets on the authorization server (RFC 9700 §2.5). The exact issuer audience and explicit JWT type distinguish client authentication assertions under [RFC 10070 §4](https://www.rfc-editor.org/rfc/rfc10070.html#section-4). Short backdated assertions and fresh `jti` values accommodate clock skew and provider replay checks. | A required provider supports only shared secrets (e.g., Cognito), rejects `typ: client-authentication+jwt`, or requires Zitadel's separate JWT-bearer + OIDC exchange registrations. |
| Template-owned form POST + `jsonwebtoken` (`aws-lc-rs`) over `oauth2`, `openidconnect`, or `huskarl` | `oauth2` hardcodes `grant_type` and lacks `private_key_jwt` signing and RFC 8693 exchange. `openidconnect` pulls `rsa` (RUSTSEC-2023-0071). `huskarl` omits backdated `nbf`, retries `use_dpop_nonce` against our no-retry rule, and anchors expiry to wall clock. | A maintained crate supports `private_key_jwt` assertions and RFC 8693 exchange under workspace crypto and deadline rules, or `Ed25519` (RFC 9864) is supported end-to-end. |
| Early background refresh (Azure.Core lead window) + double-checked lock over cutoff-only refresh or `moka` 1-key `try_get_with` | Cutoff refresh stalled 64 concurrent callers >20 ms on a 100 ms provider (DigitalOcean c-4); `moka` negative-caching relied on zero-TTL internals and a wall clock unaffected by `tokio::time::pause`. | Never for cutoff-only refresh under concurrent load. |
| Per-subject exchange single-flight (`watch` + leader guard) beside the `moka` exchanged-token cache over `moka` `try_get_with` | `try_get_with` hands the initializer's outcome to every waiter, so a leader's own deadline or cancellation would fail its waiters and a request-only token (no `expires_in`) would serve them; moka 0.12.16 panics a waiter after 200 aborted initializers (`MAX_RETRIES` in `future/value_initializer.rs`). A dropped leader hands the exchange to a waiter under both designs. | `moka` lets a waiter retry an initializer outcome it rejects and stops panicking on aborted initializers. |
| Age-gated eviction on resource `401` | Immediate eviction turns a misconfigured downstream audience into one token-endpoint call per RPC; never evicting retains revoked tokens until `exp`. | Provider revokes tokens within seconds of issuance and rate-limits refreshes. |
| Pinned `docker run` of Keycloak in `scripts/profiles/integration.sh oauth` over `env/docker-compose.yml` or `testcontainers` | OAuth-only services prune the Compose `integration` profile; `testcontainers` adds a Rust Docker driver dependency. | Compose topology becomes shared across all OAuth projections. |

## Authorization-server fit and deferred specs

The service-token cache keeps its short exclusive lock. In the outbound call
comparison (evidence commit `546723f`), a read/write lock reduced caller CPU
in all four 64-caller pairs before refresh, but the active-refresh results
varied and p99 did not improve consistently. Reopen if a deployment profile
shows sustained contention on this lock at its actual concurrency and refresh
mix.

- **Keycloak** is a self-hostable Apache-2.0 provider covering `private_key_jwt`, client credentials, and GA standard token exchange V2. Configure a confidential client with Signed JWT, service accounts, and standard token exchange. `act` delegation remains preview upstream.
- **Deferred until platform/compliance requires:** DPoP (RFC 9449), mTLS-bound tokens (RFC 8705) / SPIFFE / workload-identity federation (Railway issues no workload token), IETF Transaction Tokens draft, and JWK private-key input.
<!-- template:end outbound-auth:docs-outbound-machine-authentication-decisions -->
