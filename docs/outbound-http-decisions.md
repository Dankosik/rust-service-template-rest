# Outbound HTTP decisions

This page records the durable technical decisions behind the optional bounded outbound profile. The [adoption guide](outbound-http.md) owns normal usage, limits, and compatibility.

## Selected implementation

`infra-outbound-http` owns one client for one operator-selected trusted HTTPS origin. It builds the `hyper-util` 0.1.21 legacy pooled client over a `hyper-rustls` 0.27.10 connector, the same stack reqwest wraps, with normal system resolution (`GaiResolver`); the template-owned `infra-egress-dns` resolver and public-address classifier are removed. This preserves a private provider with a normally trusted matching certificate while keeping normal TLS verification. Deployment network controls, provider configuration, and TLS are the trust boundary. Caller-controlled or otherwise untrusted destinations require their own future SSRF decision.

The builder fixes rustls TLS with the `aws-lc-rs` provider and the platform verifier, HTTPS-only production connections, system DNS, HTTP/1, a 30-second idle pool timeout, reqwest's former TCP defaults (no delay, 15-second keepalive, 30-second user timeout on Linux), a TCP connect budget, and the configured parser header count. Redirects, provider retry, proxies, referer, and decompression do not exist in this stack, so nothing has to disable them. Hyper bounds the HTTP/1 status line and headers together by its read-buffer ceiling, 417,792 bytes by default (`http1_max_buf_size`); the client keeps that default and exposes no separate header byte limit, because the same setting also caps the adaptive body read buffer, so lowering it is a throughput decision that needs its own measurement. A parser rejection remains `Transport`. The header count stays at most 32,768 because hyper sizes a response `HeaderMap` from it and `http` 1.5 `HeaderMap` panics above that size. Dropping an exchange releases its caller-owned future, but system `getaddrinfo` is not promised to be physically abortable. HTTP/1-only connections offer no ALPN, where reqwest offered `http/1.1`; an HTTP/1 server needs none.

One TLS client configuration serves the process. Building the platform verifier loads and parses the system root store, which cost 6 ms and about 85 KB per client when every client built its own; webhook delivery builds one client per endpoint. Sharing it also shares the rustls session cache, which rustls keys by server name. Roots are read once, on the first construction; a trust-store change needs a restart, as it already did for every existing client.

Each request is an absolute `http::Uri` admitted without re-parsing: its scheme must equal the configured scheme, its host must equal the configured origin's serialized host ASCII case-insensitively, and its effective port must match; userinfo and a caller `Host` header are refused. Any other spelling of the same address, such as a non-canonical IP literal, is refused rather than normalized. The client then sets `Host` from the fixed origin, formatted once per client, and `Accept: */*` when absent, as reqwest did. It also sets the two request properties the transport owns. The version becomes HTTP/1.1: hyper-util resolves, connects, and completes the TLS handshake before it refuses a version the connection cannot send, so a request built with another version cost a connection to fail. The framing headers follow the buffered body: hyper sends a caller `Content-Length` as written (it checks the value against the body only in a debug build), and one that differs from the body leaves bytes on a pooled connection that the provider reads as the start of the next request; RFC 9112 section 6.3 forbids such a length. So `Transfer-Encoding` is removed and `Content-Length` is the body's length when there is content or the method is `POST`, `PUT`, or `PATCH`, as RFC 9110 section 8.6 recommends and as some front ends require with `411`; hyper alone sends no length for an empty body. Everything else goes to hyper as given. This replaces reqwest's `Uri` to `String` to `Url` to `Uri` conversion and per-request header-map copy, and it keeps the absolute-URL contract that replaced origin-form composition for both current adapters (OAuth2 token URL and webhook endpoint URL).

The connect budget is `HttpConnector::set_connect_timeout` at half of `Limits::operation_timeout`, capped at 10 seconds. Without it the connector waits for the operating system's connect timeout on each address in turn, so a host with several addresses whose first one drops packets spent the whole operation there and never reached the next; hyper-util divides the budget among the addresses of one family, and its 300 ms happy-eyeballs fallback still races the other family. The budget is a transport setting of the client, so it follows the configured ceiling rather than a shorter caller deadline.

