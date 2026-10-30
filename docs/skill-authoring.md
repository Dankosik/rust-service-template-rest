# Skill Authoring

This file owns the shape of repository skills under
`.agents/skills/<name>/`. Read it before adding or editing a skill.
`make check-skills` enforces the mechanical part. Two classes share the
directory:

- **Decision skills** (`rust-*`, `merge-conflict-resolution`) provide
  decision-focused methods that a model selects from the
  description. The rest of this document describes them.
- **Workflow skills** (`orchestrator`, `acceptance-unit-lead`,
  `spec-first-brainstorming`, `spec-document-designer`, `idea-refine`,
  `planning-and-task-breakdown`, `grilling`, `agent-prompt-composer`,
  `thermo-nuclear-code-quality-review`) are ported from the Go template with
  path changes only. They are carriers for the [workflow](spec-first-workflow.md)
  and the [harness](agent-harness.md): a user or a role invokes them, never
  the model on its own, and their bodies link to the phase, interface, and
  contract documents instead of restating them. See [Workflow skills](#workflow-skills).

## Files

A decision skill holds `SKILL.md`, `LICENSE` (a copy of the repository
license), and optionally `references/*.md`. Scripts, assets, and metadata
carriers are outside this class. Cursor, Codex, Grok,
and OpenCode read `.agents/skills` directly; `.claude/skills` and
`.qwen/skills` are generated symlink views (`make claude-skills-sync`,
`make qwen-skills-sync`, checked by `make check-instructions`).

## Frontmatter

Exactly two keys:

```yaml
---
name: rust-tokio
description: "Ownership. Use when Rust service tasks, select arms, locks across awaits, channels, blocking work, or startup and shutdown ordering need lifetime, cancellation, or completion guarantees."
---
```

`name` equals the directory name (lowercase, hyphens). `description` is a
routing discriminator in at most two sentences: an optional leading concept,
then `Use when`, `Use for`, or `Use to` with the observable pressure and the
decision the skill owns. Descriptions select a decision, not every task that
touches Rust; keep neighbouring triggers distinguishable (`rust-tokio` owns
task lifetime, `rust-reliability` owns budgets, `rust-testing` owns proving
layers).

## Body

Start with one H1 heading and a nonempty body. Subheadings, prose, lists,
tables, links, and fenced code are allowed; heading checks ignore fenced
code. There is no word or paragraph quota, required bold opening, standing
clause, or closing ritual. State the decision, the evidence that changes it,
and the plausible wrong default as observable pressures and actions.

Keep transferable judgment in the method. Link directly to a local semantic
owner when a concrete pressure needs its policy; do not copy the service's
current crate map, runtime path, or command inventory. Mandatory rules remain
in `AGENTS.md` and the applicable architecture, configuration, validation, and
workflow owners even when an optional skill is not selected. Execution,
review authority, and completion remain with those owners.

## References and examples

Link each reference directly from the entrypoint with a discriminating loading
trigger and the decision it changes. Keep the essential principle and concrete
contrast inline, so skipping an example cannot hide an invariant. References
provide supporting context, not a mandatory policy owner or another router.
Review judges relevance and entrypoint sufficiency; `make docs-check` owns local
links and fragments, while `make check-skills` checks supported file shape.

For a useful contrast, state its premise, protected property, accepted cost,
and the changed condition that reverses its recommendation. Compilable examples
carry assertions for the stated boundaries. Label incomplete library examples
“fragment, not compiled” and fence them as text; do not disguise them as ignored
Rust tests. Prefer a plain explanation when a fragment adds no decision value.

For changed std-only Markdown examples, run
`rustdoc --test --edition=2024 <reference.md>` from this checkout under the
toolchain in `rust-toolchain.toml`. Require nonzero executed tests and retain
the command, toolchain, file hash, and result in the task's completion evidence.
Use the available native rustdoc locator; no Cargo package, runner, or dependency
is needed. This authoring proof is separate from `check-skills` and
`check-instructions`, which remain toolchain-free.

## Change And Proof

A skill edit is an instruction change: `make check-skills` proves shape, and
a read-through of the skills that share a trigger proves they still
discriminate. Structural validity and checked examples establish neither native
loading nor better model behaviour. A behavioural comparison requires its own
accepted evaluation; it is not a routine skill-edit gate.

Skills for a capability arrive with that capability's stage
(`rust-api-contract` came with the OpenAPI contract, `rust-sqlx` comes with
PostgreSQL, `rust-tonic` with gRPC); do not add a skill for code that does
not exist.

## Workflow skills

A workflow skill's frontmatter holds exactly `name`, `description`,
`metadata` with `invocation` (`role` for a ledger carrier bound by a harness
role, `user` for an entry the user invokes) and `kind` (`carrier` or
`workflow`), and `disable-model-invocation: true`, so no harness selects it
implicitly. The description states its trigger the same way and stays within
two sentences. The body is one H1 and at most 600 words; it may hold lists
and links to `docs/spec-first-workflow/**`, `docs/agent-harness*`, and the
`.agents/contracts/*`, because the skill is the entry point and those
documents own the method. Besides `SKILL.md` and `LICENSE`, a workflow skill
may hold `references/*.md` (examples the body names) and `agents/openai.yaml`,
the Codex carrier with `allow_implicit_invocation: false`.

A decision skill carries no `metadata` block; the harness sync treats its
absence as `invocation: model`, `kind: method`. Changing a workflow skill's
method means changing the workflow document it links, not the skill.
