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

A decision skill holds `SKILL.md` and `LICENSE` (a copy of the repository
license). It may also hold focused `references/*.md` when a concrete decision
benefits from an example whose context would obscure the main method. Link each
reference at the paragraph whose unresolved question selects it; do not load
the whole reference set by default. Keep general manuals, scripts, and assets
outside decision skills. Cursor, Codex, Grok,
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

One H1 heading, then at least four prose paragraphs of 250–1000 words in
total. The ceiling guards context cost, not completeness: a rule no compiler or
lint enforces earns its words. Keep the method in prose; use links to select a supporting reference or
locate a canonical project example. Substantial code, lists, and tables belong
in the selected reference rather than the entrypoint. The first paragraph opens with the
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

Separate a transferable method from this service's conventions. Apply a named
repository owner only when working in that repository; a standalone crate must
not inherit its crate graph, specs tree, telemetry, or validation machinery.
Keep the core decision and completion condition usable when no project policy
or sibling skill is supplied. Put portable decision examples inside the skill's
own references directory; repository examples are conditional pointers, not
required files in an independently installed skill. Preserve the current
project's authority and constraints rather than inventing replacements.

Judge correctness against accepted requirements and applicable language,
protocol, and provider contracts. Existing code and tests describe the current
implementation and can contain defects. A project example locates a candidate
solution and its consumers; its presence is not proof that its choices are
required or correct. Check the demonstrated invariant and its assumptions
before recommending it.

Make design preferences conditional on the guarantee or comprehension cost
they improve. A spelling, a parameter count, or a fixed number of lines does
not by itself select an enum, builder, trait, or helper. Preserve explicit
language, protocol, safety, and project rules; for a design preference, state
the condition that selects a simpler alternative. Judge the complete operation:
moving a check or hiding a failure behind an abstraction does not remove its
responsibility. Teach the invariant the mechanism establishes and the work
that still belongs to its caller.

The closing paragraph separates review from implementation: review explains
the risk and the smallest justified change without editing; implementation
finishes with the focused proof for the changed behaviour and reports what
was exercised. Name what the skill does not require, so a mention of a tool
or technique is not read as a completion gate.

## Decision Examples

An example earns its place by making a choice, invariant, ownership relation,
or effect easier to see. Explain the requirement that selects it, the property
the code establishes, and what remains the caller's responsibility. State a
changed condition that would select another solution; show a pair when the
different code makes that boundary clearer. A syntax reminder or an ordinary
happy path alone adds no decision guidance.

Prefer prose when it states the decision just as clearly. Remove a code block
that only wraps a standard API, repeats the method, or needs more caveats than
the relation it reveals. A type-enforced invariant can be demonstrated when
the open question is which invariant to encode; explaining the syntax alone
does not earn a reference. Keep the core method usable without reading every
example, and select a specialized technique only under the condition that
actually requires it.

Keep the smallest context that preserves the argument: relevant types,
ownership signatures, and the effect or resource-lifetime boundary. Mark a
fragment's supplied context explicitly. Prefer a link to the existing owner
and its consumer for architecture or lifecycle examples; use a standalone
example when project details obscure a transferable language-level choice.
Examples do not prescribe a new type, trait, helper, crate, or parallel path.
Check all exits when a temporary representation, cancellation, or failure can
change the outcome. Choose standard types and current supported APIs first.

Keep each reference focused on one decision family, normally within 500 words
of explanation. This is an authoring guide, not a quota to fill. Preserve the
leading concept and routing discriminator; examples support the method without
becoming additional skill selectors. Link primary human-facing sources for
the reasoning and identify version-sensitive assumptions. Revisit an example
when its canonical owner, API, or requirement changes. Do not duplicate source
code whose ongoing correctness depends on that owner.

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
