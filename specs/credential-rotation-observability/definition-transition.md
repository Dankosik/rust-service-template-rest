# Definition transition

```text
status: ready
owner: Definition
result: specs/credential-rotation-observability/spec.md
review: specs/credential-rotation-observability/definition-review.md — PASS
movement_evidence: initial Definition review plus fresh scoped PASS of the corrected R3 restore explicit old/expired/malformed/recovery meaning without changing R1/R2, policy or harness authority; downstream mechanism decisions are explicitly assigned
reopen_owner: none
next_owner: Technical Design
```

## Candidate and authority

Checkout:
`/Users/daniil/.codex/worktrees/credential-rotation-observability/rust-service-template-rest`.
Branch: `codex/credential-rotation-observability-20261006`.
Base/HEAD: `699887b18594088a59bcc23a049d290d089f6da1`.
Definition created only the five files in this task directory; no commit or
runtime change was made. Ready specification SHA256:
`ba8c0acc93bbc666b24104750302a4a9bd36c3cd5d69bf42c5201bf4b5851201`.
Reviewed input hashes and lifecycle-only promotion are in the review receipt.
The bounded Definition correction changed only R3 plus review/transition and
lifecycle metadata. It restores the accepted old-credential refusal, NATS
expiry, malformed replacement and corrected-file recovery outcomes. The former
ready spec hash is superseded; R1/R2 retain their reviewed semantics.

[Intent](intent.md) owns requester meaning and delivery authority;
[Specification](spec.md) owns behavior;
[Research](research/baseline.md) owns the bounded source facts and limitations.
The existing root remains continuation coordinator. The current phase actor's
authority ends at this reviewed handoff; no other macro phase was performed.

Current owner locators, all read from this target checkout:

| Owner | SHA256 at handoff |
| --- | --- |
| [Workflow router](../../docs/spec-first-workflow.md) | `2f4b5d255e5c960d4b7fed12b679d9f7c5a69d0758a3d4f32a4b1bf04b923295` |
| [Agent Harness](../../docs/agent-harness.md) | `1aa1029cd9fecf08fd65f3a313b12522ce9d3c190287b5fbf9553b634358d7a9` |
| [Codex adapter](../../docs/agent-harness/codex.md) | `23441c0c44bcd591172588d68e2c7224b867065e9bc609d61117c5b1555d41f9` |

Use the target [AGENTS.md](../../AGENTS.md), including its coding-feedback and
Build Speed rules when their triggers apply. CodeGraph was initialized once
for this exact worktree; source navigation must continue with this root.

## Next action and preserved boundaries

The already-dispatched fresh Technical Design actor should consume the corrected
R3 and current spec hash before final review, while retaining its independent
work on metric placement, JWKS timestamp capture and feasible isolated fixtures
inside existing harnesses. Technical Design remains triggered; Planning follows
its reviewed result. Test cases and commands remain the executor's choice.

R3 explicitly retains real-broker NATS rejection of no-longer-accepted old
credentials and expired credentials followed by valid-file recovery. Malformed
content follows current validators and cannot imply accepted authentication;
adequate mocks may prove this boundary. The compact proving scenario is not a
test matrix, and Valkey gains no new expiry or password-syntax policy.

Preserve current schedules, failure/recovery and readiness policy. Do not infer
authentication from file preparation, rotation from repeated reads, or issuer
freshness/revocation from JWKS acquisition. No TLS hot reload, stale-key cutoff,
new framework/dependency upgrade, live credential changes, merge or deployment.
PR #247 is only historical evidence; compare relevant deltas if it lands instead
of importing it. Commit/push/new PR authority remains with downstream delivery.

## Proof and reopening

Definition used source/official-contract inspection and independent read-only
review. A scoped static check found no missing relative file links or trailing
whitespace. No build, test, runtime, CI or deployment result is claimed; the
authenticated fixture's concrete execution remains unproved here.

Reopen Research for changed base, dependency or provider facts; Specification
for changed observable semantics or policy; Intake only for changed requester
meaning or effect authority. Technical Design owns an infeasible fixture or
owner mechanism, returning it to the continuation coordinator if the accepted
scope cannot support it. Do not ask the requester to choose a technical API.
