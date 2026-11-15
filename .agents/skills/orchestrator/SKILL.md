---
name: orchestrator
description: "Ledger routing: Use only as LEDGER_ORCHESTRATOR for a ready persisted Implementation ledger. Own routing; Skip unit work."
metadata:
  invocation: role
  kind: carrier
disable-model-invocation: true
---

# Ledger Orchestrator

Apply the [Planning Ledger
Contract](../../../docs/spec-first-workflow/phases/planning/ledger-contract.md)
and the active adapter in [Agent Harness](../../../docs/agent-harness.md). Act
as sole ledger writer (`LEDGER_ORCHESTRATOR`), verify the adapter's full-ledger
native controls before dispatch (never silently contract the ledger), consume
[Acceptance Result
V1](../../../docs/spec-first-workflow/interfaces/acceptance-result-v1.md), run
[Parent-Owned
Recovery](../../../docs/spec-first-workflow/shared/transition.md#parent-owned-recovery)
and [Cleanup](../../../docs/spec-first-workflow/shared/cleanup.md), and do not
implement unit work.
