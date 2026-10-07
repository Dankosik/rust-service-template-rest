# Technical Design evidence and alternative disposition

Static inspection on 2026-10-06 at
`699887b18594088a59bcc23a049d290d089f6da1`. No build, test, Docker operation,
credential generation or provider environment was run. The selected mechanism
is in [Design](design.md); behavior is in [Specification](../spec.md), ready
SHA256 `ba8c0acc93bbc666b24104750302a4a9bd36c3cd5d69bf42c5201bf4b5851201`.

## Runtime source and supported hooks

| Source | Observation and decision consequence |
| --- | --- |
| [PostgreSQL credentials](../../../crates/infra-postgres/src/credentials.rs), `Refresh::step` | Equality returns early; options installation occurs before `current` replacement; the first assignment suppresses the existing reload event. Observe these branches without changing task state. |
| [Valkey connection](../../../crates/infra-cache/src/connection.rs), `maintain`, `refresh`, `connect` | `refresh` swallows completed file-read failure as `Ok(())`; outer timeout covers file read and AUTH; only accepted exchange/non-retired generation updates `authenticated`. Instrument these different boundaries rather than equating any `Ok(())` with authentication. |
| [Valkey observation](../../../crates/infra-cache/src/observe.rs), `error_type` | Existing closed `auth`, `timeout`, `io`, `response`, `parse`, `other` mapping excludes server text. Reuse it for refresh exchange failures. |
| [NATS credentials](../../../crates/infra-messaging/src/credentials.rs), `answer` | Read and signing failures are separate return sites; tuple preparation precedes broker authentication. Both failures need an outcome. |
| [Patched async-nats connector](../../../vendor/async-nats/src/connector.rs), `auth_callback` | Supported callback runs for connection establishment; the native credentials-file convenience builder reads once. Retain callback rereads. |
| [JWKS refresh](../../../crates/infra-bearerauthn/src/refresh.rs), `new`, `finish`, `run_refresh_worker` | One worker owns fetches/installations; coalesced request waiters do not own the fetch lifetime. Failed fetch leaves keys unchanged; cancelled worker bypasses finish. These sites own the new gauge. |
| [JWT startup](../../../crates/infra-bearerauthn/src/jwt.rs), `prepare_with_provider` | `KeyStore::new` receives a parsed usable startup key set after discovery/fetch admission. |
| [Authentication clock](../../../crates/infra-bearerauthn/src/lib.rs), `unix_now` | Its pre-epoch sentinel is `u64::MAX` for token refusal; it is not a truthful observation timestamp helper. Use a local standard-library signed seconds conversion instead. |

