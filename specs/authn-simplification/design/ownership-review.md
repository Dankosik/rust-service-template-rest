# Rust ownership panel review

candidate: Base `4edd184ea3cc6b6fa2b225244700fce57150ae18`; fixed [system.md](system.md) SHA256 `7a2c5b72e28b55a3cea929bb14e3ec74291c056638885dc6531dc11e54b6337f` and [ownership.md](ownership.md) SHA256 `7694f9214fceca9d6c8339f0bf6428e6ae9f8ad5bc004128c0ff5448cdd85708`; Specification SHA256 `b36ae2db73d7551bb86101394066bd2d9d7f5150975f210b4acba3b131ac92d5` and current Definition transition.

verdict: PASS. All three required disjoint lenses returned PASS on those exact bytes; no compatibility conflict survives.

findings: none.

| Independent reviewer, native Astra/high with no inherited history | Lens and attempted falsifiers | Result |
| --- | --- | --- |
| `/root/authn_design/ownership_flow_review` | Responsibility/execution paths: configured/discovered URL bypass or duplicate grammar; opaque hidden HEAD, explicit versus implicit HEAD; stale Principal/header and idempotency-before-auth; verifier disappearing without Composer; requester-cancelled shared refresh, lost notification, stale replacement and missing join. | The design assigns one owner at each crossing. Constructor rebuilds actual endpoints; final auth wraps idempotency; bootstrap retains verifier and joins refresh. PASS. |
| `/root/authn_design/ownership_graph_review` | Crate/module/dependency/visibility/generated containment: delete carrier/macro, admit opaque MethodRouter, require config-provider dependency, retain auth via idempotency coupling, remove outbound DNS accidentally, lose public-only/no-auth composition. | Pinned axum private endpoint fields justify provenance; macro delegates existing annotation generation; primitive config handoff avoids a shared URL crate; template dependencies retain independent outbound policy. PASS. |
| `/root/authn_design/ownership_cohesion_review` | File cohesion/declaration grouping/proof placement: collapse contract.rs, move URL into generic helper, split claims policy, add duplicate parsers or owner-free tests, broaden mechanical caller migration. | Each file has a present mapped responsibility; proof stays with current owners; deterministic caller migration cannot add semantics and reopens new owners. PASS. |

evidence_boundary: Read-only panel under Rust Ownership Review. Each lane independently checked supplied hashes, applicable architecture and its bounded source/library/projection evidence using CodeGraph and source reads. No edits, builds, runtime tests, live providers or remote writes. Production source stayed at base4edd184. An external task changed the original checkout branch label to codex/outbound-http-simplification while artifacts remained fixed; root owns relocation of only this task's artifacts to the isolated auth checkout before later phases.

reopen_owner: none. Technical Design Review consumes this panel without repeating its lenses; this report does not establish implementation behavior or delivery acceptance.

## F1 material-admission dependency delta

candidate: Repaired system.md SHA256 `d3393184e26248755ca03553148d72b04b3577ee5ba3edb464cc252c540595b8` and ownership.md SHA256 `3626aa6245a8b17e444d807a1734a9783933059cd5b49153d8673e5f311a4d57`; same Specification/base. The original responsibility, visibility, cohesion, provenance and lifecycle decisions are unchanged. Only the concrete JWT material-import mechanism and its direct dependency edge were added to repair Technical Design F1.

verdict: PASS for the repaired candidate. Fresh native Astra/high `/root/authn_design/material_edge_review`, no inherited history, reviewed the affected dependency/feature/projection/visibility lens. It verified all hashes; a locked offline feature-tree read confirmed aws-lc-rs 1.18.1 is already selected through jsonwebtoken/TLS and public import APIs need no extra feature. Existing workspace/authn JWT markers and whole-file removal can remove the new direct edge when JWT is excluded while leaving TLS's independent transitive backend. Private JWT internals contain admission data; no backend types escape or alternate signature verifier is added. Prior three panel PASS results remain evidence for unchanged lenses. No finding survives; no build/test/edit/remote write occurred.

reopen_owner: none.
