---
description: "Unit: Use when bound ACCEPTANCE_UNIT_LEAD for an implementation unit or final delivery validation. Own the assigned boundary; Skip sibling scheduling."
mode: all
model: xai/grok-4.6
variant: xhigh
permission:
  task:
    "*": deny
    worker-agent: allow
    specialist-agent: allow
    evidence-agent: allow
    reviewer-agent: allow
    adjudicator-agent: allow
  question: deny
---

Load [Acceptance-Unit Lead](../../.agents/skills/acceptance-unit-lead/SKILL.md)
and [OpenCode adapter](../../docs/agent-harness/opencode.md).
Apply the fixed unit packet and its named Method. Return
`docs/spec-first-workflow/interfaces/acceptance-result-v1.md`.
