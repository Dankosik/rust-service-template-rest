# Integrated implementation review

Reviewer: `/root/consumer_preparation/integrated_final_review`, freshly dispatched
with native `gpt-6-astra`, `xhigh`, `fork_turns=none`; read-only.

Candidate: template `8cf53b2818ffdb55320cd5fafeb7e9a75129a3d8`, tree
`238e0c300938cea26768dc5c3906a3f68f48f907`; resolved B
`626a0097060b06354365fda3c1b833933aacbe72`, content tree
`11ca3412c93304950dfdfab59f0a50eeeb1c1bd3`.

Verdict: **FAIL**.

## Findings

1. **TASK_DEFECT — T2: supported rehearsal entrypoint refuses before execution.**
   `scripts/ci/consumer-lifecycle-check.sh:89` supplies explicit
   `--outbound-http none`, while the public Make entrypoint exports
   `OUTBOUND_HTTP` from `make/template.mk:162`. The historical initializer
   correctly rejects the duplicate. This falsifies the executable T2 rehearsal
   outcome and prevents D1–D4 proof. Actual fixed-F invocation exited 2 after
   2.205 seconds with `OUTBOUND_HTTP may be supplied once, by flag or environment`.
   The reviewer independently checked the causal source path, input hashes and
   retained log hash. No actors or providers started. Smallest repair owner:
   T2 carrier's initializer-environment boundary.
2. **TASK_DEFECT — T3: portable ownership expands contrary to the accepted boundary.**
   `template-owned.paths:50` makes `scripts/ci/image-results.py` portable-owned;
   `make/template.mk:424` invokes its self-test unconditionally. This contradicts
   U4 and the explicit outside-portable placement in `design/ownership.md:16`
   and `design/ci.md:97`. Both template F and resolved B contain the departure;
   no adopted exception exists. Portable sync consequently acquires ownership
   of a file reserved for initialized consumers, and the portable command
   assumes its presence. Mechanical source-purity success does not settle
   semantic ownership. Smallest repair owner: T3 portable manifest/validation
   command seam.

## Evidence boundary

Local B admission is **FAIL pending finding 2**. No further defect was found
in consumer preservation, native conflict resolution, baseline ancestry,
generated API contract, or durable A/B compatibility.

The reviewer independently confirmed clean B and its content tree; A's sealing
parents; B1's B0 parent; and B0/pristine initialization tree equality. It inspected
historical rendering, isolation, conflict handling, migration refusal, evidence
binding, sealing and retry paths, and reused the six passing protocol tests only
after checking their recorded inputs against F.

B's build, 457 passing tests, docs, actionlint and migration-history log hashes
were verified. Two ignored tests retain their explicit limitations. A→B's API
document changes version metadata; migrations, jobs-worker, messaging and
domain-event source remain unchanged. Consumer greeting and local configuration
survive. CI lane selection, required uploads, aggregate failure handling,
recording and recovery custody/reconciliation assertions were reviewed. No heavy
validation was duplicated.

D1–D4 remain unproved after the actual T2 failure. C2 comparable serial/split
execution remains pending, which is not another candidate defect. Consumer
publication, registry trust, distinct digests and observed A→B→A remain pending
external authority and execution; none is a prerequisite for local source
admission.

Reopen owners: T2 and T3, coordinated by root, for bounded repairs; Completion
delivery owner for affected proof and B sealing. Root retains CI comparison,
source integration and external-effect authority. Reviewer performed no edits,
acceptance or transition. Reuse this reviewer for the bounded repair delta.
