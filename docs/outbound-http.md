# Bounded outbound HTTP

Select `OUTBOUND_HTTP=bounded` (or `--outbound-http bounded`) during
initialization to retain `infra-outbound-http`. The default `none` removes the
client, its tests and this guide. Selection adds no startup request, provider
configuration, credentials, route or readiness dependency.

## One client per fixed dependency

The provider adapter owns its endpoint, credentials, parsing, business errors,
retry eligibility and readiness policy. It constructs one reusable
`infra_outbound_http::Client` for its fixed public HTTPS dependency. Construction
does no DNS or network I/O. A client clone shares its resolver cache, connection
pool and operation slots.

```rust,no_run
use std::time::Duration;

use bytes::Bytes;
use http::Request;
use infra_outbound_http::{Client, Limits, Operation};
use tokio::time::Instant;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::new(
    "https://api.example.com",
    Limits {
        max_active: 16,
        operation_timeout: Duration::from_secs(2),
        response_header_count: 64,
        response_header_bytes: 16 * 1024,
        response_body_bytes: 1024 * 1024,
    },
)?;
let response = client.execute(
    Request::get("/v1/items?q=a%2Fb").body(Bytes::new())?,
    Operation {
        deadline: Instant::now() + Duration::from_secs(1),
        timeout: Some(Duration::from_millis(900)),
        response_body_bytes: Some(64 * 1024),
    },
).await?;
let body = response.into_body();
# let _ = body;
# Ok(())
# }
```

`Limits` has exactly five positive, representable ceilings: `max_active`,
`operation_timeout`, `response_header_count`, `response_header_bytes`, and
`response_body_bytes`. `Operation` supplies an absolute `deadline` and may
narrow the timeout and response-body ceiling; it carries no cancellation token.
An expired deadline, zero or widening override, or invalid client limit fails.
Exhaustion returns `AtCapacity` immediately, without a queue or DNS work, and a
permit stays held through the complete body read.

For request-owned work, derive the operation deadline from the stamped
`infra_http::RequestDeadline` and retain enough adapter-owned reserve to parse
the provider result and form the inbound response. Background callers supply a
finite deadline. Await `execute` directly: dropping the caller future drops the
exchange and releases admission, but cannot establish that a request already
delivered to a provider had no effect.

## Fixed authority and standard messages

The API accepts `http::Request<bytes::Bytes>` and returns
`http::Response<bytes::Bytes>`. This is a source-breaking migration: replace the former custom messages and
string targets with standard messages, remove the three outgoing limits and
constructor/operation cancellation arguments, configure the base as an origin,
and supply the complete path instead of relying on `Url::join` base-path
semantics.

It has no replacement request/response structs,
raw reqwest client, raw builder, response stream, tracker or shutdown handle.
Do not log a standard message's `Debug` output when it might contain provider
credentials or payloads.

The base URL names only the provider origin: HTTPS with a host, an optional
port, and no credentials, path, query, fragment, whitespace or controls. Literal
IP hosts are admitted with the same public-address policy as DNS answers. Each
typed request URI must be origin-form: it has a path and optional query, begins
with `/`, and has neither scheme nor authority; `*` is refused. The adapter
builds the target by applying the typed URI's path and query as URL components
to the admitted base. It never concatenates or joins an attacker-controlled
string, so the base host and port cannot change. A base path would not act as a
prefix, so a base such as `https://api.example.com/v1` is refused at
construction instead of being silently ignored; the request's `/v1/items`
selects that path, and `//other.example/path` remains a path rather than an
authority. `%23` is path or query data.

Dynamic `http` URI parsing can discard a `#fragment` before `execute` sees the
typed URI, so a resulting `/items` is admissible. An adapter that accepts a raw
target must reject a literal `#` before constructing `Uri`; it must allow `%23`.
The base URL itself always rejects fragments.

The transport denies an explicit `Host` header and removes `traceparent`,
`tracestate`, `baggage`, `X-Request-ID`, and `Accept-Encoding` before sending.
It sends HTTP/1.1 with cleaned caller headers and `Bytes`; it does not forward
extensions or let a caller select HTTP/2, an upgrade, or another destination.
Returned status, headers, version and buffered body are preserved, including
non-2xx results; response extensions are empty. The provider maps status and
parses content.

## Egress, pooling and response bounds

Only public HTTPS destinations are supported. A complete DNS answer set is
admitted before any connection: empty answers or any denied answer reject the
attempt. The shared `infra-egress-dns` resolver admits literal addresses at URL
boundaries and DNS answers at resolution; it retains the hostname for TLS SNI
and certificate verification. It uses system DNS configuration without the
hosts file, and has finite library-owned resolver cache and request bounds.

