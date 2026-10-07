# Intent: preserve time limits and authentication validity

Status: ready

## Problem

The completed time-contract research identified places where a conversion or
cached result can weaken an existing deadline, authentication lifetime, or
duration bound. Some recommendations describe behavior already correct on
current main and do not justify a change.

## Desired outcome

Implement the genuinely necessary corrections in a separate pull request,
preserving existing durations, leeway, and wire identities. This first scope
closes the independent arithmetic and authentication defects. Webhook receipt
retention remains a separate dependent decision for the continuation owner to
reconcile before final PR scope is fixed.

## Affected actors and systems

Callers of bounded outbound HTTP, protected HTTP and gRPC operations using
bearer authentication, clients consuming HTTP Retry-After, and features writing
expiring Redis cache entries.

## Scope and non-goals

Correct deadline preservation, calendar validity on introspection reuse,
authentication with unavailable calendar time, HTTP Retry-After rounding, and
unrepresentable cache TTL input. Preserve already-correct OAuth lifetime
overflow refusal. Do not alter JWT temporal compatibility, webhook signatures
or request fingerprints, messaging timestamp identities, configured skew,
retention defaults, runtime budgets, CI, toolchain, or infrastructure. No global
clock framework or new dependency is needed by this outcome.

## Constraints

Use current main `5927ffbba351af2f7fb8635316bbfa4ae5b31da6` as the source
baseline. Preserve unrelated dirty-checkout work; this task lives in its own
branch and worktree. Local implementation, relevant validation, and a separate
PR are authorized; deployment and merge are not. Each phase retains its
repository-defined ownership and review boundary.

## Success signal

Existing limits remain effective through suspension, clock changes, and numeric
boundaries; ordinary valid traffic retains its contract. Focused evidence
distinguishes repaired behavior from the original defect, while adequate
existing compatibility proof is reused.

## Open questions

The user has not selected the webhook duplicate-protection horizon versus the
outbound delivery/replay horizon. No value is inferred. That decision cannot
change this independent core contract; any later retention change requires its
own Definition delta and applicable review before implementation.
