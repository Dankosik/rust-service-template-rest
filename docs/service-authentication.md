# Service authentication

<!-- template:begin service-auth:docs-service-authentication -->
`SERVICE_AUTH=jwt` retains caller-signed service tokens for native gRPC. A
calling service signs a short ES256 JWT for one receiver with its own key; the
receiver verifies it only against the keys it pins to that caller, admits the
call by a per-method table of callers, and hands the handler the caller's
name. The profile requires `GRPC=enabled`, combines with every other profile,
and is inert until `service_auth.mode = "jwt"`. `SERVICE_AUTH=none`, the
initializer default, removes the crate, configuration section, wiring, tests
and these documents. The [decision record](service-authentication-decisions.md)
explains the contract, the rejected alternatives and the Kubernetes path.

## Select and configure

```sh
make template-init SERVICE_NAME=catalog-api \
  REPOSITORY=https://github.com/example/catalog-api \
  DESCRIPTION='Catalog API' CODEOWNER=@example/platform \
  DATABASE=none GRPC=enabled SERVICE_AUTH=jwt
```

```toml
[service_auth]
mode = "jwt"
name = "catalog"              # this service: the audience it accepts, its tokens' iss and sub
jwt_type = "example-svc+jwt"  # this trust domain's token type; no default
trust = '{"consultant": {"keys": [{"kty": "EC", "crv": "P-256", "kid": "consultant/2026-10", "x": "…", "y": "…"}]}}'
key_id = "catalog/2026-10"    # with the private key: this service also calls others
```

- `name` is a caller name: 1–63 lowercase letters, digits and hyphens, not
  starting or ending with a hyphen.
- `jwt_type` names one trust domain's tokens, such as `example-svc+jwt`. Every
  service of the domain uses the same value, and no other domain may reuse it.
  It is compared as a media type: case-insensitively, with or without
  `application/`.
- `trust` is the trust registry below. Its values are public keys, so it may
  live in a file or a variable (`APP__SERVICE_AUTH__TRUST`).
- `key_id` and the environment-only `APP__SERVICE_AUTH__PRIVATE_KEY`, a PKCS#8
  PEM EC P-256 private key, come together. Without them the service verifies
  its callers and calls no one; a private key in a file refuses startup.

Bootstrap admits every value before a listener binds and refuses startup
naming the key, and for the registry the caller and the 1-based key position,
never a value. The startup record names `service_auth.mode`, and
`service_auth_prepared` logs the name, the number of trusted callers and
whether this service signs.

`mode = "none"`, the omitted default, sends no token. In a service without
`AUTHN` its gRPC calls then need none and handlers see no caller; in a service
with `AUTHN` gRPC stays with bearer authentication. With `grpc.enabled` it is
accepted only where `app.env` is `local` or `development`, for a stand that
runs one new service among old ones that sign nothing. A stand of new services
uses `jwt` with its own keys.

## Keys and the trust registry

Each service that calls others has one signing key. Generate it and its public
JWK:

```sh
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out catalog.pem
openssl pkey -in catalog.pem -pubout -outform DER | tail -c 64 > point.bin
b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }
printf '{"kty":"EC","crv":"P-256","kid":"%s","x":"%s","y":"%s"}\n' \
  catalog/2026-10 "$(head -c 32 point.bin | b64url)" "$(tail -c 32 point.bin | b64url)"
```

The PEM becomes `APP__SERVICE_AUTH__PRIVATE_KEY` of that service only; the
JWK goes into the registry. The registry is one JSON object: each member is a
caller name, each value that caller's JWK Set (RFC 7517). Every key is an EC
P-256 public signing key with a `kid` unique within its caller; `alg`, `use`
and `key_ops`, when present, must say `ES256`, `sig` and only `verify`. A
registry that repeats a caller, names none, gives a caller no key, repeats a
`kid` within a caller, carries private (`d`) or symmetric (`k`) material, or
holds any other key refuses startup. One registry may serve the whole domain:
a receiver trusts a caller's keys only for that caller, and its table decides
what each caller may call.

Rotate a key in three deployments, since the registry is read at startup: add
the new public key beside the old one under a new `kid` and deploy the
receivers; deploy the caller with the new `key_id` and private key; remove the
old key and deploy the receivers again. To revoke a key, remove it and deploy
the receivers; a token signed with it is refused from then on.

## Declare who may call each method

The receiver's code owns the table, and it denies by default:

```rust,ignore
fn register(
    services: &mut Services,
    state: &service::AppState,
    _background: &mut service::BackgroundRegistration<'_>,
) -> Result<(), infra_grpc::Error> {
    services.describe(grpc_contracts::FILE_DESCRIPTOR_SET)?;
    services.add(CatalogServiceServer::new(Catalog::new(state)))?;
    services.allow_callers("/catalog.v1.CatalogService/Search", &["consultant"])?;
    services.allow_callers("/catalog.v1.CatalogService/Reindex", &["ingestion"])?;
    Ok(())
}
```

