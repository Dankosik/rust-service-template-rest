# Rust guidance foundations

This revision corrects source meaning and instruction ownership across the
Rust skill set. It does not establish better generated code or superiority to
another package. The user requested a coherent source-led revision, preserving
frozen comparisons and deferring new model experiments.

The fixed starting point is `1fd3e699b72d676021da1f0d8390d1cf43db90c4`.
The evidence inventory is the 110 practices in 13 sections and nine recorded
disagreements in the local 2026-11-05 research, with 75 referenced primary-source
files. Saved material lives under the Git-common `claude/rust-best-practices-2026-11-05`
and `claude/rust-idiom-check-2026-11-05` archives. Public source locators below
make this decision record usable without those local archives. The original
research explicitly did not inspect the closed books it lists as unavailable.

## Decision and evidence boundary

The source audit accounted for all 110 practices, reading their cited primary
context rather than accepting the compressed summary or quote-presence check as
proof. Separate read-only source lanes covered 63 design/API practices and 47
effect/proof practices; another lane checked nine service methods against current
canonical owners. The parent reconciled the results and owns the integrated
wording. No benchmark implementations supplied acceptance criteria for this
revision.

Language/provider contracts, accepted project policy, conditional design advice,
and hypotheses about model behavior remain distinct. A preference keeps its
selecting condition and a meaningful alternative. Local service facts are
conditional pointers to their current owners; portable methods do not import a
service framework into standalone code. Existing authority, validation, comments,
unsafe, and production-panic policies remain unchanged.

Retain the 18 existing Rust skill selectors, with pressure-based descriptions.
Six descriptions lose a service-only or false-success-only restriction where
their methods apply to Rust outside this service; framework and provider methods
remain scoped. No skill, mandatory core document, experimental harness, runtime
dependency, or release gate is added. The five existing decision references stay
because they show invariant boundaries rather than preferred syntax.

The table IDs are section-local ordinals in the frozen practice inventory.
`retain` keeps the useful decision; `qualify` restores scope or semantics;
`owner` leaves a project decision with its canonical authority; `tool` avoids
repeating a mechanically enforced convention. A retained rule need not retain
its old sentence. The destinations are the current skill names without the
`rust-` prefix.

## Design before code

| ID | Decision and disposition | Destination | Primary basis |
| --- | --- | --- | --- |
| D01 | retain: represent the data and invariant before mechanics | coder | [Pike], [ER] item 1 |
| D02 | qualify: types exclude only the states their operations prevent | idiomatic | [King], [ER] item 1 |
| D03 | retain: carry validated properties across the owning boundary | idiomatic | [King], [RA] function preconditions |
| D04 | qualify: avoid speculative capabilities; present changeability still has value | coder, structural-quality | [Yagni] |
| D05 | qualify: shared knowledge, not merely similar text; flags prompt investigation | structural-quality | [Metz], [Patterns] |
| D06 | qualify: unnecessary observable surface creates compatibility cost | structural-quality | [Hyrum], [Ousterhout] |
| D07 | retain: expose the main decision and mutation boundary for readers | coder | [Google review] |
| D08 | qualify: reusable-library and application APIs have different constraints | idiomatic, structural-quality | [Google] API context |

## Types

| ID | Decision and disposition | Destination | Primary basis |
| --- | --- | --- | --- |
| T01 | qualify: enums for mutually exclusive states; independent facts stay independent | idiomatic, representation reference | [ER] item 1 |
| T02 | qualify: Option only when absence differs from empty; decode external sentinels | idiomatic | [ER] item 1 |
| T03 | qualify: newtypes prevent meaningful unit or identity confusion | idiomatic | [ER] item 6, [Google] |
| T04 | retain: all construction and mutation paths preserve claimed invariants | idiomatic | [Microsoft] M-STRONG-TYPES-GUARD |
| T05 | qualify: clarify ambiguous boolean call sites, retain clear independent flags | idiomatic | [Google] |
| T06 | qualify: named fields aid meaning outside very local tuples | idiomatic | [Google] |
| T07 | retain: use standard semantic types where they fit | idiomatic | [API type safety] |
| T08 | qualify: builders for difficult construction, not validation or an argument count alone | idiomatic | [API type safety] C-BUILDER, [ER] item 7 |
| T09 | qualify: typestate trades dynamic checks for API and generic complexity | idiomatic | [Book] state pattern |
| T10 | qualify: From/TryFrom by semantics; intentional casts remain possible | idiomatic | [ER] item 5 |
| T11 | retain: choose arithmetic overflow behavior from the contract | idiomatic | [Book] integer overflow |

