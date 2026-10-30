# Rust skill redesign: technical design

Status: ready

This design gives unresolved Rust choices one method owner, removes stale
service descriptions from portable guidance, and makes examples checkable
without introducing an experimental framework. It implements all of
[Specification](../spec.md) R1–R7. Behavioral improvement remains unmeasured.

## Accepted inputs and alternatives

The ready specification's SHA-256 is
`7f9d92f548a29a247029d84f0fcec6c65cb0f6dfe3145c496eff9405930599c8`;
its metadata-only readiness update preserves the body reviewed at
`7e46b91c6ade660087e001b2bc4115543e686382dfbd49a84438b8a4f698ac28`.
Intent is `cab9a48bee9d499032d19de8fa1fc282797b970a9b442993f4eaf3b3d81a86d6`.
[Baseline inventory](baseline.sha256) fixes all 18 current Rust entry bodies
before editing; baseline commit is `38a3723660b71849be222397699625ef5c308473`.

Select 16 Rust entrypoints: replace `rust-coder`, `rust-idiomatic` and
`rust-structural-quality` with **rust-design** and substantially revise each
of the other 15 methods. No alias or thin coder carrier survives. Ordinary
implementation already has an execution owner in AGENTS/workflow; a generic
coder trigger adds an optional second instruction path without an independent
decision. Keeping all three design methods preserves competing ownership;
merging every domain method loses distinct failure, lifetime, budget and proof
questions. The selected merge pays a larger core read only when representation,
ownership or added structure is genuinely unresolved. Reopen this granularity
if later routing evidence finds materially different users of the core with
unnecessary context cost; no pilot gates the current rewrite.

## Authority and loading

| Layer | Owner and boundary |
| --- | --- |
| Mandatory policy | AGENTS, architecture/configuration/validation owners and workflow remain authoritative even when no optional skill loads. No skill grants authority or selects a new final gate. |
| Transferable judgment | `rust-design`: observable promises, representation, validity, mutation/effect ownership and justified structure. Other methods contribute only their independent decision. |
| Conditional domain method | Failure semantics, reliability, security, observability, tests, diagnosis and measurement. Explain the choice and reversal condition; do not recreate a local service map. |
| Library mechanism | `rust-tokio`, `rust-axum`, `rust-sqlx`: retain consequences of cancellation/layers/commit outcomes; resolve version-sensitive behavior from the selected dependency and official docs when that detail affects work. No copied API catalog. |
| Local binding | A short triggered link in the applicable skill leads directly to the existing semantic owner. No new universal local-policy router or per-skill binding files. |
| Supporting example | A reference changes a particular unresolved choice; its premise and loading trigger appear in the entrypoint. It never owns a mandatory invariant. |

The route remains `bootstrap -> selected phase/method -> relevant reference`.
Descriptions provide discovery, not a guarantee of native loading. Existing
direct catalog consumption and Claude/Qwen generated symlinks prove available
files only. Preserve workflow invocation metadata and explicit-only carriers.
Do not add forced loading, middleware, telemetry or a harness experiment.

`rust-design` is selected for an open representation, validity, ownership,
responsibility or abstraction decision. A mechanical rename or already settled
local edit needs no extra Rust method. A blocking task's permit lifetime can
need Tokio and reliability together: who retains it is distinct from how much
capacity/time is allowed. API compatibility and Axum placement likewise differ.
Testing constructs the observation; verification judges the existing receipt.

## Disposition of all current methods

The rows cover the material decision clusters, including their removals and
canonical destinations. Implementation must rewrite retained bodies, not merely
change frontmatter or strip their closing paragraphs. The common standing
clause and review/implementation ritual are removed everywhere: AGENTS and the
current workflow own those semantics. Their removal does not authorize edits
in review or expand final validation.

