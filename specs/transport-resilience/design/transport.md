# Outbound transport technical design

Status: ready. Independent [Technical Design reviews](../technical-design-review.md):
PASS for the original scope and the bounded gRPC clarification. The
[Definition delta](../definition-transition.md) is ready.

Authority: [ready behavior](../spec.md), [intent](../intent.md), and
[Definition transition](../definition-transition.md). Exact source/API and
alternative evidence: [native mechanisms](../research/native-mechanisms.md).
Base: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.

## Selection and constraints

Keep each provider's protocol client, system resolver, pooling, finality and
existing lifecycle owner. Use supported builder settings for gRPC, auth and
NATS discovery. Three inaccessible native behaviors require source-local
repairs: extend the existing sqlx-core vendor and vendor the already-pinned
async-nats 0.50.0 and aws-smithy-http-client 1.4.2. No version upgrade, universal
resolver, application retry layer, shared transport abstraction or replacement
provider is selected.

| Choice | Decisive constraint / cost accepted | Reopen condition |
| --- | --- | --- |
| Tonic native HttpConnector through `connect_with_connector_lazy` | Supported method applies the native cooperative wait bound to the TLS-wrapped dial. Promote already-resolved hyper-util to a production dependency of infra-grpc; no custom connector implementation. | Tonic changes the wrapper order or a released ordinary-lazy path supplies the same bound. |
| Auth native 2 s connect timeout under 3 s total | Leaves one second for response/body after a maximal connect; Hyper divides TCP time among address candidates. It is a fixed adapter policy, with no new knob. | Accepted provider behavior needs a different budget; keep total and request deadlines authoritative. |
| SQLx native TCP race | No supported socket hook. Existing vendor avoids wrapping Database/Connection/Executor; accept O(N) concurrent TCP attempts and changed address preference. | A supported SQLx setting or published equivalent removes the need; retained all-failure classification must stay equivalent. |
| NATS native attempt/close patch | Callbacks cannot wrap DNS/handshake or interrupt a reconnecting command loop. Retain native consumer/context state; accept custody of a 1.7 MB published source tree and four runtime source deltas. | Published client supplies both bounded candidate recovery and observable forced termination. |
| Smithy native setting propagation | Public API cannot set inner TCP timer; custom HttpClient would duplicate pooling, protocol adaptation and connection metadata. Accept one native runtime source delta and vendor/profile custody. Version 1.5.0 does not fix it. | Supported builder/native release propagates the timer with equivalent SDK classification and finality. |

No conditional fallback defect is excluded. The separate PG/NATS/S3
dispositions are closed as **adopt**, with the source limits stated in research.
These source repairs implement accepted behavior; they do not expand it into
custom transports or a provider migration.

## PostgreSQL admission and candidate selection

`Dsn` remains the sole admitted representation. After the existing URL and
driver checks, inspect the parsed URL host: only `url::Host::Ipv6` replaces
`PgConnectOptions::host` with the literal's bare string. Reuse the normalized
options for password-file replacement, pools, migrator and LISTEN construction.
Keep the original secret DSN for its existing custody only; do not rewrite text
or admit additional forms. The same bare host reaches the socket and the SQLx
TLS `ServerName` conversion. DNS names, IPv4 and every other option keep their
current value.

In `vendor/sqlx-core/src/net/socket/mod.rs`, change only the selected Tokio TCP
branch: call existing `lookup_host`, then drive owned TCP futures concurrently.
The first successful TCP stream wins; drop all other futures/streams before
the existing `WithSocket` continuation performs TLS/PostgreSQL opening. Keep
the original options host for TLS. Preserve empty-resolution `InvalidInput`
and, if all candidates fail, the error belonging to the last resolver-order
address, regardless of completion order; SQLx uses ConnectionRefused in its
existing retry branch. Do not add a new connect or retry budget.

The existing 3 s acquire/other caller deadline still bounds DNS, candidates and
protocol opening. The existing whole-return 5 s patch, session checks,
statement bounds, password refresh, max lifetime and LISTEN polling remain
separate owners. No SQL or transaction is issued on a losing candidate, and no
effect is replayed after uncertain completion. Async-io branches are unchanged
because this workspace selects Tokio.

## gRPC and authentication

