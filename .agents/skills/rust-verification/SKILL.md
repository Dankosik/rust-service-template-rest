---
name: rust-verification
description: "Evidence boundary. Use for Rust verification-only work or an unresolved decision about whether existing evidence supports a claim at its stated scope."
---

# Rust Verification

Translate the requested claim into an observable result and a failure that the
proposed evidence would detect. Then inspect what actually ran. A green command
is evidence only for the paths, inputs, configurations, and assertions it
exercised; the command's name or aggregate size cannot widen that scope.

## Claim and observation

Compilation establishes compatibility for the selected targets, not runtime
behavior. A crate's test suite establishes its exercised assertions, not every
behavior in that crate. A mounted-router test can observe middleware while
missing connection limits. A real database result can establish a transaction
outcome under its fixture without certifying a deployed database topology.
Instruction shape checks establish structure, not improved model behavior.
Use these distinctions to choose evidence that crosses the boundary in the
claim, rather than adding unrelated checks for confidence.

Inspect selection and completion: a filter matching zero tests, a skipped
integration case, or a runner that swallowed a failing child cannot carry the
intended claim. Record setup failures separately from product failures. Check
that the asserted observation discriminates the failure: a delay followed by a
successful response does not by itself prove the required ordering, and a file's
presence does not prove the process consumed it.

## Identity and reuse

Retain enough identity to reproduce or compare the result: candidate inputs,
command and filters, relevant features and configuration, toolchain, dependency
environment, and actual outcome. Distinguish fresh execution from reuse. Reuse
an adequate receipt when its observed surface and relevant inputs remain valid;
a change elsewhere need not invalidate it, while a fixture, environment, or
feature change can invalidate it without touching the asserted function.
Rerun the invalidated observation instead of treating every repair as a reason
for a full restart.

For ordinary check selection and completion use
[AGENTS](../../../AGENTS.md#validation-budget); for mixed or specialized surfaces
use [Validation Routing](../../../docs/validation-routing.md). Load the
[Evidence Contract](../../../docs/spec-first-workflow/shared/evidence-contract.md)
when deciding claim scope, reuse, additional required proof, or unavailable
infrastructure. A missing optional environment narrows the result; it does not
justify constructing a new environment to make local completion possible.
Required proof remains outstanding at its own scope.

Verification-only work is read-only. Return the supported claim or the exact
missing observation and its owner; route a discovered defect to an authorized
repair rather than silently expanding verification into implementation. Local
results cannot establish an unobserved CI run, published artifact, remote
telemetry receipt, deployment, or production behavior. Likewise, self-review
cannot establish independent review. Preserve the requested boundary even when
a narrower result is easier to obtain.
