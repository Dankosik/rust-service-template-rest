# Technical Design transition

status: ready

owner: Technical Design

result: [Selected mechanism and ownership map](design/mechanism.md), ready
SHA-256 `fcdce3ac02307f82633e31e90515ace241343f310392816d0399d2a2c9fe0867`;
[library decision](design/library-decision.md), ready SHA-256
`e8698971426c2eea5e3a290683461ff9790167d422d0dab7861e3f25db4c2011`.
Authority is the [amended ready Definition](definition-transition.md), spec
SHA-256 `bc78c10a723502a01bc40d91db174910e6fed6a9e18f1cc39e9bd1b862af3236`.

review: [Fresh Technical Design review](design/review.md), PASS with no
surviving findings. Draft-to-ready status changes are mechanical and preserve
the reviewed semantic candidate.

movement_evidence: One mechanism now closes all accepted deltas: existing
adapter retained after a supported-seam library comparison; explicit completed
failure provenance/window; request-only absent lifetime and invalid overflow;
Moka weighted retention bounding both targets; and a nonspawning owned refresh
driver with final-owner cancellation, existing shutdown input, observed
completion and terminal closed-client admission. HTTP and the synchronous
gRPC cache path share closure semantics. Placement stays in the existing
adapter and its tests/docs. Planning need not choose policy, lifecycle, API,
cache accounting or library disposition.

reopen_owner: Definition for a changed observable behavior or compatibility
boundary; Technical Design for a changed lifecycle/cache mechanism; Research
for different library versions, dependency admission or contrary primary
evidence.

next_owner: Planning. The root remains continuation/publication owner. This
actor stops before Planning and implementation.

## Concrete handoff

- Source migration: replace the unmanaged constructor with
  `Credentials::prepare(Options)` returning Credentials and RefreshDriver.
  Composition must drive and await `driver.run(existing_shutdown_future)`;
  update supported examples and all existing fixture/consumer construction.
- Closed-owner precedence is explicit: closed plus elapsed deadline yields
  Timeout; otherwise closed yields Unavailable before local composition
  refusals, cache hits or dispatch. Active owners retain the current conflict
  and missing-required-subject refusal ordering.
- The existing process root treats unexpected background task completion as
  failure. The integration lifecycle owner must distinguish expected driver
  completion after final handle release; do not blindly add an unused service
  registry or an always-running background task. Shutdown input permits the
  existing background-join phase to complete before dependencies are dropped.
- No new OAuth dependency or crypto backend is selected. Make used Tokio
  sync/macros features explicit in the adapter and preserve the locked graph;
  the published optional Huskarl native backend is not a rejection rationale.
- Local feedback and assembled validation belong to their next owners. Reuse
  existing fixtures and proof. No runtime build, Keycloak integration, CI,
  performance, memory or deployment result is claimed by this phase.

Static verification: final `make docs-check`, including review/transition
receipt links, passed (880 links, zero errors); `git diff --check` passed.
Docker is at `/usr/local/bin/docker`; include
`/usr/local/bin` in the command PATH alongside the supplied Cargo/Homebrew paths.
No environment, shared cache or runtime source was changed.
