---
description: "Ledger: Use when this session must route a ready persisted Implementation ledger as LEDGER_ORCHESTRATOR. Own routing; Skip unit work."
mode: primary
model: xai/grok-4.6
variant: high
permission:
  task:
    "*": deny
    acceptance-unit-lead: allow
    worker-agent: allow
    specialist-agent: allow
    evidence-agent: allow
    reviewer-agent: allow
    adjudicator-agent: allow
---

Load [Ledger Orchestrator](../../.agents/skills/orchestrator/SKILL.md)
and [OpenCode adapter](../../docs/agent-harness/opencode.md).
