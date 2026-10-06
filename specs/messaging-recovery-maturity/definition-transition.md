# Definition transition

## Transition Result V1

```text
status: ready
owner: Definition (Intake, supporting Research, Specification)
result: intent.md; spec.md; research/current-state.md
review: definition-review.md — PASS on D2; F1 closed
movement_evidence: all six approved outcomes have grounded behavior/failure/recovery/success dispositions; current-main drift and prior PR dependency are explicit; no requester-owned decision blocks design
reopen_owner: none now; Specification for changed behavior, supporting Research for changed facts, Intake for changed requester meaning or external authority
next_owner: Technical Design — System / Integration Design, then Rust Code / Ownership Design as triggered
```

Authoritative inputs: [Intent](intent.md), [ready behavior specification](spec.md),
[current-state evidence](research/current-state.md),
[independent review](definition-review.md).

## Fixed output identity

Checkout: `/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-messaging-recovery-maturity-20261006`.
Base: `699887b18594088a59bcc23a049d290d089f6da1`.
Only the five files under `specs/messaging-recovery-maturity/` are authored by
Definition; they are not committed. Runtime/code/tests/infrastructure are
unchanged by this phase.

| Final artifact | SHA-256 |
| --- | --- |
| `intent.md` | `6ecab38fc93aad28e41d210734b74c77332d4f79390e967effbb78e8f4711b23` |
| `spec.md` | `fb502329be927e9c7ba7e6fd8c25bdceff702ccba5c46610c60ef0f2950c31fe` |
| `research/current-state.md` | `ec1e8f946a7d7da6702ced83697d97d0b172284c212eb8b3ac6efeaf8411e6c5` |
| `definition-review.md` | `bdccf4ccccbd79177e7969343b89c522038d179c7c00d25da7889f9f938dbc4e` |

The spec's only post-review delta is `draft` to `ready`; reviewed behavior D2 is
unchanged. This receipt carries the fixed identities without hashing itself.
Static documentation validation: `make docs-check` passed on the initial
artifacts, then passed again after the review/transition links were added.
No build, runtime or production claim is made by Definition.

## Continuation and reopen conditions

The root remains continuation coordinator for the end-to-end implementation and
pull-request outcome. This handoff ends only this phase actor's work. Dispatch
a fresh Technical Design actor to select mechanisms, compare maintained/native
capabilities, close ownership, define the feasible rehearsal/measurement path,
and preserve current #254 custody while incorporating required #239 behavior.
The design must disposition concurrency, effective `MaxAckPending` and role
separation using evidence; a later empirical result may reopen its narrow
decision before any resulting runtime behavior change.

Assumptions retained: template R3/TLS observations may use bounded owned local
or CI resources and disclose host/topology limits; no independent production-zone
or service RPO/RTO/SLO guarantee follows. Reopen Intake only if such a real
service target or production effect is added. Definition does not select paid
infrastructure, deploy, merge, or modify actual operated streams/consumers.

The six accepted areas remain implementation obligations. The object-storage
flake cause is unknown and must be investigated; a passing rerun is not closure.
PR #239 is an unmerged prior candidate, not the current base or a validation
receipt for the expanded work. Refresh only affected evidence if either it or
main changes during integration.

## B4 provider-driven clarification

Disposition on 2026-10-06: **no behavior change; B4 remains ready**. Technical
Design's proposed supported maintenance contract is admitted by B4's existing
unavailable-precondition refusal, exact-record requirement and separate
publication/retirement outcomes. The specification and its reviewed hash above
are unchanged; this disposition does not accept the concrete fence mechanism.

The verified provider constraint is that NATS 2.15.0's
[`JSApiMsgDeleteRequest`](https://github.com/nats-io/nats-server/blob/v2.15.0/server/jetstream_api.go#L533-L537)
has only `seq` and `no_erase`; the locked
[`Stream::delete_message`](../../vendor/async-nats/src/jetstream/stream.rs)
sends the sequence. The delete operation carries no compare-and-delete or stream
incarnation condition. A fingerprint re-read, local lock or cooperative KV lock
therefore cannot alone enforce B4 against an unrelated stream replacement or a
delete request that remains outstanding after the client crashes or times out.

B4 permits retirement under a broker/deployment-enforced lifecycle fence that
excludes stream restore/replacement and conflicting deletion for the entire
interval in which an issued request can still take effect. Timeout, loss of the
client, or expiration of a local lock is not proof that this interval ended.
If that guarantee cannot be established or retained, the workflow refuses to
issue retirement; if publication already confirmed, it reports that confirmation
and the refused/unresolved retirement separately. No weaker exact-record or
crash/retry guarantee is adopted.

Technical Design must identify the enforcement owner, establish the fence's
feasible acquisition, lifetime and release evidence, and show refusal when its
precondition is unavailable. An owned rehearsal may supply and enforce the
fence. An adopter supplies its deployment's actual fencing boundary; a user flag
or operator assertion alone cannot be reported as enforcement, and startup does
not certify it. Supplying this precondition neither authorizes this task to
administer production streams nor requires the redrive workflow to add stream
administration.

The prior Specification PASS remains valid for unchanged behavior. Concrete
mechanism feasibility and its failure cases belong to the ongoing Technical
Design and its required independent review. Reopen B4 only if that owner cannot
satisfy the existing exact-record guarantee with a usable supported path, or
proposes to allow retirement under weaker conditions. Refusing retirement in an
unsupported operating state is already an accepted outcome; refusing every
supported demonstration would not satisfy the delivered-workflow requirement.