`allow_callers` takes a described method of an added service, or one of
reflection's two paths once `add_reflection()` was called. Another path is
`Error::UnregisteredCallerRule`, a second rule for a path
`Error::DuplicateCallerRule`, and an ill-formed or repeated caller name
`Error::InvalidCallerRule`; an empty list admits nobody. With
`mode = "jwt"`, bootstrap calls `services.authenticate_callers(verifier)`.
Once a verifier is set or any rule is declared, every method of every added
service, reflection included, needs a rule: `router` refuses with
`Error::MissingCallerRule` naming the method, and the service does not start.
[`crates/service/examples/grpc.rs`](../crates/service/examples/grpc.rs) shows
the rules for the example service.

## Read the caller

An admitted call carries the verified caller in its extensions:

```rust,ignore
async fn search(&self, request: Request<SearchRequest>) -> Result<Response<SearchResponse>, Status> {
    let caller = request.extensions().get::<infra_grpc::Caller>().map(infra_grpc::Caller::name);
    // ...
}
```

`Caller` holds the name exactly as the registry spells it and nothing else: a
caller-signed token asserts only who signed it. Fields such as a store or a
payer stay request fields that the handler checks against the caller. With
`mode = "none"`, or on the bearer path, there is no `Caller`.

## Call another service

Bootstrap puts one `infra_serviceauth::TokenSource` into `AppState` as
`service_tokens`. Take a handle for each receiver once, where the client is
composed, and bind the governed gRPC client to it:

```rust,ignore
let tokens = state.service_tokens.receiver("catalog")?;
let channel = infra_grpc::Client::try_new(destination, security, timeout)?;
let catalog = CatalogServiceClient::new(channel.with_service_tokens(tokens));
```

`receiver` fails for a name outside the caller-name grammar and, in mode
`jwt`, for a service without a signing key (`SigningError::NoSigningKey`);
both are composition mistakes that should stop startup. In mode `none` the
handle sends no header.

Every call then carries `authorization: Bearer <token>` with header
`{alg: ES256, typ, kid}` and claims `iss` = `sub` = `name`, `aud` = the
receiver, `iat` = now and `exp` = now + 60 seconds. A handle reuses its token
while `0 ≤ now − iat < 30` seconds by the system clock and signs a new one
inline otherwise, so a clock step either way signs anew. A call that already
has `authorization` is `INTERNAL`, a token that cannot be signed `UNAVAILABLE`
/ `service token unavailable`; both happen before any I/O. An HTTP client
takes the same value from `ReceiverTokens::authorization()`, a header value
marked sensitive. Never forward a received token: each token names one
audience.

## What the receiver checks

A token is valid only when all of these hold:

- the header has `alg` `ES256`, the configured `typ`, a nonempty `kid` and no
  `crit`; no header member is ever used to find a key;
- `iss` names a caller in the registry and `sub` equals it; `aud` is this
  service's `name`, as a string or a one-element array; `0 < exp − iat ≤ 60`;
- the signature verifies with the key whose `kid` matches among that caller's
  keys only;
- with a 30-second allowance, `now ≤ exp + 30`, `iat ≤ now + 30` and
  `nbf ≤ now + 30` when `nbf` is present.

Authentication runs when a call opens, inside the opening deadline and the
opening permit, before the terminal-call admission and the handler; an
admitted stream may outlive its token's `exp`. Health stays public.

| Outcome | Status | Catalog reason |
| --- | --- | --- |
| No token, foreign scheme, malformed or invalid token | `UNAUTHENTICATED` | `UNAUTHORIZED` |
| The receiver's clock precedes the Unix epoch | `UNAVAILABLE` | `SERVICE_UNAVAILABLE` |
| A verified caller outside the method's rule | `PERMISSION_DENIED` | `FORBIDDEN` |

A refused call's body is drained within the listener's existing bounds. An
admitted call has `authorization` removed before the handler.

## Observation

- `service_auth_verifications_total{outcome,reason}` counts each verification:
  `reason` is `verified`, `missing`, `malformed`, `header`, `algorithm`,
  `type`, `key_id`, `claims`, `issuer`, `unknown_key`, `signature`, `subject`,
  `audience`, `expired`, `not_yet_valid`, `lifetime` or `clock`.
- `service_auth_calls_total{caller,outcome}` counts verified callers as
  `admitted` or `not_admitted`; caller values come only from the registry.
- A verification failure logs at debug with its reason; a refused caller logs
  at warn with the caller and the method, `unknown` for a path without a rule.
- The server span of an admitted call sets `peer.service` to the caller.

Token text never appears in `Debug`, logs, metrics or spans.

## Deployment and removal

On Railway, [`.railway/railway.ts`](../.railway/railway.ts) declares the
registry and the token type as references to shared variables, one value for
the whole environment, and preserves the key id and the sealed private key,
which an operator sets per service out of band. Stands configure themselves
from their own environment files.

The profile is chosen at initialization; changing it afterwards is a refused
profile migration. A service that no longer calls others drops `key_id` and
its private key and keeps verifying its callers.
<!-- template:end service-auth:docs-service-authentication -->
