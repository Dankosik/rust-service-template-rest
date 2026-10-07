# Credential rotation observability: Technical Design

Status: ready. Independent [Technical Design Review](../design-review.md): PASS.
Authority: [Specification](../spec.md) and [Intent](../intent.md).
Base: `699887b18594088a59bcc23a049d290d089f6da1`.
This artifact owns mechanism and placement; Implementation owns concrete cases,
assertions, fixture construction and commands. No runtime proof is claimed here.

## Selected mechanism and drivers

Keep each observation with the adapter that already knows the fact. Add three
small counter families through the installed `metrics` facade and one unlabelled
JWKS acquisition timestamp gauge. Use the existing refresh tasks and challenge
callback. Do not add an observer task, global credential registry, clock-driven
control flow, configuration, endpoint, SDK or dependency.

The decisive constraint is truthful observation: a PostgreSQL options update,
NATS challenge preparation, Valkey accepted AUTH and admitted JWKS response are
different completion boundaries. A shared credential manager or a single
"rotation succeeded" event would either duplicate ownership or erase that
distinction. Separate families avoid a generic cross-provider layer and
redundant provider/stage labels. The accepted cost is three short local emission
sites/families rather than one reusable abstraction. Reopen only when another
accepted consumer needs a shared mechanism, not merely for similar syntax.

## Metric schema and material flows

All names below are canonical. Counter labels are static string literals or a
closed existing enum mapping. Only the listed pairs are emitted. Each completed
owner invocation contributes exactly one observation; cancellation before that
owner has a completed result contributes none. Descriptions are registered
beside first use under the existing facade. Register samples on actual outcomes;
do not pre-create successful samples for disabled readers or reset shared
counter handles when another adapter instance is constructed.

| Instrument | Allowed labels | Maximum series per process | Meaning |
| --- | --- | --- | --- |
| `postgres_password_file_refreshes_total` | `outcome=unchanged\|installed\|read_failed` | 3 | Completed periodic password-file steps, including the initial periodic assignment. |
| `cache_password_file_refreshes_total` | `outcome=unchanged\|auth_accepted\|read_failed`, each with `reason=none`; or `outcome=refresh_failed` with `reason=auth\|timeout\|io\|response\|parse\|other` | 9 | Completed maintained-connection file refresh; failure reasons reuse the existing `ErrorType` mapping. |
| `messaging_credentials_file_challenges_total` | `outcome=prepared\|failed`, with `reason=none` only for prepared and `reason=unreadable\|malformed` only for failed | 3 | Completed file-backed JWT/signature challenge preparation. |
| `authn_jwks_last_successful_acquisition_timestamp_seconds` | none | 1 | Wall-clock Unix seconds when usable JWKS material was last admitted into this process's current key store. |

This adds at most sixteen series, independent of credential content and test or
deployment instance counts. Process/target scrape labels remain the instance
identity. Paths, URLs, accounts, usernames, JWT subjects, key IDs, fingerprints,
raw errors and arbitrary values never become labels. Counters reset on process
restart and are not a rotation audit ledger. Existing instrumentation keeps its
names and semantics.

### PostgreSQL

`infra-postgres::credentials::Refresh::step` remains the sole writer. A completed
`read` error (including validation) records `read_failed` even when the existing
warning is suppressed as a repeat. A successful equality branch records
`unchanged`. After `PgPool::set_connect_options` and the corresponding `current`
replacement, record `installed`. The first periodic assignment counts as
installation because the task starts with `current=None`; it does not claim
that startup credentials changed. Retain `postgres_password_reloaded` only
where it currently occurs, after a later task-observed replacement.

No new database exchange follows instrumentation. Previously opened sessions,
five-second scheduling, options retention on error, and cancellation stay owned
by the existing pool/task. Neither counter nor log establishes authentication.
URL-password profiles return before this file owner and emit no file activity.

### Valkey

The existing `connection::maintain`/`refresh` boundary owns observations.
Completed file read/validation failure records `read_failed,none`, retaining
the current nonfatal `Ok(())` path, warning suppression and old authenticated
material. Successful equality records `unchanged,none`. Changed bytes go through
the existing AUTH exchange. Only the existing success boundary, after a
successful reply, the non-retired generation check and the `authenticated`
replacement, records `auth_accepted,none`; it retains `cache_password_reloaded`
at that same boundary.

