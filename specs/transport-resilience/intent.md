# Intent: resilient outbound clients

Status: ready

## Problem

The outbound transport audit identified concrete gaps in connection admission,
recovery after a stalled connection attempt, and the documentation of what
timeouts and connection reuse actually guarantee. Some apparent gaps already
have adequate library behavior or project recovery and must not become redundant
custom infrastructure.

## Desired outcome

Correct the recommendations from that audit which are genuinely necessary for
this project, with appropriate tests and documentation, in one separate pull
request. Existing client components should have production-appropriate defaults
and require only provider-specific configuration that changes real behavior.

## Affected actors and systems

Service and jobs-worker callers of PostgreSQL, gRPC, inbound authentication
providers, NATS, outbound HTTP, Redis/Valkey and S3-compatible storage; operators
configuring destinations, trust and recovery; adopters of retained profiles.

## Scope and non-goals

The selected audit fixes, their regression coverage and operational guidance
belong in this PR. Technical choices and necessity assessments belong to the
agent. The request authorizes local implementation and validation and publication
of the separate PR. It does not request deployment or merge. No new application
feature, universal transport, provider migration or infrastructure project is
implied.

## Constraints

Reuse existing libraries and their supported settings first. Preserve TLS
verification, credential custody, existing optional-profile behavior, operation
finality and the project's validation budget. Avoid provider settings or custom
resolvers that lack a demonstrated need. Preserve unrelated checkout changes.

## Success signal

The separate PR corrects the accepted defects, accounts for every material audit
recommendation, contains appropriate evidence at its actual scope, and clearly
states material limitations. Publishing a PR is separate from proving live
provider behavior; no deployment or live-runtime claim is implied.
