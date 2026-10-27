# Specification: service tokens, no-CI verification, Railway IaC

Status: ready (revised after the first review, F1–F10, and its delta recheck,
D1–D5).
Requester meaning: [Intent](intent.md). Mechanism and placement:
[decision record](../../docs/service-authentication-decisions.md). Research:
[CI inventory](research/ci-inventory.md), [Railway IaC](research/railway-iac.md).

## B1. Profile selection and coexistence

`SERVICE_AUTH` is an initializer choice, `none` (default) or `jwt`. `jwt`
requires `GRPC=enabled` and combines with every other choice. `none` removes
the crate, configuration section, wiring, tests and documentation. A lock
without the field reads as `none`; changing it after initialization is a
refused profile migration.

The source template carries every profile at once, so the gRPC listener
chooses its authentication at runtime, never by marker exclusion:

- the process verifies service tokens (`service_auth.mode = jwt`): service
  tokens and the caller table admit business calls; bearer authentication is
  not applied to gRPC;
- otherwise, with `AUTHN` retained: bearer authentication, exactly as today,
  including the existing refusal to start gRPC without a configured `authn`
  mode;
- otherwise: no authentication, as today without `AUTHN`; with `SERVICE_AUTH`
  retained configuration permits that only in development (B5).

HTTP routes keep `AUTHN` in every case. `router()` takes the bearer verifier
as `Option`: `None` is accepted only when service tokens replace it, and
bootstrap passes `None` only then, so a service with both profiles needs no
`authn` configuration for gRPC while it verifies service tokens.

## B2. Token contract

A service token is a compact JWS sent as `authorization: Bearer <token>`. It
is valid only when all of these hold:

- Header: `alg` is `ES256`; `typ` equals the configured token type,
  compared ASCII case-insensitively, with or without the `application/` prefix
  (RFC 7515 §4.1.9); `kid` is a non-empty string; no `crit` member, whatever
  its value, `null` included. No header member (`jwk`, `jku`, `x5u`, `x5c`, …)
  is ever used to find a key, and members the verifier does not read are
  ignored. A member it reads may not repeat or have another type.
- Claims, a UTF-8 JSON object, read once: `iss` is a string naming a caller in
  the trust registry; `sub` equals `iss`; `aud` is the receiver's name, as a
  string or an array of exactly that one string; `iat` and `exp` are
  NumericDates (a non-negative integer, or a non-negative finite number
  rounded to one) with `0 < exp − iat ≤ 60`; `nbf` is optional, and `null` is
  malformed. A claim the verifier reads may not repeat or have another type.
  Other claims are ignored.
- Signature: ES256 as 64 bytes R‖S (RFC 7518 §3.4) over the original first two
  segments, verified only with the key whose `kid` matches among the keys
  registered for `iss`.
- Time, with a 30-second allowance: `now ≤ exp + 30`, `iat ≤ now + 30`,
  `nbf ≤ now + 30`.

A missing value or a foreign scheme is missing; several `authorization`
values or bearer syntax outside RFC 6750 are malformed; everything else
outside the contract is invalid. Authentication happens when a call opens;
an admitted stream may outlive its token's `exp`.

## B3. Trust registry

One JSON object: each member name is a caller name, each value that caller's
JWK Set (RFC 7517). A caller name is 1–63 lowercase letters, digits and
hyphens, not starting or ending with a hyphen. Every key is an EC P-256
public signing key with a `kid` unique within its caller; `alg`, `use` and
`key_ops`, when present, say `ES256`, `sig` and only `verify`. A registry
that repeats a caller, names none, gives a caller no key, repeats a `kid`
within a caller, carries private (`d`) or symmetric (`k`) material, or any
other key, refuses startup naming the caller and the 1-based key position.

## B4. Permission table and admission

The receiver declares who may call each method:
`services.allow_callers(path, &["orders"])`; an empty list admits nobody.
Paths of an added service's described methods, and reflection's two paths
once `add_reflection()` was called, take rules; anything else, a second rule,
or an ill-formed or repeated caller name refuses registration.
`services.authenticate_callers(verifier)` makes the listener verify service
tokens. When a verifier is set or any rule is declared, every method of every
added service, reflection included, must have a rule, or the router refuses
to build, naming the method, and the service does not start.

At request time, inside the opening deadline and the opening permit, before
the business concurrency limit and the handler: a call without a valid token
is refused; a path without a rule or a caller absent from it is refused; an
admitted call carries `Caller` (its name only) in its extensions, has
`authorization` removed and sets `peer.service = <caller>` on the server
span. Health stays public. Admission sits inside bearer authentication, so
when bearer authenticates, admission only removes what is left.

Statuses: missing, malformed or invalid token → `UNAUTHENTICATED`, reason
`UNAUTHORIZED`; a clock before the epoch → `UNAVAILABLE`; not admitted →
`PERMISSION_DENIED`, reason `FORBIDDEN`. A refused call's body is drained
within the existing bounds.

## B5. Runtime modes

`service_auth.mode` is `none` (omitted default) or `jwt`.

