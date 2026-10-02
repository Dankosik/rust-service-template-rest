# Cache reliability implementation unit

Status: ready. One fixed unit; no ledger or separate test plan.
Baseline `67be869acea112af271ec8ba621cbc50ae9d36b7`, branch
`codex/cache-reliability-closeout-20261002`, worktree
`/Users/daniil/.codex/worktrees/cache-reliability-closeout/rust-service-template-rest`.

## Outcome

The optional cache recovers from established stalls and rejected password
rotation with bounded, owned background work, sanitized diagnostics, and
guidance consistent with its actual lifecycle and dependency boundary. Deliver
R1-R5 together under the existing public cache contract.

Atomicity: the supervisor, generation fencing, credentials and diagnostics
share one lifecycle and jointly establish this outcome; guidance describes
that same behavior. None is a separately accepted intermediate delivery.
Implementation may use disjoint internal lanes without adding unit gates.

## Consumes

- [Intent](intent.md) and [Specification](spec.md#accepted-behavior) — accepted
  meaning, R1-R5, exclusions and effect authority; required before coding.
- [Design selection](design/reliability.md#selection-and-evidence),
  [ownership](design/reliability.md#ownership-and-state),
  [flows](design/reliability.md#material-flows),
  [bounds](design/reliability.md#bounds-and-accepted-costs) and
  [placement](design/reliability.md#placement-and-dependency-decision) — closed
  mechanism, lifecycle budgets, dependency edges and inverse file map.
- [Technical Design review](technical-design-review.md) and
  [transition](technical-design-transition.md) — independent PASS and readiness.
- [Implementation owner](../../docs/spec-first-workflow/phases/implementation.md)
  and [validation budget](../../AGENTS.md#validation-budget) — coding feedback
  and one assembled final-validation boundary. Tests are chosen while coding.

## Provides and obligation reconciliation

| Obligation | Current-to-target delta and owner | Final observable |
| --- | --- | --- |
| R1 | `infra-cache` replaces manager replacement and detached warm-up with the design's single supervisor over canonical multiplexing, identity-fenced retirement and independent progress. `lib.rs` retains admission/public operations; private `connection.rs` owns lifecycle. | Operations remain within C with one dispatch and no replay; silent or obsolete generations retire, a successor can serve calls, late old failures cannot retire it, and retained generations/slots obey the design's bounds including public long timeouts. |
| R2 | The shared application owner held by Cache/Namespace/Probe cancels and aborts owned work on final drop; the supervisor holds no owner cycle. Existing service dependency drop remains the integration. | Final-owner release initiates cancellation without waiting out warm-up; legitimate retained handles preserve ownership, and normal/failed/interrupted startup cleanup retains its existing service boundary. |
| R3 | `credentials.rs` retains admission policy and supplies bounded reads and authenticated-versus-pending state to the supervisor; remove the streaming provider. | Rejected unchanged bytes remain retryable without traffic, usable later credentials recover within accepted bounds, unavailable files preserve a usable connection, and new connections always reread the file. |
| R4 | Existing `observe.rs` remains classification authority; setup/AUTH/PING/command paths retain only sanitized errors and the streaming driver's raw logging path is removed. | Rendered dependency logs and public diagnostics omit raw server text and cache secrets while failures remain classifiable and AUTH success alone records credential acceptance. |
| R5 | Update cache guide/decisions, architecture cache sections, backend-library selection and crate examples identified by the design. | Moka permits independent copies across replicas; composition/adapters invoke the provider for feature-owned behavior; lifecycle/rotation guidance matches the code without a speculative trait or feature. |

## Boundary and mutable owners

The [design placement table](design/reliability.md#placement-and-dependency-decision)
is the authoritative file map: `crates/infra-cache` production, credential,
observation and existing test surfaces; its manifest and the resolved lockfile;
and the named cache documentation. Existing service process tests may receive
necessary behavioral coverage; bootstrap/config/health production code remain
consumers of the preserved contract. Evidence requiring a new async close or
service mechanism reopens Technical Design first.

Remove superseded manager replacement, detached `warm_up`, `WARM_UP_TIMEOUT`,
old retry ownership and streaming credential machinery, including tests coupled
only to those mechanisms. Retain adequate behavior coverage. Add only the
design's already-declared dependency edges, remove unused old edges/features,
and refresh the lockfile through normal dependency tooling without version
upgrades or manual editing. All changed sources are manual; no generated API
contract changes are planned. Existing profile projection is consumed by its
CI gates rather than a new generator or runner.

Keep exclusions in [Specification](spec.md#deliberately-unchanged-and-excluded).
No new library/version, configuration key, topology, feature interface, retry
of user commands, telemetry redesign, benchmark, merge or deployment.

Exclusive locks: serialize mutation of the cache lifecycle/credential seam and
the crate-manifest/lockfile pair within the unit. Final validation exclusively
consumes the assembled candidate under existing repository validation locks;
no concurrent CPU-heavy validation. Documentation/test lanes, if used, need
disjoint file ownership. There is no new generator or migration lock.

## Final validation and delivery

After code, tests, cleanup and docs are assembled and all writers are joined,
the Acceptance-Unit Lead owns local final validation and independent final
delivery review. Claims are R1-R5 above and the design's stated invariants and
bounds. The executor chooses the smallest meaningful cases and commands,
reusing existing protocol, redaction, admission/TLS, credential and lifecycle
coverage; record actual commands and exercised scope with the completion result.

The planned manifest/lockfile change selects the existing workspace build and
test route in AGENTS.md. Documentation requires static consistency review and
`make docs-check`. Use current validation owners to consolidate these checks,
not duplicate them through an aggregate. Concrete tests, fixtures, assertions
and proving layers remain Implementation work; no additional local acceptance
gate is introduced. Do not create infrastructure or use heavy/full modes to
reproduce CI. Passing tests alone do not replace the final independent review
required by the changed credential and concurrency behavior.

CI-owned Valkey, profile/initializer and image gates consume the published PR
candidate, after local assembly/proof/review; they do not block coding and
remain outstanding until their actual selected CI results are obtained. The
continuation coordinator owns publishing this branch, creating/attaching the
one separate PR, and reading those results. No merge or deployment is authorized.
Local acceptance and CI/PR completion must be reported separately.

## Reopen if

Return to Technical Design for contradicted clone-drop/cancellation semantics,
generation or recovery accounting, or a required different lifecycle mechanism;
Research for changed driver evidence; Specification for an actual adopter or
behavior incompatibility; Intake for changed scope/authority. Routine coding,
test choices, focused repairs and mechanical file/command updates remain with
Implementation. Planning reopens only if a real independent acceptance boundary
or dependency invalidates this single unit.
