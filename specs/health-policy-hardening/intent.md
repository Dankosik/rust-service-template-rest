# Intent: harden the template's health policy

Status: ready

## Problem

The reviewed health-policy research found a stale-readiness recovery defect,
incomplete operator visibility of freshness, an unbounded PostgreSQL startup
session readback, and misleading timing documentation. The requester asked to
implement the recommendations that are genuinely necessary in one separate PR.

## Desired outcome

Make the template's existing health policy truthful during stalled refresh,
failed recovery and startup network silence, with matching documentation and
focused regression proof. Keep the policy portable to derived services.

## Affected actors and systems

Operators observing readiness and process startup; HTTP and gRPC health
consumers; the health owner, PostgreSQL admission and service lifecycle;
maintainers adopting the template and its deployment guidance.

## Scope and non-goals

One PR containing necessary local code, tests and documentation fixes. Preserve
readiness cadence, probe budget, failure threshold, dependency criticality,
probe routing and infrastructure. No watchdog, new degradation policy,
readiness framework, fleet tuning, merge, deployment or infrastructure changes.
The original dirty checkout is outside the writable scope.

## Constraints

Use the isolated `codex/health-policy-hardening-20261005` checkout based on
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`. Reuse existing owners and dependencies.
The requester authorized scoped implementation, validation, commit, push and
one PR; production reads and external operational changes are not part of this
outcome. Technical decisions belong to the agent within that boundary.

## Success signal

A reviewed PR fixes the concrete defects, preserves deliberate policy choices,
and states the proof actually obtained. An expired Ready result cannot be
revived by a failed round; operators can recognize stale evaluation; a silent
session check cannot indefinitely hold startup; timing guidance describes the
actual scheduling policy. Platform or fleet qualification is a separate claim.
