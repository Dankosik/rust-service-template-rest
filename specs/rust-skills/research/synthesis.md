# Stage 7 research synthesis: Rust skills

Decisions for the first skill set, pulled forward from roadmap stage 7 so the
remaining stages are implemented through skills. Sources read on 2026-09-17:
the Go template's `.agents/skills/go-*` set and `docs/skill-authoring.md` /
`docs/prompt-maintenance.md`; Dankosik's standalone packs
[rust-cli-skills](https://github.com/Dankosik/rust-cli-skills) (16 skills),
[golang-backend-skills](https://github.com/Dankosik/golang-backend-skills)
(16 skills), and [agent-skills](https://github.com/Dankosik/agent-skills)
(the language-neutral disciplines the Go template vendors as
`docs/universal-disciplines`).

## What exists and what is reused

| Source | Reused | Not reused |
| --- | --- | --- |
| rust-cli-skills | The compact one-file shape (a leading concept in bold, one decision per skill, review-vs-implementation closing paragraph, "honor supplied requirements" clause); wording for idiomatic, errors, concurrency, testing, debugging, performance, build | CLI-only decisions (arguments, terminals, filesystem, child processes, distribution); the pack's explicit refusal to prescribe an async runtime, which this template has fixed |
| golang-backend-skills | Backend framing for HTTP, observability, integrations | Go-specific mechanics |
| Go template `go-*` | Per-skill grounding in repository owners ("read the existing owner before adding policy: `internal/infra/http` owns…"), `metadata: invocation/kind`, the trigger-as-description rule, completion criteria | Decision/Review result records (`RouteNode{...}`, `BudgetPath{...}`), the specialist contract, and reference trees: they belong to the spec-first workflow and harness that arrive with stage 6 |
| agent-skills disciplines | Linked when their capability exists | Vendoring now: no jobs, messaging, PostgreSQL, or auth capability exists to reach them from |

No community Rust backend skill pack matching this template's decisions was
found worth adopting; the value of these skills is that they encode this
repository's own decisions (crate boundaries, the hardened chain, the problem
catalog, the shutdown budget, the research-first rule), which no external pack
can carry.

## Decisions

- Canonical location `.agents/skills/<name>/SKILL.md`, read directly by
  Cursor ([skills documentation](https://cursor.com/docs/skills.md) lists
  `.agents/skills/` as a project-level path and also loads `.claude/skills/`
  and `.codex/skills/` for compatibility), Codex, Grok, and OpenCode. Claude
  and Qwen discovery views are generated with the harness stage.
- Shape: one `SKILL.md` per skill, 150–350 words, frontmatter `name`,
  `description` (a routing discriminator: the observable pressure and the
  decision it owns), `metadata.invocation: model`, `metadata.kind: method`.
  No `references/` until a real branch-only pressure appears.
- Every skill names the repository owner it decides against, so an agent
  extends the existing path instead of creating a parallel one.
- The research-first rule becomes a skill (`rust-dependencies`) so it fires
  on the observable trigger (a new crate, feature, or toolchain change), not
  only from the roadmap.
- Skills for capabilities that do not exist yet (`rust-api-contract`,
  `rust-sqlx`, `rust-tonic`, `rust-delivery-platform`, messaging, jobs)
  arrive with their stage. `rust-test-strategy` and `rust-test-implementation`
  stay merged as `rust-testing` until the workflow stage separates
  implementation from review lenses.
- A structural check (`make check-skills`) validates frontmatter, name/dir
  agreement, body budget, and relative links; it proves shape, not model
  behaviour. Behavioural evaluation fixtures are deferred to the harness
  stage, when the reviewer roles that consume them exist.

## Set

| Skill | Leading concept | Ported from |
| --- | --- | --- |
| `rust-coder` | Earliest owner | go-coder, rust-implement |
| `rust-idiomatic` | Contracts | go-idiomatic, rust-idiomatic |
| `rust-tokio` | Task ownership | go-concurrency, rust-concurrency |
| `rust-axum` | Route tree | go-chi, go-http |
| `rust-errors` | Failure semantics | rust-errors, go-idiomatic error contracts |
| `rust-config` | One key, one owner | configuration-source-policy, go-coder |
| `rust-observability` | Operator evidence | go-observability |
| `rust-reliability` | Budget arithmetic | go-reliability |
| `rust-security` | Attacker path | go-security |
| `rust-testing` | Observable failure | go-test-strategy, go-test-implementation, rust-testing |
| `rust-performance` | Evidence | rust-performance, rust-memory, go-performance |
| `rust-debugging` | Causality | rust-debugging, go-systematic-debugging |
| `rust-structural-quality` | Deletion test | go-structural-quality, rust-design |
| `rust-dependencies` | Verified resolution | rust-build, go-modern-version, the research-first rule |
| `rust-verification` | Evidence boundary | go-verification-before-completion |
