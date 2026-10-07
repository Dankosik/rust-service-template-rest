# Intent: one budget for one operation

## Problem

The template has several sound local timeouts, but business code must manually
compose them. A dependency can begin a new local timer after earlier work has
spent part of the request budget. A response body can also outlive the timer
that obtained its headers and keep scarce capacity indefinitely.

## Desired outcome

Business code can consume and pass a clear operation budget and cancellation
context across HTTP, gRPC, and messaging. Existing provider adapters respect
the parent's remaining time. Credential acquisition and its protected gRPC call
form one timed operation. Object downloads have bounded collection and owned,
finite resource lifetime. Retry policy remains explicit at its existing owner
and never treats an ambiguous external outcome as permission to replay.

## Affected actors and systems

Feature authors, inbound HTTP/gRPC callers, durable message handlers, inbound
authentication, outbound HTTP/gRPC and OAuth credentials, cache, object storage,
and the existing transaction/idempotency and retry owners.

## Scope and non-goals

Produce one independently based operation-budgets pull request containing only
the necessary accepted fixes. Preserve existing defaults and protocol/finality
contracts. No universal production SLOs, new circuit breaker, new generic retry
framework, fleet jitter tuning, gateway policy, retention policy, infrastructure,
merge, or deployment. Open reference PRs are evidence, not the merged baseline.

## Constraints

The requester authorized the recommendations the agent judges necessary to be
fixed in a separate PR after the research-only report. This authorizes scoped
implementation, local validation, commit, push, and PR creation. It does not
authorize merging, deployment, or infrastructure changes.

Preserve immutable effect identities, `CommitUnknown`/`OutcomeUnknown`, existing
retry owners, cancellation-safe cleanup, and independent shared refresh owners.
No blind replay. Feature code must not acquire provider dependencies merely to
carry a budget. Existing durations are baseline limits, not newly asserted
production adequacy.

## Success signal

A feature author can propagate one operation cutoff through supported entry
points and dependencies without rebuilding timeout arithmetic. Slow auth,
credentials, queueing, preparation, or body consumption cannot restart that
cutoff. Expiry has a truthful failure outcome and releases owned resources;
an unused object stream cannot retain its storage admission slot forever.

Assumption: this task preserves the existing distinction between bounded
request opening and explicitly owned streaming lifetime; it does not impose a
new global maximum on every HTTP or gRPC response stream. Reopen Intake if the
requester instead wants such a global streaming policy.
