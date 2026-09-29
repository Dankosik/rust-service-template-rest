# Spec: one standard service-to-service authentication path

[Intent](intent.md) owns requester meaning; [synthesis](research/synthesis.md)
owns the accepted decisions D1–D7 cited here. This file owns the behavior
delta and its proof.

## Outcome

With `OUTBOUND_AUTH=oauth2-client-credentials`, a service authenticates to its
authorization server only with a private key, acquires audience-bound access
tokens for each integration, and can call an integration on behalf of the
verified inbound user through RFC 8693 token exchange. With an authentication
profile, a receiver sees the calling service, the user, and the current actor,
and each route's required scopes come from the OpenAPI contract. One guide
declares this the only supported service-to-service method.

## Behavior delta

### B1. Private-key client authentication (D1, D2)

- `integrations.<name>.oauth` requires `token_url`, `client_id`, `private_key`
  (environment only), `key_id` and `assertion_audience`; `algorithm` is
  `RS256` (default), `PS256` or `ES256`; `scopes` and `audience` are unchanged.
  `client_secret` is an unknown key and fails startup.
- Every token request posts `grant_type=client_credentials`, `client_id`,
  `client_assertion_type=urn:ietf:params:oauth:client-assertion-type:jwt-bearer`
  and a freshly signed `client_assertion`, plus `scope` and `audience` when
  configured, with no `Authorization` header.
- The assertion header is `{alg, kid, typ: "client-authentication+jwt"}`; its
  claims are `iss` = `sub` = client ID, `aud` = the configured audience as one
  string, `iat` = `nbf` = now, `exp` = now + 60 s, `jti` = a new UUID v4. Two
  requests never share a `jti`.
- A key that is not PEM, or does not match `algorithm`, fails construction
  with a sanitized configuration error naming `private_key`; no I/O happens.
- Token reuse, background refresh, deadlines, 401 eviction, bounds, sanitized
  errors and metrics keep their current behavior.

### B2. Token exchange (D3, D3a)

- A caller attaches `OnBehalfOf` (holding the verified inbound access token) to
  a request's extensions; the HTTP `AuthenticatedClient::execute` and the gRPC
  binding then send an exchanged token instead of the service token.
- The exchange posts `grant_type=urn:ietf:params:oauth:grant-type:token-exchange`,
  `subject_token`, `subject_token_type` and `requested_token_type` =
  `urn:ietf:params:oauth:token-type:access_token`, the configured `scope` and
  `audience`, and the same client assertion fields as B1.
- The response is admitted only with a Bearer `token_type`, a nonempty token
  forming a header value, and `issued_token_type` equal to the access-token
  URI; anything else is `InvalidResponse`.
- Exchanged tokens are reused per credential owner and subject token until
  their reuse cutoff; concurrent requests for one subject share one exchange;
  at most 1024 subjects are retained; a resource 401 evicts only that
  subject's entry; failures are not cached. Exchange attempts are counted with
  the same closed outcomes, labeled `grant="token_exchange"` beside
  `grant="client_credentials"`.
- A request carrying both `OnBehalfOf` and its own `Authorization` header is
  still refused before I/O.

### B3. Inbound principal (D4)

- `Principal::access_token()` returns the verified bearer token as a
  `&SecretString`; its Debug stays redacted. HTTP and gRPC both provide it.
- `Principal::actor()` returns the outermost `act` object's `sub` and
  `client_id` when present, for JWT and introspection evidence. A non-object
  `act`, or a non-string `sub`/`client_id` inside it, is malformed evidence with
  the existing failure class of each engine. Nested actors and `may_act` are
  not exposed.

### B4. OpenAPI scopes per operation (D5)

- A security requirement `{bearerAuth: [s1, s2]}` is accepted. A protected
  operation is granted when at least one requirement's scopes are all present
  in the principal; an empty list means any authenticated caller.
- A verified principal that satisfies no requirement gets `403 forbidden` with
  `WWW-Authenticate: Bearer error="insufficient_scope"`, before idempotency
  admission and the handler.
- `infra_http::require_scope` no longer exists. Other schemes, anonymous `{}`
  and mixed requirements still fail finalization.

### B5. Guide and records (D6)

- `docs/service-to-service-authentication.md` states the mandate, the
  forbidden schemes, the AS shortlist and registration notes, receiver rules
  (calling service = `client_id()`, user and actor per AS), and the
  dual-acceptance migration order. The decision record, authentication and
  outbound guides, roadmap and README reflect B1–B4.

## Constraints

Existing bounds, deadlines, sanitization and profile boundaries hold. The
initializer value `oauth2-client-credentials` and existing lock handling stay
valid. `oauth2` leaves the dependency graph. The bundle is deleted on
completion after its durable content moves to the owning documents.

## Proof expectations

- Outbound crate tests with the local fake token/resource server: form fields
  and absence of `Authorization`; assertion header and claims verified with the
  matching public key; distinct `jti`; algorithm/key mismatch refusal; token
  exchange request, response admission, per-subject reuse, coalescing, 401
  eviction of one subject, failure not cached; gRPC on-behalf dispatch;
  existing reuse/deadline/eviction tests retained.
- Config tests: new keys, `client_secret` refused, file `private_key` refused,
  algorithm values.
- Inbound tests: `act` present/absent/malformed for JWT and introspection;
  `access_token()` returns the presented token.
- HTTP tests: scoped operation granted, denied with 403 and challenge, OR of
  alternatives, denial before idempotency admission; finalization refusals.
- Initializer projections and the retained runtime graphs pass in CI; docs
  links resolve.
