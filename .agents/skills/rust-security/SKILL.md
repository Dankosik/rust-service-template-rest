---
name: rust-security
description: "Attacker paths. Use when identity, secrets, untrusted input, exposure, outbound destinations, or amplification changes what a caller can reach or make a Rust service spend."
---

# Rust Security

Trace a concrete attacker-controlled input across a trust boundary to a
protected resource or effect. Identify the authority needed for that effect,
the enforcement point and a denial an attacker cannot bypass. A stricter
parser or extra wrapper is useful only when it closes that path or implements
an accepted protocol requirement.

## Enforce the property before spending it

A correlation identifier helps observation; it does not authenticate a caller.
A verified identity does not by itself authorize a tenant or object operation.
When the operation requires identity or policy, missing or ambiguous evidence
must deny at the owner that can decide. Keep that decision before the protected
effect, and check alternate entry paths rather than trusting one HTTP handler.
An explicitly public operation remains public; uncertainty is not authority to
invent a new authentication subsystem.

Bound amplification before allocating, decoding, queueing or dispatching the
expensive work. A byte cap does not necessarily bound decoded expansion, and a
handler count does not bound detached or retained work. Choose limits for the
actual resource and lifetime; moving admission after allocation leaves the
resource exposed. Keep current controls when optimizing their mechanism.

Destination policy depends on who controls the destination and credentials.
A fixed configured endpoint narrows the attack surface; a feature with accepted
dynamic destinations needs explicit admission across resolution, redirects and
credential forwarding. Do not treat a parsed URL as authorization to reach a
host, or turn every service into a fixed-destination design by default. Follow
the current provider's accepted restrictions before choosing a mechanism.

## Make disclosure and denial observable

Trace secrets through configuration, Debug/Display, errors, telemetry and test
artifacts. A redacted wrapper protects only paths that use its redacted view;
formatting exposed bytes bypasses it. Diagnostics should identify the rejected
rule without reproducing the untrusted or secret value. For a listener or
route, include who can reach it and which deployment boundary supplies privacy.

Negative evidence must distinguish enforcement from bypass: a success case
alone cannot establish that a missing identity or oversized input is denied.
Reuse existing adequate denial coverage; a read-only review does not mandate
writing or running a new test merely because it found no defect.

For exposure, request admission and HTTP failure policy, load [HTTP
Architecture](../../../docs/architecture/http.md). For secret sources, outbound
configuration and telemetry disclosure, load [Configuration Source
Policy](../../../docs/configuration-source-policy.md) and its affected provider
owner. [AGENTS](../../../AGENTS.md#authority) retains effect authority and scope;
this method supplies neither access permission nor permission to expand controls.
