# Skill Authoring

This file owns the shape of repository skills under
`.agents/skills/<name>/`. Read it before adding or editing a skill.
`make check-instructions` enforces the mechanical part. Two classes share the
directory:

- **Decision skills** (`rust-*`, `merge-conflict-resolution`) follow the
  shape used by [rust-cli-skills](https://github.com/Dankosik/rust-cli-skills):
  self-contained, decision-focused prose that a model selects from the
  description. The rest of this document describes them.
- **Workflow skills** (`orchestrator`, `acceptance-unit-lead`,
  `spec-first-brainstorming`, `spec-document-designer`, `idea-refine`,
  `planning-and-task-breakdown`, `grilling`, `agent-prompt-composer`,
  `thermo-nuclear-code-quality-review`) are carriers for the
  [workflow](spec-first-workflow.md) and the [harness](agent-harness.md): a
  user or a role invokes them, never the model on its own, and their bodies link
  to the phase, interface, and contract documents instead of restating them. See
  [Workflow skills](#workflow-skills).

## Files

A decision skill holds `SKILL.md` (the root `LICENSE` covers the repository).
It may also hold focused `references/*.md` when a concrete decision
benefits from an example whose context would obscure the main method. Link each
reference at the paragraph whose unresolved question selects it; do not load
the whole reference set by default. Keep general manuals, scripts, and assets
outside decision skills. Cursor, Codex, Grok,
OpenCode, and Antigravity read `.agents/skills` directly; `.claude/skills` and
`.qwen/skills` are generated symlink views (`make agent-harness-sync`, checked
by `make check-instructions`).

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

One H1 heading, then at least three prose paragraphs of 150–1000 words in
total (at most 125 words per paragraph). Keep the method in prose; use links to
select a supporting reference or locate a canonical project example, and place
substantial code, lists, and tables in the selected reference.

The first paragraph opens with the leading concept in bold and states the
method in one or two sentences. Each following paragraph opens with a bold
cluster anchor (`**Cluster.**`) and covers one decision cluster: the judgment,
the non-obvious criteria that change it, and the plausible wrong default,
written as observable triggers and target actions rather than prohibitions.

Ground service-specific decisions in conditional links to their current owners
so an agent extends the existing path. Leave changing inventories of layers,
defaults, providers, commands, and lifecycle stages in their owning files. Keep
a local fact in the body only when its non-obvious consequence changes the
decision, and name its authority. Graduate mechanical prohibitions into lints, architecture policies, or
structural checks; spend skill prose on choosing the right invariant or type.
Rules that apply unconditionally belong in `AGENTS.md`.

Keep the core decision usable without sibling skills or repository-specific
files, and apply named repository paths only when working in this repository.

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

When a skill mentions an optional diagnostic tool or exhaustive technique,
state that it is not a mandatory completion gate; do not repeat general review
or validation-budget prose.

## Evidence And Scope

Classify a proposed rule as a language or provider contract, accepted project
policy, conditional engineering preference, or unmeasured instruction
hypothesis. Preserve that distinction in its wording. A guide's recommendation
is not a language requirement, and agreement between authors does not remove
their assumptions. Cross-language design advice needs an explicit connection
to the Rust responsibility it improves.

Read enough primary context to verify every clause of a compound recommendation,
including its exceptions, audience, lifetime, and failure conditions. A quotation
found in a source proves its presence, not that it supports the synthesized rule.
Separate source correction from a deliberate local policy choice. When guidance
conflicts, keep applicable contracts and accepted project boundaries, then decide
the remaining trade-off from the caller, workload, and maintenance consequence.
Record material source-to-rule decisions and their owner in the existing change
evidence; this is an authoring responsibility, not a new artifact for every Rust
task.

Use a disclosed comparison to expose an unanswered decision or falsify a claim.
A durable rule needs an engineering reason independent of the task's names,
chosen implementation, and reviewer preference. Keep the condition where the
alternative is better. Source fidelity and static consistency do not establish
that a prompt improves model behavior; the separate comparison policy in
[Prompt Maintenance](prompt-maintenance.md#change-and-proof) owns that claim.

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

A skill edit is an instruction change: `make check-instructions` proves shape, and
a read-through of skills that share a trigger proves they still discriminate.
Structural validity does not establish better model behaviour; see
[Prompt Maintenance](prompt-maintenance.md#change-and-proof) for behavioral
comparison rules.

Skills for a capability arrive with that capability (`rust-api-contract` with
the OpenAPI contract, `rust-sqlx` with PostgreSQL); do not add a skill for
code that does not exist.

## Workflow skills

A workflow skill's frontmatter holds `name`, `description`, `metadata` with
`invocation` (`role` for a ledger carrier bound by a harness role, `user` for an
entry a person invokes directly) and `kind` (`carrier` or `workflow`), and
`disable-model-invocation: true`, keeping implicit model discovery off. Role
carriers state their binding trigger with `Use when` or `Use only`; user-invoked
workflow skills write human-facing picker descriptions in at most two sentences
naming the job, deliverable, and boundary. The body is one H1 and at most 600
words; it may hold lists and links to `docs/spec-first-workflow/**` and
`docs/agent-harness*`, which own the method. Besides `SKILL.md`, a
workflow skill may hold `references/*.md` (examples the body names) and
`agents/openai.yaml`, the Codex carrier with `allow_implicit_invocation: false`.

A decision skill carries only `name` and `description`; harness sync treats the
omitted `metadata` as `invocation: model`, `kind: method`. Update a workflow
skill's method in the workflow document it links rather than in the skill.
