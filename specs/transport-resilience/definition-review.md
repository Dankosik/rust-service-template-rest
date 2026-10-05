# Definition review

## Current bounded gRPC clarification

The gRPC section now owns a native cooperative dial timeout with the original
deadline retained across idle periods. This replaces the original review's
unqualified wall-clock release reading for that section alone; the original
review below remains valid for unchanged scope.

```text
candidate: spec.md SHA256
  30bf7f325931065810dd1a4caea4d3ddfd3b7c7e1fdeb0bb46e5a31108267f66
verdict: PASS
findings: none
evidence_boundary: Fresh /root/transport_definition/grpc_delta_review,
  native gpt-6-astra/high, read-only Specification review of lines 37-67,
  Intent, existing gRPC contract and pinned dependency sources.
  Candidate identity matched before and after review. Static evidence only.
reopen_owner: none
```

Attempted falsifiers and results:

- Idle retention: Tower 0.5.3 `buffer/worker.rs:71-100,153-184` can stop
  driving readiness after canceled calls. The revised contract permits that
  idleness but requires later active calls to escape the stalled attempt.
- Deadline renewal: hyper-timeout 0.5.2 `src/lib.rs:74-89` creates one timeout
  in the retained future; Tokio 1.53.1 `time/timeout.rs:86-97` fixes its deadline.
  Resumed polling does not renew it.
- Permanent channel poisoning: Tonic 0.14.6 `reconnect.rs:95-106,133-144`
  returns to Idle, presents a stored connection error through one call and
  clears it. A subsequent call can redial.
- Late readiness: Tokio `time/timeout.rs:211-221` polls the inner future first;
  the explicitly permitted ready-result exception describes native behavior.
- Intent/finality: no requester requirement mandated eager idle cleanup or
  strict late-result rejection. Active-call recovery, caller deadlines, TLS,
  streaming lifetime and no-replay/finality remain preserved.
- Feasibility: Tonic `endpoint.rs:618-622` wraps the TLS-capable connector in
  hyper-timeout on its lazy custom-connector path. Implementation wiring and
  runtime proof remain downstream responsibilities.

Sources are in the locked crate versions under the local Cargo registry; this
review did not run or claim runtime proof. The Definition owner changed only
the lifecycle status after PASS; reviewed semantic scope is unchanged.

## Original fixed Definition review

Reviewer: `/root/transport_definition/spec_review`, fresh read-only
`reviewer-agent`, native `gpt-6-astra` / `high`, 2026-10-05.

```text
candidate: base 5927ffbba351af2f7fb8635316bbfa4ae5b31da6; SHA256:
  intent.md 1c041b9a8e6b600ee41f17d85a0baf7a0339ec57993a1c087934d98f5c511ca5
  research/baseline.md f2ac04613ff39d9a8f72e9df1bc5b21acb05eabdaef045305112fd26e8d1e7a2
  spec.md a27f407d6683effb30aa6d8e138df8a652f91d0f6e7a1799e2055438c9dcc7a1
verdict: PASS
findings: none
evidence_boundary: Three fixed Definition artifacts, shared Review and
  Specification Review; hashes matched before and after review. Supplied R2
  conclusions reused within their documented limits. No implementation or
  runtime verification was performed.
reopen_owner: none
```

Attempted falsifiers and results:

- Coverage: all six baseline findings and retained surfaces map to accepted
  behavior or an explicit disposition; no material omission survived.
- Finality/recovery: the contract prohibits renewed caller budgets, replay,
  fabricated success and stranded reconnect ownership, preserving gRPC stream
  lifetime and unknown-effect semantics.
- NATS compatibility: ordinary TLS, TLS-first, trusted-network plaintext, local
  escape hatches, mixed seeds and incompatible brokers have bounded
  dispositions; the discovery change and migration choices are explicit.
- Deferred decisions: address fallback requires an adequate repair,
  evidence-backed existing adequacy, or a Definition reopen before exclusion.
  Remaining questions are mechanism feasibility/cost, not permission to omit.
- Proof scope: proposed falsifiers do not invent infrastructure or mandatory
  runtime gates; local, CI and production claims remain separate.

After this result, the phase owner changed only the spec's status line from
draft to ready and linked this review. This mechanical lifecycle change leaves
the reviewed semantic scope unchanged under the Transition owner. The current
artifact identity is recorded in [Definition transition](definition-transition.md).
