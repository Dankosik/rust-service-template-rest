# Outbound HTTP decisions

This page records the durable technical decisions behind the optional bounded outbound profile. The [adoption guide](outbound-http.md) owns normal usage, limits, and compatibility.

## Selected implementation

`infra-outbound-http` owns one client for one operator-selected trusted HTTPS origin. It uses the already resolved `reqwest` 0.13.5 builder directly and normal system resolution; the template-owned `infra-egress-dns` resolver and public-address classifier are removed. This preserves a private provider with a normally trusted matching certificate while keeping normal TLS verification. Deployment network controls, provider configuration, and TLS are the trust boundary. Caller-controlled or otherwise untrusted destinations require their own future SSRF decision.

The builder fixes rustls TLS, HTTPS-only production construction, system DNS, no redirects, retry, proxy, referer, or automatic decompression, HTTP/1, a 30-second idle pool timeout, and the configured parser header count. Reqwest's [0.13.5 supported builder API](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html) provides these controls. It has no supported HTTP/1 aggregate response-header byte control, so a header byte ceiling is not represented as a false guarantee; a parser rejection remains `Transport`. The header count stays at most 32,768 because hyper sizes a response `HeaderMap` from it and `http` 1.5 `HeaderMap` panics above that size. Dropping an exchange releases its caller-owned future, but system `getaddrinfo` is not promised to be physically abortable.

Each request is an absolute URL, converted with reqwest's own `TryFrom<http::Request>`, and admitted when `Url::origin` equals the configured origin; a caller `Host` header or userinfo is refused. This replaces the earlier origin-form composition, which made both current adapters (OAuth2 token URL and webhook endpoint URL) split a URL they already held and let the client rebuild it.

Time uses one mechanism: reqwest's per-request total timeout, set to the earlier of the caller deadline and `Limits::operation_timeout`. Reqwest applies it from connection through the last body byte, so there is no second `tokio::time::timeout_at` and no deadline arithmetic that can overflow. An already expired deadline is refused before I/O.

The body is read with `http-body-util` 0.1.5 `Limited` and `collect` over reqwest's `http::Response<reqwest::Body>` conversion, the same mechanism axum and tower-http use for bounded bodies. An advertised `Content-Length` above the ceiling is refused before reading. `Limited` yields either reqwest's body error or its own length error, so both map to the existing closed errors.

The client has no local admission semaphore. Both current adapters already bound concurrency upstream: webhook delivery by the jobs worker slots, and OAuth2 token acquisition by its single-flight owner. A second bound only produced an `AtCapacity` result configured never to fire. Reopen this if an adapter needs a per-provider bulkhead; the canonical form is a `tokio::sync::Semaphore` or `tower::limit::ConcurrencyLimit` owned by that adapter.

Request headers are adapter-owned. The client no longer removes `traceparent`, `tracestate`, `baggage`, or `X-Request-ID`, and it still injects no trace context. Reopen this if trace propagation to trusted providers is wanted; that is a data-sharing decision for each provider, implemented with the existing OpenTelemetry propagator rather than a client-owned header list.

The selected normal transport does not own provider retry policy. `backon` 1.6.0 is already available for adapters that have independently established safe retry eligibility inside their parent deadline and cancellation policy.

## Test and fixture boundaries

The default-off `test-support` feature exposes only the named literal-loopback HTTP mock constructor. It does not weaken the HTTPS production constructor, publish a raw reqwest client, or add custom roots. The test-source owner `test/fixtures/tls.rs` shares generated material between outbound, authentication, and mounted idempotency tests. It uses the existing pinned `rcgen` 0.14.10 only through test dependencies; production has no generated certificate or custom-root path.

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

## Reopen conditions

Reopen the design if the pinned reqwest API cannot maintain the configured transport invariants, if a retained profile cannot consume the shared TLS source, or if observation cannot finalize exactly once across timeout and future drop. Reopen the definition if trusted-origin, TLS, compatibility, or observable behavior constraints cannot be satisfied. A reqwest, http-body-util, rcgen, metrics, tracing, or OpenTelemetry-convention change requires rechecking the affected decision; it does not silently change this profile.
