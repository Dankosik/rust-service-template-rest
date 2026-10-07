# Bounded outbound HTTP

Select `OUTBOUND_HTTP=bounded` (or `--outbound-http bounded`) during initialization to retain `infra-outbound-http`. The default `none` removes the client, its tests, and this guide. Selection makes no startup request, provider configuration, readiness dependency, or process task.

## One client for one trusted provider origin

A provider adapter owns its endpoint, credentials, headers, parsing, business errors, and retry eligibility. It constructs one reusable client for an operator-selected HTTPS origin; never construct a client from a request, tenant input, tenant-registered webhook URL, or another untrusted value.

`Client::new` takes a `Url` and binds the client to its origin: scheme, host, and port. The URL's path, query, and fragment are ignored, so an adapter may pass its configured endpoint URL directly. The URL must be HTTPS with a host and without userinfo. Private, loopback, and literal IP origins are valid when the operator's configuration, certificate, and deployment network trust that provider. Normal certificate-chain and hostname/IP verification remain on. The client does not claim to prevent private-network access or defend arbitrary URLs: those are deployment and future untrusted-destination boundary decisions.

```rust,ignore
use std::time::Duration;

use infra_outbound_http::{Bytes, Client, Limits, Url};
use tokio::time::Instant;

let client = Client::new(
    &Url::parse("https://provider.example")?,
    Limits {
        operation_timeout: Duration::from_secs(2),
        response_header_count: 64,
        response_body_bytes: 1024 * 1024,
    },
)?;

let request = http::Request::get("https://provider.example/v1/items?limit=10")
    .body(Bytes::new())?;
let response = client
    .execute(request, Instant::now() + Duration::from_secs(3))
    .await?;
```

The limits are adapter policy, not defaults. Each must be positive; the response header count is at most 32,768. Clones share one connection pool.

Each request carries an absolute URI on the configured origin. A request for another scheme, host, or port, a relative or asterisk target, userinfo, or a caller `Host` header is `InvalidTarget` before I/O. The client sets only what the transport owns: `Host` from the configured origin, `Accept: */*` when the request has none, HTTP/1.1 as the version whatever the request names, and the framing of the buffered body. `Content-Length` is the body's length on a request that has content or whose method expects it (`POST`, `PUT`, `PATCH`), and absent otherwise; a caller `Content-Length` or `Transfer-Encoding` is replaced, because hyper would send a wrong length as written and the provider would read the difference from the next request on that connection. Every other header reaches the wire unchanged: correlation, compression negotiation, and authorization headers are the adapter's decision.

Trace propagation is an opt-in for each client. `Client::with_trace_context()` writes the context of every attempt's client span through the process text-map propagator (W3C `traceparent` and `tracestate`), replacing a caller value of those headers, so the provider's server span joins the caller's trace. A span without a trace context, as in a process that exports no traces, writes nothing, and a caller value of those headers then passes through. A trace identifier is data shared with the provider: opt in for an origin in the same trace domain, such as another service of the same system, and leave third-party origins on the default. The built-in OAuth2 token client and webhook delivery do not opt in.

## Deadline, transport, and retry ownership

The caller supplies an absolute deadline, or preserves its existing cancellation and cutoff with `execute_with_context`. The client derives its finite local ceiling once before preparation; a request `OperationContext` extension adds another bound. Neither context restarts while the exchange is admitted, so token preparation, dispatch, and confirmed EOF consume one original allowance. An already stopped context returns `Timeout` before network work. Dropping the future ends request-owned work, but does not prove that a provider received no request or reversed a provider effect.

After full buffering and the last await, the client rechecks both contexts
before terminal observation. A successful result at or after either cutoff
becomes `Timeout`. Observation records that fixed result once; synchronous
callbacks may delay physical return without changing it or dispatching again.

A composition acquiring credentials or doing other preparation first derives
`client.operation_context(&parent)` at entry and retains that context through
preparation and execution.

The client uses Hyper's system resolver (including system hosts mappings), one pooled HTTP/1 transport, normal TLS validation, no redirects, ambient proxy, referer, or automatic decompression. A new dial resolves the configured hostname; a DNS change does not migrate an existing busy connection. The pool evicts connections after 30 s idle, but has no maximum connection age. One TCP connect budget, half of `Limits::operation_timeout` and at most 10 seconds, is divided among the resolved addresses of one family, so an address that never answers leaves time for the next one. TCP keepalive is 15 s, then 15 s, with 3 retries where supported; Linux TCP user timeout is 30 s. These socket settings do not replace the earlier operation/caller deadline, which includes the body.

