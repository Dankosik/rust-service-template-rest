# Implementation: transfer the admitted webhook body

unit: one fixed implementation unit from [Planning](planning.md)

verdict: **Implemented**; behavior, builds, measurements and final review remain
unverified by this actor.

candidate: the following assembled source blobs in the current checkout.

| File | Git blob |
| --- | --- |
| `crates/infra-webhooks/src/inbound.rs` | `ad981d30e1590dcb6ea0b31842f633b288626965` |
| `crates/infra-http/src/webhooks.rs` | `64bce8377a68db1ecb97c637159c8afc466a8910` |
| `test/tests/webhooks/inbound.rs` | `184aec41c05b7cd26ce9d1d22476b7549eb01143` |

provides: additive `Receiver::receive_bytes(Bytes)` and retained
`Receiver::receive(&[u8])` over one private `receive_inner` and private
`ReceiveBody` carrier. The verifier borrows the original bytes. Receipt
arbitration, SQL, errors, logs and enqueue remain in that one existing path.
Only the winning branch calls `Incoming::new`; its measured scope contains
the borrowed slice copy or owned handle move. `Incoming.body` now uses `Bytes`
with the existing Base64 adapter, field order, accessors and version behavior.
HTTP moves its collected body into the owned entry point. Existing profiling
attributes are preserved, and the new owned entry point has the corresponding
receiver measurement boundary.

## Test decisions

The existing integration-test owner now supplies these missing distinctions:

- Raw JSON serialization is compared directly with independent complete byte
  literals, including field order, version, Base64 padding, binary metadata,
  absent metadata, empty/short binary bodies and a 65,538-byte body. Decoding
  must recover the separately specified original bytes. This catches a native
  `Bytes` Serde representation or adapter/field-order change that decoded JSON
  value comparisons cannot detect. The existing integration crate already owns
  `serde_json`; no dependency is added to the inbound-only profile.
- Existing replay proof crosses borrowed admission and owned duplicate/rejection
  entry points, retaining the first body, metadata and receipt timestamp.
- Existing concurrent-admission proof races borrowed and owned APIs with
  different authenticated bodies and asserts that the sole job contains the
  actual winner's bytes and message ID.
- Existing enqueue-failure and commit-acknowledgement-loss scenarios enter via
  the owned API; uncertain-commit retries use the retained borrowed API.
- Existing mounted HTTP proof now admits a binary body and checks the stored
  original body, endpoint, message ID and first content type after a changed
  replay, while preserving its body/read/identity/error assertions.

These are observable serialization, public API and durable admission contracts.
There is no test-only production seam, pointer assertion or allocation mock.
The source change is an optimization, not a claimed correction of a prior
behavior defect; all golden behavior expectations also apply to the baseline.
Tests have been authored but not executed.

## Evidence and next owner

Static source/Git inspection and `git diff --check` over the three source/test
files completed; the whitespace check passed. No local build, Cargo, make,
Docker, test, service, load, hotpath, Python or Node computation was run. No
remote command was executed by this actor. Only the three owned source/test
files and this result were edited; unrelated dirty work is retained.

Root's remote formatting check identified one line-wrap delta in the new JSON
test. Implementation applied the supplied rustfmt diff manually; behavior and
scope are unchanged. The table above records the repaired test blob. Root
stopped validation before build and will rerun formatting on this candidate.

next_owner: root delivery owner, for assembled Completion. Writers are stopped;
Implementation remains available for corrections from diagnostics, tests or
the final review.

Root was asked to collect bounded compile feedback remotely with
`SQLX_OFFLINE=true cargo check --locked -p infra-webhooks -p infra-http --all-targets`
and `SQLX_OFFLINE=true cargo check --locked -p integration-tests --features integration --test webhooks`.
Cover the existing hotpath feature build as well. These commands are requested,
not observed receipts, and do not gate this Implemented handoff.

Completion still owns the matching build, relevant crate tests, existing real
PostgreSQL webhook proof (including the modified cases), optional-profile
compatibility, fixed-baseline/fixed-final allocation and ordinary release
comparisons, retained report, and final independent review. Root selects and
runs these under [Operations](operations.md) and the accepted
[Specification](spec.md)/[Design](design.md) proof boundary. The full measured
large-fixture body length, unchanged behavior and lack of reproducible
regression remain required; this receipt makes no improvement claim.
