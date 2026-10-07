# Definition Transition Result

```text
status: ready
owner: Definition (Intake, supporting evidence, Specification)
result: intent.md, spec.md, recommendation-dispositions.md
review: definition-review.md — PASS, no surviving findings
movement_evidence: all material recommendations have one grounded disposition; G1/S1 behavior and compatibility are closed; fresh independent Specification Review passed
reopen_owner: none
next_owner: Technical Design — System / Integration Design, then Rust Code / Ownership Design where triggered
```

## Fixed candidate and accepted inputs

Source `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`, branch
`codex/overload-resource-isolation-20261005`, worktree
`/Users/daniil/.codex/worktrees/overload-resource-research/rust-service-template-rest`.
Definition edited only the new artifacts listed here. The parent owns
`workflow-plan.md`; product/test/historical research files were not edited.

| Ready artifact | SHA256 |
| --- | --- |
| [Intent](intent.md) | `5fc7cfd909b8145788b39e34f3c176c0f0d14e63f23227f2cc8dced2f81ae228` |
| [Specification](spec.md) | `d37ba7d63c96da3c8d85753c5ecb644b7899ea8e8a22bb73c479ee4f8018e1c6` |
| [Recommendation dispositions](recommendation-dispositions.md) | `61ffa3270cca25207a2f8657e3dfd666f5799bae6e17ae588ce15bd04c4931b4` |
| [Specification Review](definition-review.md) | `25ed900cc3a7f2c4e4e00436b27de2c20df8373a0af0a088056913f62ce6318c` |

Historical source/report identity, current applicability and exact open-PR heads
remain in the disposition evidence. Reviewer identity is
`/root/overload_definition/specification_review`; selected native configuration
was `gpt-6-astra/high`, fresh history, read-only scope. Its fixed candidate
passed without repair. Only artifact lifecycle `draft` → `ready` changed after
review, under Transition's unchanged-semantic-scope rule.

## Settled scope

1. G1: bound all admitted business opening/authentication work before auth using
   existing K, independently preserve authenticated K through terminal status,
   keep health exemptions/zero opt-out and bounded refusal drainage, and close
   timeout/cancellation/recovery/error precedence.
2. S1: spend the existing S3 GET operation timeout from first execution through
   confirmed EOF, reclaim unpolled adapter custody at expiry, preserve integrity
   and stable terminal outcomes, and document the tighter streaming contract.
3. D1: align existing operator guidance and explicitly disposition all nineteen
   material research pressures. No guessed class/fleet quotas, new executor,
   generic framework, schema, dependency or infrastructure change is accepted.

HTTP response consumers, fast/slow class fairness, durable backlog/expiry,
tenant/endpoint priorities and fleet capacity remain service-owned decisions
with concrete reopen conditions. Existing separate PRs remain unmerged evidence;
none is imported or required to build this branch. In particular #248 does not
implement the GET lifetime requirement, and #243 does not bound SQLx waiters.

## Proof boundary and continuation

Performed: current source and owner inspection; worktree CodeGraph status/source
navigation; historical/current source comparison; relevant exact-head PR patch
inspection; static check that Definition files have no trailing whitespace and
their relative file targets exist; independent Specification Review.

Not performed or claimed: compilation, tests, benchmarks, full `make docs-check`,
CI, deployed-provider behavior or load/fleet proof. Final validation follows the
implementation owner's budget and shared validation lock. No CPU-heavy validation
was run during Definition.

No blocker or user decision remains. The root can dispatch a fresh Technical
Design actor now to select concrete ownership, race handling, timer cancellation,
bounded polling and existing-file placement for G1/S1, including optional-profile
custody and #248 documentation/source collision awareness. Review that completed
macro result before Planning. Definition has not started Design, Planning or
Implementation.

Reopen only if current source/PR drift changes a recorded disposition, G1/S1
requires a new accepted observable/knob/dependency, or a demonstrated
incompatibility invalidates the closed behavior. Existing effect authority
continues through delivery of the separate PR; no merge or deployment authority
is carried forward.
