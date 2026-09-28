# Outbound HTTP decisions

This page records the durable technical decisions behind the optional bounded outbound profile. The [adoption guide](outbound-http.md) owns normal usage, limits, and compatibility.

## Selected implementation

`infra-outbound-http` owns one client for one operator-selected trusted HTTPS origin. It builds the `hyper-util` 0.1.20 legacy pooled client over a `hyper-rustls` 0.27.9 connector, the same stack reqwest wraps, with normal system resolution (`GaiResolver`); the template-owned `infra-egress-dns` resolver and public-address classifier are removed. This preserves a private provider with a normally trusted matching certificate while keeping normal TLS verification. Deployment network controls, provider configuration, and TLS are the trust boundary. Caller-controlled or otherwise untrusted destinations require their own future SSRF decision.

The builder fixes rustls TLS with the `aws-lc-rs` provider and the platform verifier, HTTPS-only production connections, system DNS, HTTP/1, a 30-second idle pool timeout, reqwest's former TCP defaults (no delay, 15-second keepalive, 30-second user timeout on Linux), and the configured parser header count. Redirects, retry, proxies, referer, and decompression do not exist in this stack, so nothing has to disable them. Hyper has no HTTP/1 aggregate response-header byte control, so a header byte ceiling is not represented as a false guarantee; a parser rejection remains `Transport`. The header count stays at most 32,768 because hyper sizes a response `HeaderMap` from it and `http` 1.5 `HeaderMap` panics above that size. Dropping an exchange releases its caller-owned future, but system `getaddrinfo` is not promised to be physically abortable. HTTP/1-only connections offer no ALPN, where reqwest offered `http/1.1`; an HTTP/1 server needs none.

One TLS client configuration serves the process. Building the platform verifier loads and parses the system root store, which cost 6 ms and about 85 KB per client when every client built its own; webhook delivery builds one client per endpoint. Sharing it also shares the rustls session cache, which rustls keys by server name. Roots are read once, on the first construction; a trust-store change needs a restart, as it already did for every existing client.

Each request is an absolute `http::Uri` admitted without re-parsing: its scheme must equal the configured scheme, its host must equal the configured origin's serialized host ASCII case-insensitively, and its effective port must match; userinfo and a caller `Host` header are refused. Any other spelling of the same address, such as a non-canonical IP literal, is refused rather than normalized. The client then sets `Host` from the fixed origin, formatted once per client, and `Accept: */*` when absent, as reqwest did. The request goes to hyper as given. This replaces reqwest's `Uri` to `String` to `Url` to `Uri` conversion and per-request header-map copy, and it keeps the absolute-URL contract that replaced origin-form composition for both current adapters (OAuth2 token URL and webhook endpoint URL).

Time uses one mechanism: `tokio::time::timeout` around the whole exchange, set to the earlier of the caller deadline and `Limits::operation_timeout`, from connection through the last body byte. The duration is computed from `now` with saturating arithmetic, so there is no deadline arithmetic that can overflow. An already expired deadline is refused before I/O.

The body is read with `http-body-util` 0.1.5 `Limited` and `collect` over hyper's `Incoming`, the same mechanism axum and tower-http use for bounded bodies. An advertised `Content-Length` above the ceiling, taken from hyper's exact size hint, is refused before reading. `Limited` yields either hyper's body error or its own length error, so both map to the existing closed errors.

The client has no local admission semaphore. Both current adapters already bound concurrency upstream: webhook delivery by the jobs worker slots, and OAuth2 token acquisition by its single-flight owner. A second bound only produced an `AtCapacity` result configured never to fire. Reopen this if an adapter needs a per-provider bulkhead; the canonical form is a `tokio::sync::Semaphore` or `tower::limit::ConcurrencyLimit` owned by that adapter.

Request headers are adapter-owned. The client no longer removes `traceparent`, `tracestate`, `baggage`, or `X-Request-ID`, and it still injects no trace context. Reopen this if trace propagation to trusted providers is wanted; that is a data-sharing decision for each provider, implemented with the existing OpenTelemetry propagator rather than a client-owned header list.

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

The existing `metrics` 0.24.6 and tracing stack record the private attempt lifecycle. The OpenTelemetry `http.client.request.duration` instrument is exported as `http_client_request_duration_seconds`, matching the recorder's explicit Prometheus names with unit suffixes, and uses explicit second buckets that this crate exports as `REQUEST_DURATION_BUCKETS`; the composition root passes them to the Prometheus recorder. The chosen attributes are a privacy-restricted subset of the current [OpenTelemetry HTTP span](https://opentelemetry.io/docs/specs/semconv/http/http-spans/) and [metric](https://opentelemetry.io/docs/specs/semconv/http/http-metrics/) conventions: method, configured server identity, known status, static failure type, and finite outcome. Full URLs, targets, headers, request identifiers, credentials, bodies, and arbitrary error text remain excluded.

The local observation guard starts after deadline and target admission, because the duration instrument describes requests that were attempted; a refused target or an expired deadline sends nothing. It then covers complete responses, timeouts, transport/body failures, and dropped polled futures exactly once. Complete 1xx, 2xx, and 3xx responses have no span error; complete 4xx, 5xx, and uninterpretable 600–999 statuses retain an `Ok(Response)` result but use their decimal status as the error type. A body failure takes precedence over a known status. A drop reports caller cancellation with no error type; it does not infer provider outcome.

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

The accepted changes, each measured alone before combination:

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
