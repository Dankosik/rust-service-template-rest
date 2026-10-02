# Specification Review Result V1

Review date: 2026-10-02. Fresh read-only reviewer:
`/root/infra_jobs_definition/definition_delta_review`; native selection
`gpt-6-astra`, effort `high`, no inherited turns.
Method: [Specification Review](../../docs/spec-first-workflow/phases/specification-review.md)
through [Review](../../docs/spec-first-workflow/shared/review.md).

```text
candidate: source baseline 67be869acea112af271ec8ba621cbc50ae9d36b7;
  intent.md SHA256 927f762c4c7cd868c0370b7dd201f118b0b55b843392920d3ed0910ab2c3fd83;
  spec.md SHA256 9e20bb8cb2e85d0fa9a67c4c683d5c975470ffc672cd085c46ec7341960fb07b
verdict: PASS
findings: none
evidence_boundary: read-only review of the local semantic delta and invalidated
  reasoning, retaining independently verified prior PASS for unchanged scope;
  source and arithmetic evidence only, no implementation/runtime claim
reopen_owner: none
```

## Reused independent evidence

The adopted Definition is under the read-only source locator
`/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-jobs-worker-reliability-20261002/specs/jobs-worker-reliability/`.
It uses the identical source baseline. Original file identities:

| File | SHA256 |
| --- | --- |
| `intent.md` | `e7fc1ede41358ab464c25004fa298fd1256f2e31c646de0a9f11229cc9d59253` |
| Reviewed draft `spec.md` | `9ab7ba646e0a502438c5b5c215030bc3889c3e38bb62d289e9e5982cca758a95` |
| Ready `spec.md` | `dff3e736c20e1b03e7bb9a21116c9f097b126b8fa401b096402fac678b0c9943` |
| `specification-review.md` | `9d3022f764cd0a38111c46b53420858f8e56b4debe8d5597464a91610676d29f` |

That receipt records PASS from fresh reviewer
`/root/jobs_definition/spec_review` (`gpt-6-astra`, `xhigh`). It falsified
B1 full-attempt and orphaned-completion bounds; B2 old-retention-owner rollout;
B3 stale cycles, competing mutations/live unique keys and commit uncertainty;
B4 immutable publication identity and possible duplicate effects; and B3/B5
empty filtered pages, unavailable observations and complete traversal. No
material divergence survived. Static leases, one publisher slot, and combined
failure/admission were explicitly retained with reopen conditions.

The local reviewer verified the receipt and Intent hashes and independently
confirmed that changing only the adopted ready Specification status back to
draft reproduces its reviewed hash. These decisions remain unchanged in the
local Specification; its new observation and retry/restore wording received
the bounded independent review below. Source locators are provenance, not a
second writable authority or a dependency on ongoing parallel Design.

## Attempted falsifiers and result

- One engine successfully samples while its peer fails, hiding stale values:
  `Engine::start` starts sampling per engine, `sample_once` selects that
  engine's registry, and `publish_sample` advances one shared timestamp.
  B5 now requires complete process-union observation and preserves all
  last-good values/timestamp on an incomplete observation. No material gap.
- Nominal retry sums omit an interval or imply a deadline: independent
  arithmetic confirms 1,763,020 and 562,666 seconds. Source confirms quartic
  jitter and the 25/20 attempt policies; B6 excludes handler, floor, outage,
  scheduling and backpressure delays and makes no delivery-bound claim.
- Redrive or rename silently repairs poison intent: baseline decoding failures
  follow retry/exhaustion; B6 preserves it and requires compatible restoration
  or separately reviewed conversion, with effect reconciliation.
- Finite consumer TTL guarantees suppression across indefinite failed custody:
  B4 explicitly rejects that guarantee and requires durable identity for the
  permitted lifetime or reconciliation and constrained replay.

Both local candidate hashes were verified unchanged during review. After PASS,
the Definition owner changed only local `spec.md` status from draft to ready.
Its semantic scope is unchanged. The ready hash is
`c648efdde020c20a4e5a1628ddbd8281b19d170cb2303de2e8e2ee190aa9027f`.

The initial reviewer invocation reported model capacity before doing review;
one retry of the same fixed brief completed successfully. No fallback reviewer
model or broadened verification was used. No build, test, database, broker,
profile, CI, or runtime result is established by this receipt.
