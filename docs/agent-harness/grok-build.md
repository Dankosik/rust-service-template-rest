# Grok Build Harness Adapter

Use installed Grok primary-session and `spawn_subagent` controls as native
authority; structured flags and spawn fields outrank public prose.

Default subagent depth is 1; `subagents.max_depth` and
`GROK_SUBAGENTS_MAX_DEPTH` configure depth (not a fixed product ceiling). Retain
primary-session Leads until effective depth and child spawn controls establish
a deeper native route.

## Native Map

- For each preparation phase, start a fresh Grok primary session in the
  coordinator's checkout with `--prompt-file` carrying the phase brief and
  `--output-format json` for the result.
  Preserve the authorized permission settings. Omit `--resume`, `--continue`,
  and `--fork-session`. Keep the returned session identity for same-phase
  follow-ups.
- The ordinary Grok primary session is the root carrier (`--agent orchestrator`
  is optional). Bind `LEDGER_ORCHESTRATOR` under
  [Implementation](../spec-first-workflow/phases/implementation.md#carrier), or
  `ACCEPTANCE_UNIT_LEAD` for a root-local Lead; Direct Work stays on this
  session.
- Dispatch each Acceptance-Unit Lead as a primary session: `grok --agent
  acceptance-unit-lead --always-approve --output-format json`, unit locator via
  `--prompt-file` or `-p`, and `--model`/`--effort` when differing. When
  [Agent Harness](../agent-harness.md) selects isolation, create a Git worktree
  and pass its absolute path via `--cwd` (headless `-p` does not create a
  worktree from `--worktree`). Retain `sessionId`, continue with `--resume`
  (never `--restore-code` routinely), and consume
  [Acceptance Result](../spec-first-workflow/interfaces/acceptance-result-v1.md).
- Leads delegate via `spawn_subagent` (`worker-agent`, `specialist-agent`,
  `evidence-agent`, `reviewer-agent`, `adjudicator-agent`) with `prompt`,
  `description`, `subagent_type`, `background`, `isolation`, `resume_from`,
  `cwd`, and optional `model` when schema permits user-requested selection.
  Never pass `capability_mode` (tools come from `.grok/roles/<name>.toml`
  `default_capability_mode`). Children lack `ask_user_question` and pin
  `permission_mode: bypassPermissions`; read-only vs mutable is role capability,
  not plan mode.
- `/goal` is optional on the root for long-running ledgers; Rhai workflows are
  not Ledger Orchestrator.

## Models And Dispatch

Use `grok-4.6` or `grok-build` at `high` effort for Orchestrator; Lead
`--effort` follows [Agent Harness](../agent-harness.md). Children inherit the
Lead model unless a role pins stronger, and use role `reasoning_effort`
(`spawn_subagent` has no `effort` field; optional `model` is ignored with
`resume_from`). Preserve user-selected models and never encode models only in
prompt prose.

Apply [Context And Lifetime](../agent-harness.md#context-and-lifetime) and
[Nested Execution](../agent-harness.md#nested-execution). Retain every Lead
`sessionId` and child `subagent_id` before waiting (missing identity or
rejected spawn is a carrier failure). Omit `resume_from` on new lanes and
initial reviews; resume only a completed child from the same parent and agent
type (replacing the locator if a new ID returns), and track primary Leads and
subtrees explicitly (stopping one primary does not stop siblings).

At final delivery, use [Review](../spec-first-workflow/shared/review.md) and
[Implementation Review](../spec-first-workflow/phases/implementation-review.md).
