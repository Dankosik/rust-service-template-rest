# Technical Design review

candidate: Base `4edd184`; reviewed Specification SHA256 `b36ae2db73d7551bb86101394066bd2d9d7f5150975f210b4acba3b131ac92d5`. Fresh native Astra/high reviewer `/root/authn_design/system_review`, no inherited history, reviewed repaired [design/system.md](design/system.md) SHA256 `d3393184e26248755ca03553148d72b04b3577ee5ba3edb464cc252c540595b8` and [design/ownership.md](design/ownership.md) SHA256 `3626aa6245a8b17e444d807a1734a9783933059cd5b49153d8673e5f311a4d57`. Owner subsequently changed only their status from draft to ready; this mechanical change preserves reviewed semantic scope.

verdict: PASS

findings: None surviving. Initial F1 identified that jsonwebtoken from_jwk only decodes/stores components, so an RSA JWK with empty n/e could become a supposedly usable replacement. System Design repaired the material admission gate: required decoded components/lengths, existing RSA2048–8192 bound, and supported imports through the already-resolved aws-lc backend. Correctly sized invalid P-256 points also reach native validation. The same normalized material reaches canonical identity and from_jwk; no survivors refuses preparation/refresh and preserves prior keys. No second signature verifier or unsupported mathematical assurance is introduced.

evidence_boundary: Read-only Technical Design review of selected mechanisms, library fit, budgets/failure semantics and material flows; one bounded F1 recheck. The reviewer consumed [all three ownership-panel PASS receipts and the fresh dependency-delta PASS](design/ownership-review.md), retaining their non-overlapping evidence. Its other falsifiers found no surviving contradiction for actual-method/HEAD construction, native fallbacks, nbf:null, algorithm bindings/conflicts, trusted pooled transport, introspection status classification, waiter/provider deadline ownership, refresh cancellation/publication and canonical URL handoff. Evidence included pinned jsonwebtoken11.1.0, aws-lc-rs1.18.1, axum0.8.9 and utoipa-axum0.2.0 source. No builds, runtime tests, live IdP or external writes. This is design readiness, not implementation proof.

reopen_owner: none.
