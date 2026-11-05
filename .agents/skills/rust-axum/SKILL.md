---
name: rust-axum
description: "Route tree. Use when Rust service routes, extractors, layers, fallbacks, or the hardened HTTP chain need implementation or review."
---

# Rust Axum

**Route tree.** Trace an affected request from listener admission through routing, layers, extraction, and the handler to the response's full lifetime. Establish which owner sees each rejection and which guarantees survive the handoff to the body. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Read the actual composition and the resolved framework contract before changing layer order or scope. Router layers, route layers, nested routers, and service builders do not have interchangeable fallback or ordering behavior. In this service, [HTTP architecture](../../../docs/architecture/http.md) owns the hardened chain and feature placement; extend that owner rather than reconstructing a remembered layer list. Keep request identity, admission, timeout, body limits, observation, and Problem completion coherent across served routes, including not-found and method-not-allowed responses where the policy requires them.

An extractor can fail before a handler runs. Inspect an invalid input before replacing a wrapper with a native extractor: status, media type, body, or headers may change even if the successful value is identical. Keep a required mapping at its existing boundary, or remove it when the native behavior already meets the contract. Order and ownership of body-consuming extractors matter; a reusable helper does not permit consuming the request twice. Distinguish header, raw-body, and decoded-value bounds rather than assuming one limit covers them all.

Apply cross-cutting policy only at the scope that owns it. A per-route concurrency layer may create independent capacities where the application requires one shared admission bound. Authentication and authorization remain server decisions. CORS controls browser response sharing and preflight; its omission does not deny simple cross-origin requests. This service omits CORS until its accepted browser-access policy changes. Preserve method and fallback behavior when adding it, rather than introducing an empty layer for appearance.

Handler completion is not necessarily response completion. Large buffered and streaming bodies can retain memory, provider state, and permits after the handler timeout ends. Follow the [response resource-lifetime owner](../../../docs/architecture/http.md#request-and-response-resource-lifetime): acquire admission before allocating or fetching, transfer custody through the response handoff, and select a finite lifetime that can reclaim owned resources even when the consumer stops polling. A timer checked only in poll_frame cannot prove that. Once headers are committed, a body failure terminates the stream rather than replacing the response status; transport-held frames and caller-owned copies remain separate owners.

Use route templates and the existing unmatched-path policy for observation rather than unbounded raw paths. Choose proof at the layer that can observe the changed promise: an in-process router can prove routing and response shape, while connection admission, slow clients, and transport progress require the available bounded server or process harness. In this service, preserve the code-first OpenAPI owner and the failure catalog's projection; do not create transport policy in feature code.

For review, explain the affected route, layer, extraction, or lifetime consequence without editing. For implementation, use the project's selected checks and adequate existing cases, adding coverage only for material gaps in the changed boundary. Do not infer listener behavior from a handler-only test, or turn this method into a mandatory full-server test suite for a local route change.
