# T001 — Propagate and enforce one operation budget

## Outcome

Replace the current disconnected deadline/cancellation paths with the reviewed
B1–B6 contract: admitted handlers can propagate a neutral fixed context;
dependency stages spend its remaining allowance; transport opening and explicit
response lifetimes remain distinct; S3 ownership expires even without polling;
known and unknown effects retain their existing finality and retry owners.
This is one independently acceptable repository outcome. A carrier, an adapter,
or a profile update alone does not complete it.

## Consumes

- [Definition result](../definition-result.md) and [specification](../spec.md)
  — ready behavior, including B1/B5 late-known-mutation finality.
- [Technical Design result](../technical-design-result.md),
  [system design](../design/system.md), and [ownership map](../design/ownership.md)
  — ready mechanism, public contracts, C1–C13 exact placement, cleanup and
  profile custody. The ownership map's Files and Non-Rust sections are the
  authoritative writable-path inventory.
- [Technical review](../design/technical-review.md) and
  [ownership review](../design/ownership-review.md) — accepted design evidence,
  not runtime proof.
- [Baseline evidence](../research/baseline.md) — source locators and bounded
  reference-PR comparison; reference PRs are not this candidate's baseline.
- [Implementation owner](../../../docs/spec-first-workflow/phases/implementation.md),
  [Validation Routing](../../../docs/validation-routing.md),
  [contribution policy](../../../CONTRIBUTING.md), and
  [CI owner](../../../docs/ci-cd-production-ready.md) — execution, local proof,
  draft-to-ready PR delivery and current-candidate CI gates.

Implementation inputs are available at base
`78aa3a832bfb4d7e9632ce5ebbbf1680705c31af` in branch
`codex/operation-budgets-20261005`, worktree
`/Users/daniil/.codex/worktrees/operation-budgets/rust-service-template-rest`.
No live provider, CI receipt, or preapproved test inventory gates coding.

## Provides

One assembled implementation of C1–C13, matching tests chosen and written by
the executors, adapted existing callers, removed superseded deadline arithmetic
and markers, accurate API/lifetime/retry documentation, and portable profile
custody. The Lead returns one Implemented result; final validation follows once
all writers have joined. It creates no per-lane acceptance receipts.

## Boundary

Every C1–C13 responsibility is assigned to T001; none is deferred or silently
proved by a reference PR. C1 supplies the neutral always-retained leaf; C2–C4
carry it through HTTP/gRPC admission, response custody and opaque PreparedCall;
C5–C8 bound auth/cache/outbound/OAuth; C9–C10 own S3 admission and autonomous
Download resource release; C11–C12 preserve messaging/job attempt origins;
C13 closes manifests, profiles, classifier and documentation with the same
candidate. Tests stay beside each current semantic owner as mapped in design.

Preserve native Moka takeover, process-owned refresh/recovery, both gRPC
FullRpc and OpeningOnly intervals, and absent/long caller stream lifetimes.
Prepare the concrete-client-bound gRPC call before credentials. A known mutation
result from a live-started final synchronous poll retains its adapter Result;
the outer terminal owner still forbids late successful terminal delivery.
Pending mutation stop remains unknown. Complete GET success still requires
timely confirmed EOF. S3 uses a weak expiry owner without a producer queue;
body, withheld chunk, permit and observation custody terminate together.

No new retry, breaker, shared-auth registry, error catalog, duration, universal
reserve, runtime key, registry package, upgrade, or new infrastructure.
PostgreSQL's 100 ms reserve, transaction/CommitUnknown/native SQLx custody,
attempt/backoff/identity/settlement policies, generated wire schemas, config,
vendor, bootstrap and worker lifecycle retain their current owners. Do not
edit these merely to simplify propagation. A real contradiction reopens the
smallest accepted owner below.

## Mutable owners and implementation order

The Lead owns T001 integration and allocates disjoint subsets of the accepted
ownership map. These are optional execution lanes, not ledger tasks or fixed
waves. Serial implementation remains valid when delegation is not useful.

1. Establish C1's concrete neutral API and its first production artifact. The
   Lead owns shared Cargo declarations, consumer manifest edges, architecture
   registration and the deliberate minimal Cargo.lock update. Preserve all
   existing registry package versions/checksums and supported feature intent;
   use existing resolved packages only. Do not regenerate the lockfile as a
   validation side effect. Consumers may start as soon as the agreed API and
   required code are available, without waiting for checks.
2. Useful disjoint responsibility groups are transport plus inbound auth
   (C2–C5, including the prepared gRPC client), cache plus outbound HTTP (C6–C7),
   complete S3 operation/body custody (C9–C10), and messaging plus jobs
   (C11–C12). Each group owns its crate source, colocated tests and existing
   crate-specific fixtures. Shared manifests and cross-crate callers remain
   with the Lead unless explicitly transferred with no overlapping writer.
