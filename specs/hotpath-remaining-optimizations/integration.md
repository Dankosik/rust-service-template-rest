# Integration into main

Status: source assembled; lock resolution, matching validation, final integrated
review and publication pending.

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
