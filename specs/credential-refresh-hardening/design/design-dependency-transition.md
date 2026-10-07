# D1/D3 dependency delta Transition Result V1

```text
status: ready
owner: Technical Design — bounded D1/D3 reopen
result: specs/credential-refresh-hardening/design/technical-design.md
review: specs/credential-refresh-hardening/design/design-dependency-review.md — PASS
movement_evidence: versioned public API and existing feature/lock path established; fresh bounded PASS consumed
reopen_owner: none
next_owner: Implementation — existing T1 Lead
```

Current [design](technical-design.md) SHA256: `b199de02839fcda51eb162e91e7c96e03236f351e6f91e32e7507c5e89241f11`.
[Bounded review](design-dependency-review.md) identifies the fixed reviewed bytes
and lifecycle-only ready delta. [Original review](../design-review.md) remains
valid only for unchanged scope; the earlier direct-dependency decision is withdrawn.

The T1 locked-metadata failure reopened the direct AWS-LC dependency edge only.
Use `async_nats::rustls::crypto::aws_lc_rs::default_provider().secure_random`:
extract its static library interface once during existing reconnect-callback
setup, and fill fresh sample bytes for each eligible attempt. Do not install or
look up a process-global provider or allocate a provider per retry. Preserve the
accepted callback policy and zero-spread failure fallback. Remove only T1's
attempted `aws-lc-rs` manifest addition and leave Cargo.lock unchanged. No new
feature, dependency version or edge is required.

The same T1 Lead resumes its existing implementation boundary and ordinary
locked feedback; root updates consumed packet/ledger locators. No new Planning
phase or unit is required. OAuth/JWKS, R1 ranges, R2, expiry, lifecycle and proof
ownership are unaffected. T2/T3 may continue independently.

Worktree remains
`/Users/daniil/.codex/worktrees/credential-refresh-hardening/rust-service-template-rest`,
branch `codex/credential-refresh-hardening-20261005`, base
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`. This reopen changed only design and
review/transition artifacts; it did not repair production code or manifests,
run Cargo/build/tests, inspect credentials or perform external actions.
Reopen D1 only if the selected public API/features fail against actual locked
resolution. No user decision is pending; bounded reviewer completed.