3. OAuth composition (C8) consumes the landed prepared-client and outbound
   context APIs from C4/C7. Start it when those outputs are available; no test
   receipt gates that handoff. Keep gRPC/OAuth edits serial wherever public API
   repair would overlap an active writer. The Lead integrates existing callers
   serially under the ownership map's deterministic caller-adaptation rule.
4. The Lead closes C13's central profile/marker/classifier and documentation
   surfaces against the actual APIs. Crate-local documentation can be assigned
   to its crate writer; the shared operation-budget guide, architecture guides,
   profile manifest and classifier have one writer. Publish the new unconditional
   paths to the existing initializer/runtime classifier, update existing
   retention/projection carriers, and remove obsolete marker declarations.

All lanes choose their test cases, fixtures, assertions and exact commands
while coding. Final build/test/review are not schedulable implementation work.
Bounded coding diagnostics follow the Implementation owner's feedback rules.
The Lead releases finished scopes, consumes returned code and starts newly
ready work immediately. Any new overlap is serialized; it is not permission
to move accepted ownership or duplicate a mechanism.

## Exclusive locks

- Shared Cargo graph: root/consumer Cargo manifests and Cargo.lock, held by the
  Lead while changing them; an individual crate writer cannot mutate that graph
  concurrently, even through a different manifest.
- Architecture/profile/classifier custody: `quality/architecture.json`,
  `scripts/lib/template_profiles.json`, `scripts/ci/changed-surfaces.sh`, the
  mapped profile test carriers and any mechanically required initializer edit;
  one designated C13 writer.
- Every delegated semantic source/test scope has one writer. Cross-crate caller
  integration and public API corrections serialize with its affected owners.
- The canonical `tasks.md` index belongs only to the Ledger Orchestrator once
  Implementation begins. Serialize every CPU-heavy Cargo diagnostic, build and
  test under `scripts/ci/validation-lock.sh`; ordinary `make build`/`make test`
  do not implicitly acquire it. Other worktrees are active. Final validation
  starts only after all implementation writers stop. Do not clear shared caches.

## Final validation

- Claim: The assembled candidate satisfies B1–B6/C1–C13, including stopped
  pre-dispatch refusal, isolated auth waits, selected OAuth interval, transport
  terminal enforcement, confirmed EOF, no-poll resource release and retained
  effect finality without replay. Existing wire/error/retry owners and profile
  support stay consistent. These are accepted behavior claims, not an approved
  test matrix or a mandate for live-provider certification.
- Checks: The same T001 Lead is the sole delivery owner. Once all code is
  assembled and writers joined, select one non-overlapping final plan from
  AGENTS.md and the current route. Several manifests require the matching
  workspace build and tests (`make build`, `make test`); changed Markdown
  requires static consistency and `make docs-check`. Carry architecture,
  dependency/security, classifier self-test, changed-shell and profile checks
  at their existing local/CI admission boundary. At final Completion only, use
  `make plan` to resolve actual changed surfaces; `make verify` is an optional consolidated carrier,
  not a second proof pass. Executors record the concrete remaining checks for
  final validation. Run no CPU-heavy checks concurrently or full aggregate
  merely for confidence; reuse valid evidence and rerun only invalidated proof.
- Review: One fresh independent review of the final assembled delivery under
  the Implementation Review adapter, covering changed auth, cancellation,
  resource and finality interactions; repairs remain within this unit. No
  per-task or per-lane review/acceptance gates.
- Observable: Local build/tests and scoped static evidence establish only
  their exercised boundaries. Existing initializer runtime/image and selected
  provider integrations remain CI-owned; projection/build work is not multiplied
  across harness/database dimensions. Preserve every selected gate, including
  database integration if manifests/job paths select it, without adding a new
  local database claim. Do not use heavy/full flags or create environments to
  duplicate CI. Optional unavailable observations are disclosed, not blockers.
- Delivery: Commit/push and one separate draft-then-ready PR are authorized.
  Follow External Effects before those actions, attach the PR, and observe its
  selected gates on the current head after ready-for-review; draft deferral is
  not full CI success. Retain `required` and `codeql-required` as the repository's
  admission owners and consult current workflows for exact jobs. Local Accepted
  may precede CI, but the ledger remains ready until requested PR/CI Completion.
  No merge/deployment/infrastructure action is authorized by this packet.
- Additional checks: none beyond accepted behavior, applicable repository
  policy and existing selected delivery gates. No benchmark or live bucket run.

## Reopen if

Return a mechanism contradiction to System Design; a responsibility/dependency
or placement contradiction to the affected ownership-map row; behavior/finality,
new global duration/reserve/replay or failure identity to Specification. Scope
or external authority changes return to the continuation root. Routine API
spelling, tests, commands, caller adaptation and scoped coding repair remain
with Implementation. A missing optional runtime does not reopen Planning.