Construction and cloning perform no DNS or provider connection. The [client construction](../crates/infra-outbound-http/src/lib.rs) shares a process-wide TLS `ClientConfig` initialized on first use. With pinned `rustls-platform-verifier` 0.7.1, Linux system roots are a snapshot loaded when that verifier is built; restart the process to pick up changed roots. Other platforms use their verifier's OS-specific trust behavior. The client provides no global trust hot-reload guarantee, and an existing TLS connection is not revalidated on a trust-store change.

The client has no local concurrency queue, tracker, cancellation token, readiness probe, or teardown stage; the caller's own concurrency bound (for example jobs worker slots or the inbound limit) bounds its work, and library internals own their cleanup.

The TLS configuration is process-wide (`tls_config` uses `OnceLock`), including
the verifier and shared session cache. On Linux the platform verifier loads
system roots into that owner; constructing another `Client` or reconnecting
does not reload those roots. Replace the process to load changed Linux trust.
Other platforms retain their verifier's platform behavior; this API provides
no trust-store reload control. Existing TLS connections are not revalidated
when trust changes, and resumption can reuse prior authentication without a
new full certificate check. Trust removal therefore also needs the relevant
connection/session-cache lifecycle and provider policy. See the
[rotation sequence](configuration-source-policy.md#rotation-and-revocation).

Responses preserve HTTP version, status, headers, extensions, and encoded body bytes, including 3xx, 4xx, and 5xx statuses. The HTTP/1 parser enforces the configured header count and hyper's default buffer ceiling of 417,792 bytes for the status line and headers together; a parser refusal is `Transport`. Content length is rejected early when it exceeds the body ceiling, and streamed encoded bytes are bounded while they are buffered. Success requires EOF, including for empty bodies and trailers. Each data frame is copied into one accumulator and released before polling the next; the returned body retains no input frame backing. An adapter that requests compression owns decoding and any bound on decoded content.

For payload ceiling `L`, requested accumulator storage is at most `L`, or `2L` transiently when growth moves its allocation. Reservation follows received bytes, never an advertised length, and geometric growth avoids copying the accumulated body for every small frame. These bounds exclude allocator rounding and retained arenas, the transport's current frame and read buffers, and adapter parsing/cache storage; a current frame can refer to a larger backing allocation. They do not bound process RSS. Provider concurrency remains adapter-owned: webhook jobs use worker slots, and OAuth uses its shared provider-attempt capacity separately from cache capacity and same-key coalescing.

The transport never repeats a request that may have reached the provider and never reconciles an uncertain effect. Hyper-util may retry an unsent request returned after a reused pooled connection fails. That loop has no fixed numeric replay limit; a fresh-connection failure is not eligible, and the outer operation deadline still bounds the exchange. For an adapter-owned operation that is known safe to repeat, use the already selected [`backon`](https://docs.rs/backon/latest/backon/) inside the parent deadline and with explicit cancellation and eligibility policy. Do not stack retry loops or retry an operation merely because its transport outcome is uncertain.

## Error and observation contract

Construction returns a `BuildError`: `InvalidConfiguration` for a refused origin or limit, or `Tls` when the platform verifier cannot be built. An exchange returns an `Error`: `InvalidTarget`, `Timeout`, `ResponseBodyTooLarge`, or `Transport`. Retained transport errors carry no request URL, headers, or body. Provider adapters map these errors at their own boundary; this client does not construct inbound Problems.

Each polled attempt that passes deadline and target admission records a bounded client span, exported under the OpenTelemetry name of its method (`HTTP` for a non-standard method), and the OpenTelemetry `http.client.request.duration` histogram, exported in the service's Prometheus naming as `http_client_request_duration_seconds`. The signal contains only the standard method or `_OTHER`, configured origin address/port, the adapter's path template when it supplies one, a finite outcome, known status, and a static failure type. It never includes a full URL, path, query, headers, credentials, body, request identifier, or arbitrary error text. A dropped pending attempt is observed as caller cancellation rather than provider success or failure. The duration sample retains its existing observation boundary: it can include intervening observation work and exceed the operation budget. It is neither the operation-decision timestamp nor total physical-return latency.

Without a template every request to one origin looks the same: a slow or failing operation cannot be told from its neighbours. An adapter that calls more than one operation of a provider names each with a `UrlTemplate` request extension, the OpenTelemetry `url.template`:

```rust,ignore
use infra_outbound_http::UrlTemplate;

let mut request = http::Request::get(format!("https://provider.example/v1/items/{id}"))
    .body(Bytes::new())?;
request.extensions_mut().insert(UrlTemplate("/v1/items/{id}"));
```

The span is then exported as `GET /v1/items/{id}` with a `url.template` attribute, and the histogram carries a `url_template` label. The template is a `&'static str`, so it is a literal with placeholders written in the adapter, never a formatted path; each distinct template is one more series for every method, status, and failure type it meets. The extension passes through the OAuth2 resource client unchanged. Webhook delivery and the token client set none: each of their clients sends to one configured URL.

The failure type (`error.type`) of a failed exchange is one of:

| `error.type` | Meaning |
| --- | --- |
| `timeout` | A supplied deadline, cancellation or `Limits::operation_timeout` ended the exchange. A known status means the response head had arrived. |
| `response_body_too_large` | The body exceeded `Limits::response_body_bytes`. |
| `connect` | No connection was established: name resolution, a refused or unanswered TCP connect. |
| `tls` | The TLS handshake was refused: an untrusted, expired, or mismatched certificate, or a protocol alert. |
| `protocol` | The response head could not be parsed, or exceeded the header count or the head buffer. |
| `transport` | Any other transport failure, such as a connection lost during the exchange. |

An adapter maps `Transport` to its own closed outcome and drops the source, so the client tells the cause itself: each `Transport` failure logs one `outbound_http_transport_failed` warning inside the client span, whose `error` field is the library error and its sources on one line, for example `client error (Connect): invalid peer certificate: BadSignature`. That text comes from hyper, rustls, and the operating system; it can name the configured host and a resolved address, and carries no request URL, headers, or body.

## Test-only HTTP mock support

Downstream tests may enable the default-off feature from a dev dependency:

```toml
[dev-dependencies]
infra-outbound-http = { workspace = true, features = ["test-support"] }
```

`Client::new_for_test_http` accepts only an `http` URL whose host is a literal loopback IP. It is intended for a local mock server such as `wiremock`; it retains origin isolation, deadlines, and response limits. It does not expose a raw client, arbitrary HTTP, custom roots, or a production TLS bypass. `Client::new` remains HTTPS-only even when this feature is enabled.

Shared generated TLS material is test-only and is retained when authentication or outbound HTTP is selected. Tests continue to prove normal trusted-chain, hostname, and untrusted-root behavior; production receives no custom root or certificate-generation dependency.

## Compatibility and decisions

This API removes `Operation` (pass the deadline directly), `Limits::max_active` with `AtCapacity`, the per-operation response-body limit, origin-form request targets, and the removal of `traceparent`, `tracestate`, `baggage`, and `X-Request-ID`. `Client::new` and `Client::new_for_test_http` take a `Url`. `Timeout` no longer carries a source. Construction failures moved from `Error` to `BuildError` (`InvalidConfiguration`, and `Tls` in place of `ClientBuild`), so a match on an exchange `Error` needs no arm for them. The `error.type` of a transport failure is now `connect`, `tls`, `protocol`, or `transport`, where it was always `transport`; a dashboard or alert that selects `error_type="transport"` should select all four. Adopters pass absolute same-origin request URLs and drop obsolete error matches. The client now writes the request version and the body framing itself, so a request that named another version or carried its own `Content-Length` or `Transfer-Encoding` is sent as HTTP/1.1 with the body's true length, and an empty `POST`, `PUT`, or `PATCH` now carries `Content-Length: 0`. A `url_template` label appears on the histogram only for requests that supply a `UrlTemplate`. The initializer choice remains `OUTBOUND_HTTP=none|bounded`.

[Outbound HTTP decisions](outbound-http-decisions.md) records the resolved library versions, telemetry conventions, and reopen conditions. A proxy, HTTP/2, streaming, automatic decompression, a raw-target API, or an untrusted destination need a new contract and proof.
