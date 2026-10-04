# T1 — Bounded native SQLx connection return

Outcome:
Replace the indefinite native return wait after cancelled or silently stalled
I/O with the accepted five-second whole-return bound, preserving native reuse,
pool capacity and transaction finality. The backport travels correctly through
the template's dependency, Docker and optional-profile paths.

Consumes:
- [R1 and compatibility](../spec.md#r1-cancellation-releases-capacity-without-a-network-acknowledgement) — accepted behavior; the three-second acquisition budget can expire during five-second cleanup.
- [Library return and finality](../design/design.md#library-return-cancellation-and-finality) — selected runtime ownership and unchanged pending-BEGIN/COMMIT contracts.
- [Dependency custody](../design/dependency-custody.md) — verified archive, exact narrow patch, source-only locked projection, Docker/profile/classifier owners and retirement condition.
- [Ownership map](../design/ownership.md) — current code, tests and documentation owners.

Provides:
- One verified published sqlx-core 0.9.0 vendor source with its isolated return patch and provenance; unchanged remaining resolved graph.
- Complete source/delivery/profile containment and matching existing gate selection.
- Authored regression coverage and accurate active documentation of bounded cleanup and its limits.

Boundary:
Keep the source fix and its portable/removable carrier in one unit. Download
verification precedes extraction/patching; canonical Cargo/profile/classifier
edits precede consumption by projected outputs. Apply only custody's fail-closed
source projection and retain locked Cargo throughout. The initializer is not a
new general lockfile updater. Keep healthy native reuse, existing close policy,
pending-BEGIN disposal, uncertain COMMIT meaning and caller deadlines. No
application Pool/Executor facade, cleanup task, retry, capacity increase,
driver/version/feature upgrade, registry-cache edit or new CI job.

Mutable owners:
- Dependency custody: `vendor/sqlx-core/`, root `Cargo.toml` and `Cargo.lock`; only the upstream return source and local PATCHES record differ from the verified payload.
- Portable delivery/profile routing: `.dockerignore`, `build/docker/Dockerfile`, `scripts/lib/template_profiles.json`, `scripts/ci/changed-surfaces.sh` and its existing self-test.
- Primary behavioral regression: `test/tests/postgres.rs` and existing `test/tests/support/commit_proxy.rs` when a material coverage gap needs it.
- Active bounded-return/custody documentation in `docs/architecture/persistence.md` and `docs/validation/postgres.md`; preserve sections owned by other units.

Exclusive locks:
- Root dependency graph, vendor payload, profile projection and Docker source-carrier edits.
- Primary PostgreSQL regression/relay fixture files while mutated.
- The two existing persistence/validation guides while mutated; T2/T3 wait for release of overlapping files.

Final validation:
- Claim: Native capacity eventually recovers from the accepted silent cancellation/return paths without a peer acknowledgement, restart or larger pool; healthy reuse and existing finality survive.
- Checks: The assembled local criterion plus accepted R1 database observations under the existing PostgreSQL validation owner. Establish verified source identity, Cargo-accepted locked source-only graph, representative retained/absent PostgreSQL projection, dependency policy and truthful vendor-only gate routing. The permanent regression rejects the unpatched retention defect. Exact cases/commands are executor-owned; existing image and CI gates keep their existing scope.
- Observable: Capacity is locally released after bounded cleanup and later useful work succeeds once replacement connectivity permits it; no false immediate-backend-termination, rollback, COMMIT or first-replacement-success claim. PostgreSQL-absent projections contain no dangling vendor references, while retained projections resolve the actual patch.

Reopen if:
Technical Design for source/graph drift, invalid return ownership, uncontainable
delivery/profile integration or failure of the selected mechanism; Specification
for changed budgets, supported deployments or failure/finality behavior; Intake
only for requester meaning or authority. Ordinary code/test repairs stay here.

## Execution notes

The published archive was checksum-verified before extraction: 110 files,
648,948 bytes. The isolated `connection.rs` backport is byte-identical to the
reviewed PR 4407 head; `vendor/sqlx-core/PATCHES.md` records archive, source,
lock hashes and the exact delta. The one-time fail-closed lock projection
removed precisely sqlx-core's registry source/checksum fields and retained all
other record bytes. Root TOML's intended semantic edit was routed through yq;
marker-preserving output was parsed and matched against that result.

Coding feedback: baseline and patched `cargo metadata --locked --offline
--format-version 1` both resolved; normalizing only sqlx-core's source identifier
leaves the entire 588-package resolve graph, features and workspace membership
unchanged. The local core is one excluded non-workspace package. This is source
custody/static feedback, not a task acceptance gate or runtime proof.

Authored tests use the existing real-server PostgreSQL owner and joined protocol
fixture. The prior idle-only coverage could not catch a native slot retained
after cancellation or successful SQL; the new cases deliberately keep old sockets
silent while observing local release and replacement work. Independent server
reads distinguish cancelled autocommit/COMMIT from uncommitted work. A repeated
success-then-silence case checks cumulative capacity loss; cancelled acquisition
keeps the existing healthy backend. Pending-BEGIN coverage now withholds its
reply through replacement work. No production test seam was introduced.

The whole-return bound, carrier removal and finality meaning remain as designed.
No behavior run, full build/test/lint, image, scan or database suite ran at T1.
Final Completion owns unpatched negative-control observation, permanent five-second
behavior, representative profile projection, dependency policy, required build
and tests, and final review. Unrun CI/image gates remain pending.

Bounded compile-only feedback found a shared fixture consumer's exhaustive
`Fault` match in `test/tests/jobs/execution.rs`. The orchestrator released that
single caller repair to T1; it explicitly rejects the silent variant because
its existing two tests exercise connection-close faults. After the repair,
`cargo check --locked -p integration-tests --features integration --tests`
completed under the shared validation lock (3.75 seconds). No test/application
code executed. Production observer files remained T2-owned throughout.
