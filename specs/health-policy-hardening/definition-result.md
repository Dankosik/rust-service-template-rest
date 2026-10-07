# Definition result

```text
status: ready
owner: Definition (Intake and Specification)
result: intent.md and spec.md
review: Specification Review below, PASS
movement_evidence: All research recommendations have grounded dispositions; behavior, boundaries and proof expectations are closed; independent review found no material divergence.
reopen_owner: Specification for an invalidated behavior/budget assumption; Intake for changed requester meaning or authority
next_owner: Technical Design
```

## Fixed candidate and review

Checkout: `rust-service-template-rest.codex-health-policy-hardening-20261005`.
Branch: `codex/health-policy-hardening-20261005`.
Base/HEAD during Definition: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.

Reviewer: `/root/health_definition/spec_review`, fresh `reviewer-agent`,
native `gpt-6-astra` / `high`, no inherited turns. Returned Specification Review
Result V1:

```text
candidate: intent.md SHA-256 e9af77cdd6425d354344ad06c2a907bff3f4efdeceed2babf413b8ce73cdd08b; spec.md SHA-256 ccd1629f3b43271e6f02d2189016153ac2e31c6a3f27109ecb0e80fba8ce2b74
verdict: PASS
findings: none
evidence_boundary: Fixed behavior contract, applicable workflow/evidence owners, current health fold/reader/scheduling and pooled session-admission source through the checkout's CodeGraph. No build, tests or external runtime verification.
reopen_owner: none
```

The reviewer checked both hashes before and after review. The sole subsequent
spec change is `Status: draft` to `Status: ready`; no semantic change invalidates
that review under the Transition owner's unchanged-scope rule.

Attempted falsifiers rejected by the reviewed contract:

- An expired Ready, including a round that crosses expiry, revives on failure.
- A failed check's freshness is mistaken for success, or drain/cancellation
  forges a completed check.
- A frozen Ready gauge hides expiry when no health endpoint is polled.
- Readback, acquisition or cleanup silently prolongs admission indefinitely,
  or rejection returns a usable pool.
- Necessary repairs expand into a new criticality, watchdog, routing or fleet
  policy, or mocked results are claimed as PostgreSQL observations.

## Continuation and proof limits

Technical Design chooses the freshness metric representation and its clock
limits, the smallest age-aware fold change, and bounded readback/cleanup at the
existing PostgreSQL owner. The coordinator accepted the separate 5-second
readback and 5-second cleanup bounds; readiness numerical policy is unchanged.
Design must not reinterpret those bounds as a fleet SLO or whole-bootstrap cap.

Only the three files in this directory changed during Definition. The eight
relative links in the specification were checked and resolve. No production,
test, general documentation, dependency or configuration file was changed;
no build/test execution, commit, push or PR happened in this phase. Required
repository checks remain at Implementation's final-validation boundary.

Root preflight reports pinned Rust 1.99.0 and Docker/OrbStack available. Its
shell requires `/Users/daniil/.cargo/bin` added to PATH for Cargo/Make; future
commands may use `rtk proxy env PATH="/Users/daniil/.cargo/bin:$PATH" ...`.
This is a capability note, not new proof or a validation gate.

Authority remains scoped implementation, validation, commit, push and one PR;
merge, deployment and infrastructure changes are outside the accepted outcome.
The dirty original checkout remains outside the writable scope.
