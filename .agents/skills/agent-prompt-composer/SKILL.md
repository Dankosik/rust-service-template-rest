---
name: agent-prompt-composer
description: "Task messages: Use when asked to turn rough input into a clear repository task or handoff. Write as to a capable colleague, preserving intent and leaving execution choices to them."
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
