# Outbound machine authentication decisions

<!-- template:begin outbound-auth:docs-outbound-machine-authentication-decisions -->
`crates/infra-oauth2-client-credentials`, `crates/config`, and `Cargo.toml` own
the grant forms, cache bounds, refresh driver, gRPC status mapping, and
dependency pins. This record keeps only rejected alternatives, RFC/provider
constraints, and reopen conditions.

## Protocol and library choices

| Choice / Rejected alternative | Decisive non-code reason | Reopen condition |
| --- | --- | --- |
| `private_key_jwt` (RFC 7523) only for both `client_credentials` and RFC 8693 token exchange; no `client_secret` or `actor_token` | Asymmetric assertions avoid shared secrets on the authorization server (RFC 9700 §2.5); single-string issuer `aud`, backdated `iat`/`nbf`, short `exp`, and single-use `jti` satisfy `rfc7523bis` / CVE-2025-27370/27371 across Keycloak, Hydra, Authelia, Duende, Okta, and Auth0 Enterprise. | A required provider supports only shared secrets (e.g., Cognito), rejects `typ: client-authentication+jwt`, or requires Zitadel's separate JWT-bearer + OIDC exchange registrations. |
| Template-owned form POST + `jsonwebtoken` (`aws-lc-rs`) over `oauth2`, `openidconnect`, or `huskarl` | `oauth2` hardcodes `grant_type` and lacks `private_key_jwt` signing and RFC 8693 exchange; `openidconnect` pulls `rsa` (RUSTSEC-2023-0071); `huskarl` omits `audience` on client credentials, omits backdated `nbf`, retries `use_dpop_nonce` against our no-retry rule, and anchors expiry to wall clock. | A maintained crate supports `private_key_jwt` assertions and RFC 8693 exchange under workspace crypto and deadline rules, or `Ed25519` (RFC 9864) is supported end-to-end. |
| Early background refresh (Azure.Core lead window) + double-checked lock over cutoff-only refresh or `moka` 1-key `try_get_with` | Cutoff refresh stalled 64 concurrent callers >20 ms on a 100 ms provider (DigitalOcean c-4); `moka` negative-caching relied on zero-TTL internals and a wall clock unaffected by `tokio::time::pause`. | Never for cutoff-only refresh under concurrent load. |
| Age-gated eviction on resource `401` | Immediate eviction turns a misconfigured downstream audience into one token-endpoint call per RPC; never evicting retains revoked tokens until `exp`. | Provider revokes tokens within seconds of issuance and rate-limits refreshes. |
| Pinned `docker run` of Keycloak in `scripts/ci/test-integration-oauth.sh` over `env/docker-compose.yml` or `testcontainers` | OAuth-only services prune the Compose `integration` profile; `testcontainers` adds a Rust Docker driver dependency. | Compose topology becomes shared across all OAuth projections. |

## Authorization-server fit and deferred specs

- **Keycloak:** Self-hostable Apache-2.0 fit covering `private_key_jwt`, client credentials, and GA standard token exchange V2 (configure confidential client with "Signed JWT", service accounts, standard token exchange, and audience client scopes; `act` delegation remains preview upstream).
- **Deferred until platform/compliance requires:** DPoP (RFC 9449), mTLS-bound tokens (RFC 8705) / SPIFFE / workload-identity federation (Railway issues no workload token), IETF Transaction Tokens draft, and JWK private-key input.
<!-- template:end outbound-auth:docs-outbound-machine-authentication-decisions -->
