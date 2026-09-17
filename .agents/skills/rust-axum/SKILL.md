---
name: rust-axum
description: "Route tree. Use when Rust service routes, extractors, layers, fallbacks, or the hardened HTTP chain need implementation or review."
---

# Rust Axum

**Route tree.** The service is one router, and layer order and scope are observable semantics. Follow the affected request from the listener through the hardened chain to the handler and back to the client-visible result. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

The hardened chain has one owner and one order, outermost first: request-id sanitize, set, and propagate; the nosniff header; the OpenTelemetry server span; HTTP metrics; the access log; error mapping; load shedding; the in-flight limit; the request timeout; panic recovery; the body limit; then routes and fallbacks. It is applied with the router's layer method so the not-found and method-not-allowed fallbacks pass through it; the route-layer method would skip them. Inside a service builder the first layer is outermost, while successive router layer calls nest each later call outside the earlier one.

A new operation joins the router, or a feature router merged into it, with a feature-owned handler; it never edits the chain. A cross-cutting policy joins the chain only when every route needs it; otherwise wrap the feature handler. A concurrency limit inside the router must be the global variant, because the plain layer is applied once per route. Keep cross-origin requests fail-closed by omitting a CORS layer entirely; an empty one answers every OPTIONS request with success and hides the router's method-not-allowed answer.

Failures are problem values from the closed code catalog, rendered as problem JSON with the request id from the request extensions; extractor rejections and handler errors map into that catalog at the transport edge. The matched path is absent in the fallback and behind a nested service, so unmatched requests carry an explicit label, never the raw path. Bodies are read inside the handler future and therefore inside the request timeout; a response that streams after its head is not, and needs a response-body timeout on that route when one appears.

Connection-level behavior lives in the bounded server, not the router: the header timeout that doubles as the keep-alive idle bound, the header-size limit, the connection cap, and the graceful drain.

For review, walk the tree from the server through the chain to the handler and name the position of every changed node without editing. For implementation, test the mounted router with a one-shot call, asserting status, content type, problem code, and headers; a status-only assertion proves neither order nor fallback. Prove connection-level claims against the server on an ephemeral port, not for every route change.
