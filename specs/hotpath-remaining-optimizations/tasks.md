# Goal

status: done

Planning review: [Task Review / Readiness](planning-review.md), PASS.

Completion: Every area in [Specification](spec.md#outcome-and-area-coverage)
has a non-blocked evidence-backed disposition; every retained source change
meets its adoption rule; the assembled candidate has matching build/tests,
required real-PostgreSQL behavior proof, ordinary and instrumented comparisons
against the immutable baseline, and resolved independent final review. The
task-local final report records exact identities, raw evidence, limitations,
the synthetic pool recommendation/budget rule and bounded SQL disposition.
No qualifying candidate means an explicit no-new-optimization result.

Global constraints: [Intent](intent.md#constraints),
[Specification](spec.md#comparison-and-disposition),
[Technical Design](design.md#claim-scoped-execution-and-release-boundary), and
[Operations](operations.md) remain authoritative. Only root executes commands
beyond local reads/edits/Git, and all builds, diagnostics, tests, services,
loads, reduction and docs checks run on approved droplet `606085569`
(`46.101.222.57`), before `2026-10-05T01:57:04Z`. Preserve unrelated dirt and
the accepted body transfer; no push/PR/deploy, Railway writes, secrets,
production data, provider calls, new host or broader resource authority.

## Tasks

- [x] T1: Incoming serializes its Base64 fields through the existing streaming APIs, preserving exact payload bytes and admission behavior.
  - Depends on: none; baseline custody is established below.
  - Provides: independently removable payload candidate and existing-owner test updates.
  - Packet: [tasks/T1-payload.md](tasks/T1-payload.md)
- [x] T2: HTTP observation uses bounded borrowed representations while preserving the complete emitted span, log and metric meaning.
  - Depends on: none; baseline custody is established below.
  - Provides: independently attributable span/metric candidate and existing-owner test updates.
  - Packet: [tasks/T2-http-observation.md](tasks/T2-http-observation.md)

## Carrier and ready frontier

Use the existing root as Ledger Orchestrator and two Acceptance-Unit Leads
under [Implementation](../../docs/spec-first-workflow/phases/implementation.md).
T1 and T2 are independently consumable repository outcomes with disjoint
writable owners; neither consumes the other's code. Both can start after this
plan is reviewed and root opens Implementation. Leads write code and needed
tests together, return Implemented, join their writers and release scopes.
No task waits for a passing build, benchmark, review or another task's proof.
Root alone writes ledger progress after this Planning handoff and coordinates
the remote execution resource. Tests and concrete commands remain executor-owned.

Accepted inputs: [spec.md](spec.md) blob
`82c39b3adfa32073c719dc70513c39b171b2dea8`; [design.md](design.md) blob
`841d53b5fe557866329924ff7703c0369b939f0f`; its PASS review
`f6ce6d8714e59c824c0f89ac4cdf92a37800f0ec` and
[transition](technical-design-transition.md)
`f5c53a740eeb99f19ff7d6af78e34412d697ad91`.
Root's immutable archive `/root/remaining/evidence/baseline-source.tar.gz`
has SHA-256 `aae64f3624831b7ef5a60491810d812a3034511f346b7811de2b88d6b0c0e2e7`.
It contains the accepted body transfer and unrelated dirty work. HEAD alone
is not the baseline. Operations owns binary, build/config and evidence custody;
the source archive closes the pre-edit baseline prerequisite.

## Consolidated Completion boundary

Implementation returned, both writers stopped. T1 source blob
`24d581464a04e7fd7c8aabc5e4225194218f7dcb`, integration-test blob
`37ed0d081960012dde17cf97751f42fd292f6e1d`; T2 observe blob
`d89febbddc544ce62f991feb9ddd2f9ab5a6ced0`. These checkboxes record code
production only, not verified behavior or accepted optimization. Root is the
assembled delivery owner and performs one remote final validation/comparison.

After both tasks are Implemented and assembled, with no source/test writer,
root assigns one delivery owner the complete final-validation boundary.

Mechanical source refresh: remote rustfmt altered annotation/test wrapping
only, after all compile-only diagnostics passed. Fixed assembled blobs now:
inbound `f1e3cc4769d97b56be6bea1855a305a2a0a1c21e`, observe
`da5d891fae74dab9f472e735545fa27e487f9cb5`, integration test unchanged
`37ed0d081960012dde17cf97751f42fd292f6e1d`. Local/remote identities match;
matching final build/tests consume these bytes. Both task owners remain idle
and available for scoped repair. Final reviewer `/root/remaining_final_review`
is read-only and awaiting complete evidence.
Root performs that owner's remote executions serially; local execution is
forbidden even for documentation checks and result reduction. Baseline-only
SQL capture and machine-budget readback are root-owned supporting evidence,
not task acceptance or permission to start partial candidate validation.

Use [Rust validation](../../docs/validation/rust.md) for the matching build and
tests and [PostgreSQL validation](../../docs/validation/postgres.md) for the
accepted real-database claims. Select the non-overlapping proof once against
this task's delta from the frozen working-copy baseline, preserving relevant
dependency effects; unrelated pre-existing dirt is not a new task change.
The AGENTS.md several-crate route applies to the assembled two-crate change.
Reuse adequate coverage for byte/error parity, authentication/duplicate
bypass, first-winner atomicity, uncertainty/cancellation, wake behavior and
HTTP signal/lifecycle meaning. Implementation chooses missing cases and
commands; no test matrix or per-task execution gate is defined here.
Executable documentation validation also runs remotely at this boundary.

Keep measurement builds factored by exact source and required feature identity,
not workload, pool value, database scenario or task count. Reuse baseline
ordinary and hotpath binaries. Retain source deltas and binary/config receipts
so payload, span creation, metric recording and separately removable name
formatting can each be attributed. Build an isolated attribution variant only
when the affected scope cannot distinguish the mechanism on the assembled
candidate; preserve exact source/config identity for any such variant.
Ordinary release comparisons establish service outcomes; hotpath 0.28.4 with
the retained jemalloc/features and MCP-over-SSH establishes instrumented
attribution. Reuse each binary across the applicable retained workloads.
Do not multiply full builds/tests across 4/8/16 pool values or feature/workload
cross-products. Required variants still receive their actual proof.

The final assembled retained source is compared with the original frozen
baseline at equal pool size and matched fixtures, durability, CPU placement,
toolchain and generator. Preserve repeated order/spread and unfavorable valid
runs, useful-response denominators, errors/drops, service/database CPU, p95,
peak RSS and pool wait. Distinguish allocation bytes/count, CPU and async wall
time; do not sum nested costs or subtract percentiles. Baseline-versus-candidate
source comparison and pool-size comparison answer different questions.

| Required area | Completion disposition and evidence owner |
| --- | --- |
| Payload | T1: net allocated bytes per new delivery, allocation count/CPU separately, byte parity and ordinary controls, with large payloads primary and small/duplicate regressions bounded by the retained workloads. Remove rejected production delta. |
| Span and metric overhead | T2: disposition span creation and metric recording separately; evaluate name formatting independently. Preserve exported/logged values and metric identity, and report SQL-free ordinary HTTP CPU per useful response alongside instrumented attribution. Remove rejected subchanges. |
| SQL round trips | No production unit: [design's concrete rejections](design.md#sql-alternatives-and-evidence-closure) retain existing SQL/driver/wake owners. Root supplies warmed actual new-delivery wake-due, debounced and duplicate protocol observations, separating request exchanges, setup/acquire and return ping, plus latency/acquire/occupancy and database cost. Final no-supported-optimization requires that evidence; predictions and old hotpath counts are not measured exchange counts. An unexplained removable exchange reopens Technical Design; unavailable required trace blocks Completion only, not independent T1/T2 implementation. |
| Pool sizing | No production unit: delivery consumes root's actual settings/topology and budget readback, then compares admissible 4/8/16 on equal source at retained mixed 2,000 RPS. Record reserved slots, other workloads, ten synthetic operational-reserve slots, dedicated connections, worker minimums and permitted overlap once each. Select the smallest reproducibly useful admissible value under Design; report baseline/bottleneck if none qualifies. Keep global default 4. Write the measured synthetic configuration and actionable budget rule in the task report; production inputs remain unknown and unapplied. |

Root retains task-local scripts, configuration and raw evidence under Operations.
Delivery writes `report.md` when results exist; it is the Completion evidence,
not a separately schedulable documentation/test/benchmark task. Reuse existing
scripts and scenarios. No selected production change requires a dependency,
feature, schema, generated SQL metadata or public-contract update.

One independent final Implementation Review covers the assembled retained
candidate, both tasks' invariants, interactions, and all four area dispositions.
Return defects to the smallest existing owner; remove failed optimizations and
refresh only affected proof. A changed mechanism/ownership reopens Design;
changed meaning/adoption criteria reopen Definition. Unknown required evidence
remains incomplete rather than successful. Mark this ledger done only after
Accepted Completion; root then retains evidence and cleans up its exact host
within the existing operations authority.

## Bounded final repair and evidence refresh

First final review FAIL is retained in [review.md](review.md), anchored to
repeated HTTP peak-RSS regression. Root isolated original eager name formatting
in three pairs, then T2 removed only the display-name candidate. Final observe
blob is `002a082fdf8ee704905926c244418d2e27ff7b0c`; inbound and integration-test
identities above are unchanged. Final patch blob is
`0ca673eb96d7107216e87e6198c56c080eac1ab6`.
Matching remote fmt/build, 81 HTTP tests and actual configured signal parity
pass after repair. Fresh direct baseline/final HTTP pairs and final-binary
4/8/16 sizing are complete. All 87 cells are retained with zero invalid cells,
zero unexpected responses, and successful load/service terminal receipts.
No prior unaffected proof is relabeled as new final-source execution.
The same reviewer owns the bounded delta recheck. Report selects measured
synthetic pool8 on the final source; global default remains4. This ledger
became done after review and evidence/cleanup completion.

## Accepted Completion

Root accepts the retained assembled candidate after the same reviewer's PASS,
matching remote build/tests and final runtime evidence. Four dispositions:
payload adopted; borrowed span/status representations adopted with display-name
rejected; SQL no supported optimization under unchanged contracts; synthetic
pool8 selected while global default4 is preserved. All accepted inputs and
production boundaries remain unchanged.
Archive transferred and local/remote Git identities match; custody.json owns
the receipts. Exact droplet606085569 deletion requested21:01:48Z and confirmed
API404 at2026-10-04T21:02:22Z. Own SSH tunnel closed. All work is complete
within the authorized local-change/remote-validation scope; push/PR/production
are neither claimed nor performed.