Errors returned by the exchange or non-retired check record
`refresh_failed,<ErrorType::label()>` once in the maintenance result handling.
The existing outer read-plus-AUTH envelope records
`refresh_failed,timeout` when it expires; this reason deliberately does not
claim that AUTH was reached or that the server rejected credentials. `auth`
means the existing sanitized authentication rejection classification; `io`
means transport/retired-generation failure, and `response`, `parse`, `other`
preserve the bounded residual classes instead of relabelling every error as
bad credentials. Do not both emit an inner failed-exchange counter and emit the
same failure at the outer boundary. Completed read failures are already
observed and still return the existing nonfatal result.

Cancellation/retirement that wins before a result is available emits no
success; the outer timeout is a completed failed maintenance operation even
though the inner future was dropped. Instrumentation must not add an await,
change result types visible outside this private owner, retry rejected input
sooner, promote the pending password early or change generation retirement.
Connection setup remains separate: `connect` already reads the password for
each new connection, but this family observes maintenance only. Existing
startup/reconnect diagnostic errors are preserved and are not duplicated in a
second family.

### NATS

`CredentialsFile::answer` keeps the supported `with_auth_callback` path. After
read/parse/sign and assembly of the returned JWT/signature tuple, record
`prepared,none`; every challenge counts even if bytes repeat. An error exiting
the callback records `failed,unreadable` or `failed,malformed` using the existing
closed error enum. The signing-error return must be covered as well as the
currently logged read/parse failure. Admission reads stay separate.

Retain the existing `messaging_credentials_reloaded` JWT-change event. No seed
fingerprint, byte comparison history, new auth event or broker-acceptance
counter is introduced. The client sends CONNECT after this callback, and the
server may reject it: `prepared` is expressly not broker acceptance. Existing
reconnect scheduling and errors remain client-owned.

### JWKS

`infra-bearerauthn::refresh::KeyStore` remains the sole writer. At
`KeyStore::new`, after the successfully parsed usable startup set becomes its
initial state, describe/register the gauge and set its value. Store only the
gauge handle with the owner; do not add a second persisted timestamp or
per-key/issuer history. At `finish`, set it only in the successful replacement
path after `state.keys` is replaced. Sampling and setting are synchronous with
that owner operation, with no await or second worker. The one production
refresh worker serializes successful replacements; the gauge is observational
and is never read for policy.

Use `std::time::SystemTime` relative to `UNIX_EPOCH` and represent seconds as
`f64`, including the fractional part. A pre-epoch clock is represented as
negative Unix seconds from the error's duration; do not reuse `crate::unix_now`,
whose token-validation policy returns `u64::MAX` before the epoch. This tiny
local conversion has no clock-injection framework or dependency. A later
successful acquisition samples the current wall clock even for unchanged keys.
Finite clock resolution may give equal values, and corrections may move the
value backwards. No monotonic clamping or fabricated increasing timestamp is
permitted; refresh scheduling still uses the existing Tokio `Instant`.

Failed fetch, parse or no-usable-key outcomes leave both current keys and gauge
unchanged. Cooldown/coalescing alone cannot update it. Cancelling a request
waiter changes nothing; if its independently owned worker subsequently admits a
successful response, that actual acquisition updates it. Worker cancellation
that wins the fetch select never calls successful `finish` and cannot update
it. `authn_jwks_refreshes_total{result,reason}` retains its current semantics,
including no invented startup refresh count. Before a key store is admitted,
no timestamp sample exists; no zero/process-start default is registered.

The production composition owns one JWT key store, matching the existing
unlabelled process metric design. Supporting multiple simultaneously configured
issuers/stores would reopen this schema instead of silently adding identity
labels. Test recorders isolate separate constructed stores using existing
repository recording patterns.

## Authenticated fixture mechanism

### Valkey: scoped ACL state on the existing server

Use the already pinned Valkey 9.1.2 service and its supplied `CACHE_URL` admin
connection in the existing `valkey` integration target. Each fixture owns a
unique synthetic username, a restricted key prefix, its temporary password
file, cache client and any forwarding proxy. Configure that named user with
`ACL SETUSER` using `reset`, `on`, explicit passwords (no `nopass`), scoped keys
and only the commands needed by the existing adapter/redis handshake. This
includes AUTH/HELLO, PING, cache GET/SET/DEL and the client's setup metadata
commands where used. The unauthenticated default user, global password and
other test users are not changed.

