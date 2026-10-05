# Integration into main

Status: PR236 is draft; merged lock generated remotely and publication fixes
assembled. Matching exact-head validation and integrated review are pending.

The requester authorized delivery of this session's changes to the main
branch on 2026-10-05 Moscow time. Target repository:
`Dankosik/rust-service-template-rest`; frozen upstream base
`dc13579404271c45b40ee487d23875631d9906e0` (PR235, PostgreSQL pool resilience). Integration branch:
`codex/integrate-hotpath-20261005`, in a separate clean worktree.
The original dirty checkout and unrelated task bundles remain untouched.

## Retained scope

- Feature-only hotpath0.28.4 instrumentation, with the retained jemalloc,
  optional MCP/alloc/CPU features and driver tracing.
- Owned webhook body transfer and private streaming Base64 serialization.
- Borrowed standard HTTP method/protocol attributes and static numeric status
  labels. Eager span-name construction remains unchanged.
- This session's three profiling/optimization reports, source decisions,
  reproducible scripts and public synthetic evidence receipts.

The historical [measured result](report.md) and [review](review.md) apply to
their recorded Rust1.98.1 source/binaries. This integration preserves their
scientific results; it does not relabel them as measurements of current main.
Global pool default4 and production remain unchanged; the task-local synthetic
profile8 is not a production configuration.

## Upstream compatibility dispositions

Readability PR229 is already merged with terminal successful CI/CodeQL.
Keep its four checker projections, shared webhook test recorder, flattened
HTTP oracle/gRPC fixture, and its current allowances. Do not restore obsolete
three-projection documentation or add the two obsolete test allowances.
Unrelated research/benchmark bundles from other sessions are preserved locally.

Keep current main's Rust1.99.0/toolchain mirror, vendored SQLx-core provenance,
pool return/recovery ownership, image and classifier gates. The conflict in
HTTP ingress combines the upstream request deadline/attempt budget and outcome
metrics with `receive_bytes`; timeout/unavailability classification is retained.
The lockfile starts from current upstream and is intentionally re-resolved by
Cargo on a GitHub Actions runner to add the pinned optional profiler, never
hand-merged. The temporary source-generation helper is removed before the
final candidate; its results are generated-source evidence, not passing
exact-head validation of the later candidate.

## Proof and external-effect boundary

All resource execution remains remote. The requester's continuation
"через PR давай" selects the PR/CI route. GitHub Actions owns source generation
and the normal exact-head gates; nothing compiles or executes locally.
The previously proposed paid droplet is unnecessary and is not created.
After lock resolution, select one matching mixed-surface route through the
repository's existing plan/verify owners, with the default and profiling build
identities distinguished. Reuse historical performance evidence at its actual
boundary. One fresh independent integrated review covers the source delta,
upstream seams, final proof and all retained/rejected optimization dispositions.
Required exact-head GitHub checks must pass before merge; no gate bypass.
The delivery owner retains evidence, deletes its exact droplet and confirms
remote merge plus local main synchronization.

## PR source-generation receipt and repairs

PR: https://github.com/Dankosik/rust-service-template-rest/pull/236.
Actions run37245027607 generated the lock from head
`c1f2b9fed63ee46d7a065adbaf4f4ff2cfe3266d` on Rust1.99.0 using the explicit
`cargo update --workspace`, followed by locked metadata/tree inspection.
It added14 packages, including exact hotpath/drain/macros0.28.4; existing
packages were not upgraded. The default resolved graph contains neither
hotpath nor its macros; sqlx-core remains the accepted0.9.0 vendored package,
jemallocator remains0.7.0. The artifact was downloaded; the temporary workflow
was removed. No result of that source-generation job is final-candidate proof.

Initial CI37245027633 failed, as retained in local integration evidence:
unresolved lock, two unregistered optional-dependency profile markers, missing
rustup-generated host include in archive-script lint, and the public fixed HMAC
fixture. Repairs register only those existing markers, annotate only the ten
external source directives, and allow only the exact public32-character value
AND three named synthetic load.js paths for generic-api-key. Other values,
paths and detection rules retain their gates. Syntax follows
[Gitleaks8.30.1](https://github.com/gitleaks/gitleaks/blob/v8.30.1/README.md#configuration).
No genuine credential was detected or published; the scripts already identify
the literal as synthetic.

The ordinary CI route now additionally compiles hotpath without allocation
wrapping, then the alloc/MCP/CPU/Prometheus combination, and records its feature
edges. Both use the current service manifest, locked graph and same pinned
toolchain; this is separate from default build/test proof. Historical measured
scripts have only source-location comments added for publication lint; their
executed input bytes remain in the immutable historical evidence archives.
