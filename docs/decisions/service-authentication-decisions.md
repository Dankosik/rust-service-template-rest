# Service authentication decisions

<!-- template:begin service-auth:docs-service-authentication-decisions -->
`crates/infra-serviceauth`, `crates/config`, and `Cargo.toml` own the token
contract, verification cache, gRPC wiring, and dependency pins. This record
keeps only rejected alternatives, security/platform constraints, and reopen
conditions.

| Choice / Rejected alternative | Decisive non-code reason | Reopen condition |
| --- | --- | --- |
| End-to-end TLS to the configured gRPC receiver origin for caller-signed tokens | Anyone who observes a bearer token can replay it during its lifetime. Private DNS and proxy headers supply no authenticated evidence that the receiving listener has a protected connection. The receiver's logical audience can differ from its certificate hostname. | A deployment supplies an authenticated proxy-to-service transport contract with a verifiable peer identity. |
| Caller-signed short-lived ES256 JWTs (`SERVICE_AUTH=jwt`) independent of `AUTHN`, enforced on gRPC only | Avoids making every internal start/call depend on an authorization server while allowing user OIDC tokens on HTTP alongside caller-pinned tokens on gRPC; adding a second HTTP OpenAPI security scheme had no consumer. | Platform workload identity (Kubernetes projected tokens, SPIFFE) replaces static keys, or an HTTP operation must accept service tokens. |
| Lookup caller keys by `iss` before `kid` verification, no `jti` replay store, required `jwt_type` with no template default | Looking up `kid` globally let any trusted key sign as another caller in a prior service; short token lifetime on a private network avoids stateful replay storage; domain-specific `jwt_type` prevents cross-organization token confusion (RFC 8725 §3.11). | Tokens cross untrusted networks or require sender constraint (DPoP / mTLS). |
| Direct `aws-lc-rs` ES256 verification + byte-keyed verified-token memo over `jsonwebtoken::decode` on every call or `jwt-simple`/`josekit`/`biscuit` | `jsonwebtoken::decode` rebuilds the key per call (~26 µs vs ~70 ns memoized on Apple M5) and accepts any audience in a set; alternative crates pull a second crypto stack (`p256`, OpenSSL, or `ring`). | Runtime trust-registry reload or token revocation invalidates byte-keyed memoization. |
| Startup-loaded static JWK trust registry and code-owned per-RPC caller allowlists over remote JWKS URLs or config ACLs | Railway hosts no cluster JWKS endpoint, and who may call an RPC is a reviewed code contract rather than an operator runtime toggle. | Deployment moves to Kubernetes (swap token source to projected SA tokens and registry to cluster JWKS issuer adapter). |
<!-- template:end service-auth:docs-service-authentication-decisions -->