Hyper-util's `retry_canceled_requests` stays on, as it was under reqwest. It retries only a request hyper handed back unsent after a reused pooled connection fails, never a request that may have been sent or one returned by a fresh connection. The resolved loop has no fixed numeric retry limit; the outer operation deadline bounds it. Turning it off would surface every idle-connection close as a `Transport` error that the adapter must then retry itself.

Time uses one `OperationContext` across the whole exchange, set to the earlier of the caller cutoff and `Limits::operation_timeout`, from connection through the last body byte. `execute_with_context` retains the supplied cancellation lineage, and a request context extension adds a further bound. An already stopped context is refused before I/O.

After full buffering and the last await, the client rechecks both contexts
before terminal observation. A successful result at or after either cutoff
becomes `Timeout`. Observation records that fixed result once; synchronous
callbacks may delay physical return without changing it or dispatching again.

The body is read with `http-body-util` 0.1.5 `Limited` over hyper's `Incoming`. An advertised `Content-Length` above the ceiling, taken from hyper's exact size hint, is refused before reading. Each frame is polled through `BodyExt::frame`; nonempty data is copied into one initially empty `Vec<u8>`, while empty data and trailers are discarded. Every input frame leaves scope before the next poll. Success requires EOF. `Limited` remains the byte-limit/transport-error authority, including equality at the ceiling and errors after partial data. The loop stays inside the existing timeout and observation guard.

For ceiling `L`, accumulated length `n`, capacity `C`, and admitted next length `R`, growth requests `max(R, min(L, 2*C))` only when `R > C`, with saturating doubling and `reserve_exact(target - n)`. The first nonempty frame selects the first allocation; neither Content-Length nor the ceiling causes advance reservation. Every requested payload allocation is at most `L`, with at most `2L` old-plus-new requested storage during relocation, and constant bookkeeping. The allocator may round capacity upward or retain freed arenas. Transport read buffers and the current frame, which may refer to a larger backing allocation, remain additional costs. These are not process-RSS bounds. `Bytes::from(vec)` transfers the accumulator allocation at EOF without another payload copy or retained input backing, including one-frame responses. The standard library and declared dependencies provide this mechanism; no custom buffer or test-only production seam is needed.

The client has no local admission semaphore. Webhook delivery bounds attempts by the jobs worker slots. OAuth owns a shared immediate-admission semaphore across service-token and distinct-subject exchange initializers; single-flight only coalesces the same key and does not bound distinct subjects. Its cache-entry capacity is a separate storage control. A new adapter owns its own provider capacity and retry policy.

Request headers are adapter-owned. The client no longer removes `traceparent`, `tracestate`, `baggage`, or `X-Request-ID`, and by default it injects no trace context. Propagation is a data-sharing decision for each provider, so it is an opt-in of each client: `Client::with_trace_context()` injects the context of the attempt's client span with the installed OpenTelemetry text-map propagator, through the same `tracing-opentelemetry-instrumentation-sdk` 0.42.1 call the gRPC client uses, rather than a client-owned header list. The injected parent is the client span, not the caller's span, so the provider's server span nests under the attempt. Neither built-in adapter opts in: a webhook endpoint and an identity provider are outside the service's trace domain.

The selected normal transport does not own provider retry policy. `backon` 1.6.0 is already available for adapters that have independently established safe retry eligibility inside their parent deadline and cancellation policy.

## Test and fixture boundaries

The default-off `test-support` feature exposes only the named literal-loopback HTTP mock constructor. It does not weaken the HTTPS production constructor, publish the raw transport, or add custom roots. The test-source owner `test/fixtures/tls.rs` shares generated material between outbound, authentication, and mounted idempotency tests. It uses the existing pinned `rcgen` 0.14.10 only through test dependencies; production has no generated certificate or custom-root path.

