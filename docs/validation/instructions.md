# Instruction Validation

Use for changes to `AGENTS.md`, `CLAUDE.md`, the skills under
`.agents/skills`, or [Skill Authoring](../skill-authoring.md).

| Surface | Command |
| --- | --- |
| Skill shape: frontmatter, name and directory agreement, trigger, prose-only body, word budget, LICENSE copy | `make check-skills` |
| Every relative link and `#fragment` in the instruction chain resolves | `make docs-check` |

At final validation, review the changed instruction chain for consistency
(every link resolves, neighbouring skill triggers still discriminate) and run
the structural check. This completes an instruction change; it does not
require building the application or comparing model trajectories unless that
evaluation was explicitly requested. Behavioural fixtures arrive with the
agent harness stage.

CI runs `make check-skills` on the `agent_instructions` surface without
installing the Rust toolchain. Cursor, Codex, Grok, and OpenCode read
`.agents/skills` directly; the Claude and Qwen views arrive with the harness
stage.
