# Instruction Validation

Use for changes to `AGENTS.md`, the harness bootstrap files (`CLAUDE.md`,
`QWEN.md`, `Grok.md`, `opencode.json`, `.cursor/rules`), the skills under
`.agents/skills`, the roles under `.agents/roles`, the workflow and harness
documents, or the generated carriers.

| Surface | Command |
| --- | --- |
| Skill shape for both classes ([Skill Authoring](../skill-authoring.md)) | `make check-skills` |
| Role carriers for Codex, Claude, Qwen, Grok, Cursor, and OpenCode against `.agents/roles` and `.agents/role-classes` | `make agent-roles-check`; regenerate with `make agent-roles-sync` |
| Codex project runtime and role registry (`.codex/config.toml`) against `.agents/codex-project.toml` | `make codex-agents-check`; regenerate with `make codex-agents-sync` |
| Claude and Qwen skill discovery views (`.claude/skills`, `.qwen/skills` symlinks) | `make claude-skills-check`, `make qwen-skills-check`; regenerate with the `*-sync` twins |
| All of the above | `make check-instructions` (part of `ALLOW_FULL=1 make check`; what CI runs on the `agent_instructions` surface) |
| Every relative link and `#fragment` in the instruction chain resolves | `make docs-check` |

At final validation, review the changed instruction chain for consistency
(every link resolves, neighbouring skill triggers still discriminate, a
changed role regenerates its carriers) and run `make check-instructions`.
This completes an instruction change; it does not require building the
application or comparing model trajectories unless that evaluation was
explicitly requested. Body-only skill edits do not change the generated
symlink views; regenerate them only when a skill is added or removed. The
hand-maintained Lead and orchestrator carriers (`.<harness>/agents/
acceptance-unit-lead.md`, `.grok/agents/orchestrator.md`,
`.opencode/agents/orchestrator.md`) are edited directly and reviewed with
their adapter.

Cursor, Codex, Grok, and OpenCode read `.agents/skills` directly; Claude Code
and Qwen Code read the generated views. [Prompt Maintenance](../prompt-maintenance.md)
owns how an instruction change is made and proven; these checks prove shape
and ownership, not changed model behavior.