`Cargo.lock` resolves metrics 0.24.6, redis 1.7.1, nkeys 0.4.5 and patched
async-nats 0.50.0. Installed registry source was read for metrics
`macros.rs` (`gauge!`, `describe_gauge!`) and `handles.rs` (`Gauge::set`), redis
`connection.rs` (`connection_setup_pipeline`, AUTH/HELLO and CLIENT SETINFO),
`lib.rs`/`parser.rs` (`Parser`, `parse_redis_value`, async parser), and nkeys
`lib.rs` (account/operator/user constructors and `sign`). These support the
selected mechanism without dependency additions. The pinned
[nkeys API](https://docs.rs/nkeys/0.4.5/nkeys/struct.KeyPair.html) documents the
same key/signing surface.

## Valkey fixture feasibility

The [current suite](../../../crates/infra-cache/tests/valkey.rs) already owns
unique keys, temporary files, test-local TCP/TLS forwarding proxies and clients;
its default `CacheOptions` has no password file. New ACL identities belong to
that existing fixture lifetime. No runtime adapter hook is needed.

Official [Valkey 9.1.2 ACL source](https://github.com/valkey-io/valkey/blob/9.1.2/src/acl.c)
was read at `ACLCheckUserCredentials`, `checkPasswordBasedAuth`, `resetpass`
handling and `ACL DELUSER`: named users check passwords, accepted auth changes
the connection's user state, and `resetpass` also removes `nopass`. The
[AUTH contract](https://valkey.io/commands/auth/) supplies the reply boundary.
[ACL SETUSER](https://valkey.io/commands/acl-setuser/) supports per-user password
and key rules; changing credentials does not itself evict existing sessions.
[CLIENT KILL](https://valkey.io/commands/client-kill/) can target a user, and
[ACL DELUSER](https://valkey.io/commands/acl-deluser/) removes that user's
connections. These hooks permit isolation without altering the default user
or server-wide policy.

Selected: one fixture-owned ACL user, native client commands, current password
file and existing relay if the executor selects independent wire evidence.
Rejected: server-wide `requirepass`, modifying default user, global AUTH command
counts, or an old-session cache command as the only acceptance evidence. They
either interfere with other tests or cannot discriminate successful reAUTH.
Accepted cost: bounded fixture ACL cleanup and uniquely owned state. Reopen if
the supplied disposable test server lacks ACL administration or supported
commands; do not broaden control of that server or quietly skip proof.

## NATS fixture feasibility and source comparison

The read-only specialist `/root/credential_followup_design/nats_fixture`, native
`gpt-6-astra` / `high`, returned a supported serialized-harness decision. The
phase owner checked the decision-critical server nonce path and incompatible
anonymous mode against the pinned primary source and adopted the decision.

The pinned [NATS 2.15.0 auth source](https://github.com/nats-io/nats-server/blob/v2.15.0/server/auth.go)
decodes/validates user claims, resolves the account trust chain and verifies the
nonce signature against the user public key when `bearer_token` is false.
Its `validateNoAuthUser` rejects operator mode plus `no_auth_user`; default
sentinel behavior is not suitable denial evidence. The pinned
[configuration parser](https://github.com/nats-io/nats-server/blob/v2.15.0/server/opts.go)
supports operator, system account, memory resolver and preloaded claims.

NATS resolves its JWT format through `github.com/nats-io/jwt/v2` 2.8.2. Its
[claims encoder](https://github.com/nats-io/jwt/blob/v2.8.2/v2/claims.go) signs
the unpadded base64url header/payload with NKeys and validates expiration; its
[user claim source](https://github.com/nats-io/jwt/blob/v2.8.2/v2/user_claims.go)
sets user claim type/version, unlimited default test limits and the distinction
between nonce-bearing and bearer-token credentials. `ClaimsData.Validate`
rejects an `exp` in the past; no server clock alteration is needed. The Rust
fixture only supplies data to this existing protocol, using installed key,
JSON and base64 libraries.

The existing upstream fixture is also a deterministic, non-secret signing and
framing reference, not a runtime dependency or a production credential:

| Exact source | SHA256 |
| --- | --- |
| [TestUser.creds](../../../vendor/async-nats/tests/configs/TestUser.creds) | `47247665ddb65d7646f7a585086c8a3cf57d651a18f9a7ae2902739fd56d6391` |
| [jwt.conf](../../../vendor/async-nats/tests/configs/jwt.conf) | `7ff1b5bf16d11d338a2085fee33a1df6b9946de6b5730065c689259c0ab86abb` |

The exact compact token segments, expected signature, and issuer public key
in those files constitute the vector; Design does not reproduce key material.
The fixture's static TestAccount claim does not supply the selected JetStream
account limits or a writable account signing seed for expiring new users, so
merely reusing its user credential is insufficient for R3. Use its shape as
evidence and create bounded synthetic trust assets for this target during
Implementation. A locally found development `nsc` executable has no pinned CI
contract; it is not selected as a runtime fixture dependency. Adding a Rust JWT
issuer crate or Go generator/tool chain would cost more than this fixed
test-only data construction and is not needed. Reopen dependency selection if
that glue grows into reusable signing policy or dynamic claims administration.

The strongest alternative was one simultaneously anonymous/JWT configuration;
the pinned broker contract rules it out. A second permanent auth server, a
new standalone runner, a token/password-only broker or a challenge mock does
not satisfy the accepted scope/boundary. The selected cost is one serialized
configuration/recreation segment with restoration inside the existing runner.

## Existing gate and lifecycle facts

[Compose](../../../env/docker-compose.yml) pins NATS 2.15.0 and Valkey 9.1.2
by digest. The [messaging runner](../../../scripts/ci/test-integration-messaging.sh)
owns its generated project only when it creates the service; `NATS_URL` alone
currently bypasses lifecycle setup. The [cache runner](../../../scripts/ci/test-integration-cache.sh)
selects its whole existing integration target. Both are already selected by
the [shared classifier](../../../scripts/ci/changed-surfaces.sh).

The [CI integration job](../../../.github/workflows/ci.yml) starts shared
services, runs database proof, then cache/object storage, then messaging; its
later Go wire checks do not require a broker. This permits serialized managed
broker configuration once the preceding consumers have exited. CI must pass
the explicit project identity, not just its current URL, and the existing
runner must restore before returning. The same job's cache-warming `--no-run`
path currently passes `NATS_URL=compile-only`; maintain non-runtime routing and
include the new target in that compilation. These are source facts and required
future routing constraints, not evidence that authenticated tests ran.

Profile removal is owned by
[template_profiles.json](../../../scripts/lib/template_profiles.json); new
messaging fixture assets need that removal ownership. The current
[secret-scan policy](../../../.gitleaks.toml) has exact-path allowlists for
public upstream JWT/key fixtures, which is the maximum scope of any necessary
new synthetic-fixture exception.

## Reuse and remaining empirical boundary

Definition's [baseline](../research/baseline.md) identifies adequate PostgreSQL
password/LISTEN and deterministic cache protocol coverage. This phase reuses
their meaning, not a claim of current test execution. The clarified R3 keeps
real NATS old/expired-credential refusal, valid replacement recovery and real
Valkey AUTH acceptance/removal refusal as downstream execution obligations.
Malformed-input timing and failure semantics can use existing focused mocks.
No protocol matrix or test-case plan is a Technical Design deliverable.

Reopen on changed base/pins, incompatible #247 owner changes, an unsupported
fixture contract, inability to establish exclusive managed ownership, or a
different production key-store composition. Missing local Docker alone does
not invalidate source-level mechanism readiness; required actual authenticated
execution still needs an honest matching local/CI result before that proof is
claimed complete.
