# Intent: optional bounded outbound HTTP profile

## Problem

The Rust template has an authentication-private HTTP transport, but no reusable
client for a service that calls one fixed public HTTPS dependency. Copying the
auth transport would duplicate connection admission and carry token-verification
policy into unrelated integrations. An ordinary `reqwest` client would leave
destination, response size, correlation propagation, and ambient proxy behavior
to each adopter.

## Desired outcome

Deliver roadmap stage 10.2 locally as an optional, default-off profile. A
selected derived service gets a reusable fixed-authority `reqwest` client whose
actual connection destinations pass post-DNS public-address admission and whose
headers, bodies, lifetime, and correlation behavior are bounded. Complete the
applicable repository phases, independent reviews, implementation, and local
acceptance. Stop before PR, push, merge, or deployment.

## Affected actors and systems

Template maintainers, derived-service developers and initializer users; the
outbound client and its provider adapters; the existing inbound-auth provider
transport; the profile projection and validation runners.

## Scope and non-goals

Include one public HTTPS target class, limits, no ambient proxy, removal of
outbound correlation data, documentation, tests, profile markers, and initializer
selection. Preserve the existing JWT and introspection guarantees, failure
taxonomy, deadlines, and cancellation. Do not add a concrete provider,
private-network target, dynamic URL/SSRF policy, webhook, OAuth outbound auth,
retry framework, streaming API, or any other stage-10 capability.

## Constraints

Re-derive the Go template's `internal/infra/httpclient` decisions using current
Rust mechanisms and the already installed dependency graph. Keep provider
authentication, parsing, and business error mapping with the provider adapter.
Preserve the initializer's complete preflight before target mutation. Factor
canonical profile/harness transformations from Rust build/test proof by unique
runtime graph as specified in `docs/template-sync.md#validation-boundary`.

## Success signal

The selected client refuses a different authority and any non-public resolved
connection address; limits and correlation/proxy policy hold on negative and
positive cases. Unselected output contains no outbound profile runtime pack;
selected output is complete. Local review and matching validation establish
the fixed candidate, with no remote-delivery claim.