- `jwt` requires `name` (this service's caller name and accepted audience),
  `jwt_type` and `trust`. `key_id` and the environment-only `private_key`
  come together and enable outbound signing.
- `none` accepts no other key and sends no token. In a service without
  `AUTHN`, gRPC calls need no token and handlers see no `Caller`; in a service
  with `AUTHN`, gRPC stays with bearer authentication (B1). With
  `grpc.enabled`, `none` is accepted only where `app.env` is `local` or
  `development`. A mixed-version stand runs its one new service, which
  retains no `AUTHN`, this way among old services that sign nothing; an
  all-new stand uses `jwt` with stand keys. Stands configure themselves from
  their own environment files, never from `.railway/railway.ts`.

## B6. Outbound token source

Bootstrap prepares one token source and puts it in `AppState`. For a receiver
name it yields a handle whose `authorization()` returns `Bearer <token>`
(marked sensitive) with header `{alg: ES256, typ, kid}` and claims `iss = sub
= name`, `aud` = receiver, `iat` = now, `exp` = now + 60. A handle reuses its
token while `0 ≤ now − iat < 30` by the system clock and signs a new one
otherwise, so a clock step either way re-signs. In mode `none` the handle
yields no header. A receiver handle in mode `jwt` without a signing key is a
composition error. The gRPC binding adds the header to every call and answers
a call that already has `authorization` with `INTERNAL`. Token text never
appears in `Debug`, logs, metrics or spans.

## B7. Observability

`service_auth_verifications_total{outcome,reason}` with reasons `verified`,
`missing`, `malformed`, `header`, `algorithm`, `type`, `key_id`, `claims`,
`issuer`, `unknown_key`, `signature`, `subject`, `audience`, `expired`,
`not_yet_valid`, `lifetime`, `clock`. `service_auth_calls_total{caller,outcome}`
with outcomes `admitted` and `not_admitted`, caller values only from the
registry. A verification failure logs at debug with its reason; a refused
verified caller logs at warn with the caller and the method, `unknown` for a
path without a rule. The startup record names the mode.

## B8. No-CI verification mode

`REPOSITORY_CI` states which CI runs the gates the template delegates to CI:
`github-actions` (default in `make/service.mk`) or `none`. `verify.sh` reads it
from the environment or, when unset, from `make/service.mk`; a repository whose
`make/service.mk` predates the key reads as `github-actions`. With `none`:

- no step is CI-owned, and `verify.sh` passes `ALLOW_HEAVY=1` and
  `ALLOW_FULL=1` to the commands it runs, so the heavy guards admit them;
  it never sets `CI`;
- the Rust route is the workspace one CI runs on a push (`make lint`,
  `make build`, `make test`), and fmt-check also follows a dependency change;
- the plan adds the CI steps that have no plan entry when their CI selector
  matches: the range secret scan on every run; optional-profiling checks on
  Rust or dependency changes; the generator dependency policy on schema
  changes; OpenAPI compatibility against the merge base on OpenAPI changes;
  Go wire compatibility on messaging changes; the jobs reliability reference
  and the compile-only build of every integration suite with the database
  suite;
- the plan names, as not applicable, each CI check without a local
  equivalent and its residual: CodeQL (no code scanning; GitHub code
  scanning of a private repository needs a paid plan), Dependency Review
  (Rust dependencies are covered by `cargo-deny` advisories and licenses, the
  image by Trivy; workflow actions do not run), publication and signing
  (`cd.yml`), the scheduled history secret scan (run
  `ALLOW_HEAVY=1 make secret-scan-history` before a release) and
  dispatch-only messaging recovery;
- the receipt records `ci_mode: none` and its status is `verified` or one of
  the existing failure outcomes, never `partially_verified`.

With `github-actions` the plan and receipt are unchanged.

## B9. Railway IaC and build

`.railway/railway.ts` (service-owned after initialization) declares this
repository's partial (`export const partial = "<service name>"`), the
project from the linked context, and one service: GitHub source from the
initialized repository, Dockerfile build with watch patterns covering every
admitted image input, health check, restart, overlap and draining. Its `env`
declares the template's own variables; `IMAGE_TESTS` is `unit`; with
`SERVICE_AUTH=jwt` it declares the service-auth variables, the trust registry
and token type as shared-variable references, and the key id and signing key
as `preserve()` (set out of band, the key sealed). Because a declared `env`
plans the deletion of every undeclared variable, the guide requires declaring
or preserving each variable before `apply`. `.railway/package.json` and its
lockfile pin the `railway` SDK. The policy check reads the file when present
and the profile document's snippet otherwise, for repositories initialized
before the file existed; it prints which source it read, and the fallback is
removed once the template stops supporting repositories initialized without
the file (recorded in the CI/CD decisions). `changed-surfaces.sh` classifies `.railway/*`.

The image build needs no credential: no Dockerfile `ARG` may carry one
(BuildKit's secrets check in `make dockerfile-check`), and a private
dependency enters as a committed vendored source (the PR #274 method).
With build argument `IMAGE_TESTS=unit` the build runs the workspace's unit
tests (`cargo test --workspace --lib --bins`, no database) in a stage the
runtime image depends on, so a failing test fails the build and Railway keeps
the previous deployment; the default `none` skips the stage's work. The
context admits the fixtures those tests read; watch patterns cover them.
Railway passes `IMAGE_TESTS` from the service variable of that name, which
also reaches the runtime environment, where nothing reads it.

This reopens the recorded deferral "`.railway/railway.ts` generation" in
`docs/ci-cd-production-ready.md`: the adopting system's task list asks the
template to ship the file.

## Deliberately unchanged

`AUTHN`, its engines, HTTP route policy and scopes; bearer gRPC whenever the
process does not verify service tokens; `OUTBOUND_AUTH` and its mandate for
authorization-server tokens; verify behaviour with `github-actions`; the
template repository's own CI.

## Proof expectations

Unit and transport tests for every refusal in B2–B4 and the success path; a
deterministic committed vector (public key, token, `now`, outcome); router
refusal for a method without a rule; a bootstrap-level test in which a
`mode = jwt` process refuses a tokenless gRPC call, so a missing verifier
installation fails it; a log and span capture without token text; profile
projections and initialized runtime graphs with `SERVICE_AUTH=jwt` both
without `AUTHN` and with `AUTHN=oidc-jwt` (the two Cargo.lock edge rules); verify self-tests for both modes; the policy check on
`railway.ts`; an image build with `IMAGE_TESTS=unit` and no secret.