`infra-grpc::Client::with_timeout_policy` retains the endpoint's 5 s connect
budget, TLS configuration and HTTP/2 settings. Build native
`hyper_util::client::legacy::connect::HttpConnector` with the endpoint-equivalent
values: `enforce_http(false)`, TCP_NODELAY true, TCP keepalive 60 s, and TCP
connect timeout 5 s. Keep the same default local-address, keepalive interval
and retry-count choices as the current endpoint. Pass it to
`endpoint.connect_with_connector_lazy` so tonic applies its normal TLS
connector and then the outer 5 s timeout. Construction remains network-free.

The shared channel owns each dial and its native cooperative timer. While a
live queued call drives readiness, a still-pending DNS/TCP/TLS dial observes its
5 s expiry and returns a transport failure. If an earlier caller cancels and
the buffer has no live request, Tower can stop polling that retained future.
Its original deadline remains unchanged, including across idle expiry; the next
live call resumes polling without receiving another five seconds for the old
attempt. Physical socket/
future release during an idle interval is not promised.

On resumed polling, a still-pending dial yields its expired timeout. Tonic may
deliver that retained error to this call, then a subsequent call through the
same Client starts a new dial. Tokio polls the inner future before its timer,
so an already-ready connection result can instead complete even after idle
time passed the timer; this design does not add late-ready rejection. The
existing opening/FullRpc/caller deadlines still bound each individual request
and prevent expired callers from dispatching. Preserve transport-status
mapping, normal certificate checks, terminal trailers and OpeningOnly stream
lifetime. No new HTTP/2 handshake, stream-age or retry policy is introduced.

Retain the native lazy polling owner. Eager idle resource cleanup or strict
late-ready rejection would require separately driven connection ownership or
another native dependency change; the clarified accepted behavior needs
neither. A later application call is not replay of the cancelled RPC, and the
adapter does not hide the intermediate transport failure with an automatic retry.

`infra-bearerauthn::provider::build_client` sets
`connect_timeout(Duration::from_secs(2))` beside the current 3 s `timeout`.
This applies uniformly to discovery, JWKS and introspection. Native reqwest
owns DNS/TCP/TLS timeout, candidate division, socket disposal and later reuse;
the adapter retains current unavailable/failure classification, no-proxy,
no-redirect, never-retry, origin and body policy. Outbound OAuth acquisition
continues using its existing outbound HTTP owner.

## NATS attempt and recovery ownership

A native **attempt** means one server selected by the existing reconnect loop,
starting after its existing retry delay. The 5 s ceiling includes its DNS and
all TCP/TLS/INFO/authentication candidates. Infinite recovery is a sequence of
finite attempts; it is not a five-second lifetime for the dependency. Keep
native default exponential pacing (up to 4 s) and unlimited reconnects.

In vendored `src/connector.rs::try_connect_to_server`, establish one absolute
deadline before resolution and apply `timeout_at` to resolution plus the full
candidate loop. After resolution, each candidate receives remaining time
divided by the number of candidates still untried. Reuse `try_connect_to` with
the unchanged `ServerAddr`; do not substitute an IP URL. Timeout drops that
candidate and offers a later candidate its share. Return native TimedOut for
outer expiry, preserving per-server failure bookkeeping and current sanitized
client-error reporting. Keep successful state publication and reconnect
subscription reinstatement with their current native owners.

In vendored `src/tls.rs`, run the existing native-root load in `spawn_blocking`
and await it under that same deadline, as existing CA-file reads already do.
Retain root selection and per-connect loading; no custom verifier or TLS
fallback is introduced. Async timeout ends the owned wait; an already-started
OS DNS or trust-store operation can outlive it.

### Closing the native runner

Keep the current native connection runner; add no supervisor. In vendored
`src/client.rs` and `src/lib.rs`, give Client clones a Tokio watch close-request
sender and a runner-completion receiver. Add synchronous `force_close()` and
asynchronous `wait_closed() -> bool`: true means the runner explicitly reported
completion after dropping its owned resources; channel loss without that value
means completion was not observed.

At the existing native spawn, select with close priority between the complete
runner (including background initial recovery and `ConnectionHandler::process`)
and the close-request watch. Carry a clone of the same close-request sender with
each native Subscriber's existing command sender, including its short
unsubscribe-on-drop task. A Subscriber therefore continues working after the
last Client is dropped, as the native API currently permits. Sender-channel
closure terminates the runner only after these existing external owners are
gone, stopping orphaned recovery. This adds one private field and constructor/
drop plumbing in the same two source files; no new public Subscriber API or
lifecycle owner is needed. The runner itself must not retain a close-request
sender. On termination, drop the handler, sockets,
connector, command receiver and queued work before publishing completion;
publish disconnected state and the existing Closed event on forced termination.
Normal termination also publishes completion. Events remain observation, not
the completion authority; avoid duplicate normal Closed events.