Both outbound and authentication use the shared resolver and
`https_client_builder`. That builder fixes rustls, HTTPS and HTTP/1, no
redirects, proxies, referer, automatic decompression, or reqwest policy retry;
it uses a 30-second idle connection pool and each consumer's finite idle-per-host
cap. A stale reused connection may receive the library's pre-write retry only;
there is no adapter retry, idempotency inference, or replay after a request may
have reached the provider. Pool reuse does not redo DNS, while a fresh
connection receives a fully admitted answer set.

The single operation timeout covers DNS, connection, TLS, response headers and
the complete body read. Header count is limited by the HTTP/1 parser, aggregate
header bytes are checked after parsing, and the body reader rejects an advertised
or streamed excess before it is returned. Exact-cap bodies need framed EOF;
premature EOF is a transport failure. Compression is disabled, so limits apply
to encoded bytes. Parser/transport buffers and caller-owned request `Bytes` are
outside the response-body cap.

Resolver and reqwest/hyper background work is library-owned. It is neither
registered with nor joined by the service task tracker; the pool and resolver
are bounded by library policy, and process runtime shutdown is their final
boundary. JWT refresh remains separately process-owned, cancelled and joined by
the existing tracker.

## Failure mapping and test material

`Error` distinguishes invalid configuration and target, denied destinations,
capacity, response-header/body excess, timeout, transport, resolver
configuration, and client-build failures. Source-bearing reqwest errors have
their URL removed before retention. Diagnostics may expose a DNS hostname or
connection IP, but never a complete URL, credential, header, request or response
body. `Client` diagnostics remain redacted. The adapter maps errors to its
existing boundary; authentication maps provider failures to its sanitized
unavailable outcome.

Tests use generated, test-only TLS material from
`crates/infra-egress-dns/tests/fixtures/tls.rs`: an execution-time-valid isolated
root, matching DNS-SAN leaf/key, and unrelated root. The module returns DER
bytes only to test modules; it creates no production custom-root escape. The
eight former auth/outbound TLS DER fixtures are removed, while the JWT signing
key remains.

## Dependency decisions and evidence

The 2026-09-25 source review selected the already resolved reqwest 0.13.5,
Hickory 0.26.3, hyper-util 0.1.20, url 2.5.8 and http 1.5.0. Hickory's
[supported Tokio resolver](https://docs.rs/hickory-resolver/0.26.3/hickory_resolver/type.TokioResolver.html)
replaces the copied runtime and per-lookup construction. It retains system
nameservers/search configuration, disables hosts-file lookup, collects IPv4 and
IPv6 answers, and pins a cache of 8192 responses, two concurrent upstream
requests and 32 active requests per multiplexed upstream. These bounds do not
promise a global task count or prompt join after dropping a lookup.

The supported [reqwest builder](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html)
provides the selected HTTPS controls and finite pool without a custom hyper
connector. In [hyper-util's legacy client](https://docs.rs/crate/hyper-util/0.1.20/source/src/client/legacy/client.rs),
cancelled-request recovery requires a reused connection and the original
unconsumed request, before writing starts. That pre-write recovery is distinct
from reqwest policy retries, which remain disabled. DNS TTL expiry does not
terminate an established admitted TLS connection.

The dated prefix tables and their precedence live in
[`address.rs`](../crates/infra-egress-dns/src/address.rs), based on the IANA IPv4
and IPv6 registries and the explicit Azure WireServer denial. ipnet 2.12.2 owns
CIDR mechanics; the template owns this public-only policy. IPv4-mapped IPv6 and
well-known NAT64 reapply the IPv4 policy, including metadata. A generic global-IP
predicate cannot replace those accepted exceptions and threat-policy overrides.

Test-only rcgen 0.14.10 uses the existing aws-lc-rs backend, with default features
off and execution-time validity from time 0.3.55. This avoids expiring checked-in
certificates and an external OpenSSL process. The existing chunk loop remains
because a Limited adapter would add conversion machinery to the buffered API.
No crate upgrade, performance result or live-provider certification is implied.

## Reopen conditions

Changing the reqwest or Hickory versions reopens the retry, lifetime and
redaction source claims; changed IANA special-purpose registry evidence reopens
address policy. Private destinations, a proxy, HTTP/2, streaming, automatic
decompression, a raw-target API, or a general outbound client need a new
contract and proof.
