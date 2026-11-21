# Technical Design

Continue autonomously in the same session after [Specification](specification.md)
writes `specs/<task>/spec.md`. Own technical architecture (**HOW**) under
[Decision Ownership](../../../AGENTS.md#decision-ownership).

**Boundary rule:** Close every choice that can collide across parallel tasks,
alter a caller, wire, schema, config, or operator contract, break an invariant
or bound, or lock in a **one-way door**:
- persisted schema or durable state machine;
- external wire or API contract;
- `≥2`-consumer cross-crate seam;
- async durability or recovery topology;
- new crate, dependency, or profile.

Leave internals behind one module's seam open to Implementation.

## Steps

1. **Ground in existing capabilities and live code (read-only; protect the `smart zone`).**
   - Delegate wide multi-crate scans, exhaustive-type fan-out, and `Cargo.lock`
     source lookups to a fresh `evidence-agent` via [Read-Only
     Delegation](../shared/read-only-delegation.md), verifying decision-critical
     anchors directly so this session stays lean for architectural thinking.
   - **Check existing `crates/infra-*` capabilities first:** before designing
     any persistence, job, outbox/messaging, idempotency, auth, outbound
     HTTP/gRPC, webhook, cache, object-storage, or telemetry mechanism, inspect
     its existing crate owner, matching `docs/decisions/*-decisions.md`,
     [Production Contract](../../production-contract.md), and [Backend Library
     Selection](../../backend-library-selection.md).
   - Ground touched crates, callers, schemas, migrations, config, `api/`,
     [`quality/architecture.json`](../../../quality/architecture.json), and
     **one existing sibling subsystem** (verify its invariants before adopting
     its idioms; justify any deliberate divergence in `D<n>`).
   - Surface any verified premise contradiction directly; if zero coding delta
     remains, verify via [Plan Review](plan-review.md#zero-delta-check) before
     stopping.
   - *Done when:* existing `crates/infra-*` owners, `docs/decisions/` rules,
     affected callers, any modified exhaustive public workspace types, and
     baseline anchors are verified in primary sources.

2. **Map the Decision Frontier before proposing solutions.**
   - List every technical decision implied by `spec.md` as a **neutral
     question** (no proposed mechanism yet) and classify each:
     - **Forced:** one concrete shape fixed by live code, `docs/decisions/`, or
       `quality/architecture.json` with zero realistic two-engineer divergence.
       Record its primary anchor; write no `D<n>`.
     - **Two-way door:** reversible open choice outside the five one-way-door
       categories above (for example, an internal or single-consumer seam,
       local mechanism, or reversible config/signal convention). Close via the
       reuse-first steelman ladder.
     - **One-way door:** open choice in any of the five one-way-door categories
       listed in the Boundary rule above.
   - **Fast path for lightweight architecture:** when **none** of the [Design
     Review Gate](#design-review-gate) triggers hold and zero open forks
     remain, record the forced owners and pinned identifiers in a compact
     `## Architecture` section inside `spec.md` (`status: draft`) and proceed
     directly to [Planning](planning.md).
   - *Done when:* every `R<n>` and `G<n>` maps to a verified forced owner or an
     explicit open fork tagged `one-way door` or `two-way door`.

3. **Close open forks across triggered branches.**
   - Load only the branches whose observable trigger fires:

     | Trigger | Load |
     | --- | --- |
     | Any **one-way door** not already fixed by `docs/decisions/` or an existing crate contract | [Design It Twice](technical-design/design-it-twice.md) |
     | Any **runtime crossing** (DB, cache, queue/outbox, worker, network/provider, config/secret, or operator lifecycle) | [System Crossings & Probes](technical-design/system-crossings.md) |
     | Any new or changed **Rust seam**, crate edge, error contract, workspace type, or profile boundary | [Rust Architecture](technical-design/rust-architecture.md) |
     | Schema, payload, or wire change across live versions, new required config/trust material, or ordered rollout across `migrate` / `service` / `jobs-worker` | [Release Closure](technical-design/release-closure.md) |
     | Decision-changing external contract or empirical runtime uncertainty | [Research](research.md) (persist `specs/<task>/research/<topic>.md` when reused) |

   - When both fire, close **System Crossings before Rust Architecture** (revise
     System Crossings if Rust compile reality falsifies an assumption).
   - **Reuse-first steelman ladder:** before adding any mechanism, crate,
     module, seam, trait, dependency, queue, fallback, or config key, steelman
     the strongest simpler substitute along `extend owner / collapse shallow
     chain -> std -> locked Cargo.lock crate -> maintained OSS -> custom` and
     name the exact `R<n>`, `G<n>`, invariant, or bound it breaks.
   - **Uncertainty disposition:** settle static facts from primary sources now;
     run a bounded read-only [Research](research.md) probe for decisive
     external or runtime unknowns; for an unproved compile or integration bound
     over an otherwise closed choice, record an explicit unverified assumption
     (`A<n>`) with `Reopen if` and mark an `[integration-first]` slice for
     Planning. Escalate to the user strictly for a product or risk trade-off.
   - *Done when:* every triggered branch clears its own completion bar, and
     every added mechanism, crate, seam, trait, dependency, or config key
     passes the **deletion test** and reuse-first steelman.

4. **Persist architecture and clear [Design Review](design-review.md) when triggered.**
   - Apply [Design Review Gate](#design-review-gate):
     - **Substantive architecture:** write `specs/<task>/design.md` (`status:
       draft`) using [Design Artifact](#design-artifact) and run one fresh
       [Design Review](design-review.md) on `spec.md` + `design.md` (plus
       `intent.md` and `research/*.md` when present) under [Review
       Lifecycle](../shared/review.md#lifecycle). Verify every finding against
       primary code, repair confirmed defects in place, record any bounded
       `CONCERNS` in `design.md`'s `## Assumptions & reopen triggers`, flip
       `spec.md` and `design.md` to `status: ready` with `Design review: PASS |
       CONCERNS`, and continue into [Planning](planning.md) (if heavy design
       exploration and review pushed context past the **smart zone**, compact
       around the ready `spec.md`, `design.md`, and non-obvious Intake *why*
       before slicing tasks).
     - **Lightweight architecture:** record a compact inline `## Architecture`
       in `spec.md` (`status: draft`) and proceed directly to
       [Planning](planning.md), where [Plan Review](plan-review.md) audits the
       combined bundle and verifies the skip condition held.
   - *Done when:* architecture is on disk with zero unresolved forks and any
     triggered Design Review has cleared (`spec.md` + `design.md` at `status:
     ready`).

## Design Review Gate

**Substantive architecture** requires `specs/<task>/design.md` and a cleared
fresh [Design Review](design-review.md) before [Planning](planning.md) whenever
**any** of the following applies:

1. Any **one-way door** (defined in the Boundary rule above).
2. Any **runtime crossing** introducing or changing DB/tx scope,
   retries/`CommitUnknown`, cancellation, concurrency/locking,
   budgets/backpressure, trust boundaries, or operator lifecycle.
3. Changes spanning `≥2` workspace crates or adding/changing a cross-crate Rust
   seam.
4. Any **non-trivial rollout** (mixed-version schema/payload/wire evolution,
   new required config/trust material, or ordered multi-binary deploy).

**Lightweight architecture** (inline `## Architecture` in `spec.md`, skipping
Design Review) is permitted strictly when **none** of 1–4 hold.

## Design Artifact

Persist as `specs/<task>/design.md` (or inline `## Architecture` in `spec.md`
for lightweight changes). Omit empty optional sections; link unchanged code,
`api/`, migrations, and `research/*.md` instead of copying them. Keep `D<n>`,
`S<n>`, and `A<n>` stable on reopen, continuing `A<n>` numbering from `spec.md`
so IDs never collide across files.

- **Header:** `# Design: <short outcome>`, `status: draft | ready`, `Design
  review: pending | PASS | CONCERNS`, and link to `spec.md`.
- **`## Decisions`** *(omit when all choices are forced)*: `### D<n> — <title>`
  citing `R<n>`/`G<n>`/bound, rejected steelman (exact invariant or bound it
  breaks), accepted cost, and objective `Reopen if`.
- **`## Crossings, failure semantics & rollout`** *(when runtime crossings or
  rollout apply)*: entry-to-finality / crossing table, concrete System Probe
  contracts, and rollout/mixed-version impact per [Release
  Closure](technical-design/release-closure.md).
- **`## Rust seams & ownership`** *(when code seams change)*: touched
  `<crate>::<module>` actions (`extend | add | replace | delete`), owned
  invariants, workspace-graph/profile/exhaustive-type fit, and `### S<n> —
  <seam>` (signature, error/retryability/lifetime contract, dependency
  category, and happy + worst-case failure/cancel **caller walkthrough**).
- **`## Handoff deltas for Planning`** *(record only cross-cutting items not
  already pinned inside `D<n>`/`S<n>`)*:
  - **Pinned shared identifiers:** config keys, error codes, metric names, and
    wire/table names that parallel tasks must match verbatim.
  - **Shared mutated surfaces:** canonical lock paths + modified exhaustive
    public types with cross-crate fan-out.
  - **Superseded paths & generated contracts:** shallow modules, wrappers, and
    obsolete tests to delete; `api/`, `.sqlx/`, `crates/grpc-contracts`.
  - **Durable doc updates:** `docs/decisions/` (add a new ADR only when hard to
    reverse, surprising without context, and the result of a real trade-off, or
    when overturning a rejected choice), `docs/repo-architecture.md`,
    `docs/production-contract.md`, or `docs/backend-library-selection.md`.
  - **Execution risks & staging:** unverified compile/integration assumptions
    (`-> [integration-first]`), wide-refactor blast radius
    (`-> expand–migrate–contract`), and multi-release boundaries.
- **`## Assumptions & reopen triggers`:** `A<n>` technical assumptions,
  uncached crate checks, unverified compile bounds, and carried Design Review
  `CONCERNS` + objective `Reopen if`.

## Calibration Bar

- **Steelman vs strawman `D<n>`:**
  - *Strong:* *"Unique-key upsert + idempotent retry over session-level
    Postgres advisory lock, rejected because session locks collide with
    PgBouncer transaction pooling (`docs/railway-deployment-profile.md`).
    Reopen if the API pool moves to direct or session-pooled connections."*
  - *Weak:* *"Store in Postgres over an in-memory HashMap because memory loses
    data on restart."* (Strawman alternative; non-objective reopen.)
- **Concrete witness vs checkbox prose on System Probes:**
  - *Strong:* *"`UNIQUE (tenant_id, idempotency_key)` guards insert; outbound
    budget `2s + 200ms + 2s = 4.2s <= 10s` `OperationContext` (single retry
    owner: `OrderClient`); interleaving witness: `T1 commits order -> process
    crashes before HTTP reply -> T2 retries same key -> reads committed receipt
    -> returns 201 without duplicate`."*
  - *Weak:* *"Idempotency handles retries; unique constraint prevents races;
    timeouts fit budget."* (Zero mechanism, zero arithmetic, zero witness.)
- **Deep vs shallow `S<n>`:**
  - *Strong:* 1–3 high-leverage entry points owning the invariant
    (*`OrderLedger::commit(&mut Tx<'_>, ValidatedOrder) -> Result<Receipt,
    CommitError>`*), proven with a happy + failure/cancel caller walkthrough
    hiding SQL, serialization, and error classification behind the seam.
  - *Weak:* 4 pass-through layers (`handler -> service -> repo -> mapper`)
    forcing the caller to coordinate call order or match raw SQLx errors.
- **Seam contract vs prescribed internals:**
  - *Strong:* Pin the public/cross-crate seam signature, `&mut Tx<'_>` scope,
    `service_failure::Code` mapping, and shared config/metric names; leave
    private helpers and `.rs` file layout to Implementation.
  - *Weak:* Prescribing `src/service.rs`, `src/repo.rs`, `src/mapper.rs`,
    private helper bodies, or unit test names.
