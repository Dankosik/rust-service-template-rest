# Bounded outbound HTTP

Select `OUTBOUND_HTTP=bounded` (or `--outbound-http bounded`) during initialization to retain `infra-outbound-http`. The default `none` removes the client, its tests, and this guide. Selection makes no startup request, provider configuration, readiness dependency, or process task.

## One client for one trusted provider origin

A provider adapter owns its endpoint, credentials, parsing, business errors, and retry eligibility. It constructs one reusable client for an operator-selected HTTPS origin; never construct a client from a request, tenant input, tenant-registered webhook URL, or another untrusted value.

The production constructor accepts an HTTPS origin with a host and optional port. Private, loopback, and literal IP origins are valid when the operator's configuration, certificate, and deployment network trust that provider. The client rejects userinfo, a path other than `/`, query, fragment, controls, and whitespace. Normal certificate-chain and hostname/IP verification remain on. It does not claim to prevent private-network access or defend arbitrary URLs: those are deployment and future untrusted-destination boundary decisions.

```rust,ignore
use std::time::Duration;

use infra_outbound_http::{Bytes, Client, Limits, Operation};
use tokio::time::Instant;

let client = Client::new(
    "https://provider.example",
    Limits {
        max_active: 16,
        operation_timeout: Duration::from_secs(2),
        response_header_count: 64,
        response_body_bytes: 1024 * 1024,
    },
)?;

let request = http::Request::get("/v1/items?limit=10").body(Bytes::new())?;
let response = client.execute(
    request,
    Operation {
        deadline: Instant::now() + Duration::from_secs(3),
        response_body_bytes: Some(64 * 1024),
    },
).await?;
```

The limits are adapter policy, not defaults. Each must be positive and representable. Clones share operation slots; exhaustion returns `AtCapacity` immediately without a queue. A permit remains held until the complete response body is read or the exchange future ends.

Relative origin-form targets are composed through URL path and query setters. They cannot replace the configured origin. Absolute, authority, and asterisk targets are refused. The caller must not supply `Host`; it is `InvalidTarget` before I/O. The client removes only `traceparent`, `tracestate`, `baggage`, and `X-Request-ID`; it preserves an adapter-supplied `Accept-Encoding` header and does not inject any correlation or authorization header.

## Deadline, transport, and retry ownership

The caller supplies an absolute deadline. The exchange uses the earlier of that deadline and its start plus `Limits::operation_timeout`; an already expired effective deadline returns `Timeout` before network work. The optional per-operation response-body limit can only narrow the client ceiling. Dropping the future releases admission and ends request-owned work, but does not prove that a provider received no request or reversed a provider effect.

The client uses normal system resolution (including system hosts mappings), one pooled HTTP/1 transport, normal TLS validation, no redirects, ambient proxy, referer, automatic decompression, or reqwest retry. Construction and cloning perform no DNS or network I/O. The client has no tracker, cancellation token, readiness probe, or teardown stage; library internals own their own cleanup.

Responses preserve HTTP version, status, headers, and encoded body bytes, including 3xx, 4xx, and 5xx statuses. The HTTP/1 parser enforces the configured header count; a parser refusal is `Transport`. Content length is rejected early when it exceeds the body ceiling, and streamed encoded bytes are bounded while they are buffered. An adapter that requests compression owns decoding and any bound on decoded content.

The transport never retries or reconciles an uncertain effect. For an adapter-owned operation that is known safe to repeat, use the already selected [`backon`](https://docs.rs/backon/latest/backon/) inside the parent deadline and with explicit cancellation and eligibility policy. Do not stack retry loops or retry an operation merely because its transport outcome is uncertain.

## Error and observation contract

The client returns `InvalidConfiguration`, `InvalidTarget`, `AtCapacity`, `Timeout`, `ResponseBodyTooLarge`, `ClientBuild`, or `Transport`. Retained reqwest errors are sanitized without a URL. Provider adapters map these errors at their own boundary; this client does not construct inbound Problems.

Each polled attempt records a bounded client span and the `http.client.request.duration` histogram in seconds. The signal contains only the standard method or `_OTHER`, configured origin address/port, a finite outcome, known status, and a static failure type. It never includes a full URL, path, query, headers, credentials, body, request identifier, or arbitrary error text. A dropped pending attempt is observed as caller cancellation rather than provider success or failure.

## Test-only HTTP mock support

Downstream tests may enable the default-off feature from a dev dependency:

```toml
[dev-dependencies]
infra-outbound-http = { workspace = true, features = ["test-support"] }
```

`Client::new_for_test_http` accepts only an `http` origin containing a literal loopback IP and the same origin grammar. It is intended for a local mock server such as `wiremock`; it retains target isolation, deadlines, admission, and response limits. It does not expose a raw client, arbitrary HTTP, custom roots, or a production TLS bypass. `Client::new` remains HTTPS-only even when this feature is enabled.

Shared generated TLS material is test-only and is retained when authentication or outbound HTTP is selected. Tests continue to prove normal trusted-chain, hostname, and untrusted-root behavior; production receives no custom root or certificate-generation dependency.

## Compatibility and decisions

This API deliberately removes `Operation.timeout`, `Limits.response_header_bytes`, `ResponseHeadersTooLarge`, destination-denial errors, and DNS imports. Adopters keep the client operation timeout and their absolute deadline, remove obsolete error matches, and use the shared TLS source or named mock constructor in tests. The initializer choice remains `OUTBOUND_HTTP=none|bounded`.

[Outbound HTTP decisions](outbound-http-decisions.md) records the resolved library versions, telemetry conventions, and reopen conditions. A proxy, HTTP/2, streaming, automatic decompression, a raw-target API, or an untrusted destination need a new contract and proof.