## Ownership and borrowing

| ID | Decision and disposition | Destination | Primary basis |
| --- | --- | --- | --- |
| O01 | retain: borrow the data needed through an appropriate view | idiomatic | [API flexibility], [RA] |
| O02 | retain: ownership transfer is explicit when retention or consumption needs it | idiomatic | [API flexibility] C-CALLER-CONTROL |
| O03 | qualify: a deliberate clone can reduce excessive ownership complexity within cost bounds | idiomatic, ownership reference | [Google] borrows/copies, [Patterns] borrow-clone |
| O04 | qualify: shared ownership serves sharing or materially clearer ownership | idiomatic | [Google] |
| O05 | retain: interior mutability has an invariant and access protocol | idiomatic | [Book] interior mutability |
| O06 | qualify: take/replace only for actual transfer with a valid replacement state | idiomatic, ownership reference | [Patterns] mem-replace |
| O07 | correct: RAII covers scope exit/unwinding, not aborts, leaks, or durable completion | idiomatic | [ER] items 11 and 18 |
| O08 | retain: explicit fallible completion, non-panicking best-effort destruction | errors, idiomatic | [API dependability], [ER] |
| O09 | qualify: expose ownership wrappers only when fundamental to the API | idiomatic, structural-quality | [Microsoft] M-AVOID-WRAPPERS |

## Errors and panics

| ID | Decision and disposition | Destination | Primary basis |
| --- | --- | --- | --- |
| E01 | owner: Result for expected failures; preserve the repository's production-panic restriction | errors, AGENTS | [Book], [ER] item 18 |
| E02 | qualify: catch_unwind is not propagation or invariant repair; unwind-only isolation remains | errors | [Google] panic recovery |
| E03 | qualify: error representation follows consumer recovery needs, not crate count | errors | [Anyhow], [Thiserror], [Palmieri] |
| E04 | retain: public error interoperability needs Error/Display/Debug; Result does not enforce it | errors | [API interoperability] C-GOOD-ERR |
| E05 | retain: typed distinctions serve callers; context serves diagnostic consumers | errors | [Palmieri] |
| E06 | qualify: preserve causes and inspect actual rendering; storage is not display | errors | [Palmieri], [Error] |
| E07 | correct: From for context-free conversions; map_err for context/foreign errors; match for recovery | errors | [Microsoft] M-FROM-ERROR |
| E08 | qualify: ignoring failure needs an accepted best-effort policy | errors | [Result] |
| E09 | retain: pattern matching exposes the established case | idiomatic, errors | [Google] |
| E10 | qualify: concise composable error text follows the applicable convention | errors | [API interoperability] |

## Functions and control flow

| ID | Decision and disposition | Destination | Primary basis |
| --- | --- | --- | --- |
| F01 | qualify: cohesion and complexity matter more than line count | coder | [Linux], [Ousterhout] |
| F02 | retain: extract a complete decision; keep mutually dependent fragments together | coder, structural-quality | [Fowler], [Ousterhout] |
| F03 | qualify: early returns expose conditions; nesting is a signal, not a fixed limit | coder | [Linux], [RA] |
| F04 | qualify: move a condition only to its real context owner, preserving the callee contract | coder | [Push ifs] |
| F05 | qualify: group parameters by meaningful relationship, not a numerical cutoff | coder | [Google], [Clippy] |
| F06 | qualify: existing operations must preserve ordering, effects, and failure timing | coder, collections reference | [Google], [Collections] |
| F07 | qualify: functions for receiverless actions; private context structs remain useful | coder, structural-quality | [Microsoft], [RA] |
| F08 | correct: returned results and deliberate caller-buffer reuse are both valid | coder | [API predictability] |
| F09 | qualify: name or annotate types when inference obscures relevant meaning | coder | [RA], [Clippy] |

## Traits and generics

