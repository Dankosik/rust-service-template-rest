# Overload resource hardening

status: ready

Completion: G1 and S1 are implemented with their companion D1 source/operator
guidance and existing optional-profile projections; the assembled candidate
satisfies the repository's matching build, relevant tests and actual changed
documentation/profile checks, with one independent final delivery review
resolved. Deliver that candidate in one separate commit/push/pull request and
record the current-head required CI result; pending required CI remains pending.
No merge, deployment, infrastructure provisioning or new workload is included.

Global constraints: [Intent](intent.md), [Specification](spec.md),
[Mechanism](design/mechanism.md), [Ownership](design/ownership.md), and
[dispositions](recommendation-dispositions.md) own scope and exclusions.
[Planning result](planning-result.md) owns the obligation reconciliation and
carrier handoff. The root binds as sole `LEDGER_ORCHESTRATOR`; task Leads own
implementation and one assigned delivery owner owns assembled final validation.
No task-local validation or review gates. Shared CPU-heavy validation is serial
at the final boundary under the repository Git-common validation lock. No
optional provider environment becomes a new acceptance prerequisite. The
executor applies test-audit's authoring/value gate; required regression
pre-fix falsification and post-fix passing evidence belong to the same final
validation boundary, with one matching non-overlapping local build/test route.

## Tasks

- [x] T1: Business gRPC openings are bounded before authentication, with their independent terminal bound and accurate resource-scope guidance.
  - Depends on: none.
  - Provides: G1 code, executor-written proof and all non-storage-specific D1 companion guidance; released shared documentation owners for T2.
  - Packet: [tasks/T1-grpc-opening.md](tasks/T1-grpc-opening.md).
  - Execution: `/root/overload_grpc`, ACCEPTANCE_UNIT_LEAD, Implemented and all writers joined. Seven-file bounded diff SHA256 `a966cfeb5732b521b628af01d5bf6556ad0903bc567b62610a211310bdb7e6c6`; executor notes packet SHA256 `6e0d0c939aa62079a156796774ff9c3d08d1a824670456d7a98d830acf09906c`. Compile-only `cargo check --locked -p infra-grpc --tests` exit0 (1m24s) under shared validation lock; no behavioral or acceptance claim. Shared documentation owner released.
- [x] T2: Complete S3 GET operations release active custody by their original deadline, including unpolled returned downloads.
  - Depends on: T1 Implemented output and released `docs/configuration-source-policy.md` ownership; implementation scheduling dependency only, no passing evidence or acceptance required.
  - Provides: S1 code, executor-written proof and storage-specific D1 companion guidance; complete assembled implementation for final validation.
  - Packet: [tasks/T2-storage-get-lifetime.md](tasks/T2-storage-get-lifetime.md).
  - Execution: `/root/overload_storage`, ACCEPTANCE_UNIT_LEAD, Implemented with all source/doc writers and diagnostics joined. Nine-file current diff SHA256 `e965d0472c1b792f68e1c2a717d5e0b833523891792a1c83e1f9e6f2413ef88d` includes preserved T1 shared-policy changes; packet executor notes SHA256 `48e6b5d473d684e3d2723e967f0220b888e55a7949c7e47803e91e478c0b1ed3`. Corrected-code `cargo check --locked -p infra-object-storage --tests` exit0 (1.24s), shared lock; no behavioral/acceptance claim. Shared documentation released.
  - Updated accepted Design input: `design-result.md` SHA256 `52c47282c1c351e03f6da588989f56b0b299ced8243943535c51b36830027df5`, mechanism `8e839978bbf5097d8aaf9a7ea2997e2c5f15f9a0e825654d6781b87ce9b38fb0`, ownership `da6fa1adb82455962354a40278782e5394ec0d03820b8aca7414673f20f897fd`; fresh narrow failure-hint Design review PASS. Failed Download uses unknown-upper SizeHint with EOSfalse so HTTP1 cannot skip its stable error as an empty successful body; success hints/headers unchanged. Same T2 files/graph/behavior scope.

## Completion result

All planned code tasks are Implemented and locally Accepted as one assembled code bundle. `/root/overload_storage` returned Completion: matching workspace build/test875passed0failed1existing CI-only ignored, storage55/55, gRPC32/32; intended baseline/failed-hint failures and restored candidate pass; final fmt/docs1443links0errors; fresh independent assembled Review PASS/no findings. All writers/readers joined. Current tracked diff SHA256 `5db8c3d7f9c2d900d42aa205b9803acf70cefda8d79d6794e6c68643dbf2bf44`, Rust `92ad16f93726722ea2fa2b85792cf4a574245701a9d2f8c303ff559c8e144547`. No repeated root validation. Root owns the outstanding PR publication/current-head CI, retained in completion.md after execution-ledger archive/cleanup.

Completion progress: fmt/docs passed (1432 links, 0 errors); bounded mechanical repairs applied by original T1/T2 owners before readers resumed. T1 fixture repair changed only transport.rs, SHA256 `12e29c9873ed0f1559015c537861f63ecac71dde4485f0a02707ffe0a21ca712`. Affected lint, duplication and unused-dependency checks passed. Native template-quality-projections stalled in existing macOS git-cat-file pipe backpressure, owned leaf reader terminated/cleanup joined; exact scope remains unverified locally, to be taken from current-head Linux CI. All source temporary baseline substitutions/restoration are exclusively owned by Completion; no other source readers/writers enter until restoration checkpoint. Consistent runtime validation environment disables debug symbols/incremental cache while preserving assertions/optimization semantics and uses own target; no cache deletion.
