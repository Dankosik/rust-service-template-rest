# Intent: contract-derived, library-backed authentication

Status: ready

## Problem

The Stage 10.1 review identifies an authentication composition gap and excessive
custom parsing, transport, and refresh machinery. Per-route opt-in can diverge
from OpenAPI, provider diagnostics obscure actionable failures, and legitimate
provider key sets/private HTTPS deployments are unnecessarily rejected.

## Desired outcome

Implement the review's recommendations as one coherent authentication repair,
verify disputed recommendations against current code and primary sources, and
open a separate pull request. Authentication must follow the assembled API
contract, use existing libraries for generic mechanisms, expose verified scope
and expiry, and preserve bounded work, trustworthy identities and safe errors.
Record a development policy against unsupported extra strictness while retaining
normative protocol requirements and necessary application constraints.

## Affected actors and systems

API callers, feature authors, service operators, configured OIDC providers, the
authentication/configuration/HTTP/bootstrap boundaries, documentation, and
initializer outputs. The existing idempotency profile consumes verified identity
and must remain correctly ordered behind authentication.

## Scope and non-goals

Includes contract enforcement, JWT/JWKS/discovery/introspection, configuration,
provider transport, refresh lifecycle, diagnostics, and affected template/docs
projections. Account for every review recommendation. Optional additions may be
dispositioned with reasons. Retain jsonwebtoken plus the small owned refresh
cache; an all-in-one crate experiment is separate work. Independent Stage 10.2
outbound HTTP policy is assessed but not silently changed. No product routes,
new identity provider, provider credentials, merge, deployment, or live-provider
certification is requested.

## Constraints

The user authorizes local edits/validation, commits, push and a separate PR.
The current base is `4edd184` on `codex/authn-contract-and-libraries`. Preserve
unrelated work. Prefer existing libraries and adequate existing tests; factor
projection checks from runtime validation rather than multiply full builds by
harness and database choices. Protect secrets; configuration controls the IdP
destination and tokens never choose it.

## Success signal

The reviewed implementation meets the behavior in [spec.md](spec.md), all
recommendations have an explicit disposition, affected documentation and
initializer profiles agree, applicable local checks and exact-head CI evidence
support a separate PR, and the handoff distinguishes local completion from CI
and any unrequested deployment.
