# Rust Ownership Review: bounded outbound HTTP

Three fresh reviewer-agent lanes reviewed one fixed Ownership Map V1 through
`docs/spec-first-workflow/rubrics/rust-ownership-review.md`.

Initial candidate hashes:

- system.md: `a31e80ef778d200ae2600d611c1189fccf191d6c579b5a807004a0b963d4435b`
- ownership.md: `f2368a411a6c614f990347261f10b44d52171a32ec9dc9d0ec0fccb271ce162c`

| Lens | Native reviewer identity | Verdict | Attempted falsifier and result |
| --- | --- | --- | --- |
| Responsibility/execution paths | `/root/technical_design/ownership_responsibility` | PASS | Shared DNS absorbing auth policy, unowned cancellation, invented request budget, early permit release and broken OR-retention were not sustained against the fixed map and current source. |
| Crate/module/dependency/visibility/containment | `/root/technical_design/ownership_boundaries` | PASS | No cyclic/unsupported shared owner, transport/deadline escape, pruning gap or generated/manual authority inversion identified. |
| File cohesion/naming/grouping/test placement | `/root/technical_design/ownership_cohesion` | PASS | Concrete responsibilities justify files; moved DNS tests remain with mechanism, auth integration tests remain private, outbound TLS fixture inputs survive auth pruning. |

Every reviewer verified candidate identity. Independent evidence included the
ready Specification, relevant architecture owners, current manifests, auth
DNS/provider source, inbound deadline source and initializer/state projection.
No reviewer changed files, implemented code or claimed runtime proof.

The owner then corrected an evidence-backed address-policy gap in system.md
and named the correction in the Egress DNS responsibility row: deny ambiguous
6to4/reserved IPv6 and private IPv4 embedded in well-known NAT64 while retaining
existing explicit public exceptions. Current candidate:

- system.md: `9cd2ff85766ac57f0d4e19b2f48efad80d73187a5a0f75bf2b7796f0cda198b7`
- ownership.md: `a7406131413e6c6a26b4a3559922bd983e08ad073dbf32429df16b1ce8822c0f`

Placement, dependency, visibility, generated containment, files and test
locations did not change. Lens 2/3 PASS verdicts remain valid for that unchanged
semantic scope under Transition. Fresh reviewer `/root/technical_design/ownership_responsibility_delta`
rechecked Lens 1's changed behavior/policy edge and returned PASS on the
current hashes, findings none. It attempted to falsify preservation of prior
denials/public exceptions, common literal/DNS ownership and auth failure
mapping, and unchanged HTTP/lookup lifetime custody. No contradiction was
found against the specification and current provider/DNS paths.

Synthesis: all three lenses PASS on the current semantic scope. No unresolved
ownership finding remains; Technical Design Review may consume this panel.
This is design evidence, not implementation or runtime parity proof.
