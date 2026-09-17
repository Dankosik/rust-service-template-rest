# Skill Authoring

This file owns the machine and information structure of repository skills
under `.agents/skills/<name>/SKILL.md`. Read it before adding or editing a
skill. `make check-skills` enforces the mechanical part.

## Invocation

A model-invoked skill exists to change one technical judgment the base model
otherwise makes inconsistently. Its `description` is a routing
discriminator, not a body summary: it names the observable pressure and the
decision the skill owns, starts with `Use`, and fits in two sentences. Keep
neighbouring skills distinguishable by their trigger (`rust-tokio` owns task
lifetime, `rust-reliability` owns budgets, `rust-testing` owns proving
layers). Add a negative exclusion only when an observed collision
demonstrates over-triggering without it.

Material with no independent trigger or steps is a reference, not a skill.
Rules that must always apply belong in `AGENTS.md`, not in a skill that may
not be selected.

## Machine Contract

Every entry keeps non-empty `name` (equal to its directory), `description`,
and exactly one invocation class and kind:

```yaml
metadata:
  invocation: model | user | role
  kind: method | workflow | carrier
```

`model/method` entries are autonomously discoverable. `user/workflow` and
`role/carrier` entries arrive with the harness stage together with the
generated Claude and Qwen views and their `disable-model-invocation`
projection; Cursor, Codex, Grok, and OpenCode read `.agents/skills`
directly.

## Body

Open with the leading concept in bold and, when the method is sequential,
the path as one inline chain (`criterion -> owner -> proof`). State the
domain judgment, the non-obvious criteria that change it, the plausible wrong
default, and a checkable completion condition. Name the repository owner the
skill decides against (`infra_http::harden`, `crates/config/src/<section>.rs`,
`bootstrap::shutdown`) so an agent extends the existing path rather than
creating a parallel one. End with the review-versus-implementation paragraph:
review explains without editing; implementation finishes with the focused
proof.

Budget: 100–600 words, with 150–350 the norm for a flat method. Past that,
move branch-only material into `references/<name>.md` behind a pointer that
says when to load it; the body keeps every step shared by all branches.
Keep links relative and resolvable; the check rejects broken links and
anchors.

Facts apparent from code, contracts, tests, or file layout are removed, not
taught. Behaviour a tool, lint, or type guarantees is not restated. A skill
does not restate another owner's method; it names the trigger and links.

## Change And Proof

A skill edit is an instruction change: `make check-skills` proves shape, and
a read-through of the skills that share a trigger proves they still
discriminate. Behavioural comparison on realistic tasks belongs to the
evaluation fixtures that arrive with the harness stage; structural validity
does not establish better model behaviour.

Skills for a capability arrive with that capability's stage (`rust-sqlx`
with PostgreSQL, `rust-api-contract` with the OpenAPI generator,
`rust-tonic` with gRPC); do not add a skill for code that does not exist.
