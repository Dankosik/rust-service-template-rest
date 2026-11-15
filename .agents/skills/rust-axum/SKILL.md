---
name: rust-axum
description: "Route tree. Use when changing Axum routes, extractors, layers, fallbacks, or the hardened HTTP chain."
---

# Rust Axum

**Route tree.** Trace an affected request from listener admission through routing, layers, extraction, and handler execution to full response-body completion, identifying which owner handles each rejection and which guarantees survive body handoff.

**Layer composition.** Read the live router composition and Axum's layer/fallback ordering semantics before moving middleware: `crates/infra-http` owns the hardened HTTP chain and `crates/service/src/api.rs` mounts features onto it. Keep request identity, shared admission, timeouts, body limits, telemetry, and Problem responses coherent across matched routes, `404`, and `405` paths.

**Extractors and limits.** Verify invalid-input status, media type, Problem body, and headers before swapping custom extractors (`infra_http::extract`) with native ones. Place the single body-consuming extractor last, and enforce header, raw-body, and decoded-value limits at their distinct boundaries.

**Cross-cutting scope.** Mount shared concurrency or admission limits once at the application scope they govern rather than duplicating independent per-route pools. Enforce authentication and authorization on the server; add CORS only when an accepted browser-sharing policy requires it, preserving method routing and fallbacks.

**Response body lifetime.** Account for memory, upstream streams, and concurrency permits retained after handler return: acquire admission before allocating or fetching, transfer custody into the response body wrapper in `crates/infra-http`, and enforce wall-clock reclamation even if a slow client stops polling frames. Treat post-header stream errors as stream terminations rather than status rewrites.

**Telemetry and testing.** Record low-cardinality route templates and the unmatched-path sentinel in telemetry, keep transport policy in `crates/infra-http` and `crates/service-failure`, and choose mounted-router or listener tests according to whether middleware or socket behavior is under test.
