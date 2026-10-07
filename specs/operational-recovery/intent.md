# Intent: operational recovery through useful work

Status: ready. Definition baseline: `699887b18594088a59bcc23a049d290d089f6da1`.

## Problem

The health research and PR #243 exposed operational risks wider than isolated
probe fixes. The requester accepted the recommendation to deliver a coherent
improvement across lifecycle integration, required background work, overload and
dependency recovery, real consumers, and developer verification. Subsequent
merges already implement a substantial part of that recommendation; the follow-up
must build on the current code rather than replay the old branches.

## Desired outcome

1. Integrate the accepted logging, lifecycle and reliability work into one
   consistent result with an explicit disposition of PR #243's remaining scope.
2. Give required background work a truthful failure and recovery policy for
   unexpected completion, panic and loss of expected progress.
3. Distinguish local admission/pool pressure from dependency outage, including
   the interaction of multiple instances, and establish useful-work recovery.
4. Establish the recovery behavior at HTTP and gRPC consumers, including health
   Watch and deployments with diagnostics disabled.
5. Reduce avoidable cold builds and resource waste, make verification waiting
   understandable and cancellable, and improve diagnosis of disk exhaustion
   using the repository's current build, cache and validation owners.

## Affected actors and systems

API callers, gRPC health consumers, operators and derived-service maintainers;
the service and retained jobs-worker composition roots; readiness, listeners,
registered background managers and required dependency adapters; local developers
and the existing PR/CI delivery path.

## Scope and non-goals

The overall request authorizes scoped local code, tests, documentation and script
changes, non-destructive validation, commits, push and reviewable PR delivery
with required CI. This actor owns Definition only. Production deployment, merge
to remote `main`, infrastructure changes, purchases, interruptions of unrelated
runners, global cache deletion and machine security changes are excluded.

The current runtime architecture and retained optional profiles are the baseline.
Concurrent open PRs are separate mutable work. Workload-specific production SLOs,
fleet capacity and platform restart configuration belong to the derived service;
their absence does not defer template-owned safe failure/recovery mechanisms.

## Constraints

Preserve existing dependency/session truth, bounded admission, completion custody,
drain precedence, resource cleanup and sanitized failures. Reuse adequate owners
and proof. Do not introduce a second health framework, duplicate supervisors or
speculative tuning. Keep local, reused CI and real deployment evidence distinct.
Respect worktree-owned build output and the existing validation lock.

## Success signal

A reviewable follow-up PR supplies the missing behavior and consumer evidence,
retains previously accepted behavior at its supported boundary, disposes every
recommendation against current evidence, and has the required CI result for its
actual candidate. A green probe without restored useful work is insufficient.
Definition finishes when the behavior is fixed and independently reviewed so
Technical Design can proceed without inventing requester meaning.
