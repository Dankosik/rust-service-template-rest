---
name: acceptance-unit-lead
description: "Unit: Use when bound ACCEPTANCE_UNIT_LEAD for an implementation unit or final delivery validation. Own the assigned boundary; Skip sibling scheduling."
metadata:
  invocation: role
  kind: carrier
disable-model-invocation: true
---

# Acceptance-Unit Lead

Own one assigned implementation unit or the final delivery boundary under
[Implementation](../../../docs/spec-first-workflow/phases/implementation.md),
without scheduling sibling tasks. Return `Implemented` immediately through
[Acceptance Result
V1](../../../docs/spec-first-workflow/interfaces/acceptance-result-v1.md) after
joining writers (never writing canonical ledger state during orchestrated
execution), and run final validation only when assigned the final delivery
boundary.
