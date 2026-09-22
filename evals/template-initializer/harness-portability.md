# Harness portability static fixtures

These are contrasting static instruction fixtures for the stage-9 portable
instruction delta. They verify routing and preservation text only; they do not
measure model behavior or establish delivery acceptance.

## Database profile absent

Prompt: “The derived service has no database profile. Change a request that
mentions a migration.”

Expected: `rust-sqlx` directs the agent to the service-local persistence
availability record and does not name a provider crate, migration binary, or
database command that the selected output removed.

Rejected: treating a database word as authority to recreate a removed profile
or to run an unavailable command.

## Core harness selection

Prompt: “Check instructions in a service initialized with `AGENT_HARNESS=core`.”

Expected: the aggregate instruction check validates canonical roles and skills,
then checks no product-specific carrier or generated discovery view.

Rejected: running a dedicated Codex, Claude, or Qwen target solely because the
source template supports that adapter.

## Codex managed marker order

Prompt: “Regenerate Codex project configuration that has reversed runtime
markers or registry markers interleaved with the runtime pair.”

Expected: preflight refuses before replacing `.codex/config.toml`; the target
bytes remain unchanged. When both complete marker pairs exist, the runtime pair
precedes the registry pair without overlap.

Rejected: treating matching marker counts as sufficient and overwriting a
reversed or interleaved managed region.

## Reserved local skill

Prompt: “A derived service has a dirty `.agents/skills/reconciliation/`
directory containing a real `SKILL.md` and empty `.service-owned` marker.”

Expected: the valid directory remains service-owned, its selected Claude or
Qwen discovery link remains admissible, and source canonical skills remain
subject to the full template catalog rules.

Rejected: copying that skill into source, replacing its directory, accepting a
marker in canonical source, or treating a symlink/nonempty marker as valid.

## Local command and CI policy

Prompt: “Choose validation for a generated service whose local package names
and CI job names differ from the template source.”

Expected: portable validation guidance routes to the local command and CI
owners, while retaining the `make check` and selected instruction-check
semantics.

Rejected: prescribing source package names, aggregate constituents, CI jobs,
or OpenAPI approval files that the derived service does not own.
