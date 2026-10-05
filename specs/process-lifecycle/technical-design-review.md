# Technical Design review

Review Result V1:

```text
candidate: design/overview.md SHA256 c0116dcce076e48413ec41013b896c27d6acd0449d819dd853ee015b8461cc32; design/evidence.md SHA256 15d16825ca91b001bdcbae4ee82d403fe71f07384d9f3d5f2cafbb7603064d3d; base 5927ffbba351af2f7fb8635316bbfa4ae5b31da6; accepted spec SHA256 30b2f0124032cc290c670fbb37c983a7dc62d14c37defed0d44d193b46d9764f
verdict: PASS
findings: none surviving; TD-1 and TD-2 closed
evidence_boundary: Fresh read-only Technical Design Review, followed by one bounded delta recheck of two anchored findings. Both repaired candidate hashes independently verified. Earlier unaffected conclusions retained. Design/source evidence only; no implementation, build, test or live-provider proof.
reopen_owner: none
```

Reviewer: `/root/lifecycle_design/technical_review`, fresh native
`reviewer-agent`, `gpt-6-astra`, `high`, no inherited turns. Method:
[Technical Design Review](../../docs/spec-first-workflow/phases/technical-design-review.md)
under [shared Review](../../docs/spec-first-workflow/shared/review.md).

The first result failed on two bounded mechanism gaps:

- **TD-1, signal ownership:** the worker's detached signal-forwarding task had
  no observed completion path, while watch closure became permanent pending.
  Repair removes that application task and gives the existing `Signals` owner
  direct native receiver ownership, explicit receiver-closure failure, and the
  unchanged first-stop deadline/repeated-signal behavior.
- **TD-2, diagnostics exception:** a timeout shape covering both unconfirmed
  accept termination and a slow connection could silently broaden the scrape
  exception. Repair keeps `Drained::TimedOut` connection-only after successful
  accept completion and adds a distinct voting `ServerError::AcceptTimeout`.

The one delta recheck passed both repairs and their affected ownership/outcome
maps. The original falsifiers no longer apply; Planning needs no further
architecture choice. The reviewer also checked locked Tokio native receive
cancellation and the task panic/drop guard behavior.

The unchanged conclusions cover retained pool/broker/provider/listener cleanup,
whole-budget listener drain, named failures, forced-join accounting within the
existing dependency allocation, primary-failure precedence, the 18.5 s tail,
and unconditional unwind dependencies across absent profiles. These conclusions
do not establish implementation correctness or termination of blocking work.

After PASS the owner changed only the design status from `draft` to `ready`.
This mechanical lifecycle update preserves the reviewed semantic scope under
[Transition](../../docs/spec-first-workflow/shared/transition.md). The current
identity is recorded in the [Technical Design transition](technical-design-transition.md).
