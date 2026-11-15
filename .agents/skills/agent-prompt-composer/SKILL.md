---
name: agent-prompt-composer
description: "Turn rough notes into a clear, self-contained repository task or handoff message that preserves intent and leaves execution choices to the recipient."
metadata:
  invocation: user
  kind: workflow
disable-model-invocation: true
---

# Agent Prompt Composer

Apply [Prompt Composition](../../../docs/prompt-composition.md) to turn the
user's input into a clear, ready-to-send work message for a capable colleague.
Use [Intake](../../../docs/spec-first-workflow/phases/intake.md) only when a
missing user-owned decision materially changes the request.
