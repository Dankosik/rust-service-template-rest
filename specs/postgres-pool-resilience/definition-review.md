# Definition review

## Current candidate

```text
candidate: intent.md SHA256 f8a0f671878ddeeae9a01c2af9cad7859cf3d7d5b3f3a73e81404add0eecfab4; spec.md SHA256 95a535c9f127e370e1466bc11b8cac612c41fad105899545740e825c8dda53e9; base 67be869acea112af271ec8ba621cbc50ae9d36b7
verdict: PASS
findings: none
evidence_boundary: fresh full Specification Review of the changed Definition; static source and retained-probe evidence, no new runtime claim
reopen_owner: none
```

Fresh reviewer `/root/pool_definition/reopened_spec_review` was dispatched with
`gpt-6-astra`, `high`, and fresh history. It applied shared Review and
Specification Review and verified both candidate hashes before and after
inspection. This is independent of the original reviewer and verdict.

Attempted falsifiers and results:

- Authority (`spec.md:28–40, 198–220`): technical choices are distinct from user
  constraints; alternatives remain evidence-driven and dependency custody is
  described as temporary. No authority divergence survived.
- Recovery/finality (`spec.md:44–91`): three-second acquisition may time out
  during five-second cleanup; uncertain COMMIT, pending-BEGIN safety, healthy
  reuse and physical-server-session limits remain explicit and consistent.
- Diagnostics (`spec.md:93–124`): named operation coverage and truthful outcomes
  do not require an application pool facade; placement remains Design-owned.
- Sizing/readiness (`spec.md:126–186`): connection owners, worker validation,
  pooler distinctions and current readiness policy remain covered. Current
  mode-dependent worker validation remains authoritative, including outbox capacity.
- Proof (`spec.md:188–214`): the retained one-second probe on PostgreSQL 18.6
  supports mechanism feasibility only. No five-second backport or full integration
  acceptance is claimed; required local observations remain bounded.

The reviewer checked supplied authority, research/custody, persistence,
configuration and validation owners, current pool/transaction/observation/probe
code, and the retained probe source/log hashes. It ran no builds or database
scenarios and made no edits, acceptance or phase transition. Following PASS,
the Definition owner changed only the spec lifecycle sentence from draft to
ready; all reviewed semantic content is unchanged. Downstream dependency,
delivery and behavioral proof remains pending with Technical Design/Implementation.

## Historical initial Definition review (superseded)

The records below preserve the prior fixed candidates and their evidence. Their
three-second assumption and universal acquisition requirement are no longer
active; current intent/spec own behavior.

```text
candidate: intent.md SHA256 a3fd14ea998355fd586af58d2030d22590a23b015cb43def742f49bcaf811657; spec.md SHA256 1482c9a4d111124671955697d26e550314c0b0c41c00b72b5ff8c298c5c7fca5; base 67be869acea112af271ec8ba621cbc50ae9d36b7
verdict: PASS
findings: none
evidence_boundary: fixed Definition static review; no runtime claims
reopen_owner: none
```

Fresh independent reviewer: `/root/pool_definition/spec_review`, dispatched
through the Codex collaboration harness with `gpt-6-astra`, `high`, fresh history.
Review method: [Specification Review](../../docs/spec-first-workflow/phases/specification-review.md)
through [Review](../../docs/spec-first-workflow/shared/review.md).
The reviewer verified both hashes before and after review and changed no files.

Attempted falsifiers and results:

- Cancellation coverage and finality (`spec.md:29–56`): BEGIN, statements,
  pre-commit verification, COMMIT and readiness are covered; local capacity
  release does not claim rollback, and commit uncertainty/retry rules remain.
- Diagnostics (`spec.md:60–80`): direct-pool and readiness acquisitions are
  explicit; acquisition outcomes stay separate from SQL execution. Compared
  `in_tx_with`, `Observed::waited` and `PostgresProbe::check`.
- Sizing (`spec.md:84–106`): replica overlap, workers, direct connections,
  other applications, reserves and PgBouncer client/backend counts are covered.
- Policy and proof (`spec.md:110–163`): current defaults and compatibility stay
  explicit; local observations do not claim production sizing. Concrete cases
  belong to Implementation and mechanism verification to Technical Design.
  Compared persistence, configuration, PostgreSQL validation and Evidence Contract.

No surviving material divergence. No build, database scenario or driver-mechanism
verification was performed by the reviewer. After review the Definition owner
changed only the lifecycle sentence in `spec.md` from draft candidate to ready
reviewed Definition; the reviewed semantic scope is unchanged.

## Bounded delta recheck

The coordinator requested explicit attribution of the three-second release
bound as a technical assumption. The same reviewer completed the one bounded
delta recheck permitted for this result:

```text
candidate: spec.md SHA256 6a612e4543aea2dc4e85515df999fd55725fa6cc759bf03dcd660b2864ddfdef; unchanged intent.md SHA256 a3fd14ea998355fd586af58d2030d22590a23b015cb43def742f49bcaf811657
verdict: PASS
findings: none
evidence_boundary: static delta at spec.md:37–49 and lifecycle wording at spec.md:3; prior unaffected PASS retained
reopen_owner: none
```

The reviewer verified the hashes and attempted to falsify attribution to user
authority, separation of slot release from replacement connectivity, and silent
relaxation of the bound. None survives: the basis is explicitly technical,
replacement acquisition is separate, and contrary driver evidence returns to
Specification with a bounded alternative. No runtime proof was claimed or
required, and the reviewer performed no edits or phase transition.