The adapter gets the named user in its DSN and the fixture file through
`CacheOptions::password_file`; its runtime admission need not allow
unauthenticated operation. Password replacement uses the named user's password
set, including removal of previous passwords when their exclusion matters.
Valkey `resetpass` removes both passwords and `nopass`. A rejected pending file
can later become valid by changing only that user's allowed password set; the
existing maintenance retry remains authoritative.

Changing passwords does not invalidate old sessions. The fixture therefore
supports observation of the actual maintenance AUTH exchange on the same
adapter-owned connection, not just subsequent cache commands. Existing
loopback forwarding-proxy ownership and the installed redis 1.7.1 RESP parser
are sufficient if wire observation is selected: forward to the real server,
keep request/reply association, never synthesize AUTH success or substitute an
admin connection for the adapter. Keep any captured synthetic material local
to the fixture and out of test output. The executor chooses the concrete
boundary oracle; this is feasibility evidence, not a mandatory new proxy layer.

A subsequent connection can be isolated through `CLIENT KILL USER <owned-user>`
or the existing per-fixture proxy, without restarting Valkey or disconnecting
other users. Cleanup stops fixture clients/proxies, deletes only owned keys and
calls `ACL DELUSER <owned-user>`; that command also closes that user's sessions.
Use a fixture-owned cleanup guard/explicit async cleanup in the existing style,
and the existing runner's disposal as final cleanup for a managed container.
No `FLUSHALL`, shared ACL reset, `MONITOR`, server-wide counters or mutation of
another test's connection is necessary. Supplied endpoints must be disposable
test servers with these fixture administration rights; missing rights fail the
authenticated fixture rather than silently passing or altering a live server.

### NATS: a serialized authenticated segment of the existing broker harness

Use the same pinned NATS 2.15.0 image and existing `nats` Compose service, in the
same disposable integration project. The existing messaging script runs its
ordinary anonymous suites to completion, then recreates that service with a
temporary Compose override selecting a checked-in synthetic JWT configuration.
There is no second service, stack or runner. Do not edit the checked-in normal
configuration at runtime. In CI, database/outbox proof already precedes the
messaging script; all those processes and the anonymous messaging clients must
have terminated before the service configuration changes. No parallel consumer
may hold that broker while the authenticated segment owns it.

The auth configuration contains a synthetic self-signed operator claim, a
system-account claim, a distinct application-account claim with JetStream
limits enabled, `resolver: MEMORY` and `resolver_preload`. The operator trusts
the application account, and that account signs user claims. Both anonymous
access and default-sentinel fallback are absent. The broker therefore validates
the account/operator chain, user claim validity and the user's nonce signature.
The existing normal configuration and normal anonymous suites remain unchanged.
The official server rejects combining `no_auth_user` with operator mode, so
simultaneous anonymous and JWT-authenticated proof on one configuration is not a
supported substitute. A default-sentinel fallback can substitute credentials
after JWT decoding fails and is also unsuitable for the required denial proof.

Keep public, explicitly synthetic trust material and the test account's signing
seed in the messaging fixture assets. Generate each fixture's user key and
claims inside the authenticated Rust integration target with installed
`nkeys` 0.4.5, `serde_json` and `base64`. Set the NATS claim type/version and
user limits explicitly, leave `bearer_token=false`, and use `exp` for the
accepted expiry boundary. Native expiration can stop accepting previously
accepted old material; no production expiry mechanism or runtime issuer is
added. The executor may combine old-credential and expiry outcomes when that
demonstrates both accepted requirements. Claims for the isolated static trust
chain need no expiry; generated user lifetimes are fixture-owned.