| ID | Decision and disposition | Destination | Primary basis |
| --- | --- | --- | --- |
| G01 | retain: standard traits when semantics fit, with useful inherent methods allowed | idiomatic | [Google], [API interoperability] |
| G02 | retain: implement From; Into remains useful as a bound | idiomatic | [From] |
| G03 | qualify: anticipate downstream trait needs for published libraries; applications use current need | idiomatic | [Google], [API interoperability] |
| G04 | correct: intentional Debug/Display representations can be manual; redaction is not the sole reason | idiomatic, errors | [API debuggability], [Microsoft] |
| G05 | qualify: derive only when its implementation has the required meaning | idiomatic | [Google] |
| G06 | retain: equality, ordering, and hashing must agree | idiomatic | [Clippy], standard trait contracts |
| G07 | qualify: sensible infallible zero-argument construction agrees with Default | idiomatic | [Clippy] new-without-default |
| G08 | qualify: traits need variation or an extension responsibility; ordinary functionality remains inherent | idiomatic, structural-quality | [Google], [Microsoft] |
| G09 | correct: concrete, enum, generic, and dyn choices follow actual consumers and costs | structural-quality | [Google], [Microsoft] M-DI-HIERARCHY scope |
| G10 | retain: Deref provides smart-pointer-like access, not inheritance | idiomatic | [API predictability], [Patterns] |
| G11 | correct: operators retain mathematical or logical meaning | idiomatic | [Google] |
| G12 | qualify: bounds belong at required operations, with representation constraints retained | idiomatic | [Google], [API flexibility] |
| G13 | qualify: decide dyn compatibility from required consumers | idiomatic | [API future-proofing] |
| G14 | retain: macros must justify work beyond functions, generics, or traits | structural-quality | [Microsoft] |

## Naming

| ID | Decision and disposition | Destination | Primary basis |
| --- | --- | --- | --- |
| N01 | tool: preserve standard casing, including constants/statics, through project lint/style policy | AGENTS, compiler/lints | [RFC 430] |
| N02 | qualify: conversion prefixes convey ownership and conventional cost, not measured performance | idiomatic | [API naming] |
| N03 | qualify: getter convention retains meaningful keyed or single-value get | idiomatic, project style | [API naming] |
| N04 | qualify: iterator names follow the represented sequence; str bytes/chars remain valid | idiomatic, project style | [API naming] |
| N05 | retain: unfamiliar abbreviations must not obscure meaning | coder, project style | [Google] |
| N06 | qualify: names must identify responsibility; no word blacklist or mandatory rename | structural-quality, project style | [Microsoft] |
| N07 | qualify: constructors use established conventions or a clearer domain verb | idiomatic | [API predictability] |

## Visibility and API surface

| ID | Decision and disposition | Destination | Primary basis |
| --- | --- | --- | --- |
| V01 | qualify: privacy protects invariants and API evolution; passive data may be public | idiomatic, structural-quality | [API future-proofing], [RA] |
| V02 | correct: avoid duplicate user-facing paths, retain internal re-exports and deliberate facades | structural-quality | [Microsoft] M-SINGLE-PATH |
| V03 | owner: use the project's import policy and its allowed inline-module/re-export cases | coder, AGENTS | [Google] |
| V04 | qualify: must_use where accidental discard is wrong and the type does not already say so | idiomatic | [Google], [Result] |
| V05 | qualify: thread-crossing requirements determine Send/Sync; deliberate thread-local types remain | idiomatic, tokio | [API interoperability] |

## Concurrency and async

| ID | Decision and disposition | Destination | Primary basis |
| --- | --- | --- | --- |
| A01 | qualify: coherent protection; callback and guard decisions preserve atomicity and control lock lifetime | tokio | [ER] item 17, [Atomics basics] |
| A02 | qualify: derive ordering from synchronization; scrutinize SeqCst without automatic weakening | tokio | [Atomics ordering] |
| A03 | retain: avoid blocking guards across await; inspect the actual operation | tokio | [Tokio shared state] |
| A04 | correct: synchronous mutex choice includes contention and executor progress | tokio | [Tokio shared state] |
| A05 | correct: yielding cannot make blocking I/O nonblocking; CPU and blocking work have distinct costs | tokio | [Ryhl blocking], [Tokio tasks] |
| A06 | retain: cancellation may discard owned progress and violate a surrounding invariant | tokio | [Rain cancellation] |
| A07 | qualify: losing owned futures are dropped; acceptable loss is contract-dependent; pinning alone is insufficient | tokio | [Tokio select] |
| A08 | retain: meaningful task outcomes require an observing owner | tokio | [Tokio spawning] |
| A09 | qualify: bound queued work through the queue or a finite system admission bound | tokio, reliability | [Tokio channels], [Ryhl actors] |
| A10 | qualify: notifications where suitable, owned bounded polling where necessary | tokio | [Tokio Notify], [Tokio shutdown] |
| A11 | qualify: a task-owned resource is an option, not a mandatory actor framework | tokio | [Ryhl actors] |
| A12 | retain: decide, signal, and observe shutdown under the accepted lifecycle | tokio, reliability | [Tokio shutdown] |
| A13 | correct: timeout is cooperative, not preemption or proof of stopped effects; explicit Future interfaces remain valid | tokio | [Tokio timeout], [Microsoft] |