| Current skill | Disposition and substantive resulting method |
| --- | --- |
| coder | Merge earliest capable owner, deterministic decisions without forced extra layers, far-side callers/effects and resource consequences into design. Remove generic execution/report/build instructions duplicated by AGENTS. Replace copied crate map with architecture binding. |
| idiomatic | Merge ownership/borrow/move/share choices, validity through every constructor/conversion/default/deserialization/mutation, enum/newtype tradeoffs, Eq/Ord/Hash coherence, ordering/overflow/byte-text semantics into design. Keep raw DTOs, clones and plain fields legitimate when no invariant requires more. Move failure identity policy to errors; task completion to Tokio. Remove lint facts and fixed boxed-future prescription. |
| structural-quality | Merge deletion/current-constraint test, responsibility cohesion and realistic change cost into design. Judge a one-implementation trait or one-use helper by present dependency/ownership needs. Remove name-based rejection of util/common and mandatory simulated future-change ritual; compare an actual plausible change only where the structure decision is open. Bind crate placement to current architecture. |
| errors | Retain distinct failure, absence, invalid input, partial success and unknown outcome; typed identity only where consumers need recovery distinctions, preserve causes across translations. Explicit completion versus best-effort Drop, and cleanup failure versus original failure stay inline. Remove mandatory one-enum-per-owner/opaque-only-at-root prescriptions; transport and exit policies link to HTTP/lifecycle owners. No swallowing uncertainty into success. |
| tokio | Rework around admission, execution custody, cancellation request, observation and join. Keep losing-future/drop implications, guard/reentry/atomicity, actual blocking permit lifetime, residual work and wakeup/fairness inline. Cancellation safety is operation-specific (a dropped send may lose its value/queue place); avoid blanket API lists. Timeout or abort of a waiter does not establish execution stopped. Local tracker/shutdown/probe policy links to lifecycle. |
| reliability | Keep parent deadline arithmetic including preparation, attempts/backoff and response/cleanup needs; retry requires repeatability and treatment of unknown effects. Separate bounded queue, rejection, degradation and readiness choices. Remove stale no-queue architecture and compulsory reserve/config invention. Missing technical values return to the responsible decision owner, with evidence/bounded assumptions, not automatically to the user. Bind accepted budgets and admission to operation/config/lifecycle owners. |
| testing | Keep smallest observing layer, independently derived expectations, real dependency when needed for the claim, deterministic coordination and task cleanup. Replace literal-only/no-arithmetic/no-loop rule with independence from the implementation: independent arithmetic, table loops or a simpler reference oracle may be correct. A round trip is insufficient by itself for a shared encoder/decoder defect. Property/mutation tests are selected for a concrete gap, never mandatory for every parser or every survivor. Local harness policy remains with validation/contribution owners. |
| verification | Keep claim-to-observation, exact inputs, skipped/zero-selected tests, evidence reuse and limits of local results. Remove stale command inventory and simplistic “crate tests prove a crate” wording. Read-only verification remains explicit. Bind check selection/completion to AGENTS and validation; no new environment to repair optional proof. |
| debugging | Keep first divergence, competing explanations, discriminating observations, minimal reproducer and causal repair. Replace “hang has no owner/bound” certainty with hypotheses including deadlock, starvation and external wait. Distinguish diagnostic scope from fix authority. Bind local lifecycle event/exit meanings instead of copying them. |
| performance | Keep workload/metric/baseline identity, variance, scale and retained-memory/capacity arithmetic; move only the bottleneck evidence supports. Replace “startup clone does not matter” with frequency/size/lifetime evidence. Preserve behavior/error/order/resource checks; static hypothesis is not a speedup. Local release-profile constraints and command comparison rules retain their current owners. |
| api-contract | Keep observable distinctions and compatibility in actual clients, optional/absent/null/default/security/retry differences, and generated-versus-manual authority. Remove static descriptions of generator/catalog/tool rejection rules; bind code-first composition, security finalization, breaking approval and regeneration to HTTP architecture. An implementation is not a contract merely because its schema compiles. |
| axum | Keep request/response path, layer scope/order, extraction/fallback and response-body lifetime as library decisions. Bind the exact hardened chain, extractor wrappers and server limits to HTTP architecture, which already differs from this skill's old chain. Preserve distinction between mounted-router and connection-level observation without requiring both for every edit. |
| config | Keep one semantic owner, source/precedence/validation boundary, missing versus explicit empty, typed snapshot and cross-field/structure validation. Bind current APP sources, secret rules and key placement to configuration policy. Remove compulsory new loader test + validation test + policy line when existing proof/unchanged policy suffices. |
| observability | Keep operational question, distinction between success/failure/degradation, correlation custody, label cardinality/privacy/cost, probes as control inputs and local emission versus received telemetry. Bind actual event/naming/listener/exporter policy to current telemetry/config/lifecycle owners. Do not assume a new dependency universally requires readiness. |
| security | Keep attacker-controlled path, enforcement before amplification/effect, identity versus correlation, secret disclosure and negative evidence sufficient to distinguish bypass. Remove duplicated chain/config values, universal fixed destination assumption and “no findings still requires deny test”; existing adequate proof may suffice and review remains read-only. Bind security/exposure/secret policy to AGENTS, HTTP and config; current accepted restrictions stay effective. |
| sqlx | Keep transaction/commit knowledge and safe caller action, retry effects and reconciliation, connection/lifecycle custody and real-server evidence limits. Bind all DSN, pool, schema and migration choices to persistence plus PostgreSQL validation. Preserve precise distinction: a locally classified CommitFailed proves no committed write under its owner; CommitUnknown does not. Missing profile does not authorize constructing one or stop unrelated work. |
| dependencies | Keep reuse versus maintained mechanism, resolution/features/MSRV/trust and upgrade compatibility. Research only decision-changing alternatives at their resolved versions; record the decision in the accepted artifact, not a compulsory new synthesis/scratch project for every tweak. Bind version placement/pins/lockfile policy to CONTRIBUTING/AGENTS. Remove full-check completion and exact OTel-version anecdotes: local budget owns checks; CI gates remain unchanged. |
| delivery-platform | Keep candidate/artifact identity, gate selection versus swallowed failure, evidence coverage, publication/readback and rollback distinction. Bind exact classifier/job/pin/image/signature topology to CI/CD owner instead of copying it. Remove implied authority for heavy checks/publication; selected owner and existing gate policy decide. |

