# Transport research baseline

Status: ready as Definition input; no implementation proof.

## Identity and custody

The continuation coordinator supplied reviewed audit R2 against
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`, dated 2026-10-05. Its independent
reviewer `/root/transport_research_review` reported PASS with no findings for
R1 plus the PostgreSQL literal-IPv6 correction. That report exists in the
conversation, not as an on-disk receipt. This document preserves the supplied
evidence and its limits; it does not claim to rerun that review or any runtime
probe. Definition also checked the current architecture and provider guides.

Locked source boundary: reqwest 0.13.5, hyper-util 0.1.21, tonic 0.14.6,
sqlx 0.9.0 (including the repository's sqlx-core whole-return patch), redis
1.7.1, async-nats 0.50.0, aws-sdk-s3 1.150.0 and
aws-smithy-http-client 1.4.2. External crate sources are available under
`/Users/daniil/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`.
Refresh a finding if its adapter or relevant dependency changes.

## Decision-changing findings

| Finding and primary source | Supported conclusion | Limit / counter-evidence | Definition disposition |
| --- | --- | --- | --- |
| `crates/infra-postgres/src/dsn.rs`, sqlx-postgres `options/parse.rs`, vendored sqlx-core `net/socket.rs`: URL host brackets reach Tokio's `(host, port)` dial | Accepted literal IPv6 DSNs can reach address resolution as `[::1]` instead of `::1` | Coordinator's native macOS `getaddrinfo` probe succeeded for bare IPv6 and failed with gaierror 8 for brackets; real PostgreSQL/Linux connection was not exercised | Correct literal representation without weakening TLS/IP identity |
| `crates/infra-grpc/src/client.rs`, tonic transport `channel/endpoint.rs` and `service/reconnect.rs` | Ordinary lazy channel connect timeout covers inner TCP; DNS/TLS can retain a pending reconnect after an individual RPC expires | Supported connector extension can bound the full dial; existing FullRpc/OpeningOnly policies already bound caller waits | Bound the retained dial and prove later same-client recovery |
| `crates/infra-bearerauthn`, reqwest `async_impl/client.rs` and `connect.rs`, hyper-util legacy HTTP connector | Auth provider total timeout is 3 s, with no distinct connect timeout; first blackholed address can spend the total budget | Reqwest's native connect timeout covers DNS/TLS and configures divided TCP candidate budgets; OAuth token acquisition uses a different client | Add a finite connection sub-budget while preserving 3 s total and trust policy |
| `crates/infra-messaging/src/messaging.rs`, async-nats `connector.rs` | Reconnect name lookup occurs before native per-socket 5 s timeout; initial admission has an outer bound, reconnect does not | Unlimited reconnect count is useful recovery; finite attempts need not impose permanent failure after a retry count | Require bounded full reconnect attempts and continued recovery |
| async-nats `connector.rs` and `options.rs`: initial INFO before TLS, discovery enabled, TLS-first disabled | An on-path actor can plant another TLS-valid reconnect hostname through unauthenticated INFO | NKey seed is not sent; valid TLS for the replacement does not authenticate the first INFO. Native TLS-first and ignore-discovery options exist; trusted-network mode deliberately has a different trust boundary | Accept discovery only within the declared authenticated/network trust boundary |
| SQLx/Tokio serial candidates; async-nats serial 5 s sockets; Smithy connector plus hyper-util | A first blackholed candidate can starve a healthy later address within the existing outer bound | No fault result yet for these exact paths. Redis `select_ok` races all TCP/TLS candidates, and outbound HTTP already has a two-address blackhole fixture | Technical Design closes bounded, provider-specific fallback decisions; no blanket resolver replacement |

## Retained evidence and limits

- DNS is consulted on a new dial. Idle timeouts do not rotate a busy socket.
  HTTP idle 30 s and reqwest/S3 idle 90 s are not maximum connection ages.
- PostgreSQL pool idle 10 min and life 30 min are soft reuse policies; server
  statement timeout 8 s is not a socket deadline. The sqlx-core whole-return
  5 s patch and real-PgBouncer/commit-proxy silent-socket regressions already
  address distinct failure paths. The jobs LISTEN pool deliberately disables
  idle/lifetime eviction and has polling recovery. Preserve these owners.
- Redis races address candidates. The application's bounded PING, owned
  connection generations and write-once recovery tests address silent sockets;
  replacing them with ConnectionManager alone would lose behavior.
- gRPC's active-stream PING 60 s / timeout 20 s requires compatible server
  enforcement. It is not discovery or load balancing.
- S3 operation timeout ends at GET headers; Download's later 5 s stall grace
  does not impose a whole-stream lifetime. Callers own that lifetime.
- Certificate/credential reload differs by protocol and material. State the
  actual snapshot/restart versus reconnect reload behavior, without promising
  global hot rotation.

Primary external authorities previously inspected by R2:
[reqwest ClientBuilder](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html),
[gRPC keepalive](https://grpc.io/docs/guides/keepalive/),
[NATS encryption](https://docs.nats.io/learn/security/encryption),
[AWS SDK Rust timeouts](https://docs.aws.amazon.com/sdk-for-rust/latest/dg/timeouts.html).
System resolution remains the least-cost baseline. Hickory is asynchronous but
does not migrate live sockets; hyper-hickory 0.8 and reqwest's optional resolver
use different Hickory generations. No dependency migration is accepted on that
basis. c-ares is maintained in its relocated repository; abandonment is not a
valid selection argument.

No build, fault suite, container, provider certification, real-database or Linux
proof is claimed by this synthesis. Technical Design owns any narrow evidence
needed to resolve its remaining mechanism decisions; Implementation owns
concrete test authoring and final validation.