## Documentation and comments

| ID | Decision and disposition | Destination | Primary basis |
| --- | --- | --- | --- |
| C01 | owner: mandatory documentation of every public item is replaced by the accepted project comment policy | AGENTS, coder | [Rustdoc], [Google] |
| C02 | retain: warranted documentation has a useful initial summary | coder, project style | [RFC 1574] |
| C03 | qualify: describe relevant errors/panics and unsafe caller obligations where applicable | project style, selected method | [API documentation] |
| C04 | qualify: examples demonstrate use without incidental panic handling or compulsory per-function examples | project style | [API documentation], [Microsoft] |
| C05 | tool: preserve checked intra-doc links under the project's rustdoc policy | AGENTS, lint | [API documentation] |
| C06 | qualify: explain facts code cannot express; distinguish interface documentation from implementation narration | coder, AGENTS | [Google review], [Ousterhout] |
| C07 | retain: update comments/examples when their promised behavior changes | coder | [Book], [Google review] |

## Testing

| ID | Decision and disposition | Destination | Primary basis |
| --- | --- | --- | --- |
| Q01 | qualify: test the unit's consumer interface, not necessarily language-level pub or only end-to-end | testing | [SWE testing], [Matklad testing] |
| Q02 | qualify: behavior-focused tests; interactions matter when they are the promise | testing | [SWE testing] |
| Q03 | correct: independent oracle; useful setup, coordination, tables, and properties may contain logic | testing | [SWE testing] |
| Q04 | retain: name the defect the proof could detect | testing, verification | [Google review] |
| Q05 | qualify: select layers and discovery tools for material behavior, not a universal matrix | testing, verification | [ER] item 30 |
| Q06 | qualify: controlled time/I/O, compatible clocks, and synchronization distinct from automatic clock advance | testing | [Tokio testing], [Microsoft] |

## Performance

| ID | Decision and disposition | Destination | Primary basis |
| --- | --- | --- | --- |
| P01 | retain: measure the relevant workload before claiming an optimization | performance | [Perf book], [Pike] |
| P02 | qualify: choose algorithms for actual input sizes; small n is not a supplied workload fact | performance | [Pike], [ER] item 20 |
| P03 | qualify: avoid unnecessary allocation; reservation follows useful size knowledge | performance, coder | [Perf allocations], [RA] |
| P04 | retain: collections follow operations and ordering requirements | idiomatic, performance | [Collections] |
| P05 | qualify: owned storage and borrowed views each need the right lifetime and cost | idiomatic, performance | [ER] item 20, [Book] |
| P06 | correct: enum layout includes representation, discriminants, alignment, and padding | performance | [Perf sizes] |

## Tools, unsafe, dependencies

| ID | Decision and disposition | Destination | Primary basis |
| --- | --- | --- | --- |
| U01 | tool: formatting follows the existing rustfmt policy | AGENTS, validation | [Google], [ER] item 32 |
| U02 | owner: obey project lints and narrowly justified exceptions; do not impose a new global lint matrix | AGENTS, coder | [ER] item 29, [Microsoft] |
| U03 | owner: this workspace forbids unsafe; source permission is not project authority | AGENTS, idiomatic | [Google], [Microsoft] |
| U04 | retain: compare dependency costs with custom mechanism and maintenance costs | dependencies | [ER] item 25, [RA] |
| U05 | correct: diagnostics follow the project's telemetry; intentional CLI output remains valid | observability, errors | [Microsoft] M-LOG-NOT-PRINT |

## Recorded disagreements

