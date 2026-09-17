---
name: rust-security
description: "Use when identity, secrets, caller-controlled input, exposure of a listener or header, outbound destinations, or resource amplification change what an attacker can reach."
metadata:
  invocation: model
  kind: method
---

# Rust Security

Security work walks **attacker paths** from a trust boundary through the
enforcement point to an observable denial, and proves the denial.

`boundary -> caller-controlled input -> enforcement -> denial -> negative proof`

Decide against the existing owners. `infra_http::harden` bounds bodies (413),
sheds overload (503), caps handler time (504), sanitizes panics (500), sets
`nosniff`, and accepts an inbound `X-Request-ID` only in
`^[A-Za-z0-9._~-]{1,128}$`; `infra_http::Server` bounds headers (431),
header time, and connections; CORS is fail-closed by having no layer.
`crates/config` keeps secrets out of files, holds them as `SecretString`,
and refuses ambient OpenTelemetry credentials under a typed endpoint so one
collector's token never reaches another. `unsafe_code` is forbidden.

Caller-controlled bytes (headers, paths, bodies, query) are data until an
extractor turns them into a typed value; log them only through bounded,
allow-listed fields. A caller-provided correlation id is not identity.
Missing identity, an ambiguous tenant, or an absent policy denies. A new
listener, header, or diagnostic route is an exposure decision: the
application listener serves the contract only, the diagnostics listener
serves `/metrics` only and depends on deployment network policy.

Secrets never appear in `Debug` output, error text, logs, problem bodies, or
test fixtures; the `SecretString` wrapper and the file pre-scan are the
mechanism, and a new secret field must use them. An outbound destination is
pinned by configuration, never derived from request input. Work per request
is bounded before it is spent: decode limits, connection caps, and the
in-flight limit exist so a client cannot buy unbounded CPU or memory.

Every accepted control needs focused negative proof: an allow test also
passes against a bypass. The existing tests reject an oversized header, an
oversized body, a silent client, an over-cap connection, a malformed request
id, an ambient credential, and a secret in a file; extend that family for a
new control.

For review, follow each path into a finding with the enforcement point and
the missing denial proof; no findings still requires the deny test. For
implementation, add the control at the earliest boundary that observes the
input and land its negative test in the same change.