`infra-messaging::close_client` still requests graceful drain and waits within
the passed deadline. Replace its separate Event::Closed watch with the native
completion receipt. If cancellation, deadline, already-expired entry or an
unobserved drain failure prevents graceful completion, synchronously request
force-close. Await its completion only while the original deadline has time
left; never start a fresh five-second wait. Forced closure stays
`TimedOut`/`UnobservedClose` and therefore degraded, even if completion becomes
observable. A request to close is never reported as observed completion.

Readiness continues reading the native connection state through the existing
cached refresher. Bootstrap/shutdown stage ownership and budgets stay put.
Consumers keep the same durable cursor, native subscription state and handlers.
Publishing after possible dispatch remains ambiguous on cancellation/timeout;
the adapter never retries that publish or fabricates an ACK. Existing DLQ-before-
source-ACK ordering and durable effect deduplication remain unchanged.

## NATS discovery and configuration

Add the single provider capability `messaging.tls_first: bool`, default false,
environment `APP__MESSAGING__TLS_FIRST`. Carry it through `MessagingOptions` and
the jobs-worker composition. Both typed config validation and direct adapter
admission reject true with any plaintext configured seed, using sanitized
configuration errors. Credentials/trusted-network/local exceptions retain
their current admission rules.

| Configured seed/security mode | Native options / discovery |
| --- | --- |
| All TLS, default ordinary handshake | `ignore_discovered_servers()`; use configured seeds only. |
| All TLS, explicit TLS-first | `tls_first()`; keep authenticated INFO discovery and normal hostname verification. No downgrade on incompatibility. |
| All admitted plaintext trusted/local network | Keep native discovery inside that declared network boundary. |
| Mixed TLS/plaintext | `ignore_discovered_servers()`; TLS-first is invalid. |

Compute policy from admitted configured seeds, not from an INFO advertisement.
Native discovered address syntax may omit TLS schemes; TLS-first and the
connection's required-TLS policy still enforce TLS on every discovered dial.
No extra hostname/IP blacklist or certificate bypass is introduced.

Ordinary-TLS deployments that relied on cluster discovery must list failover
seeds or configure TLS-first on broker and client. Compatible ordinary-TLS
brokers still connect. TLS-first selected against an incompatible broker fails
through the bounded native admission/recovery path. Mixed versions may share a
broker: old clients retain their old discovery behavior; new clients apply
the configured matrix. No server-side topology mutation is performed here.

## S3 native fallback

In vendored aws-smithy-http-client 1.4.2
`src/client.rs::base_connector_with_resolver`, set the native Hyper connector's
connect timeout from `self.connector_settings.as_ref().and_then(
HttpConnectorSettings::connect_timeout)`. Leave the existing outer connector
timer, TLS wrapper, native DNS, pool and SDK attempts unchanged. The current S3
3.1 s setting now also supplies Hyper's per-family candidate division. No new
adapter configuration or custom transport is needed.

The patch applies wherever this pinned Smithy client receives connector
settings, including its credential HTTP consumers. Preserve existing error
mapping: native inner expiry is an I/O connector failure, outer expiry remains
a connector timeout. Neither classification grants extra SDK attempts. The
object-storage operation deadline, retry limits and mutation OutcomeUnknown
mapping remain authoritative. GET streaming still outlives header completion;
the caller owns a whole-stream deadline beyond the existing stall detector.

## Source and file custody

All placement is forced by current provider/composition ownership; no new
workspace crate or cross-provider module is created.

