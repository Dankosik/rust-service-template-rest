---
name: rust-axum
description: "Route tree. Use when Rust service routes, extractors, layers, fallbacks, or the hardened HTTP chain need implementation or review."
---

# Rust Axum

**Route tree.** The service is one router, and layer order and scope are observable semantics. Follow the affected request from the listener through the hardened chain to the handler and back to the client-visible result. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

In this service, read the [HTTP request path](../../../docs/architecture/http.md#request-path) and its harden owner before changing the chain. Preserve the guarantees across its actual layers: request identity and problem completion, observation of rejected requests, shared in-flight admission that spends the request budget, and the intended timeout and body-limit scope. The router's layer method includes not-found and method-not-allowed fallbacks; route-layer would skip them. Inside a service builder the first layer is outermost, while successive router layer calls nest each later call outside the earlier one. Follow the current composition instead of recreating a remembered layer list.

A new operation joins the router, or a feature router merged into it, with a feature-owned handler; it never edits the chain. A cross-cutting policy joins the chain only when every route needs it; otherwise wrap the feature handler. A concurrency limit inside the router must share one semaphore, because a plain limit layer is applied once per route. Omit CORS until browser cross-origin response access is required; omission does not block simple cross-origin requests or replace server-side authorization. An empty CORS layer can hide the router's method-not-allowed answer for OPTIONS.

Failures here are problem values from the closed code catalog, rendered as problem JSON with the request id from the request extensions; handler errors map into that catalog at the transport edge, and request data enters through the transport crate's extract wrappers, whose rejections already are problems. Before replacing a wrapper with a native extractor, trace an invalid input: rejection can occur before the handler, and its default status, media type, body, or headers may differ from the contract. Retain the required mapping at its existing owner; remove it when the default already meets that contract. The matched path is absent in the fallback and behind a nested service, so unmatched requests carry an explicit label, never the raw path. Bodies are read inside the handler future and therefore inside the request timeout; a response that streams after its head is not, and needs a response-body timeout on that route when one appears.

Connection-level behavior lives in the bounded server, not the router: the header timeout that doubles as the keep-alive idle bound, the header-size limit, the connection cap, and the graceful drain.

For review, walk the tree from the server through the chain to the handler and name the position of every changed node without editing. For implementation, test the mounted router with a one-shot call, asserting status, content type, problem code, and headers; a status-only assertion proves neither order nor fallback. Prove connection-level claims against the server on an ephemeral port, not for every route change.
