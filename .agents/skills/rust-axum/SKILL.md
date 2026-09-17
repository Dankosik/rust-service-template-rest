---
name: rust-axum
description: "Use when a change adds or moves axum routes, extractors, layers, fallbacks, or changes what the hardened HTTP chain does to a request or response."
metadata:
  invocation: model
  kind: method
---

# Rust Axum

The service is **one route tree**. Layer order and scope are observable
semantics, and `infra_http::harden` is their single owner.

The chain, outermost first: request-id sanitize → set → propagate → `nosniff`
→ OpenTelemetry span → metrics → access log → error mapping → load shed →
in-flight limit → request timeout → panic recovery → body limit → routes and
fallbacks. It is applied with `Router::layer`, so the 404 and 405 fallbacks
travel through it; `route_layer` would skip them. Inside a `ServiceBuilder`
the first layer is outermost; successive `Router::layer` calls nest the
later call outside the earlier one.

A new operation joins `infra_http::router` (or a feature router merged into
it) and its feature-owned handler; it never edits the chain. A cross-cutting
policy joins the chain only when every route needs it; otherwise wrap the
feature handler. Concurrency limits inside `Router::layer` must be
`GlobalConcurrencyLimitLayer`: the plain layer is applied per route. Keep
CORS fail-closed by omission; an empty `CorsLayer` answers every `OPTIONS`
with 200.

Failures are `infra_http::Problem` values from the closed `Code` catalog;
extractor rejections and handler errors map into it at the transport edge
with `request_id` from the request extensions. `MatchedPath` is absent in
`Router::fallback` and behind `nest_service`; label unmatched requests
explicitly, never with the raw path.

Streaming or large bodies stay under `http.max_body_bytes` and the request
timeout, because extractors run inside the handler future. A response body
that streams after the head is not bounded by the request timeout; add
`ResponseBodyTimeoutLayer` on that route when one appears.

For review, walk the tree from `Server` through `harden` to the handler and
name the position of every changed node. For implementation, test the mounted
router with `tower::ServiceExt::oneshot`, asserting status, `Content-Type`,
problem `code`, and headers; a status-only assertion proves neither order nor
fallback. Connection-level behavior (header timeout, `431`, connection cap,
drain) is proven against `infra_http::Server` on an ephemeral port.
