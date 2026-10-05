# Health-policy hardening: Implementation completion

Status: ready

## Candidate and scope

Unit: the single fixed unit in [Planning](planning-result.md), consuming the
ready [Specification](spec.md) and [Design](design.md). Implementation changed
only their seven required source/test/documentation files; the existing silent
relay required no change. Runtime readiness values, registration, criticality,
routing, dependencies and infrastructure are preserved.

Checkout: `rust-service-template-rest.codex-health-policy-hardening-20261005`;
branch `codex/health-policy-hardening-20261005`;
base `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
The tracked `git diff --binary` SHA-256 is
`dbcb4ba9fd61845a98a4847169af75eb6b9e22a03ac3cb60a689f4aec55086bf`.
Accepted spec/design/planning input hashes were rechecked unchanged.

R1 uses completion-time previous-publication freshness in the existing fold.
R2 publishes completion timestamp and stale-bound gauges through the existing
health writer. R3 bounds complete pooled session verification and rejection
cleanup to five seconds each, retaining the original rejection. R4 updates the
four existing documentation owners and their operating limitations.

## Local evidence

`make plan` selected workspace Rust validation (`outside_crates` for the
integration test) and documentation validation. Under AGENTS and
[Validation Routing](../../docs/validation-routing.md#ordinary-local-completion),
the consolidated ordinary local plan is `make build`, `make test`, and
`make docs-check`. The new feature-gated PostgreSQL test also receives compile-only
`cargo check --locked -p integration-tests --features integration --test postgres`.
All CPU-heavy execution takes the existing Git-common validation lock; Cargo
uses the pinned toolchain with `/Users/daniil/.cargo/bin` on PATH.

One causal negative control temporarily restores only the old freshness
predicate and runs
`cargo test --locked -p health tests::failed_refresh_only_absorbs_a_previous_ready_that_is_fresh_at_completion -- --exact --nocapture`,
then restores identical candidate bytes before the positive suite. It failed
as intended: 0 passed, 1 failed, reporting `Ok(())` where stale Ready must be
withdrawn. The initial attempt found a test compile error from an unavailable
`futures_util::poll!` feature; standard `std::future::poll_fn` repaired that
fixture without a production seam or dependency change. The negative control
was then rerun and observed, and fixed bytes restored.

`make build` passed (5m39s). `make test` completed its workspace/no-fail-fast
run; health passed 25/25, infra-postgres 29/29, and service lifecycle 15/15.
The only failure was unchanged OAuth test
`tests::exchanged_cache_evicts_large_payloads_before_the_entry_target`, with
`Resource(Timeout)` at `crates/infra-oauth2-client-credentials/src/tests.rs:925`.
The aggregate is recorded as failed, not PASS. All other test targets and
doctests passed; the actual-Go wire export is explicitly CI-ignored, and
feature-gated database suites are not claimed as executed. The same built OAuth
test binary was then run in isolation:
`target/debug/deps/infra_oauth2_client_credentials-1a435a22543e66a1 tests::exchanged_cache_evicts_large_payloads_before_the_entry_target --exact --nocapture --test-threads=1`.
It passed (1 passed, 0 failed, 12.76s), without source changes. This reconciles the
single failed scope while retaining all other passing results; it is not a
fresh clean aggregate and does not establish the original timeout's cause.
No unrelated OAuth code or timing value was changed.

`cargo check --locked -p integration-tests --features integration --test postgres`
passed (54.00s), including the new silent-readback test and existing relay.
This establishes feature-gated compilation, not PostgreSQL behavior. The build
and check emitted an existing deprecation warning in excluded vendored SQLx;
no vendor or dependency source changed.

`make docs-check` passed: 1289 total links, 566 unique, 1106 OK, 0 errors,
183 excluded. Its existing read-only, network-disabled offline lychee analysis
was run separately while CPU validation waited; it is not a CPU-heavy build.
`git diff --check` passed. A 900-second lock wait expired while another task
was making progress; only the two remaining scoped checks were requeued.

## Independent review

Reviewer `/root/health_implementation/implementation_review`, fresh native
`reviewer-agent`, `gpt-6-astra` / `high`, `fork_turns: none`, applies
[Implementation Review](../../docs/spec-first-workflow/phases/implementation-review.md).
Its source read found no anchored blocking defect and checked the fixed hash.
A bounded delta recheck accepted the standard-library polling repair; the
reviewer independently verified the updated final hash above and consumed the
completed receipts. Final verdict: **PASS**, findings: none, reopen owner: none.

The review falsified stale Ready recovery, equality and crossing-expiry cases,
completion/drain/cancellation metric semantics, whole-readback timeout coverage,
retained rejection through bounded cleanup, silent-relay ordering, and agreement
of R1–R4 guidance. It accepted the scoped OAuth reconciliation while preserving
the failed aggregate and unknown timeout cause. It claims no PostgreSQL runtime,
CI, deployment or fleet observation. The review remained read-only and did not
perform acceptance on the Lead's behalf.

## Remaining delivery and claim boundary

Real PostgreSQL session-silence, pooler and cleanup observations remain pending
CI. The new regression waits for admission while its existing relay remains
silent, then explicitly shuts down and joins the fixture. Written/compiled code
is not a database observation. CI-owned initializer, SQLx, messaging and other
applicable delivery gates remain with CI. No fleet, production, remote socket
termination, deployment or runtime routing claim is made.

The root continuation coordinator owns the authorized commit, push, one PR and
CI delivery after local handoff. This Lead has performed no remote write,
merge, deployment or production access.

## Local Acceptance Result

```text
unit: Completion (health-policy-hardening fixed unit)
verdict: Accepted
candidate: base 5927ffbba351af2f7fb8635316bbfa4ae5b31da6; tracked diff SHA-256 dbcb4ba9fd61845a98a4847169af75eb6b9e22a03ac3cb60a689f4aec55086bf
evidence: Matching build, workspace test scopes with the single failed OAuth scope reconciled by an isolated unchanged-binary PASS, gated PostgreSQL compile-check, documentation check, and observed old-predicate negative control. No clean aggregate rerun or database observation is claimed.
review: Fresh independent Implementation Review PASS; no findings; bounded test-fixture repair recheck PASS.
next_owner: Root continuation coordinator for commit, push, one PR and applicable exact-head CI, including empirical PostgreSQL proof.
```

`HANDOFF_READY` applies to this locally accepted fixed unit. The larger requested
PR/CI outcome remains pending. The Lead remains available for causal in-scope
repairs from delivery evidence. Reopen this implementation for an observed
in-scope defect; changed behavior or mechanisms return to their existing spec or
design owner. No additional user-owned decision is required.
