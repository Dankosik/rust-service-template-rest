# Design: mechanisms and ownership

Consumes [spec](../spec.md) B1–B5 and [synthesis](../research/synthesis.md).
Ownership is forced by current crates; no crate, module or directory is added
except the one new guide.

## Outbound: `crates/infra-oauth2-client-credentials`

- `oauth2` is removed from the crate and the workspace. `Options` becomes
  `{ token_url, client_id, private_key: SecretString, key_id, algorithm:
  Algorithm, assertion_audience, scopes, audience }`, where `Algorithm` is a
  crate enum `Rs256 | Ps256 | Es256` mapped to `jsonwebtoken::Algorithm`.
- `Inner` keeps the fixed `token_http` client, endpoint, the one-token cache and
  the refresh lock unchanged, and gains:
  - `signer`: the `jsonwebtoken::EncodingKey` (built with `from_rsa_pem` or
    `from_ec_pem`, feature `use_pem` on the crate's `jsonwebtoken` dependency),
    the prepared `Header { alg, kid, typ: "client-authentication+jwt" }`, the
    client ID and assertion audience. `prepare` signs one probe assertion; a
    failure is `ConfigurationError { key: "private_key", .. }`.
  - `exchanged`: `moka::future::Cache<[u8; 32], Arc<Token>>`, capacity 1024,
    key = SHA-256 (aws-lc digest) of the subject token, per-entry `Expiry` from the token's
    remaining reuse time. Moka expiry only reclaims memory: a hit is used only
    when `Token::is_reusable(tokio::time::Instant::now())`, so every decision
    stays on the Tokio clock. An exchanged token without `expires_in` serves
    only its own request and is not stored.
- One private `post_form(&self, grant, fields, deadline)` sends
  `application/x-www-form-urlencoded` (`url::form_urlencoded::Serializer`) with
  `Accept: application/json` through `token_http`, keeps the current status
  mapping (5xx `Unavailable`, other non-2xx `Rejected`, transport/limit/timeout
  as today), and decodes one private serde `TokenResponse { access_token,
  token_type, expires_in: Option<u64>, issued_token_type: Option<String> }`
  (unknown members ignored, a present non-JSON media type refused). Both grants
  build their fields as borrowed pairs and call it. `Token` construction
  (Bearer check, header value, reuse cutoff, refresh-ahead instant) moves into
  one function shared by both grants.
- Assertion claims are a private serde struct; time is `SystemTime` seconds;
  `jti` is `uuid::Uuid::new_v4()` (uuid `v4` feature). A runtime signing error
  is a new closed `AcquisitionError::Assertion` (label `assertion`).
- `OnBehalfOf(SecretString)`: public, `Clone`, redacted `Debug`, constructed by
  `OnBehalfOf::new(SecretString)`. HTTP `AuthenticatedClient::execute` and the
  gRPC `Service::call` remove it from the request extensions. With it, the
  token comes from `exchanged` via `try_get_with` (one exchange per subject,
  bounded by `start + FETCH_TIMEOUT`); each caller waits under
  `timeout_at(own deadline)`. Without it, the existing service-token path runs.
  A resource 401 invalidates the subject's entry only when the cached `Arc` is
  the one that request used.
- Metrics: `oauth2_token_acquisitions_total{grant, outcome}` with `grant` in
  `client_credentials | token_exchange`; outcomes gain `assertion`.

## Configuration: `crates/config`

`OAuthConfig` replaces `client_secret` with `private_key: SecretString`,
`key_id`, `algorithm` (decoded to a config enum, default `RS256`) and
`assertion_audience`. Validation: nonblank `private_key`, `key_id` and
`assertion_audience`; `algorithm` in the three values. PEM parsing stays in the
adapter. The secret guard already treats `private_key` as secret; tests cover
the file refusal and environment admission for the new key.

## Inbound: `crates/infra-bearerauthn`, `crates/infra-http`

- `Identity` gains `access_token: SecretString` and `actor: Option<Actor>`.
  Each engine passes the verified token text into `Principal::new` (the
  introspection cache stores it with the entry for the same token). `Actor {
  subject: String, client_id: Option<String> }` with accessors; the JWT and
  introspection claim structs read `act` as an optional borrowed object whose
  `sub` is required and `client_id` optional; unknown members and nested `act`
  are ignored.
- `contract::Policy` stores per method `Access::Public` or
  `Access::Protected(Arc<[Box<[String]>]>)`. `is_public` becomes `access`,
  accepting bearer requirements whose value is an array of strings.
  `authenticate_protected` checks the alternatives against the sorted
  `principal.scopes()` after verification and before `next.run`, returning the
  existing insufficient-scope 403. `require_scope` and its export are deleted.
  The OpenAPI gate test in `crates/service/tests/openapi.rs` accepts scoped
  bearer requirements.

## Initializer, docs

- `Cargo.toml` loses the `oauth2` workspace dependency marker content; the
  crate gains `jsonwebtoken` (`aws_lc_rs`, `use_pem`), `moka` (`future`),
  `uuid` (`v4`), aws-lc SHA-256 as the introspection cache uses, `serde`, `serde_json`. Lock projection keeps exact
  reachability; feature edges only the outbound crate enables (for example
  jsonwebtoken `use_pem` → `pem`/`simple_asn1`, uuid `v4`) get guarded
  `_project_feature_edge` entries when `outbound_auth == "none"`, and the old
  `url`/`serde` edge entry is removed if no longer produced.
- `docs/service-to-service-authentication.md` is retained with the
  `outbound-auth` profile (listed in `remove_when_unselected`); links to it from
  profile-independent docs sit inside `outbound-auth` marker blocks.
