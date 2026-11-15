---
name: rust-security
description: "Attacker paths. Use when identity, secrets, untrusted input, listener/header exposure, egress destinations, or resource amplification affect security."
---

# Rust Security

**Attacker paths.** Trace caller-controlled input across every trust boundary to its enforcement point and observable effect, naming the protected invariant, the capability an attacker gains on bypass, and the negative test that proves enforcement.

**Security owners.** Extend the established owners in `crates/infra-http`, `crates/service/src/bootstrap`, and `crates/config` for listener binding, request admission, authentication, egress, and secret policy, preserving their vetted defaults.

**Trust boundaries and fail-closed auth.** Validate paths, headers, queries, bodies, and correlation IDs at their owning boundary before trusting them downstream. Fail closed whenever required identity, tenant, or authorization evidence is missing or ambiguous while keeping explicitly public routes open, and add input restrictions only for concrete threats or accepted contracts.

**Control semantics.** Verify the exact guarantee each mechanism provides: server-side auth enforces access (whereas CORS governs only browser response sharing and preflight), correlation-ID sanitizers replace invalid headers per contract, and sanitization, rejection, redaction, and rate/size bounds each require their own negative assertion beyond happy-path tests.

**Secrets and egress.** Redact secrets across error chains, `Debug`, logs, metrics, and validation rejections, pairing wrappers with allow-listed diagnostic fields. Keep outbound requests on configured provider endpoints unless an explicit egress policy governs credentials, redirects, DNS resolution, and private-network reachability for dynamic URLs.

**Amplification and proof.** Bound decoded allocations, queue depth, concurrency, response retention, and post-cancellation work before spending resources, preserving admission and redaction across refactors and reporting any unexercised security proof explicitly.
