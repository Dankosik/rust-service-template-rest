# Definition Transition Result V1

```text
status: ready
owner: Definition
result: specs/outbound-http/spec.md; intent.md; research/synthesis.md
review: specs/outbound-http/definition-review.md (PASS)
movement_evidence: requester intent is concrete; fixed-authority, public-DNS,
  limits, propagation, auth preservation, profile, and proof semantics have one
  disposition; current primary evidence closes the mechanism-feasibility
  questions needed for Definition; independent Specification Review passed the
  repaired fixed contract. Candidate identities are in definition-review.md.
reopen_owner: none
next_owner: Technical Design (System / Integration Design, then Rust Code /
  Ownership Design as triggered)
```

Technical Design must select the reqwest-facing API, header-limit enforcement
point, resolver/task ownership, shared public-address policy boundary, profile
marker inventory, and initializer/validation transformations. The auth
transport's JWT/introspection policy and its count-only header claim remain
separate. New runtime policy cannot claim parser-byte limits that reqwest does
not expose. The initializer must retain full preflight before target writes;
96 canonical projections are separate from 12 runtime build/test graphs.

Authority remains local stage-10.2 delivery. No PR, push, merge, deployment,
new concrete provider, or other stage-10 capability is authorized. Reopen
Definition only if observed evidence changes its behavior, source constraints,
or requester meaning. The parent coordinator continues the request.

The lifecycle-only `draft` to `ready` edit changed `spec.md` SHA256 to
`08bd870382beb5fa3008380e85e7981fae75cd9180532e5579aba9b017cc268d`;
the reviewed behavior and other reviewed file hashes are unchanged.
