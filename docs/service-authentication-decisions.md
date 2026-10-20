# Service authentication decisions

<!-- template:begin service-auth:docs-service-authentication-decisions -->
Decisions for `SERVICE_AUTH=jwt`, recorded 2026-10-17 against `origin/main`
c0b4f320. The template owner approved the rule amendment on 2026-10-13: a
caller-signed short JWT whose key the receiver pins to that caller, and later
platform tokens, alongside the authorization-server path. The
[guide](service-authentication.md) owns adoption and observable behaviour.

## Contract and scope

| Decision | Strongest rejected alternative and why | Accepted cost and reopen condition |
| --- | --- | --- |
| S1: a caller may issue its own service token; the receiver pins each caller's keys. The authorization server stays a valid issuer on the `OUTBOUND_AUTH` path. | Authorization-server tokens only, the earlier mandate. Every call and every start then depends on one more critical service whose signing key forges any identity, and a deployment without one cannot authenticate callers at all. | The caller holds a long-lived private key. Reopen when the platform issues workload identity (Kubernetes service-account tokens, SPIFFE), which removes that key. |
| S2: a separate profile, `SERVICE_AUTH=none\|jwt`, independent of `AUTHN`. | A new `AUTHN` value. `AUTHN` is one engine per process, but a cabinet backend verifies user tokens on HTTP and service tokens on gRPC at once, and service tokens are admitted by caller rather than scope. | Two authentication sections in one service. None. |
| S3: inbound enforcement on gRPC only; the token source also serves HTTP clients. | Service tokens on HTTP operations through a second OpenAPI security scheme. No current receiver needs it: every internal call of the adopting system is gRPC, and its one HTTP receiver is a Go service. A second scheme beside `bearerAuth` would reopen the HTTP policy compiler's single-scheme rules. | An HTTP-only template service cannot receive service tokens and keeps the authorization-server path. Reopen when a template service must accept service tokens on an HTTP operation. |
| S4: the listener picks its authentication at runtime: service tokens when the process verifies them, otherwise bearer tokens when `AUTHN` is retained, otherwise none. Admission sits inside bearer authentication, and `router()` takes the bearer verifier as `Option`, `None` only beside service tokens. | Exclude bearer gRPC wiring by profile markers. The source template keeps every profile at once and must build and pass its own bearer gRPC tests; two layers that each consume and strip the one `authorization` value would admit nothing. | `router()`'s bearer argument became `Option`, and a router without either authentication refuses to build. A service with `AUTHN` running `service_auth.mode = "none"` keeps gRPC on bearer, so it cannot stand in for an unsigned caller's peer. None. |
| S5: the token is a compact JWS with `alg` ES256 only, `typ` equal to the configured `jwt_type`, a required `kid` and no `crit`; `iss` = `sub` = caller name; `aud` exactly the receiver (a string or a one-element array); `exp − iat` in (0, 60] seconds; a 30-second clock allowance on `exp`, `iat` and `nbf`. No `jti` is required or sent. | RS256/PS256 as well (larger keys and tokens, two key families to rotate) and EdDSA (`jsonwebtoken` 11.1.0 cannot sign Ed25519, RFC 9864). A required `jti` with a replay cache: tokens live 60 seconds on a private network and the receiver keeps no state. | A stolen token works for up to 60 seconds plus the allowance, for the methods its caller may call; an admitted stream may outlive `exp`. Reopen when tokens leave the private network or a sender-constrained token (DPoP, mTLS-bound) is required. |
| S6: the receiver reads `iss` from the payload before the signature check, then verifies only with the key whose `kid` matches among that caller's keys. | Look keys up by `kid` across all callers and compare `sub` afterwards. That is the defect found in an existing Go service, where any trusted key could sign as any configured caller, and it forces globally unique key ids. | The payload is parsed before the signature check, inside the listener's 16 KiB header bound and after the opening permit. None. |
| S7: `jwt_type` is configuration with no default. | A template constant. The template serves unrelated organizations; the type names one trust domain's token family (RFC 8725 §3.11), and a shared constant would let one organization's tokens type-check in another's services. | Every service of a domain repeats the value; on Railway it is one shared variable. None. |
| S8: the trust registry is one JSON object of caller names and their JWK Sets of EC P-256 public keys, read once at startup; any entry it cannot use refuses startup. | A key set fetched from a URL with refresh. Railway hosts no such endpoint and the refresh machinery adds a runtime dependency; skipping a bad entry would turn a rotation mistake into refusals at call time. | A registry change takes effect at the next deployment, so planned rotation overlaps two keys across deployments. Reopen on Kubernetes, where the cluster JWKS becomes an issuer adapter. |
| S9: the permission table lives in the receiver's code (`Services::allow_callers`) and denies by default. Once a verifier is set or any rule is declared, every method of every added service needs a rule or the router refuses to build. | Require a complete table on every listener of the profile. A registration that declares no rule and verifies no token is bearer- or development-only, and the source template's existing tests build such routers. A configuration-driven table was rejected too: who may call a method is reviewed contract, not an operator knob. | A development run without rules starts; the first run that verifies tokens refuses without them. None. |
| S10: `service_auth.mode = "none"` sends no token; without `AUTHN` it admits gRPC calls without one. With gRPC enabled it is accepted only where `app.env` is `local` or `development`. | No off switch. A stand that runs one new service among old ones, which sign nothing, could not run. | A development deployment of a service without `AUTHN` can run unauthenticated gRPC. Reopen if another environment needs it; it should then get test keys, not `none`. |
| S11: the handler sees `Caller`, the verified name only. | Expose the claims. A caller-signed token can assert nothing but the caller's identity that the receiver may trust; store and payer stay request fields that the handler checks against the caller. | Rules finer than a method are handler code. None. |
| S12: one token source per process; each receiver handle reuses its token while `0 ≤ now − iat < 30` by the system clock and signs a new one inline otherwise. | A background refresh task: signing is local and takes microseconds, so a task adds lifecycle custody and removes no waiting. Signing every call adds one ECDSA signature per call for nothing. | A token is used until 30 seconds before its expiry. Reopen on Kubernetes, where the source reads a projected token file instead. |
| S13: failures map to the catalog codes `UNAUTHORIZED` (`UNAUTHENTICATED`), `FORBIDDEN` (`PERMISSION_DENIED`) and `SERVICE_UNAVAILABLE` (a clock before the epoch). | The bearer `AUTHENTICATION_*` codes, which exist only with `AUTHN`; missing and invalid stay apart in the receiver's metrics and logs. | A caller cannot tell a missing token from an invalid one by the status. None. |

