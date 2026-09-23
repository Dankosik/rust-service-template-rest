# Definition review and movement evidence

Reviewer: `/root/definition/definition_review`, native `reviewer-agent`,
`gpt-6-astra`, high effort, fresh history. Adapter: Specification Review.
Baseline: `098b4ab18dd5b2d158a94e126798d8cc429ad735`.

## Review Result V1

```text
candidate: intent.md SHA256 1c6a386ce1c11887e9c6d2f110f02054b11397f2884ae30d0d34485c8252df9f;
  spec.md SHA256 4197e78d42678adb704903853ec3842d2823fe08774f849c620af949b5f263fc;
  research/synthesis.md SHA256 b6abe6ff4cc6dbd3e1321eea6f8a843aab6699694d39074e96988a029da08467
verdict: PASS
findings: none
evidence_boundary: fresh read-only Specification Review of requester intent,
  roadmap 10.2, auth transport/DNS and documented guarantees, initializer
  validation, evidence policy and matrix arithmetic; one bounded same-reviewer
  delta recheck of the only finding. No builds, runtime fixtures, or delivery.
reopen_owner: none
```

The initial review returned `FAIL` on the response-body wording: a peer can
declare a short `Content-Length`, finish that HTTP frame, and send more bytes
that reqwest does not expose as that response. The contract now distinguishes
oversized framed bodies, incomplete frames, and bytes after a complete frame.
The same reviewer verified only that paragraph changed and returned `PASS`.
Its [RFC 9112 section 6.3](https://www.rfc-editor.org/rfc/rfc9112.html#section-6.3)
falsifier supports the repaired scope.

Other attempted falsifiers found no surviving divergence: DNS-to-connection
coupling, proxy/redirect/retry controls, aggregate versus count-only header
claims, cancellation custody, auth preservation, and 96-projection / 12-runtime
factorization. `make docs-check` passed again after the reviewed Definition
artifacts were added (670 total links, 0 errors); it is a static link check,
not implementation evidence.

The only post-review edit to reviewed behavior is the lifecycle word `draft`
to `ready` in `spec.md`. Per Transition's unchanged-semantic-scope rule, the
`PASS` applies to the same contract; the current hash is recorded in the
transition. Reopen Specification for a changed framing guarantee or any other
changed observable behavior.
