# Definition review

```text
candidate: Base 699887b18594088a59bcc23a049d290d089f6da1 plus the fixed consumer-lifecycle Definition artifacts listed below
verdict: PASS
findings: none surviving; F1 closed
evidence_boundary: Fresh read-only Specification Review, followed by one bounded delta recheck; no build, runtime, CI, publication or production execution
reopen_owner: none
```

Reviewer: native fresh-context `reviewer-agent`
`/root/consumer_definition/definition_review`, model `gpt-6-astra`, effort
`high`. Review method: shared Review and Specification Review at the repository
base above. Returned 2026-10-06. The phase owner records the result; the reviewer
did not edit or accept the phase.

## Fixed repaired candidate

| Artifact | SHA-256 checked by reviewer |
| --- | --- |
| [Intent](intent.md) | `7bdc361b98dbf5a282a1fbc037559c3f3fb029cc67c3085872907f36dc92bd27` |
| [Specification](spec.md) | `4c3c30c37d3680a83a481a6e1a84e41e21a1b0b408235e5189f485e414bdccd7` |
| [Research](research/synthesis.md) | `aba05c96b10055f6086d08a2caadd2c55727be1e57b90fd856bb0406bb26dfcc` |

After PASS the phase owner changed only the Specification's status from `draft`
to `ready` and added this receipt and the Transition. That mechanical lifecycle
update does not change reviewed behavior.
The ready Specification hash is
`6bf7341943436a10b78466474644bd078bee3107391e6ae2cd444325193d7e82`;
Intent and Research retain their reviewed hashes.

## Attempted falsifiers and result

- Release substitution: local builds, repeating one digest, and merely starting
  a process cannot satisfy the published run/rollback outcome. No divergence.
- Upgrade data loss: ambiguous baselines, dirty state, overlapping edits,
  consumer-only files, generated authority and interrupted acceptance have
  explicit outcomes. No divergence.
- Recovery certainty: the contract distinguishes native backup from observed
  restore, durable identities from counts, and local proof from production
  RPO/RTO and cross-store consistency. No divergence.
- CI shortcuts: selected shape coverage, source/image identity, comparable
  native measurements and total cost remain required. No divergence.
- External authority: remote target, visibility, plan support, cost and effect
  remain with the coordinator. PR250 historical evidence remains distinct from
  pending current integration admission. No divergence.

Initial F1 was material: D1 required stopping old workers before the additive
custody migration, while the jobs owner permits the migration and requires all
old retention owners stopped before custody/recovery activation. The repaired
D1 explicitly admits the former sequence and preserves the activation gate.
The same reviewer repeated that falsifier and closed F1.

The bounded recheck also verified the attestation-plan constraint against the
actual dependency action and its pinned upstream README. It applies to the
native `actions/attest` adapter's upload, not cosign signing in general;
`create-storage-record: false` disables different metadata. Definition remains
ready while the coordinator resolves the concrete external target.

This verdict establishes consistency of the four accepted behaviors and their
boundaries. It does not establish completed release/recovery, observed CI
improvement, or admission of the newer PR250 candidate.