| Original disagreement | Reconciled decision and boundary |
| --- | --- |
| Function length | Cohesion and reasoning cost decide; neither a line target nor compulsory extraction. |
| Combinators versus loops | Match the operation's data, order, effects, and failure timing; no syntax ranking. |
| Public fields versus getters | Invariants and compatibility needs decide; passive records need not acquire accessor machinery. |
| unwrap versus expect | Preserve the stricter production-panic policy already accepted by this repository. |
| Comments | Preserve AGENTS' local policy; do not misrepresent Martin's position as an absolute ban on helpful comments. |
| Early optimization | Assess performance relevance and actual workload; measure before spending complexity or making a gain claim. |
| Poison recovery | The adopted Google policy forbids recovery in this service. Standard-library recovery capability does not override it; elsewhere permission and restored invariants are both required. |
| non_exhaustive | Preserve the explicit workspace exception and exhaustive cross-crate matches; public library evolution has different constraints. |
| Generic inputs | Choose from real consumer flexibility and complexity, preserving concrete application APIs when sufficient. |

## Service-owner reconciliation

All nine service-focused methods were inspected. The following changes correct
instruction drift; none authorizes a runtime policy change.

| Method | Correction | Canonical owner |
| --- | --- | --- |
| api-contract | Catalog belongs to service-failure; infra-http projects it. Reuse adequate changed-boundary proof. | [HTTP contract](../../../docs/architecture/http.md#the-contract), [repository architecture](../../../docs/repo-architecture.md) |
| axum | Buffered and streaming response resources outlive headers; polling-only timers cannot guarantee reclamation. | [Response lifetime](../../../docs/architecture/http.md#request-and-response-resource-lifetime) |
| config | Defaults may use impl Default; dependent validation can belong to composition; file secret admission recognizes names. | [Configuration source policy](../../../docs/configuration-source-policy.md) |
| debugging | A hang is not explained by owner/timeout presence; this service's hook withholds backtraces regardless of RUST_BACKTRACE. | [Diagnostic policy](../../../docs/configuration-source-policy.md), [runtime lifecycle](../../../docs/architecture/runtime-lifecycle.md) |
| delivery-platform | Selected jobs require success; local, CI, and post-merge ownership can differ; publication needs authority. | [Validation routing](../../../docs/validation-routing.md), [delivery policy](../../../docs/ci-cd-production-ready.md) |
| observability | A new dependency does not automatically gate readiness; intentional CLI output is not telemetry. | [Integration](../../../docs/architecture/integration.md), [telemetry](../../../docs/configuration-source-policy.md) |
| reliability | Criticality selects readiness; original cutoffs persist; terminal reserve is owner-specific. | [Operation budgets](../../../docs/operation-budgets.md), [runtime lifecycle](../../../docs/architecture/runtime-lifecycle.md) |
| security | CORS is not server authorization; invalid correlation IDs can be replaced; review does not mandate new denial execution. | [HTTP architecture](../../../docs/architecture/http.md), [integration](../../../docs/architecture/integration.md) |
| sqlx | Native/autocommit access and dedicated migration sessions exist; commit classifications differ; guarded nontransactional migrations are allowed. | [Persistence](../../../docs/architecture/persistence.md), [migrations](../../../migrations/README.md) |

The dependency and verification methods retain their existing proportional
research and evidence-boundary behavior. Existing ownership, representation,
collections, responsibility, and execution-lifetime references remain valid.
The new authoring guidance addresses the recurring source-compression and
copied-policy failure modes without adding another skill or ordinary-task gate.

## Completion and reopen boundary

The expected instruction delta is better selection of already justified
representations, mechanisms, diagnostics, and proof boundaries. Retention cases
include independent facts, intentional copies, long cohesive matches, caller
buffer reuse, atomic staging, real failure adapters, and accepted best-effort
cleanup. Boundary cases include externally visible effects, cancellation,
unknown commit outcomes, secret diagnostics, and runtime policy unavailable to a
standalone skill.

Completion requires the integrated static review, instruction/carrier checks,
relative-link checks, and the existing changed-surface validation route. It does
not require a Rust runtime change or a model experiment. Reopen an individual
rule when its primary contract, accepted project policy, or applicability
changes, or concrete counter-evidence shows that the stated condition no longer
selects the intended trade-off. Further wording changes without such a reason
are not evidence of improvement.

[API type safety]: https://rust-lang.github.io/api-guidelines/type-safety.html
[API flexibility]: https://rust-lang.github.io/api-guidelines/flexibility.html
[API dependability]: https://rust-lang.github.io/api-guidelines/dependability.html
[API interoperability]: https://rust-lang.github.io/api-guidelines/interoperability.html
[API debuggability]: https://rust-lang.github.io/api-guidelines/debuggability.html
[API predictability]: https://rust-lang.github.io/api-guidelines/predictability.html
[API future-proofing]: https://rust-lang.github.io/api-guidelines/future-proofing.html
[API naming]: https://rust-lang.github.io/api-guidelines/naming.html
[API documentation]: https://rust-lang.github.io/api-guidelines/documentation.html
[Google]: https://google.github.io/styleguide/rustguide.html
[Google review]: https://google.github.io/eng-practices/review/reviewer/looking-for.html
[Microsoft]: https://microsoft.github.io/rust-guidelines/agents/all.txt
[ER]: https://www.lurklurk.org/effective-rust/print.html
[Book]: https://doc.rust-lang.org/book/
[Patterns]: https://rust-unofficial.github.io/patterns/
[RA]: https://rust-analyzer.github.io/book/contributing/style.html
[Pike]: https://www.lysator.liu.se/c/pikestyle.html
[King]: https://lexi-lambda.github.io/blog/2019/11/05/parse-don-t-validate/
[Yagni]: https://martinfowler.com/bliki/Yagni.html
[Metz]: https://sandimetz.com/blog/2016/1/20/the-wrong-abstraction
[Hyrum]: https://www.hyrumslaw.com/
[Ousterhout]: https://github.com/johnousterhout/aposd-vs-clean-code
[Fowler]: https://martinfowler.com/bliki/FunctionLength.html
[Linux]: https://www.kernel.org/doc/html/latest/process/coding-style.html
[Push ifs]: https://matklad.github.io/2023/11/15/push-ifs-up-and-fors-down.html
[Clippy]: https://rust-lang.github.io/rust-clippy/stable/index.html
[Collections]: https://doc.rust-lang.org/std/collections/index.html
[From]: https://doc.rust-lang.org/std/convert/trait.From.html
[Error]: https://doc.rust-lang.org/std/error/trait.Error.html
[Result]: https://doc.rust-lang.org/std/result/
[Anyhow]: https://github.com/dtolnay/anyhow
[Thiserror]: https://github.com/dtolnay/thiserror
[Palmieri]: https://www.lpalmieri.com/posts/error-handling-rust/
[Atomics basics]: https://marabos.nl/atomics/basics.html
[Atomics ordering]: https://marabos.nl/atomics/memory-ordering.html
[Tokio shared state]: https://tokio.rs/tokio/tutorial/shared-state
[Tokio tasks]: https://docs.rs/tokio/1.54.1/tokio/task/
[Tokio select]: https://docs.rs/tokio/1.54.1/tokio/macro.select.html
[Tokio spawning]: https://tokio.rs/tokio/tutorial/spawning
[Tokio channels]: https://tokio.rs/tokio/tutorial/channels
[Tokio Notify]: https://docs.rs/tokio/1.54.1/tokio/sync/struct.Notify.html
[Tokio shutdown]: https://tokio.rs/tokio/topics/shutdown
[Tokio timeout]: https://docs.rs/tokio/1.54.1/tokio/time/fn.timeout.html
[Tokio testing]: https://tokio.rs/tokio/topics/testing
[Ryhl blocking]: https://ryhl.io/blog/async-what-is-blocking/
[Ryhl actors]: https://ryhl.io/blog/actors-with-tokio/
[Rain cancellation]: https://sunshowers.io/posts/cancelling-async-rust/
[RFC 430]: https://rust-lang.github.io/rfcs/0430-finalizing-naming-conventions.html
[RFC 1574]: https://rust-lang.github.io/rfcs/1574-more-api-documentation-conventions.html
[Rustdoc]: https://doc.rust-lang.org/rustdoc/how-to-write-documentation.html
[SWE testing]: https://abseil.io/resources/swe-book/html/ch12.html
[Matklad testing]: https://matklad.github.io/2021/05/31/how-to-test.html
[Perf book]: https://nnethercote.github.io/perf-book/general-tips.html
[Perf allocations]: https://nnethercote.github.io/perf-book/heap-allocations.html
[Perf sizes]: https://nnethercote.github.io/perf-book/type-sizes.html