## Mandatory local policy preservation

These are retained authorities, not new optional copies. The implementation
owner verifies a removed local rule against its destination before deleting
it; an uncovered real rule moves to that owner in the same scoped change.
Do not rewrite unrelated architecture merely to make old skill prose true.

| Policy removed from skill prose | Retained canonical owner |
| --- | --- |
| authority, secrets, scope, review, ordinary completion, locked Cargo, unsafe/comment/style rules | AGENTS; shared Review; Evidence Contract |
| crate/provider/feature direction, composition and actual permitted exceptions | docs/architecture/boundaries.md; quality/architecture.json; project-structure-and-module-organization.md |
| hardened request path, code-first document, Problems, effective security, compatibility and route/body distinctions | docs/architecture/http.md; its linked profile owners; runtime/generated source remains factual authority |
| background registration, blocking-work admission/custody, runtime teardown, probe policy and exit meaning | docs/architecture/runtime-lifecycle.md |
| configuration precedence, explicit empty, unknown fields, APP/secret/telemetry sources and budgets | docs/configuration-source-policy.md; operation-budgets.md |
| pool/transaction/schema/commit classification and real database proof | docs/architecture/persistence.md; docs/validation/postgres.md |
| libraries, default features, test harness and gate selection | CONTRIBUTING.md; docs/backend-library-selection.md; docs/validation-routing.md and matching leaf; make/template.mk |
| actual CI/image/publication guarantees | docs/ci-cd-production-ready.md; current scripts/workflows |

Current evidence already justifies removing stale duplication: HTTP architecture
has a bounded permit wait and consolidated observation middleware; old skills
describe no queue and separate layers. Configuration policy owns empty overrides
and all secret carriers. Persistence owns CommitFailed/CommitUnknown in detail.
No runtime behavior is changed to resolve these instruction discrepancies.

## Format, examples and checking

`docs/skill-authoring.md` remains the format owner. Decision frontmatter remains
exactly name/description with existing naming and concise trigger validation.
Keep a starting H1, nonempty body, repository LICENSE and supported files.
Allow subheadings, prose, lists, tables, links and fenced code. Remove both word
bounds, paragraph count, bold opening and mandatory closing/standing wording.
Do not replace them with another prose quota. Description limits and workflow
metadata/body/license rules are unchanged.

Decision directories may additionally contain `references/*.md`, directly
linked with a discriminating trigger. No assets/scripts/metadata carrier are
needed for this change. `check-skills.py` owns only that shape; extend its
existing entry check to allow these Markdown references and retain workflow
and service-owned reservations. Local links/fragments stay `make docs-check`'s
responsibility; review owns relevance, entrypoint sufficiency and routing.
Check ordinary Markdown headings outside fences rather than rejecting code
comments as headings. No new dependency, generic parser or evaluator is needed.

Add two small conditional references:

- `rust-design/references/representation-examples.md`: checked std-only
  contrasts for raw input versus a validated value (all public construction
  paths considered), and identity/order/hash over the same meaningful key.
  Retain the cheap DTO/derived-trait solution when those semantics already fit.
- `rust-errors/references/outcome-examples.md`: checked std-only example
  preserving absent input versus invalid present input with `Result<Option<_>,
  _>`, including the legitimate alternate policy where absence is an error.

Each contrast states premise, protected property, accepted cost and the changed
condition reversing its recommendation. Inline principles keep constructor
bypass, trait coherence and absence/failure distinction visible without reading
the examples. The entrypoints also carry each essential concrete contrast in
short form: a raw DTO may deserialize directly while a validity-promising type
must route deserialization through validation; ordering a record by timestamp
while equality uses only ID is incoherent; absent input may be `Ok(None)` while
malformed present input remains `Err`. References supply full compilable context
and additional conditions, not the only explanation of the choice. Other methods
use compact prose contrasts or precise existing
code links where they add value; no per-skill code/example quota. Library
fragments must be labelled “fragment, not compiled” and fenced as text, rather
than presented as tested programs. Avoid fragments when a plain explanation is
clearer. No comparative benchmark fixtures are created.

