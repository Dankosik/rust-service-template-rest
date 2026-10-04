# Native-pool design ownership review

```text
candidate: base 67be869acea112af271ec8ba621cbc50ae9d36b7; design.md SHA256 6b3e40a042e5de4fb164f98b6fe3a200670e12b54a1fc2e713b4f8716b9792aa; ownership.md SHA256 1692d92f430938fadf8b72f103b820d3f2a21fc3ebb867a4355579f71bf15fc3; dependency-custody.md SHA256 220290c617e8a2e0053e82886f466bff30e567beea846708a847a7e1f802c184; spec.md SHA256 0411915db5fbe82fcde601ca6f1621248046cfc3b491f3151983c59e606ff480
verdict: PASS
findings: none across all three lenses
evidence_boundary: fresh read-only Rust Ownership Review of fixed design/map/source-custody; source feasibility, not implementation or runtime acceptance
reopen_owner: none
```

The [Rust Ownership Review](../../../docs/spec-first-workflow/rubrics/rust-ownership-review.md)
panel was selected for the several-crate observation helper and new local
dependency-source containment. Three fresh Codex reviewers were dispatched
with `gpt-6-astra`, `high`, and no inherited history. Each verified the fixed
hashes before and after its review and made no edits or runtime checks.

## Responsibility and execution paths

Reviewer `/root/pool_design/native_ownership_flow`: PASS, no findings.
Attempted falsifiers:

- Capacity retained outside the bounded future: source confirms Floating owns
  the connection and DecrementSizeGuard through callback/ping/close; expiry
  drops the same ownership chain.
- BEGIN/COMMIT protection lost: existing DiscardOnDrop, not_aborted,
  pre-commit verification and commit classification remain explicit.
- Named acquisitions bypass or duplicate observation: current raw acquisitions
  and the two implicit direct-pool queries match design's coverage table;
  in_tx consumers inherit one acquire. Native connect_with with minimum zero
  has one acquire and synchronous release.
- Observation changes cancellation or shutdown authority: no added timeout,
  spawned acquire or custom cleanup owner; native errors and caller budgets
  remain, and shutdown can still report Closed::TimedOut.

Primary anchors included SQLx pool/connection.rs:134, pool/inner.rs:614,
pool/options.rs:537; transaction.rs:165,249,318; native close and current
service/worker close callers. No surviving incompatible ownership edge.

## Crate, source and delivery containment

Reviewer `/root/pool_design/native_ownership_boundaries`: PASS, no findings.
Attempted falsifiers:

- Vendor cannot stand alone: the published archive checksum matches; its 110
  files total 648,948 bytes, normalized manifest has no workspace inheritance,
  and Rust 1.94 is within the existing toolchain. Source-only locked projection
  has explicit identity/equivalence refusal checks; actual Cargo acceptance
  remains Implementation proof.
- Cargo-chef overwrites patched source: independently fetched 0.1.78 source
  collects workspace manifests and stubs only those targets. Exclusion plus
  copying the real dependency before cook is coherent.
- Absent PostgreSQL leaves a dangling patch: initializer local-edge/root
  traversal prunes unreachable dependency records, while the profile inventory
  owns vendor and structural-reference removal together.
- Vendor changes bypass gates: existing classifier/validation owners expose
  the selected routes; custody requires vendor classification, and existing
  affected-crate routing falls back for non-workspace Rust.
- Observer reverses crate edges: current consumers already depend on
  infra-postgres; helper/internal visibility and native public types preserve
  the graph and composition owner.

Primary anchors included dependency-custody.md, template_init.py:1430,1630,
affected-crates.sh:197 and the published Cargo-chef skeleton sources. No new
runtime, source, generated or provider owner is missing.

## File cohesion and proof placement

Reviewer `/root/pool_design/native_ownership_files`: PASS, no findings.
Attempted falsifiers:

- Responsibility/file inverse map gap: each changed application file and
  isolated dependency delta has a present reason; non-Rust ownership resolves
  to source custody.
- Observation helper is unnecessary structure: current statement observation
  measures the full statement future and cannot independently classify/time
  acquisition. The proposed helper belongs with observe.rs and has production
  callers, with its threshold beside pool budgets.
- Vendoring disguises application pool ownership: immutable upstream payload
  and the sole runtime patch are explicitly separated in custody.
- Proof lands in a wrong or duplicated owner: real-server regression remains
  in test/tests/postgres.rs with existing transport fixtures; observer unit
  proof stays beside observe.rs. No vendor test copy, new production testing
  seam or consumer-by-consumer pool-recovery duplication is introduced.

## Synthesis

All required lenses pass on the same candidate, with no conflicting finding.
The Technical Design owner adopts this panel result. The broader Technical
Design reviewer consumes it and reviews remaining mechanism, requirement,
proof-feasibility and operational traces without repeating these ownership
lenses. No build, lock resolution, image, database behavior, CI or deployment
result is implied by this static panel.
