# Technical Design review

```text
candidate: HEAD 699887b18594088a59bcc23a049d290d089f6da1; design/design.md f624e206fdfed1cb8512a3d35afe96087f3d0b5f08de64f08006cb1f3ed5fce6; design/evidence.md d5dc09a5cc22d106e919f45af615537655cdeddad57b4802b07fdf10bf5bec29
verdict: PASS
findings: none
evidence_boundary: independent static Technical Design Review of metric truth/update placement, material flows, installed library/server hooks, authenticated fixture isolation, existing harness ownership, CI selection and restoration; no build/test/runtime execution
reopen_owner: none
```

Reviewer: `/root/credential_followup_design/design_review`, fresh native
`reviewer-agent`, selected with `gpt-6-astra`, reasoning effort `high`, no
inherited turns. Native dispatch accepted those settings and returned its
identity; native status confirmed active execution and then delivered the
completed read-only result on 2026-10-06. Method:
[Technical Design Review](../../docs/spec-first-workflow/phases/technical-design-review.md)
under [Review](../../docs/spec-first-workflow/shared/review.md).

## Fixed inputs and candidate

| Artifact | SHA256 |
| --- | --- |
| [Intent](intent.md) | `08812c2e6c27c4a793f6b70712157e4afbbad37f5709f9b600e5ac1e194cd222` |
| [Specification](spec.md), including reviewed narrow R3 correction | `ba8c0acc93bbc666b24104750302a4a9bd36c3cd5d69bf42c5201bf4b5851201` |
| [Design](design/design.md), before lifecycle-only promotion | `f624e206fdfed1cb8512a3d35afe96087f3d0b5f08de64f08006cb1f3ed5fce6` |
| [Design evidence](design/evidence.md) | `d5dc09a5cc22d106e919f45af615537655cdeddad57b4802b07fdf10bf5bec29` |

The reviewer verified the fixed hashes before and after inspection. Following
PASS, the phase owner changed only Design's status and added its review link.
Its resulting ready hash is in [Transition](design-transition.md). No mechanism,
scope, accepted input or proof boundary changed during promotion.

## Attempted falsifiers and result

- False authentication from file progress: not found. PostgreSQL installation
  and NATS preparation remain distinct from accepted authentication. Valkey
  success requires successful AUTH and the generation check; completed read
  failures and outer timeout have distinct observations without double count.
- False JWKS freshness: not found. Startup admission, successful replacement
  and worker cancellation align with the gauge boundaries. Failure/coalescing
  leave the value unchanged; usable reacquisition updates it. The one
  production key-store limitation is explicit.
- NATS authentication bypass or unsupported JWT construction: not found.
  Pinned server/library source supports trust-chain/claims/nonce validation and
  the chosen fixed fixture framing. Operator mode cannot share `no_auth_user`;
  serialized configuration ownership closes that constraint.
- Valkey old-session or `nopass` success: not found. The design excludes these
  as acceptance proof and admits actual AUTH observation with an isolated
  user/password set through existing supported ACL mechanisms.
- Isolation loss, restoration failure or hidden CI omission: not found.
  Managed project/endpoint identity, exclusive sequential ownership,
  restoration/disposal, failure propagation and compile-only routing are
  explicit. Unmanaged servers are not reconfigured and omitted auth proof
  cannot produce a full passing claim.

The reviewer checked current Rust owners and vendored async-nats callback,
Compose, runner, CI order/selection and profile-removal ownership, and consulted
pinned NATS authentication/JWT and Valkey ACL primary sources. Concrete cases,
assertions and commands remain Implementation-owned. Source feasibility does
not establish real-server execution or release acceptance.

## Disposition

No repair or upstream reopen is required. Reopen Research for contradictory
resolved APIs and Technical Design for infeasible isolation/restoration.
The phase owner accepts this result only for the reviewed mechanism boundary;
R3's actual authenticated execution remains pending Implementation/CI.
