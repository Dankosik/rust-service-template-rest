# Technical Design review

```text
candidate: base 67be869acea112af271ec8ba621cbc50ae9d36b7; design/design.md SHA256 6b3e40a042e5de4fb164f98b6fe3a200670e12b54a1fc2e713b4f8716b9792aa; design/ownership.md SHA256 1692d92f430938fadf8b72f103b820d3f2a21fc3ebb867a4355579f71bf15fc3; design/dependency-custody.md SHA256 220290c617e8a2e0053e82886f466bff30e567beea846708a847a7e1f802c184; ownership-review.md SHA256 f9195c46df5f45d8dec167e48d7162fd147efb067065ec96960c848b7764a416
verdict: PASS
findings: none
evidence_boundary: fixed Technical Design and matching ownership-panel receipt; independent static source/evidence review, not implementation or runtime acceptance
reopen_owner: none
```

Authoritative Definition: intent.md SHA256
`f8a0f671878ddeeae9a01c2af9cad7859cf3d7d5b3f3a73e81404add0eecfab4` and
spec.md SHA256
`0411915db5fbe82fcde601ca6f1621248046cfc3b491f3151983c59e606ff480`.

Fresh independent reviewer `/root/pool_design/native_design_review` was
dispatched through Codex with `gpt-6-astra`, `high`, and no inherited history.
It applied [shared Review](../../docs/spec-first-workflow/shared/review.md)
and [Technical Design Review](../../docs/spec-first-workflow/phases/technical-design-review.md).
It verified all candidate/authority hashes before and after inspection and
consumed the matching three-lens [ownership receipt](design/ownership-review.md)
without repeating those lenses. No edit, build, Cargo resolution, database
experiment or remote write was performed.

Attempted falsifiers and results:

- **Maintenance choice unsupported:** none survives. The design compares
  supported hooks and alternate pools, recognizes Generic Deadpool's ability
  to retain SQLx, and includes the full source/delivery cost and retirement.
- **Cleanup contradicts acquisition/finality:** none survives. Three-second
  acquisition can fail during five-second cleanup; pending-BEGIN disposal and
  CommitUnknown remain, and local slot release is distinct from backend exit.
- **Diagnosis conflates wait, execution or cancellation:** none survives.
  Native outcomes, independent acquisition timing, finite operation labels,
  cancelled-acquisition silence and named-path coverage are explicit.
- **Source choice leaves downstream invention:** none survives. Published
  archive identity, isolated patch, source-only locked projection, Cargo
  confirmation, portable delivery/profile handling and reopen conditions are
  fixed. Actual resolution and image behavior remain implementation evidence.
- **Sizing omits owners or changes readiness:** none survives. Replica
  overlap, LISTEN, migrators, reserves and the 82/100 example are coherent;
  worker minima and readiness/staleness arithmetic match current owners.
- **Historical probe overclaims acceptance:** none survives. The log proves
  native retention and the recorded one-second feasibility seam, while the
  chosen five-second patch, diagnostics and delivery acceptance remain with
  Implementation and existing proof owners.

The Technical Design owner adopted PASS with no unresolved objection. It then
changed only the lifecycle sentence of each design artifact from draft to
ready, after asserting its exact reviewed hash. This mechanical identity
refresh preserves the reviewed semantic scope under shared Transition.
Current ready SHA256:

| Artifact | SHA256 |
| --- | --- |
| design/design.md | `01093c8dc4584ad01e578b41e55f816b8fe89d8ff3c0a6929102fd007c53fac6` |
| design/ownership.md | `e5d1fa567ec7d821b6b7b1157118dfcb3c1d1f25a6cec28a5b54c9cec68ec492` |
| design/dependency-custody.md | `d35f34e3daee259b8a8d89515617f7ea1256230ee5d4766ffac9fc6caf1dc9d7` |

The stopped facade reviews are historical non-verdicts and contribute no PASS
to this candidate. No source or behavior repair was required by the current
review; no bounded delta recheck or reviewer rotation was needed.
