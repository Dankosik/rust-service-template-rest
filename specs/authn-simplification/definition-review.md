# Definition review

candidate: Base `4edd184`; fixed files reviewed by fresh native Astra/high
`/root/authn_definition/definition_review` with no inherited conversation:

- intent.md SHA256 `ac757d50dbf7b3271c46d5f9a007a5a4ebfa7e4d4fb63b58080a28160fa422d3`
- spec.md SHA256 `7245d15bc4827af0143b05f5f5d1837773a2379671183be2b6abff8f329e3dc0`
- research/synthesis.md SHA256 `21bf40b8a3d3b456a274fd989cb3298303ecf78cade3a8b242ea1528dd691f33`

verdict: PASS

findings: None surviving. Initial review identified F1: when RS256 and PS256
were both configured, an RSA JWK without alg could be reused across algorithms,
contrary to the adopted RFC 8725 section 3.1 obligation. The bounded repair binds
each key before token selection, skips ambiguous omitted-alg entries and rejects
conflicting algorithm assignments for identical public material. The same
reviewer independently rechecked that delta and all candidate hashes; the
falsifier now rejects the ambiguous entry before it can authorize a token.

evidence_boundary: Read-only Specification Review of intent/spec/research,
relevant Rust methods, pinned jsonwebtoken validation/decoding, existing JWT
source via CodeGraph, auth documentation, Stage 10.2 disposition and primary RFC
sources. Bounded delta recheck reused unaffected reasoning. No builds or runtime
tests. Owner changed only spec status from draft to ready after PASS; that
mechanical status refresh does not change reviewed semantic scope.

reopen_owner: none

## Technical Design carry

Preserve the difference between implicit HEAD inheritance and an explicit
undocumented HEAD handler; a plain method/MatchedPath table alone is not evidence
that this distinction holds. The numeric-null issue is closed by the bounded
reopen below; no custom JWT preprocessing is required.

## Numeric-null reopen review

candidate: spec.md SHA256
`956a06bd1036fef8556e5d30d0980587c264063fee631a801cfb1f2687602f0d`;
research/synthesis.md SHA256
`c0e1dccdb314727d8977a0646982cdf9f202a7839b0d299079ec63657d6aa595`.
Fresh native Astra/high reviewer
`/root/authn_definition/numeric_null_review`, no inherited conversation.

verdict: PASS

findings: None surviving. Technical Design requested removal of the agent-added
nbf:null acceptance assumption. JWT omission remains allowed, present null is
401 under standard Validation; active introspection present null remains
wrongly typed provider evidence, 503, while omission and inactive handling are
unchanged. Other optional-null rules remain unchanged.

evidence_boundary: Read-only review of R4/R7 delta and research explanation,
with prior unaffected Definition evidence retained. Reviewer confirmed both
hashes before and after review and traced jsonwebtoken decoding.rs:284–288 and
validation.rs:184–201,274–275,346–380. Independent validation deserialization
distinguishes omitted nbf from present null. Falsifiers for accidental omission
rejection, changed classification, unrelated null behavior and required
preprocessing found no divergence. No builds/runtime tests. Owner subsequently
changed spec status only from draft to ready, preserving reviewed semantic scope.

reopen_owner: none