Template-wide effects of the profile: `infra_grpc::Error` is `Clone` but no
longer `Copy` in every projection, since a missing rule names its method, and
`router()` keeps a constant `service_tokens = false` where the profile is
absent.

## Crates and placement

| Need | Options examined | Decision |
| --- | --- | --- |
| ES256 verification | `jsonwebtoken::decode` 11.1.0 builds the aws-lc key on every call, parses header and payload several times, accepts any audience of a set, and checks neither `typ` nor a lifetime cap. `jwt-simple` adds a second crypto family (RustCrypto `p256`) beside the workspace's aws-lc. `josekit` needs OpenSSL, `biscuit` `ring`. | One header and one payload parse into the members the contract reads, so a `crit` of any value, `null` included, is seen; aws-lc `ParsedPublicKey` with `ECDSA_P256_SHA256_FIXED` per admitted key; `jsonwebtoken`'s `Algorithm` for the header and its `Jwk` type for registry admission, as `infra-bearerauthn` does. |
| ES256 signing | `jsonwebtoken::encode` with `use_pem` (already locked for the OAuth client assertion); direct aws-lc `EcdsaKeyPair`. | `jsonwebtoken::encode` with an `EncodingKey` from PKCS#8 PEM; construction signs once to prove the key. |
| Placement | An engine inside `infra-bearerauthn`, which `AUTHN=none` removes and whose principal is scope-based; modules inside `infra-grpc`, which cannot serve HTTP clients. | New crate `infra-serviceauth` (trust registry, verifier, token source; no transport); `infra-grpc` owns admission and the client binding behind the `service-auth` marker; `service` composes them. |

## Kubernetes path

Two adapters change and nothing else: the token source reads a projected
service-account token per audience instead of signing, and the trust
registry gives way to an issuer adapter that reads the cluster's JWKS and maps
`system:serviceaccount:<namespace>:<name>` to the caller name with a pinned
namespace (RS256, 600-second tokens, so the lifetime cap moves with that
adapter). `Caller`, the permission table, admission and the client bindings
stay. Network policy and node-to-node encryption are platform work.

## Deferred, with the change that reopens each

- Service tokens on HTTP operations: S3.
- Signing without enforcing, for adopting tokens across running services one
  at a time: when such an adoption is required; the target system is built
  and switched whole.
- A production trust registry, key rotation with overlap and a key-age
  warning: adopter work on this contract (S8).
<!-- template:end service-auth:docs-service-authentication-decisions -->
