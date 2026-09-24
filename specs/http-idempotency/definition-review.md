# Definition review and movement evidence

Adapter: [Specification Review](../../docs/spec-first-workflow/phases/specification-review.md)
under shared [Review](../../docs/spec-first-workflow/shared/review.md), with the
Material Rule and Falsifier rubrics. Baseline: clean `main` at
`d24d1737d1647d9bfdf2b15ecb258630ec9cd326`; the candidate is the untracked
`specs/http-idempotency/` bundle.

Both reviewers were native `reviewer-agent` carriers selected with the Opus
model field, with fresh history and a read-only boundary. Each received:

- the fixed hashes;
- the verbatim request and the coordinator's Definition checklist;
- the repository owners;
- the Go, sqlx, PostgreSQL, and draft evidence.

Final result: **CONCERNS**, with its one bounded obligation (C1) discharged by
the Specification owner below. Movement is permitted.

## Review 1: first reviewer (`abdc60295029a80d4`)

```text
candidate: intent.md f67e695511da81efd527fc9bf501fc0bcd88badfe9aeeca7b5305db3801f1236;
  research/synthesis.md bb43840f295d4f3b53c389cfdf3b352ad8bbe29efa9cbc70a0c0c9e03afc3db1;
  spec.md 1e4aae6533172388218b02199c85934f77d3b1e138319042a6b41a8460445180
verdict: FAIL
findings: F1, F2 (FAIL); F3 (CONCERNS); notes N1-N3
evidence_boundary: static, read-only: repository owners and named code, initializer
  scripts, sqlx 0.9.0 and tower 0.5.3 source, Go HEAD 749473366b, IETF draft rev 07,
  crates.io; no build, test, database, or docs-check run
reopen_owner: Specification
```

- **F1.** Step 2 claimed runtime `authn.mode = "none"` always answers 503.
  The unchanged contract still answers 401, 400, and 431 on credential syntax.
- **F2.** Header-carried typed inputs were outside the semantic input, so
  replay versus 422 was undecided.
- **F3.** The claim that `none` output is unchanged had no falsifier.
- **N1.** The roadmap's statement that stage-10 items are independent.
- **N2.** The `outcome` before execution was unspecified.
- **N3.** The count of historical lock shapes was wrong.

Surviving falsifiers:

- agreement in both directions;
- the non-waiting 409 (D1), the authentication requirement (D3), and
  success-only replay (D5);
- commit-unknown readback and sqlx 0.9 cancellation;
- the 16/128/8 arithmetic;
- the crate survey.

The owner repaired all six items. The same reviewer's bounded delta recheck
was in flight when the coordinator session restarted, and it was lost without
a result. The coordinator directed one fresh independent review of the
current candidate, consistent with shared Review's rule for an unavailable
reviewer.

After the restart, the owner re-read the files and made four clarifications
before fixing the next candidate:

- the Outcome seam wording;
- an encoding-stability bullet;
- "for arbitration" in the first arbitration row;
- a rewrap.

## Review 2: fresh reviewer (`ae03a0235945ed0c6`)

```text
candidate: intent.md f67e695511da81efd527fc9bf501fc0bcd88badfe9aeeca7b5305db3801f1236;
  research/synthesis.md 3eef8fdfccaa200363e0355c77af092aec9ce8616c5df0d4fea7ef6de84eca49;
  spec.md 3c66250a5ffcf14411aeb5e9e41278c347f640d670fdfe4c35e3842cf45bf45c
verdict: FAIL
findings: S1 (FAIL); S2 (CONCERNS); notes R1-R4; F1-F3 and N1-N3 closed without new divergence
evidence_boundary: static, read-only: the full candidate, the exact delta since review 1
  (reconstructed files hashed back to the review-1 identities), repository owners and
  code, sqlx/tower source, Go HEAD 749473366b, IETF draft rev 07, crates.io; no build,
  test, database, or docs-check run
reopen_owner: Specification
```

