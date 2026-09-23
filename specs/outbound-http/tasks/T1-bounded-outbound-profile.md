# T1 — Complete bounded outbound profile

Outcome:
A source template that currently has only auth-private outbound transport gains
one selectable `OUTBOUND_HTTP=none|bounded` profile. A generated service can use
the fixed-authority public-HTTPS API under its finite limits and runtime custody;
default output removes the outbound pack. Auth retains its accepted HTTP and
identity behavior while both consumers use the one corrected public-address
predicate and tracked DNS implementation.

Consumes:
- [Intent](../intent.md) — local stage-10.2 delivery and external-effect limit.
- [Specification: selection](../spec.md#outcome-and-selection),
  [destination admission](../spec.md#destination-and-connection-admission),
  [finite work](../spec.md#finite-work-and-returned-data),
  [auth coexistence](../spec.md#propagation-and-auth-coexistence), and
  [profile completion](../spec.md#profile-proof-and-completion-boundary) — accepted behavior.
- [System API](../design/system.md#public-transport-api-and-data-ownership),
  [flow and custody](../design/system.md#material-request-flow-and-custody),
  [lookup/deadline reuse](../design/system.md#lookup-cancellation-and-request-deadline-reuse),
  and [auth preservation](../design/system.md#auth-preservation-and-proof-boundaries)
  — closed mechanism and failure semantics, including the explicit IPv6 correction.
- [Rust ownership](../design/ownership.md#rust-files),
  [manifest/profile ownership](../design/ownership.md#manifests-and-profile-inventory),
  and [documentation/validation ownership](../design/ownership.md#exact-documentation-and-validation-owners)
  — placement, cleanup, inventory, compatibility, and proof carriers.
- [Technical Design Transition](../technical-design-transition.md) and
  [review](../technical-design-review.md) — ready input and review boundary.
- [Repository architecture](../../../docs/repo-architecture.md),
  [contribution policy](../../../CONTRIBUTING.md#validate-a-change), and
  [initializer validation boundary](../../../docs/template-sync.md#validation-boundary)
  — current source and validation ownership. The Implementation Lead loads
  selected leaf owners and matching skills before governed edits.

Provides:
- Complete `infra-outbound-http` API and `infra-egress-dns` leaf, with auth
  integration and readonly `infra_http::RequestDeadline` access.
- Strict initializer/lock/profile support, selected guide and complete pruning,
  with existing source checks and 96-projection/12-runtime proof runners updated.
- Tests beside their code owners and concrete final commands recorded by the
  executor for the assembled delivery owner; implementation alone is unverified.

Boundary:
This task includes the entire usable profile. DNS extraction, transport,
deadline access, projection and guide are companion layers, not independently
accepted intermediate products. A single task avoids a split that could accept
a client whose generated graph or safe resolver does not yet exist. The Lead
may assign disjoint internal lanes against the closed contracts; manifest,
marker and canonical inventory integration stays serial under the locks below.
No lane or completed subresult creates a test/review gate.

The following obligation map fixes coverage, not coding steps or test cases:

| Accepted obligation | Current-to-target delta and cleanup | Owner |
| --- | --- | --- |
| Shared admitted DNS | Extract current auth predicate/resolver/tracked runtime into the neutral leaf, move its proof, remove duplicate definitions, retain auth-private error translation/raw-answer fixture; apply the accepted mapped/NAT64/2000::/3/6to4 correction while retaining denials and public exceptions | System flow/custody; ownership Rust files |
| Bounded exchange | Add the selected public API and private reqwest policy; authority and literal admission, post-DNS all-answer admission, finite limits, propagation removal, single absolute deadline, admission permit through framed body, static errors, no proxy/redirect/retry/decompression/idle reuse | System API and flow/custody |
| Inbound lifetime | Expose existing deadline read-only and retain stamping order under auth OR outbound; provider owns response reserve, credentials, parsing and business errors | System lookup/deadline reuse |
| Auth coexistence | Replace only DNS imports/translation; preserve JWT/introspection, JSON/200 rules, 3-second cap, 100 ms reserve, 32-exchange admission, count-only header claim, no-replay and cancel/join custody | Specification auth coexistence |
| Manifests and generated lock | Add two path crates with existing dependencies/features, replace auth Hickory edge; deliberately regenerate Cargo.lock without upgrades, preserving guarded projection edge handling | Ownership manifests/profile inventory |
| Default and selected output | Add outbound input/env/lock/profile field; implement outbound, egress-dns and request-budget marker selection, exact inventory and full preflight-before-write; no-lock source remains capability-complete | Ownership manifests/profile inventory |
| Historical lock and sync | Admit only the two named legacy shapes plus the new complete shape; preserve matching replay bytes and refuse partial/unknown shapes; retain portable-ownership purity and no resurrection | Ownership manifests/profile inventory |
| Evidence-carrying projection | Extend existing equality keys, normalization, diagnostics and self-tests to 96 projections and 12 sequential public-init/build/test representatives from one private source candidate; extend exact candidate allowlist and directory admission | Ownership documentation/validation |
| Existing gate routing | Extend `scripts/ci/changed-surfaces.sh` and its self-test so new DNS/outbound crate paths and the guide select the existing `module_initializer` surface, as current auth counterparts do; no new gate or workflow | Contribution policy and existing classifier |
| Adoption | Add selected-only guide and exact companion architecture/first-feature/structure/commands/initializer docs and markers; show limits, reserve, deadline and cancellation hookup; stage 10.2 completion statement takes effect only with final acceptance | Ownership documentation/validation |

No concrete provider, bootstrap wiring, credential/config surface, readiness
change, OpenAPI operation, private-network mode, raw-client escape, streaming,
retry framework, new registry dependency/toolchain, Docker/deployment profile,
new CI gate or other stage-10 capability belongs to this unit. Existing TLS
fixture bytes may be copied under the outbound crate's test-only owner so
outbound-only pruning cannot remove required test inputs.

Mutable owners:
- Egress DNS leaf and bounded outbound crate; auth provider DNS adaptation and
  existing auth proof; inbound hardening deadline/re-export and their proof.
- Workspace/new-crate/auth manifests and deliberate Cargo.lock regeneration.
- Canonical profile inventory, state/initializer/sync consumers and make input
  forwarding; existing initializer/sync/purity/projection fixtures and tests.
- Existing initializer runtime runner, candidate snapshot allowlist/directory
  admission and changed-surface classifier/self-test, limited to this profile.
- Selected guide and companion documentation named in the ownership map;
  roadmap stage 10.2 only. Implementation may record its chosen final commands
  in this packet; the Orchestrator alone owns canonical ledger state.

Exclusive locks:
- Workspace dependency manifest and Cargo.lock generation/projection edges.
- Canonical profile inventory and all source marker registrations across Rust,
  Cargo and docs; every changed path/id must match before handoff.
- Initializer candidate snapshot/allowlist, shared runtime representative runner
  and existing classifier. These overlapping owners are integrated serially.
- Shared validation lock at final validation; no concurrent CPU-heavy commands.

Final validation:
- Claim: The complete selected profile enforces the Specification on its real
  transport boundary, auth retains its required parity except for the accepted
  stricter address admission, and the public initializer produces all supported
  complete graphs with safe replay/pruning/sync and full preflight.
- Checks: One assembled final plan under
  [Validation Routing](../../../docs/validation-routing.md) and the
  [Evidence Contract](../../../docs/spec-first-workflow/shared/evidence-contract.md).
  Carry the existing required source build and workspace tests for multi-crate/
  manifest changes, documentation links, applicable changed-shell checks,
  contribution-policy dependency/secret checks, and accepted initializer
  source/projection/12-runtime route. Exact cases and commands are chosen while
  implementing and recorded here; reuse valid equivalent results without
  duplicating their covered claims. No per-task build, test or review pauses,
  no per-harness Rust builds, no added full-repository aggregate or live-provider/
  PostgreSQL runtime claim. Run the required independent final delivery review
  once on the assembled fixed candidate because destination admission and
  cancellation materially affect security and concurrency safety.
- Observable: The bounded API returns only admitted same-authority responses
  within accepted byte/header/time limits, refuses the specified invalid and
  over-limit paths truthfully, releases operation capacity and retains DNS
  shutdown tracking. The source build/tests and existing auth proof pass; all
  96 canonical projections prove the retained runtime equality, and 12 unique
  graphs each pass public initialization and one matching build/test sequence.
  Default output contains no outbound pack; selected output retains every
  required source, test and guide. All required checks and final review must
  actually pass before Completion is accepted or roadmap 10.2 is called done.

The executor chooses and writes tests within this task. The delivery owner
executes them after all code and tests are implemented and all writers joined.
No external runtime or service access is a coding dependency. Missing required
final evidence leaves implementation complete with verification incomplete;
optional unrun observations do not silently become gates. A final roadmap
status refresh is acceptance bookkeeping and cannot substitute for evidence.

Reopen if:
A pinned reqwest/Hickory/error-source or IANA assumption fails: Research, then
only affected Design decisions. A required lifecycle, dependency direction,
public API, marker or projection mechanism cannot follow the fixed design:
Technical Design. Changed destinations, byte semantics, propagation, supported
selection or auth meaning: Specification. Changed desired outcome or external
authority: Intake. Mechanical file locations and lane-lock adjustments retain
this outcome; return an actual missing input to the Lead/Orchestrator without
asking the user to choose a technical implementation.

## Executor handoff and selected commands

Implementation uses the existing source checkout and disjoint DNS, bounded
client, initializer/state, and proof-runner owners; shared manifests, inventory
and documentation integrate serially under the Lead. No task result is an
acceptance receipt. Roadmap 10.2 remains pending until Completion.

The deliberate lockfile mutation was
`/opt/homebrew/bin/rtk proxy /Users/daniil/.cargo/bin/cargo update --workspace --offline`.
This is the packet's explicit lock-regeneration action, distinct from validation
commands, which retain `--locked`. Cargo added only `infra-egress-dns` and
`infra-outbound-http` and changed auth's local edge. Comparison against source
HEAD confirmed all 495 registry package source/version/checksum tuples unchanged;
no registry dependency or toolchain was upgraded. Cargo.lock was not hand edited.

After all writers join, the implementation feedback command is one compile-only
pass over production and test targets:

```sh
/opt/homebrew/bin/rtk proxy /Users/daniil/.cargo/bin/cargo check --locked --offline --all-targets -p infra-egress-dns -p infra-outbound-http -p infra-bearerauthn -p infra-http
```

The delivery owner runs the following assembled final commands sequentially,
with the repository's pinned Cargo available on PATH and shared validation lock
for CPU-heavy commands. They are selected here, not claimed executed:

```sh
/opt/homebrew/bin/rtk proxy make fmt-check
/opt/homebrew/bin/rtk proxy make changed-surfaces-check
/opt/homebrew/bin/rtk proxy bash scripts/ci/template-init-check.sh --self-test
/opt/homebrew/bin/rtk proxy make shellcheck SHELL_FILES="scripts/ci/template-init-check.sh scripts/ci/changed-surfaces.sh"
/opt/homebrew/bin/rtk proxy make docs-check
/opt/homebrew/bin/rtk proxy make deny
/opt/homebrew/bin/rtk proxy make secret-scan
/opt/homebrew/bin/rtk proxy bash scripts/ci/validation-lock.sh -- make build test
/opt/homebrew/bin/rtk proxy env ALLOW_FULL=1 make template-init-check
```

The last command includes source safety/purity/sync cases, projection self-tests,
all 96 projections, and 12 public-initializer/build/test representatives; do not
repeat those subsets as extra acceptance gates. An explicit caller
`CARGO_TARGET_DIR` may retain the cache across the representatives. A required
independent final delivery review consumes the fixed assembled candidate after
all writers join. No full-repository aggregate, PostgreSQL runtime, image,
remote CI or external delivery result is added by this plan.

Implementation feedback completed: the four-crate/all-target compile-only pass
returned exit 0 (42.08 s). Its single unused-import warning was removed. Focused
outbound compile-only diagnostics after the raw-authority/limit and test-fixture
repairs also returned exit 0 without warnings. These checks execute no tests
and establish no transport or profile acceptance. Python syntax, shell syntax,
marker-pair registration, rustfmt on edited Rust owners, and diff whitespace
were checked during coding. Behavioral checks and independent final review
remain the delivery owner's pending work.

## Scoped validation repair continuation

The first full attempt, `.git/codex/template-init/attempt.Z96JHP`, completed
source suites, all 96 projections and runtime graphs 1–10. Graph 11 passed
public init/build but exposed intermittent auth TLS-fixture setup and a
single-read request-capture assumption; graph 12 was not reached. The full
attempt remains failed and is not relabelled as an aggregate pass.

The repair confines explicit fixture trust to its local CA, reads complete
bounded HTTP request frames, and bounds fixture lifetimes. Normal auth client
construction still supplies no custom root and retains its HTTP, identity,
timeout, admission and cancellation policy. A duplex fragmentation regression
covers the request-capture repair.

The existing runner now supports `--runtime-graphs` with distinct comma-separated
IDs 1..12, in DATABASE / AUTHN / OUTBOUND_HTTP nested order. This focused mode
requires `ALLOW_FULL=1`, uses the same private snapshot and complete public
initializer/build/test loop, and labels its receipt `mode=runtime-graphs` with
the requested IDs. Default full mode still selects all twelve graphs and source
suites; recorder/selector self-tests prove selection and fail-fast behavior.

The scoped continuation is:

```sh
/opt/homebrew/bin/rtk proxy bash scripts/ci/template-init-check.sh --projections-only
/opt/homebrew/bin/rtk proxy env ALLOW_FULL=1 bash scripts/ci/template-init-check.sh --runtime-graphs 3,4,5,6,9,10,11,12
```

Auth-free graphs 1, 2, 7 and 8 retain prior-candidate evidence only when refreshed
canonical projected-tree digests match their original receipts byte for byte.
The changed auth crate is pruned in those graphs; the runner and this packet are
source-only. Source identity in lock provenance is reported separately. Auth
runtime graphs are rerun; source auth/HTTP tests, source build, changed script
checks, current secret scan and reviewer delta recheck cover the repair. No
partial receipt independently claims the full matrix.
