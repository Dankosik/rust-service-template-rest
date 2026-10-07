# Technical Design transition

## Transition Result V1

```text
status: ready
owner: Technical Design — System / Integration Design and Rust Code / Ownership Design
result: design/system.md T3; design/ownership.md map T2; research/design-evidence.md
review: technical-design-review.md PASS; design/ownership-review.md three compatible PASS lenses
movement_evidence: B1–B6 mechanisms, material flows, native-provider constraints, exact ownership, fixture/resource boundaries and empirical reopens closed; concrete pool-injection and native-header-limit findings repaired
reopen_owner: none now; named narrow reopens below
next_owner: fresh Planning, followed by required Task Review / Readiness
```

Authoritative input: ready [specification](spec.md), unchanged SHA-256
`fb502329be927e9c7ba7e6fd8c25bdceff702ccba5c46610c60ef0f2950c31fe`,
[intent](intent.md), and
[Definition B4 clarification](definition-transition.md#b4-provider-driven-clarification).
Current Definition transition hash after its mechanical provider locator repair:
`27417e469b55a74987bffb1cc37bbcc2689a87bcae7fc32457794de4d90cd56b`.

## Fixed output

Checkout:
`/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-messaging-recovery-maturity-20261006`.
Base: `699887b18594088a59bcc23a049d290d089f6da1`.
The phase added only the six linked design/evidence/review/transition files
under this task directory. No runtime, test, dependency, infrastructure or
remote mutation occurred. Definition and unrelated work remain owned separately.

| Final artifact | SHA-256 |
| --- | --- |
| [System design](design/system.md) | `0b7e771d926625cb4755f5898386f557cb7b1423eb81146b076b9f1bcd2130e5` |
| [Ownership map](design/ownership.md) | `685e088e7b35442626c7a46e5cfe0fb1781df3f8d35828002f6022a6dd74a44d` |
| [Provider evidence](research/design-evidence.md) | `9463551e61909d5528989982249911346f6d3c54d4de50adaa01e23e62a3d72f` |
| [Ownership review](design/ownership-review.md) | `84709563baa20f0b3a4aeda8c07b7b1a8c9250ad349730b42cd5d4e0c31ec9f5` |
| [Technical review](technical-design-review.md) | `6b5956e395b2d6ea5ff2051fde79080dc70e2232c9a5f600919cc54114b88f79` |

Ready labels and map-consumer label are mechanical lifecycle changes after
review. Review receipts retain pre-label hashes; decisions/ownership remain
the reviewed bytes. This transition does not hash itself.

## Decisions the next owner must preserve

- Extend native stream admission with ACK/file/default persistence and both
  total-transfer and 65,535-byte native header bounds, preserving #254 custody.
- Keep durable-effect receipt and mutation inside the existing Tx. Unique-key
  arbitration, then a fresh READ COMMITTED statement, resolves same-ID races
  and unknown COMMIT; no plain-read absence or automatic closure retry.
- Add only the selected deferred `Registration::with_postgres_messages` edge
  to share the worker's admitted pool with typed effect handlers. Keep one
  registry, pool, startup owner and teardown path; prune the API with outbox.
- Native DLQ deletion lacks CAS. The executable workflow uses the original
  owned broker lifetime, exclusive topology custody and fresh generations
  after abandoned lifetime termination; without that prerequisite it refuses
  mutation. A local assertion, final read or expiring lock is not a fence.
- Rehearse native NATS/PG restore and measure actual R3/TLS paths within the
  resource bounds. Only 4.8 GiB local free space was observed; build-space
  feasibility must be established separately or use existing authorized CI.
  Never replace unobserved claims with a synthetic/R1 result.
- Retain publisher concurrency 1, effective broker-default MaxAckPending and
  shared roles initially. Actual attributable findings may reopen only the
  corresponding design choice before a runtime change. All three still need
  final evidence-backed dispositions, and all six outcomes need implementation.
- Causally resolve the object-storage flake and extend existing validation-lock
  feedback/custody; no passing-rerun closure, weaker assertions, auto-reclaim of
  potentially live child work or host/cache changes.

Planning may separate the coherent feedback repair from messaging delivery;
the main-based messaging candidate semantically incorporates #239 and preserves
main #254/#255. Do not transplant the old whole tree. Root owns final PR
composition/publication and exact-candidate CI under existing authority;
no merge or deployment is authorized.

## Evidence and continuation

`make docs-check` passed after final links were present: 2,005 links checked,
1,177 unique, zero errors. Provider
source/API/tool-help and current-code evidence establish feasibility, not an
executed backup, measured throughput, production durability or implementation
acceptance. Runtime proof and causal diagnostics belong to Implementation's
assembled final-validation boundary, with concrete tests selected by executors.

The root remains end-to-end continuation coordinator; this ready result ends
only this phase actor. Dispatch fresh Planning with these artifacts. No new
user decision is needed. Reopen Specification for changed behavior, Research
for changed provider facts, this design for a supported empirical runtime delta
or broken composition assumption, and the platform/resource owner for unavailable
fencing or execution resources. Preserve independent authorized work while
closing the smallest affected input.