- **S1.** The new encoding-stability bullet ("any change bumps the version")
  contradicted the guide and Go's committed rule. Under that rule, only a
  meaning change bumps; an encoding-only change waits for live records to
  expire or accepts both encodings. A retry across a deployment could replay
  or get 422.
- **S2.** Authorization must come before the seam, and a retry whose check
  reads state that the effect changed does not replay. That needed stating.
- **R1.** "At most one attempt runs work" overclaimed; the guarantee is at
  most one key holder.
- **R2.** The credentials exclusion and the negotiation headers needed
  stating precisely.
- **R3.** An `Idempotency-Key` declared without `x-idempotent: true` should be
  refused.
- **R4.** The test-support verifier fixture exists only for introspection.

Surviving falsifiers: everything listed for review 1, plus the widened
semantic input, header stripping, the agreement superset of Go's declaration
check, and the draft claims.

## Bounded delta recheck (same reviewer `ae03a0235945ed0c6`)

```text
candidate: intent.md f67e695511da81efd527fc9bf501fc0bcd88badfe9aeeca7b5305db3801f1236;
  research/synthesis.md bf48091c7af098a95af0ca59c9bdef0c1b54a5212e068487dc26a17c905932a3;
  spec.md 2afd705d34d209fbaa61838ef8e6f701daa0bc95f609f4cc90eae176fd218821
verdict: CONCERNS
findings: S1, S2, R1-R4 closed; C1 (CONCERNS, new in the delta); two non-blocking notes
evidence_boundary: static, read-only: verified hashes and that the delta (built against
  snapshots of the review-2 files) matches the real change; the repaired spec and
  synthesis delta against authn.rs, infra-bearerauthn lib.rs/jwt.rs, transaction.rs,
  sqlx source, and Go's committed guide; no build, test, database, or network
reopen_owner: none required
```

- **C1.** After a deliberate meaning change, an honest retry gets 422. The
  Outcome proviso did not list that case, and the client guidance said 422
  "means the key was misused". A client that resends under a new key could
  repeat the effect.
- **Note 1.** "Never inside the work" also forbade extra re-checks inside the
  transaction.
- **Note 2.** Waiting for old-encoding records to expire requires pausing
  the operation. The compatibility comparison is the usable path, so Technical
  Design's seam must let an operation supply it.

Attempted falsifiers:

- field renames, encoded defaults, encoder drift, and meaning changes (S1);
- revoked authorization after commit, DELETE ownership, and authorization
  inside the work (S2);
- a server-side transaction ending mid-work (R1);
- negotiation-only retry differences (R2);
- an advertised key without `x-idempotent` (R3);
- the fixture claims (R4).

## Owner discharge of C1 and the lifecycle edit

The Specification owner applied exactly the reviewer's smallest repair at the
three anchors, spec only:

- the Outcome proviso now lists the version exception;
- the client guidance says 422 means the key is bound to a different recorded
  request (misuse or a meaning change), that the recorded request may already
  have committed, and that a client must reconcile before resending under a
  new key;
- note 1 takes the reviewer's wording, "not only inside the work", and allows
  extra checks inside the work.

This aligns the text with the already reviewed version and authorization
rules. It changes no behavior, table row, or proof. Note 2 is carried to
Technical Design in the [transition](definition-transition.md). After the
discharge, `spec.md` was
`6e80b38e1177303bd22de7ef33a5a8c8643f445f40ee5740233940816f9eb350`. The
lifecycle-only `draft` to `ready` edit then gives the final `spec.md` hash
recorded in the transition. Under Transition's unchanged-semantic-scope rule,
the CONCERNS verdict applies to this contract.

## Static checks and evidence freshness

`make docs-check` passed after every candidate revision (688 links, 0 errors).
It passed again over the full bundle, including this record and the transition
(695 links, 0 errors). It is a static link check, not implementation evidence.

The Go checkout's HEAD moved to `7a671b5bfbdfa0c40b163a5d32f2c86048787f7e`
during Definition. No cited idempotency path changed from `749473366b`, so the
Go evidence stays current.
