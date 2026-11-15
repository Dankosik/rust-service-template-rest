# Prompt Maintenance

Read this owner only before editing agent instructions, tool descriptions,
roles, or skills.

## Evidence Boundary

OpenAI's [model guidance](https://developers.openai.com/api/docs/guides/latest-model)
owns model-specific prompting guidance. Select the guide for the actual target
model; resolve the latest model only when the request calls for it. Anthropic's
[context-engineering
guidance](https://claude.com/blog/the-new-rules-of-context-engineering-for-claude-5-generation-models),
[Claude Code documentation](https://code.claude.com/docs), and [prompting best
practices](https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/claude-prompting-best-practices)
own current Claude guidance; the guide for the target Claude model, such as
[Prompting Claude Fable
5.2](https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/prompting-claude-fable-5-2),
owns its model-specific behavior. Installed Grok native schemas own Grok controls;
the same context-engineering density rules apply when writing Grok carriers.
Cursor's [rules](https://cursor.com/docs/rules.md),
[skills](https://cursor.com/docs/skills.md), and
[subagents](https://cursor.com/docs/subagents.md) own current Cursor controls.
OpenCode's [agents](https://opencode.ai/docs/agents),
[skills](https://opencode.ai/docs/skills), [config](https://opencode.ai/docs/config),
and [CLI](https://opencode.ai/docs/cli) own current OpenCode 1 controls.
Product system-prompt history is observational evidence, not a harness
contract.

Re-derive old constraints against the current target model and harness. [Agent
Harness](agent-harness.md) owns native controls.

Audit the instruction path actually loaded, including conditional skills and
examples, for conflicting authority and unowned stops. Vendor examples are
candidates to adapt, not overrides of repository phase, delegation, or proof
contracts. For model-specific changes, record the effective model and source
date in the change evidence; repository policy edits do not require a new model
comparison merely to finish.

## Classification

Give each instruction one disposition before editing it.

| Instruction type | Disposition |
| --- | --- |
| Safety, authorization, secret, or irreversible-effect boundary | Keep explicit and fail closed in the earliest required owner. |
| Non-obvious product or team policy | Keep in its narrowest semantic owner. |
| Conditional method | Put behind its observable trigger. |
| Structure or process justified only by hypothetical future reuse | Remove; retain only when a current accepted constraint and owner require it. |
| Behavior guaranteed by a tool, schema, sandbox, or generator | Remove from prose or emit once only where the guarantee is absent. |
| Fact apparent from canonical code, contract, test, or file layout | Remove. |
| Restatement of another owner | Replace with a trigger and link. |
| Example that defines required behavior | Replace with a schema or fixture when possible; otherwise retain it with its contract. |
| Example of only the ordinary path | Remove unless it makes a non-obvious engineering choice visible; retain its selecting condition and the condition that changes the choice under [Skill Authoring](skill-authoring.md#decision-examples). |

Keep a hard boundary explicit; express judgment as the broadest principle that
preserves the measured behavior. If ordinary repository context already makes a
rule derivable, do not teach it again.

## Ownership And Interfaces

One file owns one meaning. Bootstrap files contain only rules needed before
routing. Routers choose the next owner; they do not explain that owner's method.
Move repeatable fields, allowed values, tool capabilities, sandbox guarantees,
and generated carriers into interfaces rather than narrative checklists.

The instruction graph flows `bootstrap -> router -> phase -> method ->
reference`. Lower layers do not reselect higher layers. A selector selects; it
does not restate the selected owner's method. Reference indexes contain only
selection pressures, links, and the decision effect of loading them.

Roles encode capability, permission, isolation, and context freshness. Skills
encode expertise and method. A model-invoked skill needs an independent
observable trigger that changes the method; aliases and explicit workflows do
not compete in autonomous discovery.

Write durable instructions as observable triggers, actions, completion criteria,
or stop conditions. State allowed behavior and reserve prohibitions for safety,
authority, or decisive exclusions. Expose only material the current task can
act on. A local stop identifies the dependent action and receiving owner;
[Decision Ownership](../AGENTS.md#decision-ownership) and [Parent-Owned
Recovery](spec-first-workflow/shared/transition.md#parent-owned-recovery) govern
whether the enclosing task continues. Preserve specialist rigor without
turning missing technical policy into a user decision.

`AGENTS.md` owns global authority, safety, Direct Work eligibility, and the local
validation budget; [Workflow Router](spec-first-workflow.md) owns structured
phases and shared workflow contracts; [Prompt Composition](prompt-composition.md)
owns task messages; `.agents/roles/` and `.agents/skills/` own role scopes and
domain judgment; and code, OpenAPI, tests, and generated sources own
repository/runtime facts.

[Skill Authoring](skill-authoring.md) owns skill invocation, metadata,
body/reference boundaries, and catalog constraints. Propagation to derived
repositories follows the portable-adoption policy when that lifecycle is in use.

## Change And Proof

Change one instruction group at a time and prefer removal. Preserve exact hard
boundaries and move text before rewriting its meaning. Compare bootstrap and
mandatory read sets before and after; word count proves context cost, not
behavior.

Also compare active semantic owners and catalog branches on representative
routes. A shorter file that adds another selector has increased context cost.

Instruction edits prove only an instruction-level change. Structural checks
prove ownership, generation, links, and shape—not model behavior.

A behavior-changing instruction edit records its observed pressure, the exact
routing, selection, or stop delta expected to change, one boundary case, and one
retention case in existing change evidence. Static consistency review and the
matching existing structural check can complete an accepted policy change;
label them as static evidence, not measured model behavior.

A claim of measured improvement requires a controlled comparison (same model,
effort, tools, and frozen loaded instruction set on fresh tasks with blinded
assessment, a no-skill control when claiming skill value, and unavailable
infrastructure or empty test commands separated from code failures). Do not make
that experiment a prerequisite for policy edits, promote an isolated trajectory
into a general claim, or build a new evaluation harness just to close the task.

Treat a reader's explanation as a claim to verify against the task, code,
failure paths, and resolved API before turning it into an instruction. Separate
contract violations and maintenance costs from aesthetic preferences; keep
evaluations focused on technical-choice ownership, disagreement resolution, and
recovery without user-facing technical questions, while retaining explicit
phase stops and unavailable-authority cases.