Use native Markdown doctests directly for the two std-only files:
`rustdoc --test --edition=2024 <reference.md>` from this checkout, under the
toolchain in rust-toolchain.toml (currently 1.99.0). Include assertions for the
stated boundaries, require nonzero executed tests, and retain command, toolchain,
file hash and result in ordinary completion evidence. No Cargo package,
dependency, example runner, compilation cache or experimental environment is
added. Implementation confirms the native command's available options before
using it. Deliberate non-compiling contrasts are text, not ignored Rust tests.

Document this authoring-time check in Skill Authoring and Instruction Validation
for changed compilable examples. It is not a hidden dependency of check-skills
or check-instructions: docs-only CI must remain toolchain-free. There is no new
CI gate or classifier surface. Technical correctness of examples is evidence
about those examples, not evidence of educational benefit or native loading.

## Live consumer migration and delivery

Update AGENTS' ordinary-coder statement to conditional design use and name
the current workflow/local budget as execution/completion owner. Update the
Rust ownership design and structure-only review references, and collapse the
three thermo-nuclear review bullets into design's actual question without
changing that workflow's invocation or review scope.

In specialist-neighbors.json replace the three names with rust-design, merge
duplicate cluster entries and edges, remove self-pairs and rewrite decisive
axes. Keep only real independent pairs: design/errors distinguishes
representation from failure/recovery meaning; design/Tokio distinguishes
owned data representation from execution custody; design/dependencies
distinguishes needed abstraction from resolution/admission. The former
coder/change-control edge to merge-conflict-resolution has no independent
design decision and is removed; a known fix need not load design merely
because debugging finished. Preserve unrelated edges and schema.

Update the two classifier self-test sample paths to rust-design. The existing
`.agents/*` instruction rule already covers references; keep its gate meaning.
Update make's check-skills help description. Regenerate selected Claude/Qwen
views with existing sync targets only after additions/removals are assembled;
the symlink generator and dynamic profile projection discovery already handle
whole directories and require no content-copy mechanism. Their existing
checks remain proof of shape/projection, not native loading.

Historical roadmap stages, research synthesis and consumer-lifecycle completion
records keep their names/counts as historical facts. No bulk replacement.
The old untracked comparison directory is immutable. Record current completion
and candidate identities in this task's existing artifacts; do not rewrite
earlier observations. Future model/harness/library changes reopen affected
content or evaluation claims, not every skill by default.

Planning can assign disjoint writable lanes:

| Lane | Exclusive writable files | Dependency |
| --- | --- | --- |
| Core and outcomes | rust-design directory; remove coder/idiomatic/structural-quality directories; rust-errors and its reference | This ready design |
| Runtime and evidence | rust-tokio, reliability, testing, verification, debugging, performance directories | This ready design |
| Boundaries and platform | rust-api-contract, axum, config, observability, security, sqlx, dependencies, delivery-platform directories | This ready design |
| Format and consumers | docs/skill-authoring.md; docs/validation/instructions.md; scripts/check-skills.py; make/template.mk help; AGENTS; two workflow phase references; thermo-nuclear carrier; specialist-neighbors.json; classifier self-test sample names | This ready design; final assembly waits for content lanes |
| Final assembly owner | Selected generated skill views and task completion evidence; no concurrent edits in lanes above | All content/control lanes return; then sync, checks, review |

This is an ownership allocation, not an Implementation ledger. One final owner
runs applicable local checks once: check-instructions, docs-check, changed
classifier self-test and direct doctests; focused checker feedback only for
the format change's real failure modes. Existing profile tests are reused if
their covered source actually changes; no full initializer matrix or runtime
suite is added merely because the catalog size changed. Shell lint is scoped
to the changed shell script, following its existing validation route. Final
independent review covers all skills, owners and carrier assembly. No model
comparison, experimental runner/evaluator, new comparative fixture, remote
write or runtime build is authorized by this design.

The [future evaluation protocol](evaluation-plan.md) is delivered now as a plan.
It does not block local completion. Rollback restores the changed instruction
sources and regenerated views from the recorded pre-change identities,
preserving unrelated dirty work and all historical comparison artifacts.

## Movement

Technical Design Review must permit movement on this fixed design and its
evaluation plan. Planning owns scheduling and final-validation routing, not
method granularity or example architecture. Reopen this design for a concrete
unsupported delivery/checking mechanism or lost semantic owner; reopen
Specification only for changed requester behavior or scope. Technical choices
do not return to the user merely because alternatives exist.

Independent [Technical Design Review](review.md) returned PASS with no surviving
findings. This readiness/receipt update changes no design decision.

```text
status: ready
owner: Technical Design
result: specs/rust-skills-redesign-2026-10-30/design/design.md and evaluation-plan.md
review: specs/rust-skills-redesign-2026-10-30/design/review.md
movement_evidence: Fixed design independently passed; method ownership, policy preservation, format, example proof, consumer migration and parallel writable boundaries are closed.
reopen_owner: none
next_owner: Planning
```
