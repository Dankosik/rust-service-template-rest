---
name: rust-security
description: "Attacker paths. Use when identity, secrets, untrusted input, listener/header exposure, egress destinations, or resource amplification affect security."
---

# Rust Security

**Attacker paths.** Follow caller-controlled input across its trust boundary to the enforcement point and observable consequence. Identify the protected property, the capability an attacker gains if it fails, and evidence that distinguishes the intended control from a bypass.

Use the current project's security and architecture owners. `crates/infra-http`, the composition root in `crates/service/src/bootstrap`, and `crates/config` own listener exposure, request admission, identity, egress, and secret sources. Preserve their accepted defaults instead of copying remembered limits or adding new authentication and rate-limiting machinery.

Treat paths, headers, queries, bodies, and correlation identifiers as untrusted data until the owning boundary establishes the property needed by the consumer. A request id is not identity. Where an operation requires identity, tenant, or authorization, missing or ambiguous evidence must not grant access; explicitly public operations remain public. Distinguish syntactic parsing from authorization and from resource limits. Do not add stricter input rejection without a concrete threat or accepted contract that justifies its compatibility cost.

State what each control actually does. CORS governs browser response sharing and preflight, not server-side authorization; omitting it does not deny simple cross-origin requests. An invalid correlation identifier may be replaced rather than cause request denial, according to the owner's contract. Sanitization, rejection, withholding information, and bounded resource use are different outcomes and require different observations. A middleware's presence or an allow-path test does not prove the intended property.

Keep secrets out of all observable representations, including error causes, `Debug`, logs, telemetry, and rejected-input diagnostics. A secret wrapper covers only its defined behavior; inspect conversions, field admission, and third-party formatting, and use bounded allow-listed diagnostic fields where policy requires them. Fixed provider destinations come from their accepted configuration owner; dynamic or caller-controlled destinations require a separate decision about credentials, redirects, resolution, and reachable networks rather than implicit permission from an HTTP client.

Bound attacker-amplified work before spending it. Account for decoded allocation, queued payloads, concurrency, response retention, and work that survives cancellation, not only input bytes. Preserve the existing enforcement scope when changing an extractor, adapter, or execution owner. A bound that depends on polling or a cooperative runtime must expose that assumption. Do not turn an optimization or refactor into a silent removal of admission, redaction, or failure semantics.

Reuse or extend focused negative coverage that would fail for the relevant bypass, including sanitization or bounded work when that is the promise. A missing proof is a stated gap, not permission to invent a test environment for a read-only check.
