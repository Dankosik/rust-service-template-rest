# Technical Design review

Reviewer: fresh read-only `/root/messaging_design/technical_review`, native
`gpt-6-astra`, `xhigh`, no inherited conversation. Method:
[Technical Design Review](../../docs/spec-first-workflow/phases/technical-design-review.md)
through shared [Review](../../docs/spec-first-workflow/shared/review.md).
Consumed the [three-lens ownership PASS](design/ownership-review.md) without
repeating its review.

## Review Result V1

```text
candidate: Technical Design T3 on base 699887b18594088a59bcc23a049d290d089f6da1
verdict: PASS
findings: none remaining; T2 TD-F1 closed
evidence_boundary: fixed design and ready Definition; source/provider mechanism checks; one bounded delta recheck of transfer header admission
reopen_owner: none
```

| Reviewed artifact | SHA-256 |
| --- | --- |
| T2 `design/system.md` | `8beb22d03b99c5d019a815f4e55a88ddc515b32bbc80728f638f2deac4e8f1e9` |
| T3 `design/system.md` before ready status | `2d57aabf7e1954d4619190ed33726859a5e6b173f2af70802a5618ca28a231b3` |
| `design/ownership.md` before ready status | `d52c7eca2075a4024f74093b3b23a87be16d4051489865dc1e7db3eaefebac84` |
| `research/design-evidence.md` | `9463551e61909d5528989982249911346f6d3c54d4de50adaa01e23e62a3d72f` |
| `design/ownership-review.md` | `84709563baa20f0b3a4aeda8c07b7b1a8c9250ad349730b42cd5d4e0c31ec9f5` |
| Ready `spec.md` | `fb502329be927e9c7ba7e6fd8c25bdceff702ccba5c46610c60ef0f2950c31fe` |

The reviewer reconstructed B1–B6 flows, uncertainty and restoration, provider
limits, original-store lifecycle fencing, archive/tool feasibility, empirical
reopen and delivery composition. It found no extra surviving mechanism gap:
PostgreSQL unique arbitration reconciles racing unknown COMMIT; the original
cluster/fence survives ambiguity; abandoned lifetimes end before replacement;
missing replay authority stays missing; representative observations remain
execution obligations rather than design claims.

TD-F1 identified an omitted provider constraint. NATS2.15 checks total message
bytes and independently rejects header bytes greater than `math.MaxUint16`.
A 65,500-byte valid route under an increased native control-line limit gave
`D=204,925`, within the admitted total limits, while actual DLQ headers already
exceeded 65,535. Retained source custody did not satisfy B1's usable-transfer
guarantee.

T3 adds `H=min(S,8192)+A <= 65_535` alongside `D=S+A`. The original falsifier
now gives `H=73,853` and is refused before consumer use. The same reviewer
performed the single permitted bounded delta recheck and returned PASS;
unaffected T2 reasoning and map/panel scope remain current. This changes no
source subject grammar, ownership interface or ready Specification behavior.

The owner subsequently changed only lifecycle labels to ready (and identified
map T2 as consumed by design T3). Final hashes belong to the
[Technical Design transition](technical-design-transition.md). No runtime
test, measurement, deployment or implementation acceptance is claimed here.
