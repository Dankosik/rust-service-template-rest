---
name: rust-security
description: "Attacker paths. Use when Rust service identity, secrets, caller-controlled input, listener or header exposure, outbound destinations, or resource amplification change what an attacker can reach."
---

# Rust Security

**Attacker paths.** Walk each path from a trust boundary through the caller-controlled input and the enforcement point to an observable denial, and prove the denial. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Decide against the existing owners. The hardened chain bounds request bodies, sheds overload, caps handler time, sanitizes panics into an uninformative server error, sets the nosniff header, and accepts an inbound request id only within a strict grammar of at most one hundred twenty-eight safe characters. The bounded server limits header size and header time and caps connections, and closes a client that connects without sending. Cross-origin requests are fail-closed because no CORS layer exists. The config crate keeps secrets out of files, holds them as redacted secret strings, and refuses ambient OpenTelemetry credentials when a typed endpoint selects the collector, so one collector's token never reaches another. Unsafe code is forbidden workspace-wide.

Caller-controlled bytes in headers, paths, queries, and bodies are data until an extractor turns them into a typed value; log them only through bounded, allow-listed fields. A caller-provided correlation id is not identity. Missing identity, an ambiguous tenant, or an absent policy denies. A new listener, header, or diagnostic route is an exposure decision: the application listener serves the contract only, the diagnostics listener serves the metrics route only and relies on deployment network policy to stay private.

Secrets never appear in debug output, error text, logs, problem bodies, or test fixtures; a new secret field uses the secret string type and inherits the file pre-scan. An outbound destination is pinned by configuration and never derived from request input. Work spent per request is bounded before it is spent, so decode limits, the connection cap, and the in-flight limit stay in place; an optimization that removes one is a regression.

Every accepted control needs focused negative proof, because an allow test also passes against a bypass. The existing tests reject an oversized header, an oversized body, a silent client, an over-cap connection, a malformed request id, an ambient credential, and a secret in a file; a new control extends that family in the same change.

For review, follow each path into a finding that names the enforcement point and the missing denial proof, without editing; no findings still requires the deny test. For implementation, add the control at the earliest boundary that observes the input and land its negative test together with it. Do not add authentication, authorization, or rate limiting scaffolding ahead of the profile that owns it.