Cargo-shear 1.13.4 scans Cargo target directories and misses the shared
`#[path]` fixture outside those directories. Each of its three consuming
packages therefore records only `rcgen` in `package.metadata.cargo-shear.ignored`.
Package exceptions also count as workspace usage, so no workspace-wide ignore
is needed. The mounted-test exception leaves with that profile; auth and
outbound exceptions leave with their crates. Remove these exceptions when the
pinned scanner follows the shared source.

## Observation boundary

The existing `metrics` 0.24.6 and tracing stack record the private attempt lifecycle. The OpenTelemetry `http.client.request.duration` instrument is exported as `http_client_request_duration_seconds`, matching the recorder's explicit Prometheus names with unit suffixes, and uses explicit second buckets that this crate exports as `REQUEST_DURATION_BUCKETS`; the composition root passes them to the Prometheus recorder. The chosen attributes are a privacy-restricted subset of the current [OpenTelemetry HTTP span](https://opentelemetry.io/docs/specs/semconv/http/http-spans/) and [metric](https://opentelemetry.io/docs/specs/semconv/http/http-metrics/) conventions: method, configured server identity, known status, static failure type, and finite outcome. The span's `otel.name` is the method, or `HTTP` when the method is not a standard one, as the convention names a client span without a low-cardinality target; the `tracing` span name stays `outbound_http` for log correlation. The convention's low-cardinality target for a client is `url.template`, which only the caller knows, so an adapter may supply it as a `UrlTemplate` request extension: the span is then named `{method} {url.template}` and the span and the histogram carry the template. It is the same carrier the OAuth2 client uses for `OnBehalfOf`, and the mechanism `reqwest-tracing` offers as its `OtelName` extension. The type holds a `&'static str`, which keeps a formatted path, and with it an identifier, out of a metric label at compile time. `url.full`, which the convention requires on a client span, stays excluded: a path or query can hold a provider secret (a webhook URL often does), and the template answers the operator's question without it. Full URLs, targets, headers, request identifiers, credentials, bodies, and arbitrary error text remain excluded.

The local observation guard starts after deadline and target admission, because the duration instrument describes requests that were attempted; a refused target or an expired deadline sends nothing. It then covers complete responses, timeouts, transport/body failures, and dropped polled futures exactly once. Complete 1xx, 2xx, and 3xx responses have no span error; complete 4xx, 5xx, and uninterpretable 600–999 statuses retain an `Ok(Response)` result but use their decimal status as the error type. A body failure takes precedence over a known status. A drop reports caller cancellation with no error type; it does not infer provider outcome. The duration sample keeps its existing boundary inside observation, so intervening observation work may make it exceed the operation budget. It is neither the decision timestamp nor total physical-return latency. No deferred reporting task or telemetry policy change is introduced.

A transport failure is classed from the typed causes the libraries retain, never from their text: `tls` when a `rustls::Error` is among the causes, `connect` when hyper-util marks the error as a connect error, `protocol` when hyper reports a parse error, and `transport` otherwise. A rustls error arrives as the payload of an I/O error, which `source()` skips, so the walk follows that payload. Name resolution stays inside `connect` because hyper-util exposes no type for it. The classes answer the operator's first question (cannot reach, cannot trust, cannot understand, lost in flight) with four label values.

The cause itself is logged by the client, once, as `outbound_http_transport_failed` inside the client span, the same way the gRPC client logs `grpc_client_transport_failed`. Both built-in adapters reduce a transport error to a closed outcome (`transport_uncertain`, `AcquisitionError::Transport`) and drop its source, and hyper and hyper-util print only the failed step in their own text (`client error (Connect)`), so the event prints the error with its sources on one line. The text stays out of span attributes and metric labels. Timeouts and body-limit refusals have no library cause and log nothing here.

## Hot-path decisions and measured scope

Measured on 2026-09-28 against `4824ffc` on a DigitalOcean c-4 (4 dedicated vCPU, Ubuntu 24.04, Rust 1.98.1, locked dependencies): release client and a hyper/rustls provider on disjoint CPUs over loopback, one current-thread runtime, the service's own `infra-telemetry` JSON subscriber and Prometheus recorder. The primary metric is `perf stat` user instructions per request, `(I(N) − I(0)) / N`, which repeated within 0.5%; allocation counts come from a counting allocator. Short responses carry a 32-byte body.

| Workload | Instructions per request | Allocations per request |
| --- | --- | --- |
| HTTPS GET, 200 | 95.2k → 61.7k (−35%) | 89 → 42 |
| HTTPS GET, 500 | 106.9k → 64.8k (−39%) | 96 → 43 |
| HTTPS POST 1 KiB | 113.9k → 73.8k (−35%) | 108 → 54 |
| HTTP loopback GET | 97.2k → 54.3k (−44%) | 94 → 40 |
| HTTPS 64 KiB body | 281.8k → 246.0k (−13%) | 96 → 49 |
| New connection and TLS handshake | 1.073M → 1.033M (−4%) | 242 → 181 |

Without a subscriber or recorder, a short HTTPS GET falls from 72.6k to 45.9k instructions and from 67 to 27 allocations. At 64 concurrent requests on two runtime workers, throughput rises 39–50% and CPU per request falls 28–33%. Client construction falls from 6.2 ms to 1.6 µs, and the resident memory of a client with one open connection from 108 KB to 24 KB.

These historical results describe the measured revision and do not measure the incremental collector or the OAuth provider capacity introduced later.

The accepted changes at that revision, each measured alone before combination:

- The hyper-util transport instead of reqwest: −25% instructions on a short HTTPS GET, −34% over plain HTTP. Reqwest's conversions, header copy, and redirect and retry layers were all work for features this client disables.
- Server identity formatted once per client as shared strings for the span and metric labels: −3%, six allocations.
- `Host` formatted once per client: −1.7% to −3.2%.
- Span status and port recorded as integers, the OpenTelemetry attribute type, instead of formatted strings: −2.2%.
- One `tracing::record_all!` at the end of an attempt instead of a record per field: −5% on success and −15% on an HTTP error, because the JSON layer re-serializes every span field on each record.
- The shared TLS configuration: construction time and per-client memory above.

Labels still reserve their final count, an error status shares one decimal string between its two labels, and static error types stay static, as recorded by the earlier allocation study (patch SHA-256 `d6f79888d5447405926a7752e6237c031b2fb82298b80ccd55f2ed81c3749472`).

Measured and rejected:

- A per-client metric-handle cache: a further −5.7% on a short GET, but a handle binds to the recorder current at its first attempt, so an attempt before recorder installation, or inside a test's local recorder, would silently misroute that key for the client's lifetime.
- Content-length presizing with a single-frame fast path: −19% wall time only for 1 MiB bodies, no gain at 64 KiB, and more allocated bytes for chunked bodies. Neither adapter receives such bodies, and reserving before bytes arrive would let a provider's declaration hold memory.
- A fixed 16 KiB hyper read buffer: +8% instructions at 64 KiB and more than double the wall time at 1 MiB.

The span itself remains the largest observation cost: removing it would save another 16k instructions and 15 allocations per short request. It is the contract, so it stays. The harness, raw results, and variant patches are retained outside the repository; recheck these figures when the transport, subscriber, compiler, or workload changes.

## Reopen conditions

Reopen the design if the pinned hyper-util and hyper-rustls APIs cannot maintain the configured transport invariants, if a retained profile cannot consume the shared TLS source, or if observation cannot finalize exactly once across timeout and future drop. Reopen the definition if trusted-origin, TLS, compatibility, or observable behavior constraints cannot be satisfied. A hyper, hyper-util, hyper-rustls, rustls, http-body-util, rcgen, metrics, tracing, or OpenTelemetry-convention change requires rechecking the affected decision; it does not silently change this profile.
