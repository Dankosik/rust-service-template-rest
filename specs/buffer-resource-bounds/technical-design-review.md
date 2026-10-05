# Technical Design review

Method: [Technical Design Review](../../docs/spec-first-workflow/phases/technical-design-review.md)
under shared [Review](../../docs/spec-first-workflow/shared/review.md).
Source baseline: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
Specification SHA256: `cace772a9cd747bfe414855f5520160960407dc6998aac8cffd472633b52b507`.

## Initial fixed candidate

- Selected design SHA256: `8a3ac52348192ba15dbbc8f14995abf7de992b1c2dac2be244ec41645e84347b`.
- Ownership map SHA256: `bc374ebd6dff7e34efff330e87d3fd2d59417243927511e7382fea1666a1b42b`.
- Reviewer: `/root/technical_design/technical_review`, fresh read-only collaboration
  agent, native `gpt-6-astra` / `xhigh`; accepted selection and active identity
  observed. The effort reflects interacting API/cancellation/lifetime invariants.
- Verdict: PASS; findings: none; reopen owner: none.

The reviewer consumed Definition and the [three-lens ownership PASS](design/ownership-review.md)
without repeating those lenses. It independently attempted S1 held-final-chunk
and exhausted-tail falsifiers; S2 2xx bypass, HEAD metadata, successful GET,
trailer/empty frames, checksum order and mutation-outcome falsifiers; S3 late
serialization failure/count/retention falsifiers; S4 cancellation, successor
identity and maintenance starvation; and S5 polled ACK cancellation/expiry,
request reclamation races, final headers, cross-resource payload and DLQ ordering.
Pinned source supported each selected mechanism and feasible proof.

Evidence boundary: design and scoped source only. No implementation, tests,
builds, runtime validation, acceptance or transition by the reviewer.

## Bounded final source-selection delta

That review also surfaced the smaller upstream handler-only pruning mechanism in
[draft PR 1629](https://github.com/nats-io/nats.rs/pull/1629), inspected head
`7db17cf15830a1a65e7ba73cecda65aee72b1ea7`. The phase owner selected it with the
still-required native ACK retention fix. It replaces the custom receiver/dirty
flag plumbing, removes the material client.rs patch, and explicitly accounts for
the finite max(256, 2 × peak live requests) metadata reserve. No public behavior,
dependency version, profile boundary, new task or parser changes.

Fixed candidate for the fresh bounded delta review:

- [Selected design](design/selected-design.md) SHA256:
  `ac1fb7be01fd9d93cab214603c1f28201a08501fb83c9b8ce6cbe7e2a8e134ca`.
- [Ownership map](design/ownership.md) SHA256:
  `d4a6bebde0bcdefa287bf74078629faad994d88cf2168999ba560cb1bf2a5bec`.

Initial S1–S4, ordinary publication checks, Definition and unaffected ownership
reasoning remain current. The final review covered only changed native mechanism,
metadata arithmetic, source provenance and the collapsed R6 file-map delta.

- Reviewer: `/root/technical_design/native_delta_review`, fresh read-only
  collaboration agent, native `gpt-6-astra` / `xhigh`; accepted selection and
  running identity observed.
- Verdict: PASS; findings: none; reopen owner: none.
- Evidence: immutable upstream commit `7db17cf15830a1a65e7ba73cecda65aee72b1ea7`,
  resolved async-nats0.50.0 source and fixed candidate, with hashes independently
  rechecked unchanged. No implementation, test, build or runtime claim.

Attempted falsifiers included repeated abandonment, burst drain, cancelled queued
requests, removal of live receivers, late replies, ACK polling cancellation,
timeout scheduling another full wait, acker expiry releasing capacity before
receiver closure, and missing source/profile ownership. The reviewer supported
the max(256, 2 × peak live requests) entry ceiling and at most P queued plus running
ACK cleanup ownership. The two-file source patch retains native dispatch/parsing
and introduces no task, channel or receiver wrapper.

After PASS the owner changed only the design lifecycle status to ready and replaced
the obsolete descriptive phrase “guarded oneshots” with “oneshot receivers”, as
explicitly identified during review. The same reviewer confirmed that wording did
not override the already-reviewed mechanism. These are mechanical lifecycle/text
refreshes under Transition; the semantic PASS remains current. The current hash is
recorded in the [Technical Design transition](technical-design-transition.md).
