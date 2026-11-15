# Instruction Validation

Use for changes to `AGENTS.md`, the harness bootstrap files (`CLAUDE.md`,
`QWEN.md`, `Grok.md`, `opencode.json`, `.cursor/rules`), the skills under
`.agents/skills`, the roles under `.agents/roles`, the workflow and harness
documents, or the generated carriers.

| Surface | Command |
| --- | --- |
| Skill shape for both classes ([Skill Authoring](../skill-authoring.md)) | `make check-skills` |
| Role carriers for Codex, Claude, Qwen, Grok, Cursor, and OpenCode against `.agents/roles` and `.agents/role-classes` | `make agent-roles-check`; regenerate with `make agent-roles-sync` |
| Selected Codex project runtime and role registry (`.codex/config.toml`) against `.agents/codex-project.toml` | `make codex-agents-check`; regenerate with `make codex-agents-sync` |
| Selected Claude and Qwen skill discovery views (`.claude/skills`, `.qwen/skills` symlinks) | `make claude-skills-check`, `make qwen-skills-check`; regenerate with the `*-sync` twins |
| Canonical sources and selected generated views | `make check-instructions` (part of `ALLOW_FULL=1 make check`; what CI runs on the instruction surface) |
| Every relative link and `#fragment` in the instruction chain resolves | `make docs-check` |

At final validation, review the changed instruction chain for consistency
(every link resolves, neighbouring skill triggers still discriminate, a
changed role regenerates its selected carriers) and run `make check-instructions`.
This completes an instruction change; application builds and model-trajectory
comparisons run only when explicitly requested. Body-only skill edits leave the
generated symlink views unchanged; regenerate symlinks only when adding or
removing a skill. `scripts/agent-roles-sync.sh` generates both the canonical
role carriers and the session-agent carriers (`acceptance-unit-lead` and
`orchestrator`); hand-maintained adapter files outside that generator (such as
`.opencode/commands/orchestrator.md` and `.opencode/plugins/task-subagents.js`)
are edited directly and reviewed with their adapter.

Cursor, Codex, Grok, and OpenCode read `.agents/skills` directly; selected
Claude Code and Qwen Code installations read their generated views. [Prompt
Maintenance](../prompt-maintenance.md) owns how an instruction change is made
and proven; these checks prove static shape and ownership rather than model
behavior.
