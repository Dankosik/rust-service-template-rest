# Technical Design review

## Current narrow cache reopen

candidate: Production anchor `a896e21f27acffc03193e6d43613c6e5173603b0` plus disjoint mechanical cleanup; this review fixes only the corrected-cache design artifacts. Fresh native Astra/high reviewer `/root/authn_cache_design/cache_design_review`, no inherited history, verified these hashes before and after the final bounded recheck:

- [design/system.md](design/system.md): SHA256 `b08d3e3fcaa186157e2470e0244986249571075d84a7e9b69268ec959b778720`
- [design/ownership.md](design/ownership.md): SHA256 `9f51a0a2ac316d2d1807e245742a33110e48899322eb696f484f2d9290cee1cd`
- [research/cache-mechanism.md](research/cache-mechanism.md): SHA256 `eca06a4743838a2e9c19c220e15361cbe230052eebcbb3450d6492277841c174`
- Authoritative [spec.md](spec.md): SHA256 `1231edb682201d7922ab1ec7d1b46615195bf402f5a5ade2c5710966735bc83e`

verdict: PASS

findings: None surviving. Initial F1 found that a cache lookup immediately before expiry could finish cloning or resume after its fixed deadline, then pass the ordinary temporal validator's 30-second leeway. System Design now makes lookup candidate selection only: after cloning, a final synchronous success decision samples clocks afresh and checks request reserve, strict monotonic lifetime, strict wall-clock exp and shared exp/nbf validation. An expired candidate takes the ordinary provider path. The reviewer repeated that falsifier on the repair and closed F1 in its single bounded recheck.

evidence_boundary: Read-only Technical Design review of the cache mechanism, finite config defaults/bounds, exact-token and immutable-context isolation, nonrenewing bounded retention, deadline/temporal checks, failure exclusions, confidentiality, and existing-owner/projection closure. CodeGraph and drift-aware reads confirmed existing adapter/claims owners; installed Moka source confirmed its supported best-effort capacity alternative. Other falsifiers found no surviving gap in default-off configuration, count and allocation bounds, nbf revalidation, permit bypass, cache-only admission failures or negative/error/stale exclusions. The existing ownership panel and earlier Technical Design review remain valid only for their unchanged scope. No production edits, builds, tests or runtime claims were part of this review.

reopen_owner: none

# Prior review retained for unchanged scope

candidate: Base `4edd184`; reviewed Specification SHA256 `b36ae2db73d7551bb86101394066bd2d9d7f5150975f210b4acba3b131ac92d5`. Fresh native Astra/high reviewer `/root/authn_design/system_review`, no inherited history, reviewed repaired [design/system.md](design/system.md) SHA256 `d3393184e26248755ca03553148d72b04b3577ee5ba3edb464cc252c540595b8` and [design/ownership.md](design/ownership.md) SHA256 `3626aa6245a8b17e444d807a1734a9783933059cd5b49153d8673e5f311a4d57`. Owner subsequently changed only their status from draft to ready; this mechanical change preserves reviewed semantic scope.

verdict: PASS

findings: None surviving. Initial F1 identified that jsonwebtoken from_jwk only decodes/stores components, so an RSA JWK with empty n/e could become a supposedly usable replacement. System Design repaired the material admission gate: required decoded components/lengths, existing RSA2048–8192 bound, and supported imports through the already-resolved aws-lc backend. Correctly sized invalid P-256 points also reach native validation. The same normalized material reaches canonical identity and from_jwk; no survivors refuses preparation/refresh and preserves prior keys. No second signature verifier or unsupported mathematical assurance is introduced.

evidence_boundary: Read-only Technical Design review of selected mechanisms, library fit, budgets/failure semantics and material flows; one bounded F1 recheck. The reviewer consumed [all three ownership-panel PASS receipts and the fresh dependency-delta PASS](design/ownership-review.md), retaining their non-overlapping evidence. Its other falsifiers found no surviving contradiction for actual-method/HEAD construction, native fallbacks, nbf:null, algorithm bindings/conflicts, trusted pooled transport, introspection status classification, waiter/provider deadline ownership, refresh cancellation/publication and canonical URL handoff. Evidence included pinned jsonwebtoken11.1.0, aws-lc-rs1.18.1, axum0.8.9 and utoipa-axum0.2.0 source. No builds, runtime tests, live IdP or external writes. This is design readiness, not implementation proof.

reopen_owner: none.