| Responsibility | Existing files / permitted new artifact |
| --- | --- |
| PG normalization and TCP selection | `crates/infra-postgres/src/dsn.rs`; `vendor/sqlx-core/src/net/socket/mod.rs`; update `vendor/sqlx-core/PATCHES.md` without changing the whole-return source. |
| gRPC lazy dial | `crates/infra-grpc/src/client.rs`, `crates/infra-grpc/Cargo.toml` (existing hyper-util becomes production dependency with client/client-legacy/tokio features). |
| Auth sub-budget | `crates/infra-bearerauthn/src/provider.rs`. |
| NATS native attempt/termination | New published-source `vendor/async-nats/`, runtime changes limited to `src/connector.rs`, `src/client.rs`, `src/lib.rs`, `src/tls.rs`; `PATCHES.md` owns exact delta/provenance/retirement. |
| NATS trust/config/close use | `crates/infra-messaging/src/messaging.rs`; `crates/config/src/messaging.rs` and existing loader/config carriers; `crates/jobs-worker/src/bootstrap.rs`; current MessagingOptions construction sites. |
| S3 propagation | New published-source `vendor/aws-smithy-http-client/`, runtime delta in `src/client.rs`; `PATCHES.md` owns exact delta/provenance/retirement. |
| Dependency/profile/delivery custody | Root `Cargo.toml`, `Cargo.lock`, `.dockerignore`, `build/docker/Dockerfile`, `scripts/lib/template_profiles.json`, `scripts/ci/changed-surfaces.sh` and existing classifier/initializer fixtures as affected. |
| Behavior proof | Existing provider/package tests and shared fixture owners; native-source tests where private mechanics require them. Executor chooses exact cases/commands; no duplicate runner or new environment. |
| Operator contract | Existing `docs/architecture/persistence.md`, `docs/grpc.md`, `docs/authentication.md`, `docs/durable-messaging.md`, `docs/object-storage.md`, `docs/outbound-http.md`, `docs/cache.md`, `docs/architecture/runtime-lifecycle.md`, `docs/configuration-source-policy.md` and existing config examples only where their assertions change. |

For both new vendors, verify the published archive against registry/lock
checksum, retain licenses/normalized manifest and record the published upstream
revision when supplied (otherwise record its absence and archive identity),
changed-file inventory, exact diff and retirement. Preserve versions/features;
obtain the source-selection lock change through the existing authorized locked
dependency workflow rather than hand-editing unrelated lock data. Root patch,
workspace exclusion, Docker sources and initializer removal must travel
together with the owning messaging/object-storage profile. Classify changes
for their real dependency, integration, initializer and runtime-image gates.
Do not copy a vendor into a profile which excludes its provider.

Retire each patch only with an acceptable published native equivalent and the
affected cancellation/fallback/finality proof; remove its entire carrier
together. SQLx's TCP patch and whole-return patch have separate retirement
conditions: retiring one does not silently discard the other. The release
action in this task is one separate PR; merge/deploy remain outside scope.

## Proof and operational statements

Required behavioral distinctions, without prescribing an extra test phase:

- PG admitted IPv6 reaches bare dial/TLS identity; a first pending TCP
  candidate does not starve a healthy later one; all-failure order and existing
  pool return/finality remain intact. Observed PostgreSQL behavior uses the
  existing real-database validation owner.
- The same gRPC client survives a dial stalled before or during TLS, observes
  the original connection ceiling when readiness is driven, then later
  succeeds; cancelled/shorter callers never renew that original attempt's
  deadline. Evidence and docs distinguish idle physical cleanup and native
  ready-before-timeout polling. OpeningOnly streams retain their own lifetimes.
- The auth provider client expires a stalled connect within 2 s, can use a
  later candidate and subsequent request, and retains the 3 s body deadline.
- NATS bounds a reconnect stalled before TCP, then recovers through the same
  owner. A later address receives time; forced close terminates native recovery
  and reports degraded/unobserved results honestly. A raw native Subscriber
  remains usable after its last Client is dropped, while explicit force-close
  still terminates it. Discovery proof separates
  injected pre-TLS INFO from accepted TLS-first/trusted-network INFO.
- S3's healthy later same-family TCP candidate receives an opportunity under
  the unchanged outer timer; existing retry and mutation ambiguity survive.

Implementation assembles final validation once under repository routing.
Tests of the private native mechanism may use that dependency's existing test
harness; this does not authorize a new environment or duplicate infrastructure.
Local source analysis is not runtime proof, and CI-owned gates remain CI-owned.

Documentation must distinguish system DNS on a new dial from migration of a
busy socket, and idle eviction from maximum lifetime. Preserve Redis's existing
candidate race and owned recovery. Record rotation from actual source: PG
password-file polling and CA loading on new connections; NATS file credentials
and path-based trust loading on reconnect versus startup inline credentials;
gRPC construction-time material; auth/HTTP and S3 client/trust construction and
platform-verifier limits; AWS workload-credential refresh where selected.
Do not promise global trust or credential hot reload. Describe gRPC active-call
60 s PING / 20 s timeout and compatible peer enforcement. No live provider
certification or OS-syscall cancellation guarantee is added.

Reopen this design for an unavailable native/API shape or a non-mechanical
ownership change. Reopen the exact Definition item before excluding behavior
or replacing it with material custom transport/migration. No user-owned input
is open.
