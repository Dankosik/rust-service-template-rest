# Assembled review

The independent reviewer returned `FAIL` for `f770ce7cbe26e856be734d797e6b95dbbbff709c`. Both findings belong to the experiment overlay in T5. The reviewer found no surviving shipping-code or ownership defect after tracing all four review axes.

- The R4 timing matrix exercises empty HEAD and nonempty POST. It needs a matched empty POST, PUT, or PATCH that reaches the changed static-zero framing branch and verifies `Content-Length: 0` at the peer. [Caller request construction](../../experiment/caller/outbound_http.rs#L59)
- The R8 pairing check reports CPU and p99 when one arm opens an extra TLS connection during a warmed workload. The reviewer passed synthetic receipts with identical call/resource work and zero versus one measured HTTP connection opens to `matched_deltas`. It returned `peer_work_matches=True` and emitted both timing deltas. Preserve the distinct connection behavior of headers-only, late-error, and retry cases while tightening warmed-path evidence. [Pairing check](../../experiment/run.py#L369)

Product correctness receipts are retained in this directory. Comparative Linux results and provider closure remain pending. Review must clear both findings before provisioning.
