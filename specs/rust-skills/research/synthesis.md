# Stage 7 research synthesis: Rust skills

The initial decisions below are historical. The 2026-11-06 revision at the end
records the current shape and scope; docs/skill-authoring.md owns its policy.

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
- Shape: exactly the rust-cli-skills shape, at the maintainer's request.
  One directory per skill holding `SKILL.md` and `LICENSE`; frontmatter with
  only `name` and `description` (an optional leading concept, then `Use
  when/for/to` with the pressure and the decision, at most two sentences);
  one H1 and four to seven prose paragraphs of 250–500 words with no links,
  lists, or code blocks; an opening bold concept with the standing clause
  about settled choices; a closing review-versus-implementation paragraph.
  The Go template's `metadata.invocation/kind` block is not carried: Cursor
  and Codex do not need it, and the harness stage can add it back if a
  `user` or `role` skill ever requires the projection.
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
- A structural check (`make check-skills`) enforces that shape: frontmatter
  keys, name/dir agreement, trigger and sentence count in the description,
  one heading, prose-only paragraphs, word budget, and the LICENSE copy; it
  proves shape, not model behaviour. Behavioural evaluation fixtures are deferred to the harness
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

## 2026-11-06: portable decisions and completed outcomes

The maintainer requested an instruction update in PR #332 after reviewing two
blind comparisons on six standalone programs with three repetitions per arm.
They explicitly deferred new tests and comparative runs. This change adopts
the relevant uncommitted skill revisions from the main checkout while keeping
that checkout untouched. The starting PR head was 79c77e65; the measured ours
arm was d6d2af9d, not this candidate.

The local comparison records are rust-simple-check-2026-11-05 and
rust-codex-check-2026-11-05 under .git/claude. Our trust scores were 17/54 and
11/54; those are dependent comparisons within six tasks, not 54 independent
tasks. Reader reasons repeatedly identified custom argument parsing and
serialization, unfinished behavior, and blocking calls from async paths. Both
sets had important failures. Acceptance by the rule checker excludes lost
requirements, and a successful test command can execute zero tests. These
observations motivate a candidate; they do not identify the causal effect of
any one instruction or prove this revision superior.

| Owner and disposition | Expected decision change | Boundary case | Retained case |
| --- | --- | --- | --- |
| AGENTS.md and rust-dependencies: replace unconditional research procedure with proportional evidence | Compare library adoption against the custom logic and failure paths it replaces; distinguish intentional lockfile changes from locked validation | A routine parser or serializer does not require a new research tree | New capabilities keep an accepted decision record; upgrades and trust changes retain compatibility and security evidence |
| rust-coder: retain current representation and clarity revision, make completion actionable | Repair known in-scope defects, verify the affected runtime boundary, and distinguish unavailable proof | A correct build cannot establish runtime schema execution or child-process shutdown | Explicit phase stops, unavailable tools, and deferred tests remain honest proof limits |
| rust-errors: distinguish application reporting from recovery contracts | Preserve causes and context without inventing unconsumed variants; successful early termination stays success | A CLI can use an existing contextual error type | Public library and recovery boundaries retain typed errors; the service retains its problem catalog |
| rust-tokio: apply ownership to synchronous calls as well as task handles | Keep blocking queries, commits, and syncs off executor workers with one completion owner | An async mutex does not make a synchronous transaction nonblocking | Cheap bounded computation remains inline; admission and execution lifetime remain bounded |
| rust-idiomatic and rust-structural-quality: retain the current decision examples and responsibility criteria | Keep types and helpers that own a real invariant or make one rule easier to change; keep independent facts independent | Moving a payload differs from inspecting retained state | Concrete local data and intentional shared ownership remain valid |
| rust-testing: distinguish selected behavior from command success | Inspect whether relevant cases ran and whether their boundary proves the promise | Zero tests or an ignored case is not behavioral evidence | Reuse adequate coverage; no universal mutation, fuzzing, or new-environment gate |
| rust-verification: align evidence wording with selected CI gates | Limit a test result to executed behavior and use the actual remote gate result | A local full check does not prove CI | Existing required gates remain mandatory |
| Skill Authoring and checker: allow selected Markdown references | Keep substantial examples behind their choosing condition and ship portable examples with the skill | An independently installed skill lacks the service's docs and bootstrap | One focused prose entrypoint, discriminating triggers, 250–1000 body words, license, and project authority remain |

This reopens the initial reference-tree exclusion and universal service
grounding at the maintainer's request. It preserves the current positive
structure guidance instead of adding a line-count goal or mandatory crate list.
rust-coder remains the ordinary implementation method; neighboring skills load
for an unresolved decision in their stated surface rather than on every edit.
No runtime code, dependency selection, authorization boundary, or CI gate is
changed.

Consistency review retained the PR's actual service lifecycle owner:
Background uses JoinSet in crates/service/src/bootstrap/shutdown.rs, while the
jobs worker has its own tracker. The copied draft's generic tracker wording
must not undo that correction. The historical 250–500-word, no-reference shape
above is superseded by the current 250–1000-word policy and linked examples.

Primary contracts consulted for the candidate include
[Cargo's locked resolution](https://doc.rust-lang.org/cargo/commands/cargo-generate-lockfile.html),
[Tokio's blocking-task lifetime](https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html),
[clap's derive interface](https://docs.rs/clap/4.6.7/clap/_derive/_tutorial/index.html),
and [Serde's derives](https://serde.rs/derive.html). They support the mechanisms,
not a measured prompt improvement. The examples state their selecting and
alternative conditions; existing repository code supplies conventions, while
accepted requirements and language/API contracts still constrain correctness.

Validation for this revision is static consistency, instruction shape and
generated carriers, and relative links. Rust tests, example execution, checker
self-tests, and new model comparisons are deferred by request. Existing remote
merge checks remain enabled; their results are reported separately. The prior
PR measurements do not cover this final text.

Local check-instructions passed for all 28 skills and generated carriers.
The docs-check image-input check passed, but its Lychee run could not start:
the local validation lock reported ownership-unavailable during Docker native
readback. The PR's docs gate owns the remaining link proof; do not record this
local attempt as a passed link check.
