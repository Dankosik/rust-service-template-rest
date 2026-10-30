---
name: rust-delivery-platform
description: "Gate evidence. Use when CI selection, build inputs, image checks, publication or recovery changes which candidate may ship and what its delivery evidence actually establishes."
---

# Rust Delivery Platform

Follow one candidate from source inputs to the artifact a consumer runs. For
each claimed guarantee, identify the gate that observes it, the artifact it
consumes and the failure that prevents promotion. A green status without the
intended observation is not evidence of delivery readiness.

## Distinguish selection from execution

A surface classifier can legitimately skip a gate when the change does not
reach its claim. A selected gate that swallows a failed command or reports
success without exercising its input is a different outcome. Follow changes to
classification through local and CI consumers; otherwise one route can look
covered while another bypasses it. Unknown changed surfaces need the existing
fail-closed policy, not an inferred safe skip.

Gate cost should follow what it observes. A fast syntax check may catch a
workflow error without proving image lifecycle; an image build may prove
packaging without proving readiness, drain or release provenance. Keep these
claims distinct, reuse adequate coverage and add a gate only for a current gap.
Removing or waiving an existing requirement belongs to its policy owner;
a convenient local pass cannot substitute for it.

## Keep identity through transformations

Record the source revision and material build inputs that determine the
candidate, then retain the produced digest through scans, signing, attestation
and publication. Checking one tag and publishing a rebuilt tag breaks that
identity even when both have the same human name. A scanner also needs the
metadata for its claimed surface: a green image scan without embedded Rust
dependency information cannot establish the dependency claim.

Publishing, verifying a registry receipt, promoting a tag and deploying are
separate effects. Read back the digest the consumer will resolve; successful
upload alone does not establish that promotion selected it. Retain partial
promotion outcomes so retry cannot hide which names already moved. Required
signer identity and provenance follow the actual workflow topology, so moving
publication between carriers can change what consumers trust.

A rollback requires a usable prior artifact plus compatible configuration,
schema and payload expectations. Reverting source or repointing a tag alone
does not establish recovery. Choose recovery evidence for the failure the
change could cause rather than treating a successful startup as all-purpose
rollback proof.

For changed classifier/job, tool pin, image, signature or publication policy,
load [CI/CD Production Readiness](../../../docs/ci-cd-production-ready.md) and
its linked current scripts/workflows. [AGENTS](../../../AGENTS.md#validation-budget)
and the selected execution owner determine local versus CI proof and timing.
This method grants no heavy-check, publication or deployment authority; report
only the observed candidate and leave unobserved CI, registry or runtime claims
explicitly pending.
