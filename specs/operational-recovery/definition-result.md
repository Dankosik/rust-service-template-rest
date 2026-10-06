# Definition result: operational recovery

```text
status: ready
owner: Definition
result: intent.md; spec.md; research/baseline.md
review: Review Result V1 below, PASS
movement_evidence: all five accepted recommendations have grounded dispositions; behavior and proof boundaries are fixed; fresh independent review found no material divergence
reopen_owner: none
next_owner: Technical Design
```

## Authoritative candidate

Worktree: `/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-operational-recovery-20261006`.
Branch: `codex/operational-recovery-20261006`.
Source/instruction baseline: `699887b18594088a59bcc23a049d290d089f6da1`.
No production or test file changed during Definition; artifacts are uncommitted.

| Artifact | Current SHA-256 |
| --- | --- |
| [Intent](intent.md) | `4072afd2863e3138405eb669ff5d48374f052f92d6485fd945e83f818f9d6428` |
| [Specification](spec.md) | `ced137a1a4d2b29311cbba94b02790d30077803eb6f9dbe9945ed25218c12ab7` |
| [Supporting baseline](research/baseline.md) | `178e2e3a45cee7fffd367a90033ecd0d625b07522fc72358719ab3d09909a638` |

The reviewer fixed the specification at
`5f82dc2a7620dd67c499cd3fc79860e388442dc6acb79632064ddf0a5a2753c8`.
The only subsequent specification edit changed its status from draft/pending
review to ready/PASS. The semantic boundary and all reviewed rules are unchanged;
this mechanical identity refresh retains the verdict under Transition.

## Review Result V1

```text
candidate: spec.md SHA-256 5f82dc2a7620dd67c499cd3fc79860e388442dc6acb79632064ddf0a5a2753c8 with intent/research hashes above
verdict: PASS
findings: none
evidence_boundary: fresh independent read-only Specification Review of R1-R4, current source owners, source-supported recovery/lock gaps, official platform and command-scoped compiler-cache contracts; no build, test or runtime experiment
reopen_owner: none
```

Reviewer: `/root/operational_definition/spec_review_complete`, fresh history,
native reviewer-agent dispatch with `gpt-6-astra` / `xhigh` accepted and observed
running/completed. The native status surface reports lifecycle, not a separate
effective-model field. Method: [Specification Review](../../docs/spec-first-workflow/phases/specification-review.md)
through [shared Review](../../docs/spec-first-workflow/shared/review.md).

Attempted falsifiers found no surviving issue: failed dependency rounds becoming
restart; late completion/stop concealing a primary gap; already-complete
supervision making R1 unnecessary; probe-only recovery or a fleet-wide claim;
unsupported lock/cancellation scope; mandatory global cache changes or false
cache success; arbitrary disk reserve or destructive cleanup. The reviewer
checked relevant health, process, lock and verifier source, and official
[Cargo](https://doc.rust-lang.org/cargo/reference/environment-variables.html),
[sccache](https://github.com/mozilla/sccache#usage),
[Railway](https://docs.railway.com/deployments/healthchecks) and
[Kubernetes](https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-startup-probes/)
contracts. Existing CI receipts were not treated as proof of this candidate.

An earlier reviewer passed the narrower pre-clarification candidate. Goal 5 was
then reopened from the root's accepted-intent clarification and current script
evidence; a fresh reviewer covered the expanded boundary. Only the result above
governs movement.

## Decisions and next work

1. Retain #254's current background/manager custody, logging and provider bounds.
   PR #243 health bytes are already adopted; its pool work is superseded by the
   current stronger lifecycle. Remaining timing/topology guidance must be
   rehomed without reviving obsolete shutdown or startup arithmetic.
2. Implement process failure when the armed readiness driver's monotonic
   completion gap exceeds its existing freshness bound, in both roots. Ordinary
   completed dependency failures remain alive and recover in place; late success
   cannot erase a terminal core-progress loss. No general pending-task watchdog.
3. Close actual HTTP/gRPC/Watch/no-diagnostics and bounded two-instance useful-work
   recovery evidence. Preserve defaults and existing operation uncertainty;
   neither local observations nor green probes establish production fleet health.
4. Improve the existing validation lock's safe ownership and waiting feedback,
   opt-in command-scoped compiler caching and resource/receipt diagnostics. Keep
   FIFO unchanged without starvation evidence, protect secrets, retain separate
   worktree targets and preserve caches. Current machine cache absence is explicit.

Technical Design owns mechanism/placement, task-local cache provisioning and
safe supported configuration, and whether delivery updates #243 or uses a new
PR with clear supersession. Implementation chooses concrete proving cases and
commands under the current Validation/Build Speed owners. No test plan is an
upstream input. Continue to Planning and Implementation after reviewed Design;
the root retains the request through reviewable PR delivery and required CI.

## Evidence and authority boundary

Definition used source/diff inspection and static checking of these artifacts'
relative links/fragments and trailing whitespace (PASS). It ran no Rust build,
test, CPU quota workload, new environment, commit, push or remote write. The
repository's complete docs-check remains with assembled delivery validation;
the static check here is not claimed as that command. Root-supplied existing
CI results remain scope-qualified in the baseline artifact.

Authority permits scoped implementation, non-destructive proof, commit/push and
PR delivery. It excludes remote main merge, deployment/production mutation,
purchases, unrelated runner interruption, shared cache deletion and global
machine security/configuration changes. No user-owned ambiguity remains.
Reopen the smallest owner if baseline/profile/authority changes, if a proposed
mechanism cannot meet this behavior within existing ownership/budgets, or if
evidence introduces a new observable policy fork. A missing implementation case
or command does not reopen Definition.