The only local format glue is fixture JSON and standard compact JWT framing:
the exact UTF-8 header is `{"typ":"JWT","alg":"ed25519-nkey"}`; encode header
and the emitted claim JSON separately with base64url without padding, join
them with one ASCII `.` and pass those exact bytes to `nkeys::KeyPair::sign`
using the account signing key. Base64url-encode the signature without padding
and append it after the second dot. User claims carry account `iss`, user
public-key `sub`, issued-at time, `nats.type=user`, `nats.version=2`, explicit
unlimited test user limits and any chosen `exp`; any fixture claim ID is
synthetic. No JSON reserialization occurs between signing and sending. This
uses native Ed25519/NKey signing and the pinned server decoder; it does not
implement token validation, a general JWT issuer or production signing code.
For the deterministic non-secret framing/signature reference, reuse the
published upstream JWT in `vendor/async-nats/tests/configs/TestUser.creds`
against the TestAccount public key in `jwt.conf`: the signed bytes are exactly
its first two compact-token segments and the dot, and the expected signature is
the decoded third segment. The immutable fixture hashes and source contract
are recorded in [Design evidence](evidence.md). Do not copy those credentials
to production configuration.

The additional integration target is `credential_rotation` in
`crates/infra-messaging/tests/credential_rotation.rs`, admitted by the existing
`integration` feature. Its current responsibility is separation of broker
configuration/lifetime, not an additional test framework. All broker-dependent
clients in this target use synthetic credentials against its authenticated
endpoint. Each fixture owns temporary credential files, user material, unique
subjects/stream names and a test-owned relay where connection interruption is
needed. Close clients/relay tasks and remove fixture streams before returning;
managed broker disposal/restoration remains the script's responsibility. Reuse
existing relay mechanics without changing production code for test control.
Malformed file/tuple handling remains at the existing adapter boundary and may
reuse the adequate mocks identified by Specification.

`scripts/ci/test-integration-messaging.sh` is the only lifecycle owner. Its
managed mode records the exact project and original Compose inputs before
mutation, installs cleanup before changing the service, swaps only the `nats`
configuration mount, and resolves the actual published port after recreation
(an ephemeral host port may change). Keep the override and normal Compose
inputs available until restoration completes. Restore the original service
configuration and wait for its normal health before removing temporary inputs;
on a script-owned project, final disposal can replace restoration. A failed
authenticated segment remains failure even if cleanup succeeds. Restoration
failure is also a failed harness result, reported with the retained managed
project identity; do not swallow it or allow dependent suites to continue.
No cleanup deletes unrelated services or externally owned volumes.

The test-only input contract is:

| Invocation context | Authority and routing |
| --- | --- |
| No supplied `NATS_URL` | Existing script creates/owns its disposable project; it runs anonymous and authenticated segments and disposes that project. |
| Supplied `NATS_URL`, `INTEGRATION_COMPOSE_MANAGED=1` and explicit `INTEGRATION_COMPOSE_PROJECT` | The caller delegates exclusive lifecycle control of this disposable project's `nats` service. The script checks that the supplied URL resolves to that service's published endpoint before switching; it restores the normal configuration for the caller. |
| Supplied unmanaged `NATS_URL` plus `NATS_AUTH_URL` | The first endpoint serves ordinary anonymous coverage. The second is a caller-owned disposable server already configured with the documented synthetic trust fixture. Use it without server reconfiguration; fixture-owned subjects/streams and client connections are the only mutations. |
| Supplied unmanaged `NATS_URL` without authenticated endpoint | Preserve access to ordinary coverage but refuse a full integration success with the missing authenticated scope named. Do not provision or reconfigure a substitute. |
| Compile-only invocation | Compile all existing integration targets including `credential_rotation` before any Docker, endpoint, config or ownership work. The existing `NATS_URL=compile-only` cache-warming path must remain non-runtime. |

These are test harness inputs, not application configuration. Managed mode is
for an owned disposable test service only; a boolean alone without matching
project/endpoint identity and exclusive ownership is insufficient. CI assigns
one explicit Compose project identity to its existing service startup and
passes that identity plus the managed flag to the messaging script. Existing
sequential CI steps establish the exclusivity boundary; no new job or parallel
broker consumer is introduced. Test filters must apply coherently across the
ordinary and authenticated targets; partial selections cannot claim the full
R3 outcome.

The authenticated fixture belongs under `env/nats/credential-rotation.conf`
with its explicitly synthetic account signing input in the messaging test
fixture directory. `scripts/lib/template_profiles.json` removes both with the
messaging profile, and the new target registration is removed with its crate.
Existing classifier patterns already route `env/nats/*`, the messaging crate,
the script and workflow to messaging integration. Add no unrelated classifier
rules. Secret-scan policy may name only exact public synthetic fixture paths
if the existing scanner flags them, following the current upstream fixture
allowlists; never disable a rule or allow arbitrary credentials.

