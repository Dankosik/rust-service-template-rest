# Native transport mechanism evidence

Status: ready as Technical Design evidence; no implementation proof.

Checked 2026-10-05 against the [Definition](../spec.md) and its
[baseline](baseline.md). Paths below are relative to the repository unless
prefixed with a crate name/version, in which case they name that published
crate's source. Installed source root:
`/Users/daniil/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f`.
Read-only evidence lanes were `/root/transport_design/nats_mechanism` and
`/root/transport_design/fallback_mechanisms`; the phase owner checked the
decision-critical source. No build, fault probe or new environment ran.

## Supported mechanisms

| Surface | Exact source | Decision-changing evidence |
| --- | --- | --- |
| gRPC | tonic 0.14.6 `src/transport/channel/endpoint.rs:522-550,611-627` | `connect_with_connector_lazy` applies tonic's TLS connector, then an outer `hyper_timeout::TimeoutConnector` using the endpoint's connect timeout. Ordinary `connect_lazy` uses the inner HTTP connector's TCP timeout only. Passing native Hyper `HttpConnector` preserves system DNS/candidate behavior. |
| gRPC polling lifetime | Tower 0.5.3 `src/buffer/worker.rs:71-100,153-183`; tonic 0.14.6 `src/transport/channel/service/reconnect.rs:62-149`; hyper-timeout 0.5.2 `src/lib.rs:68-88`; Tokio 1.53.1 `src/time/timeout.rs:211-223` | The buffer discards cancelled messages before polling readiness. With no next live message it may leave the connection future unpolled. The original native timeout deadline is retained, but socket/future drop can wait for a later poll. A still-pending dial then returns expired timeout; Tonic can give that error to one call before a subsequent call redials. Tokio polls the inner future first, so a ready result can win even after idle expiry. This is cooperative timeout/recovery evidence, not eager idle cleanup or strict late-ready rejection. |
| Auth | reqwest 0.13.5 `src/async_impl/client.rs:453,922,1475`; hyper-util 0.1.21 `src/client/legacy/connect/http.rs:339-347,771-787,966-972` | Public `connect_timeout` configures both the HTTP connector's divided TCP candidate budget and the complete connector's outer timer. Existing total timeout still includes body completion. [Public API](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.connect_timeout). |
| PG IPv6 | `crates/infra-postgres/src/dsn.rs`; sqlx-postgres 0.9.0 `src/options/parse.rs`, `src/connection/tls.rs:54-63` | Normalize only `url::Host::Ipv6` into `PgConnectOptions::host` after existing admission. The resulting options host also supplies TLS's IP identity. Do not replace DNS names with a chosen IP. |
| PG candidates | `vendor/sqlx-core/src/net/socket/mod.rs:184-203`; Tokio 1.53.1 `src/net/tcp/stream.rs:118-135` | Tokio resolves then awaits sockets serially. No socket-specific timeout exists. PG options directly establish the connection and expose no connector/resolver hook. The baseline's `net/socket.rs` locator means `net/socket/mod.rs`. |
| PG parent/finality | `vendor/sqlx-core/src/pool/inner.rs:334-350,377-385`; sqlx-postgres 0.9.0 `src/connection/stream.rs:44-50` | Existing pool acquisition bounds connect using remaining time. The resolver-order last error affects SQLx's ConnectionRefused retry branch; a race must preserve that final error on all-failure. Only the winning TCP stream may enter TLS/protocol setup. |
| NATS attempt | async-nats 0.50.0 `src/connector.rs:345-470` | Attempt count/pacing operates per server. DNS precedes the per-address 5 s timer; the first socket can exhaust the surrounding admission budget. `try_connect_to_server` is the smallest owner of DNS plus all candidate handshakes. |
| NATS extensions | async-nats 0.50.0 `src/options.rs:785-825`; `src/connector.rs:276-328` | `reconnect_to_server_callback` selects an existing server and delay, then private native dialing continues. It cannot supply a socket, wrap the dial future or separate a chosen IP from the TLS hostname. |
| NATS shutdown | async-nats 0.50.0 `src/lib.rs:683-711,989-1017,1090-1130`; `src/client.rs:918`; project `crates/infra-messaging/src/messaging.rs:356-391` | Native reconnect awaits indefinitely outside command polling. Drain is an ordinary queued command, and the adapter's timeout stops waiting but cannot stop that native runner. Existing native task creation is the smallest out-of-band cancellation boundary. |
| NATS trust | async-nats 0.50.0 `src/connector.rs:544-620`; `src/options.rs:588,951` | TLS-first precedes INFO, while ordinary TLS follows it. Public `tls_first` and `ignore_discovered_servers` enforce the selected discovery matrix. Hostname verification uses the original `ServerAddr::host`. [Protocol authority](https://docs.nats.io/learn/security/encryption). |
| NATS local blocking | async-nats 0.50.0 `src/tls.rs:27-41,61-62` | CA-file reads already use `spawn_blocking`, but native-root loading is synchronous inside the async connector. Moving that existing load to `spawn_blocking` makes the owned wait cancellable while retaining trust/reload behavior. |
| S3 candidates | aws-smithy-http-client 1.4.2 `src/client.rs:208-275,842-875`; hyper-util 0.1.21 `src/client/legacy/connect/http.rs:730-787,966-972` | Smithy supplies an outer DNS/TCP/TLS connect timer but leaves the inner Hyper TCP timer unset. Happy Eyeballs races address families; addresses within a family remain serial. Propagating the existing connector setting to `HttpConnector::set_connect_timeout` enables native division. |
| S3 extension limits | aws-smithy-http-client 1.4.2 `src/client.rs:835-839,900-914,1027-1043,1100` | Public resolver hooks only change resolution. `wrap_connector` and `build_with_tcp_conn_fn` are crate-private; `build_with_connector_fn` is doc-hidden and returns the sealed Connector. A public custom HttpClient would own HTTP adaptation, pooling, errors, metadata, poisoning and timeout application currently provided by Smithy. |

## Published alternatives and custody

The live crates.io API reported:

| Crate | Latest release / date | Effect on choice |
| --- | --- | --- |
| async-nats | 0.50.0 / 2026-07-20 | Already pinned; no newer published fix. Official project is [nats.rs](https://github.com/nats-io/nats.rs). |
| sqlx-core | 0.9.0 / 2026-05-21 | Already pinned and vendored for a separate return bound. No new driver or resolver is needed. Official project is [SQLx](https://github.com/launchbadge/sqlx). |
| hyper-util | 0.1.21 / 2026-09-24 | Already resolved; public TCP settings already support candidate division. Official project is [hyper-util](https://github.com/hyperium/hyper-util). |
| aws-smithy-http-client | 1.5.0 / 2026-09-30 | Published source still lacks both TCP-timeout propagation and a supported custom-TCP builder. Upgrade alone does not fix the defect and would also raise the runtime-api minimum to 1.19 from resolved 1.18. Retain 1.4.2. Official project is [smithy-rs](https://github.com/smithy-lang/smithy-rs). |

These are actively released upstream libraries; the decision is to retain their
protocol implementations and narrowly repair inaccessible native behavior,
not to replace them. No new advisory-free or full dependency-policy claim is
made here; existing dependency checks remain with delivery.

The verified async-nats 0.50.0 archive has SHA256
`d83a251fa1a4c9d0fe6e816b7acd60549e473e08d14f27a1d992c2675abff05f`,
upstream revision `9b382a2a01b5404cd66bee6c2b4f0c82c9943063`, 118 files and
1,712,200 uncompressed bytes. Inspected source/manifests match the archive and
its checksum matches Cargo.lock. New vendoring therefore has a measurable
source/custody cost, even though the runtime delta is confined to four files.

The retained Smithy 1.4.2 cached archive has SHA256
`7bd25384a4e437aa8d8f339afad4b69e786b936a7cb10db668a7aaf66717b1a8`,
49 files and 669,119 uncompressed bytes; it contains no `.cargo_vcs_info.json`.
Implementation verifies that archive identity against its lock/registry source
before vendoring and records the absent revision without inventing one.

The inspected 1.5.0 Smithy archive matched registry SHA256
`51c89cc3f1f281d659a67a519a1b5c6d445b5ce09fa7e5aee40c2c2707e9509d`.
Its `src/client.rs:306-314` still constructs the TCP connector without timeout;
`:245` remains crate-private and `:1080` doc-hidden. The new pool TCP builder
at `src/client/pool/builder.rs:413-426` also omits timeout, while its custom TCP route
at `:508` is test-util/unstable and doc-hidden. Its unchanged Rust 1.94.1 minimum
fits this project's 1.99 toolchain, but compatibility alone is no reason to
upgrade an ineffective alternative. Implementation must verify the retained
1.4.2 archive and record its exact source provenance before vendoring it.

## Conditional dispositions and proof limits

- **PostgreSQL: adopt a narrow repair in existing vendored core.** Resolve with
  the existing Tokio system path and race owned TCP futures; preserve the
  resolver-order final failure and drop losing sockets before TLS. Accept O(N)
  simultaneous TCP sockets for N returned addresses and changed winning address
  preference. Preserve the whole-return patch byte-for-byte. No concurrent
  authentication, SQL, new resolver, new dependency or app retry follows.
- **NATS: adopt a narrow pinned-source repair.** One server-attempt deadline,
  remaining-time candidate shares, and out-of-band termination of the existing
  native runner. The smaller source patch preserves native subscription and
  credential owners; recreating clients would require replacing those owners.
- **S3: adopt a narrow pinned-source repair.** Propagate the existing 3.1 s
  setting to inner Hyper TCP admission. For two same-family addresses the first
  stalled TCP candidate receives at most 1.55 s; the outer DNS/TLS timer remains
  3.1 s. No new public configuration, retry or custom HTTP transport.

No conditional fallback item is excluded, so no fallback exception is being used.
Source is discriminating here: it proves the serial pending futures and the
missing supported hooks. A separate scratch network probe would not change
the mechanism selection. Implementation must prove the changed behavior using
existing package/fixture owners at its final-validation boundary.

The later T2 gRPC evidence above prompted the reviewed clarification of only the
connection-timeout clause in [Definition](../definition-transition.md). The
selected native mechanism remains: live callers
drive the five-second timeout, idle time does not renew it, and the same client
can recover after any retained transport failure. A separately driven task or
additional dependency patch for eager idle cleanup is unnecessary under that
explicit clarification. This design actor inspected the pinned source only;
T2 owns its regression and any resulting execution evidence.

Address fallback gives later candidates an opportunity after timely DNS; it
cannot promise successful connection under arbitrary DNS/TLS delay. PG's TCP
race does not rescue a peer whose TCP succeeds but whose TLS/PostgreSQL opening
then stalls; that remains within the existing parent deadline. Dropping an
async lookup or blocking-load await ends owned transport waiting, not an
already-started OS resolver/trust-store call. No hard cancellation of OS work
is claimed.
