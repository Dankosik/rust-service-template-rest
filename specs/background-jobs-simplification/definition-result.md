# Definition result

```text
status: ready
owner: Definition
result: specs/background-jobs-simplification/spec.md
review: Independent review below: PASS
movement_evidence: All requested behaviors and compatibility dispositions are closed; no material divergence survived independent review.
reopen_owner: none
next_owner: Technical Design — System / Integration Design, then Rust Code / Ownership Design where placement is not mechanically forced
```

Read [intent](intent.md), [specification](spec.md), and
[source evidence](research/definition-baseline.md). Implementation, Planning,
builds and tests have not begun. The continuation coordinator owns the next
fresh phase actor. One separate PR is authorized; merge/deploy are not.

## Independent Review Result V1

Candidate: those three uncommitted files at base
`4edd184ea3cc6b6fa2b225244700fce57150ae18`.
Reviewer: fresh native `/root/jobs_definition/definition_review`, requested
`gpt-6-astra`, `high`, with no inherited turns.
Verdict: **PASS**. Findings: none. Reopen owner: none.

The reviewer independently checked the candidate, supplied recommendation
scope, Specification Review and shared Review/evidence policies, existing
adopter/architecture contracts, original migration and roadmap. Falsifiers
covered duplicate dispatch/double refund after lost acknowledgement; release
overwriting a known drain result; stale transactional completion committing
business effects; JSONB data loss or validation aborting caller transactions;
stale/censored observations; disjoint claims and unknown-kind isolation.
No material divergence survived. PostgreSQL JSON constraints and River
completion/snooze primary documentation supported the behavioral decisions.
Mechanism enforcement and runtime proof remain later owners' responsibilities.
No files, builds, tests or database state were changed by the reviewer.

Reviewed SHA-256 identities:

- `intent.md`: `af82d20c8a9807e72fa20822088f19d3d4ceab354e67750f47b328a65a37c020`
- `spec.md`: `ddc81d08e6afb594a548f984e61e899f4f02a5f55dead0f95d4fa468fd811207`
- `research/definition-baseline.md`: `899b8f31ef5149a0018b5851ab37b47707251c31fe69d745b1d97409da5493c9`

After review, only the specification's status line changed from draft to ready;
the reviewed semantic scope is unchanged under shared Transition.

## Remaining decisions and limits

There is no open requester-owned question. The 60-second lease safety margin is
an accepted local choice, not a measured outage guarantee. JSONB normalization
is explicit; incompatible legacy data fails migration intact. No live database
or production compatibility claim has been made.

Technical Design owns the safe transactional completion API, indexed claim and
observer queries and exact bounded sampling policy, trace carrier representation,
rollout/preflight mechanics, installed random source, and lifecycle extraction
decision from actual duplication. It reopens Definition if an accepted invariant
cannot hold or a new observable policy is needed. Applicable proof remains
within existing validation routing, with CI-owned gates taken from CI.

Methods used: Specification, supporting Research, `rust-reliability`,
`rust-errors`, and the durable-background-jobs contract reference.
