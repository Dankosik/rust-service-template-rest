# Definition review

```text
candidate: base 699887b18594088a59bcc23a049d290d089f6da1; corrected R3 candidate 80df2a884dd56ac8e77c45ac0f517ff01fc939b6c5ce4a531e99c48d53e54a51 plus retained review of unchanged inputs and R1/R2 below
verdict: PASS
findings: none
evidence_boundary: fresh scoped Specification Review of corrected R3 and invalidated proof/authority interpretation; original full review retained only for unchanged semantic scope; no builds/tests/runtime observation
reopen_owner: none
```

## Current R3 correction review

Reviewer: `/root/credential_followup_definition/r3_correction_review`, fresh
native `reviewer-agent`, selected with `gpt-6-astra`, reasoning effort `high`.
Native spawn accepted the settings and returned its identity; status confirmed
active execution and the completed result. Read-only review completed 2026-10-06.

The continuation owner restored the exact accepted requester item: refusal of
old credentials, expiry, malformed replacement and recovery after publication
of a corrected file. The earlier generic rejection wording could omit these
observable cases. Only R3 and lifecycle metadata changed; R1/R2 and the accepted
effect authority are unchanged. The reviewer matched corrected spec SHA256
`80df2a884dd56ac8e77c45ac0f517ff01fc939b6c5ce4a531e99c48d53e54a51`
and the unchanged intent/research hashes below. Subsequent ready promotion
changes only lifecycle text; the final hash is in the Transition receipt.

Scoped verdict: **PASS**, findings: **none**, reopen owner: **none**.
Attempted falsifiers:

- Lost old/expired behavior: R3 explicitly requires real NATS broker refusal
  and recovery from a valid replacement, bounded to new authentication.
- Substitution of a read, mock reply or old session for authentication: R3
  excludes these and retains the actual local/CI run boundary for the claim.
- New malformed/expiry policy: expiry is NATS credential expiry; malformed
  input follows current validators, adding no Valkey syntax or expiry policy.
- Invented matrix/infrastructure: the executor chooses compact scenarios and
  reuses adequate mocks inside existing test harnesses.
- Conflict with R1/R2: the composition still separates preparation, accepted
  authentication and JWKS acquisition; their review needs no reopening.

This review consulted current validators and existing failure/recovery coverage
under shared Review, Specification Review and Evidence Contract. Concrete fixture
mechanism/isolation remains Technical Design; no build/test/runtime claim follows.

## Retained initial review of unchanged semantic scope

Initial reviewer: `/root/credential_followup_definition/specification_review`, fresh
native `reviewer-agent`, selected with `gpt-6-astra`, reasoning effort `high`.
Review completed 2026-10-06 in the assigned worktree. The native spawn accepted
those settings and returned the reviewer identity; native status confirmed a
running lane and later its completed result. No file mutation was delegated.

Fixed reviewed SHA256:

| Artifact | SHA256 |
| --- | --- |
| [Intent](intent.md) | `08812c2e6c27c4a793f6b70712157e4afbbad37f5709f9b600e5ac1e194cd222` |
| [Specification](spec.md), before lifecycle-only promotion | `f436708caaa5708808d7fae1fb2df628a77f55b8caffdcb5928e8467beb4b1eb` |
| [Research](research/baseline.md) | `32ce70d6bdd2fc58809a02ec1cb966a6e682d0fe4f5b5976df9d8b504b898855` |

The initial reviewer independently matched these hashes, HEAD and the supplied
workflow, harness and adapter hashes. The initial lifecycle promotion changed
only status and its review link. Its former R3 text is superseded by the scoped
correction above; retain the original verdict for unchanged semantic scope only.

## Attempted falsifiers and result

- A file read could masquerade as authentication. R1 distinguishes PostgreSQL
  connect-option installation, NATS challenge preparation and successful Valkey
  AUTH, matching their current source owners. Existing event meanings remain.
- JWKS time could advance without a usable acquisition. R2 excludes failed,
  invalid, empty, cooldown and cancelled fetch paths, while including a fresh
  successful acquisition of the same keys. Cancelling a waiter is not itself
  acquisition; an independently completed worker fetch still qualifies.
- Old sessions or disabled authentication could satisfy rotation proof. R3
  excludes both explanations. The existing NATS test uses unauthenticated
  server admission; the Valkey helper does not supply a password file.
  Real-server acceptance has distinct value from that current coverage.
- The proof requirement could silently authorize new infrastructure or policy.
  Intent limits fixture changes to synthetic material in existing harnesses;
  R3 leaves mechanism/isolation to Technical Design and requires an actual
  local/CI run before the corresponding proof claim.

The reviewer checked source, runners and Compose configuration, and consulted
the [NATS 2.15.0 authentication implementation](https://github.com/nats-io/nats-server/blob/v2.15.0/server/auth.go)
and [Valkey AUTH contract](https://valkey.io/commands/auth/). No blocking
behavioral divergence survived. Concrete fixture feasibility and execution
remain downstream work, not evidence established by this verdict.
