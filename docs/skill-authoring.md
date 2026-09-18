# Skill Authoring

This file owns the shape of repository skills under
`.agents/skills/<name>/`. Read it before adding or editing a skill.
`make check-skills` enforces the mechanical part. Two classes share the
directory:

- **Decision skills** (`rust-*`, `merge-conflict-resolution`) follow the
  shape used by [rust-cli-skills](https://github.com/Dankosik/rust-cli-skills):
  self-contained, decision-focused prose that a model selects from the
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

A decision skill is one directory holding exactly `SKILL.md` and `LICENSE`
(a copy of the repository license). No reference trees, scripts, or assets;
material that needs them is documentation, not a skill. Cursor, Codex, Grok,
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

One H1 heading, then four to seven prose paragraphs of 250–500 words in
total. No links, lists, tables, sub-headings, or code blocks; name APIs,
crates, modules, and files in prose. The first paragraph opens with the
leading concept in bold, states the method in one or two sentences, and ends
with the standing clause: honor supplied requirements and preserve settled
choices outside the requested change; resolve only what the task leaves
open. Each following paragraph is one decision cluster: the judgment, the
non-obvious criteria that change it, and the plausible wrong default, written
as observable triggers and actions rather than prohibitions.

Ground the skill in this repository's owners so an agent extends the
existing path: the hardened chain, the config section files, the health
crate, the shutdown plan in bootstrap, the make targets. Facts apparent from
code, tests, or file layout are not restated, and behaviour a lint or type
already guarantees is not taught. Rules that must always apply belong in
`AGENTS.md`, not in a skill that may not be selected.

The closing paragraph separates review from implementation: review explains
the risk and the smallest justified change without editing; implementation
finishes with the focused proof for the changed behaviour and reports what
was exercised. Name what the skill does not require, so a mention of a tool
or technique is not read as a completion gate.

## Change And Proof

A skill edit is an instruction change: `make check-skills` proves shape, and
a read-through of the skills that share a trigger proves they still
discriminate. Behavioural comparison on realistic tasks belongs to the
evaluation fixtures that arrive with the harness stage; structural validity
does not establish better model behaviour.

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
