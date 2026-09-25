# Technical Design transition

status: ready

owner: Technical Design, narrow System / Integration and existing-owner refinement for introspection caching

result: [design/system.md](design/system.md#optional-introspection-cache), [design/ownership.md](design/ownership.md), supported by [cache mechanism evidence](research/cache-mechanism.md)

review: [technical-design-review.md](technical-design-review.md#current-narrow-cache-reopen), fresh native Astra/high PASS after one bounded F1 repair/recheck. Prior [ownership panel](design/ownership-review.md) and Technical Design reviews are retained for unaffected scope.

movement_evidence: Corrected Definition requires delivery of operator-enabled, default-off caching. The design closes its mechanism without a new crate, module, dependency, runtime or background worker: an introspection-owned std map admits only successful verified evidence under finite count and allocation bounds; immutable verifier ownership isolates trust contexts and exact secret-token keys. Fixed retention is bounded by TTL and exp with no skew extension. Final cache admission rechecks both expiry clocks, request reserve and common exp/nbf policy after intervening lookup/cloning work. Cache misses/fullness/contention/oversized evidence use existing verification; negative/error/stale results are never substituted. Finite config names/defaults/bounds, private temporal evidence, bootstrap conversion, default-off activation, docs and introspection-only marker containment all have existing owners. Planning can adjust the existing delivery unit without inventing these decisions.

reopen_owner: none for movement. Specification owns changed caller meaning or revocation requirements; System Design owns a changed shared-state/lifecycle/eviction requirement; Rust Ownership owns a genuinely new semantic owner. Helper bodies and exact tests/commands remain Implementation work.

next_owner: Planning, fresh actor dispatched by root, narrow correction of the existing T1 packet and final proof/closure inputs. Stop this actor before Planning.

## Candidate and authority

Worktree: `/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-authn-contract-and-libraries`, branch `codex/authn-contract-and-libraries`. The original broad design anchor was `4edd184`; this reopen reads production `a896e21f27acffc03193e6d43613c6e5173603b0` plus the Lead's disjoint mechanical cleanup. Exact reviewed design/input hashes are in the current review receipt. Only task-local design, research, review and transition artifacts were edited by this phase actor; no cache production code or ledger was changed.

The original request already authorizes implementation, local validation, commit, push and a separate PR. No further technical confirmation is needed. Merge, deployment, live-provider certification and independent Stage 10.2 policy changes remain outside scope. Unchanged R1–R10 decisions and proved implementation work remain accepted inputs; earlier no-cache wording is superseded only for the corrected feature and must be removed from the active Planning/Implementation carriers by their owners.

## Narrow implementation carries

Config adds introspection-only `cache_enabled = false`, `cache_capacity = 256` (1–1024), `cache_ttl = "30s"` (1s–5m), retaining normal source policy. Bootstrap converts these into optional cache options. Existing `introspection.rs` owns bounded state and all hit/miss paths; `claims.rs` retains private verified nbf evidence and one temporal policy; `lib.rs` keeps sealed Principal and optional private allocation accounting plus public adapter options. No new dependency or module is necessary. Disabled behavior continues its existing provider exchange per admitted request.

Planning retains existing owners for loader/config proof, adapter fixture/clock/isolation/failure proof, bootstrap construction, documentation, and template projections. Whole-file introspection exclusion plus its existing markers remove cache config/types/helpers in JWT-only/no-auth outputs and retain them in introspection-only output. Document delayed revocation detection for valid cached positives. Structural projections stay factored from expensive runtime builds. The delivery owner retains final assembled validation and independent authorization/concurrency review; prior no-cache implementation evidence does not prove this feature.

Static consistency evidence: final scoped `git diff --check` passed. Final `make docs-check` passed after the review/transition receipts were written: exit 0, 849 links checked, zero errors. This result sentence is the only subsequent edit and changes no link or reviewed design. No build, runtime test or live-provider result is claimed.