## Ownership and dependency decision

Runtime placement is mechanically fixed by existing owners; no new crate,
module, public interface, dependency edge, configuration or generated contract
is required. Each existing provider file retains its state and failure owner.

| Existing owner/file | Present responsibility in this change |
| --- | --- |
| `crates/infra-postgres/src/credentials.rs` | Local periodic outcome emission and matching existing-owner coverage. |
| `crates/infra-cache/src/connection.rs` | Maintenance observation at read/accepted AUTH/outer result boundaries. |
| `crates/infra-cache/src/observe.rs` | Existing bounded failure mapping; place a local metric description/helper here only if needed by the current cache split. |
| `crates/infra-messaging/src/credentials.rs` | Challenge-result emission, including signing failure. |
| `crates/infra-bearerauthn/src/refresh.rs` | Gauge registration, handle and successful-install updates. |
| `crates/infra-cache/tests/valkey.rs` | Per-fixture ACL state, file path, actual server evidence and cleanup. |
| `crates/infra-messaging/tests/credential_rotation.rs`, crate manifest and local test fixture assets | Authenticated-only target, synthetic user claims, owned credential files and isolated broker objects. |
| `env/nats/credential-rotation.conf` | Static public synthetic operator/account trust input for the authenticated segment; keep normal `nats-server.conf` unchanged. |
| `scripts/ci/test-integration-messaging.sh` | Existing runner's serialization, supplied/managed input admission, compile-only selection, configuration override and restoration/disposal. |
| `.github/workflows/ci.yml` | Explicit identity/control of its existing disposable Compose project and existing messaging runner invocation; no new job. |
| `scripts/lib/template_profiles.json` | Removal ownership for new messaging-only fixture assets. |
| Existing provider/observability guides | Metric names/meaning, event boundaries, absence/reset/clock and acquisition-versus-issuer distinction. |

Existing `metrics` 0.24.6 counter/gauge/description APIs meet the observation
requirement; redis 1.7.1 already exposes raw `cmd`, native AUTH/HELLO and RESP
parsing. Their resolved local sources, the patched async-nats 0.50.0 callback,
and official pinned server contracts are the evidence authority. No custom
metrics SDK, file watcher, Redis client or JWT authentication implementation is
selected. No crate upgrade is justified. A fixture helper is test-local and
must not become a production credential library.

## Proof boundary, compatibility and reopening

Existing unit/protocol coverage owns exact deadline, cancellation, secret-safe
failure, pending-password retry and no-traffic behavior. PostgreSQL password
rotation and LISTEN reconnect coverage are reused. R3 still requires actual
authenticated server execution in the accepted local/CI path; a source-only
check, mock AUTH response, skipped fixture, compile-only job or zero selected
tests does not discharge it. The executor chooses focused cases and commands
within current validation policy. This Design neither runs environments nor
adds a test matrix or local heavy gate.

The selected CI route includes authenticated fixtures. The existing cache runner selects the complete
`valkey` target with its integration feature; adding the fixture there keeps
it in the current `cache_integration` route. Missing fixture capabilities fail
that selected target; optional local Docker unavailability remains an accurately
reported unrun scope and does not authorize provisioning.

The messaging runner explicitly selects the authenticated target after broker
configuration admission; the existing `messaging_integration` gate observes
both segments. Its exit result includes authentication-segment and cleanup
results, so anonymous-only success cannot conceal an omitted fixture. No
`#[ignore]`, optional endpoint check returning success or `--no-run` receipt
counts as the required broker execution. A CI receipt must identify the actual
authenticated cases selected and executed on the assembled candidate.

The combined failure path remains intentionally independent: file reads or
challenge preparation may keep progressing while server authentication fails
and the JWKS gauge stays old. Existing keys/sessions may remain usable. These
signals introduce no readiness, expiry, eviction, revocation or alert policy.

Reopen Specification for changed signal/authentication semantics; Research
for contradictory resolved APIs/pins/base facts; this Design for infeasible
fixture isolation, changed material flow, additional key-store composition or
a genuinely required new dependency. PR #247 is not a dependency: if it lands,
compare affected refresh owners and retain only compatible decisions. Planning
may consume the reviewed ready result; this actor stops before Planning,
implementation, build/test/runtime actions and commit/push.
