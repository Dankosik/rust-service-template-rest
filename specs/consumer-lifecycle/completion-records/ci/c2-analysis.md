# C2 terminal comparison: incomplete

C2 is **incomplete**. Serial run `37541180687` passed all 23 selected jobs;
Split run `37546407132` passed 21 and failed `secrets` and `required`. Both are
native `workflow_dispatch`, attempt 1. Successful image proof and earlier
ordinary draft PR CI do not substitute for the failed whole selected gate.
No repeat measurement is initiated by this report.

The accepted [CI design](../../design/ci.md) owns measurement and repair.
[Final pair observations](final-pair-observations.json) bind every number below
to raw API files, native log ZIPs, per-command records and an external detailed
report. [Execution history](execution-history.json) preserves all five related
runs, including cancelled, failed and superseded work.

## Observed elapsed time and cost

| Native observation | Serial | Split | Split minus Serial |
| --- | ---: | ---: | ---: |
| Run creation through `required` completion | 2,497 s | 1,788 s | −709 s |
| First selected job start through `required` completion | 2,494 s | 1,779 s | −715 s |
| Initial queue: creation to first selected job start | 3 s | 9 s | +6 s |
| `changes` completion through `image` completion | 2,483 s | 1,765 s | −718 s |
| Assigned runner intervals, all 23 jobs | 9,894 s | 10,121 s | +227 s |
| Assigned image source/derived/aggregate intervals | 2,477 s | 2,247 s | −230 s |

These are diagnostic observations, **not a scheduling speedup result**.
The selected gate failed and the actual execution environment and cache restores
did not match. The elapsed difference cannot be attributed to scheduling.

Serial's combined source/derived builder ran 22:32:12–23:13:21 UTC (2,469 s),
then its derived relay ran for 3 s and image aggregate for 5 s. Split started
both builders at 23:25:01 UTC: source ended 23:33:07 (486 s), derived ended
23:54:13 (1,752 s), and image aggregate ran 23:54:15–23:54:24 (9 s).
`image` was the last completed prerequisite of `required` in both runs; the
following allocation waits were 3 s and 2 s. All native image jobs stayed below
the retained 90-minute ceiling. Later selected-job allocation delays remain in
the raw job records; none is subtracted to manufacture a whole-workflow result.

Per-command source build/migration/security/SBOM times were 600/25/11/1 s
serial and 416/26/15/1 s split. Derived fetch/artifact commands took 5/1,792 s
serial and 4/1,715 s split. Those command intervals differ from native job/setup/
cleanup intervals. Each mandatory native artifact upload step recorded 1 s.
Mandatory image artifact sizes total 506,309 bytes serial and 521,459 bytes
split; these stored archive sizes do not measure total network transfer.

The final pair used 20,015 observed assigned runner-seconds. All five related
runs used 43,271: initial cancelled control 3,691; successful superseded control
9,465; failed-docs serial 10,100; replacement serial 9,894; final split 10,121.
These are intervals for assigned runners, not charges, rounded billable minutes,
or a count of every ordinary development CI run. Root owns quota/cost authority.
Docker daemon CPU/RSS, total registry transfer, temporary-disk peaks and actual
paid consumption remain unmeasured. No broader cost claim is made.

## Actual input differences

The serial control `fbada6da0c3d118942b91073c2582688d7aca637` differs from source
`fb33186d0eb1fab14ff37d071a4e2d6eac9bb45c` only in scheduling/output transport.
Readback matched all 1,363 non-scheduling Git objects and 698 Docker-context
objects. Across the four derived graphs, normalized profiles, OpenAPI hashes,
lock hashes and expected inventories match. Actual generated revisions, image
IDs and SBOM hashes retain their separate native identities.

`VCS_REF` is intentionally different and is assigned to `VERGEN_GIT_SHA` before
release compilation. Embedded commit bytes, OCI revision labels and lifecycle
identity expectations therefore differ. Equal recipe/source inputs do not
establish binary byte equality or a result for a later metadata/source commit.

The whole-gate comparison has three material failures of its prerequisites:

1. **History input drift and failed gate.** Root's evidence owner proved both
   finding commits are outside source/control ancestry and in the unrelated
   `refs/heads/codex/transport-recovery-20261006`. Serial did not fetch that ref
   and scanned 451 commits; split fetched it and scanned 456. Frozen tree equality
   did not freeze `--all` ref/history input. Six redacted findings and their
   custody remain in [the failure record](final-split-history-failure.json).
   Credential contents and new exceptions are absent from this report.
2. **Runner-image mismatch.** Serial used 12 jobs on `20260927.320.1` and 11 on
   `20261004.327.1`; split used 16 and 7 respectively. Both actual split image
   builders used the former, while the combined serial builder used the latter.
   The common Ubuntu label and pinned Rust target do not erase that difference.
3. **Actual Cargo restore mismatch.** Six selected jobs changed requested keys
   and observed restore result, as listed below. Stable cache API storage alone
   was insufficient to establish identical execution conditions.

| Selected job | Serial restore | Split restore |
| --- | --- | --- |
| `quality` | hit | miss |
| `integration` | hit | miss |
| `initializer (webhooks)` | hit | miss |
| `initializer (messaging-oauth)` | hit | miss |
| `initializer (source)` | miss | hit |
| `initializer (webhooks-messaging)` | miss | hit |

The compact observations retain requested/restored keys, native log hashes and
matching API cache versions. All four before/after snapshots have the same 15
stable cache objects, totaling 10,479,073,385 bytes; access timestamps may change.
No `runtime-image` key appears. Every source/derived build in both arms retained
normal import and actually compiled every cargo-chef cook stage, with no cooked
stage marked `CACHED`; no importer error was observed. Other cached stages and
within-run reuse remain present. This supports the recorded shared cooked-stage
miss condition, not universally cold Cargo/tool/registry caches. It does not
repair the six selected-job restore differences.

## Proof custody and repair boundary

Both arms expose source and derived native upload IDs/digests matching their
actual aggregate `needs` values; actual selection was source plus `1,7,47,65`,
source migration rehearsal and no draft deferral. Each arm's six successful
command-log hashes, 28 derived stage-log hashes, four derived SBOM hashes and
source SBOM hash match retained files. Native ZIP CRCs pass. Split's full log ZIP
is 2,500,527 bytes, 265 entries, SHA-256
`82069e23c26ffb44001523912b61d0ae015871e20f9a0ef00af558f6dce9236a`.
Full logs, APIs, SBOMs and native proof bundles stay outside Git under
`/Users/daniil/Projects/consumer-lifecycle/20261006/ci-final-split-proof-fb33186`
and the corresponding serial directory. Native image artifact digests are
archive identities, distinct from OCI image IDs.

Root has adopted a separate candidate-admission correction: complete reachable
`HEAD` ancestry, including merged parents, with native shallow-history refusal;
the broad all-ref audit remains an explicit native CLI operation. The local
[history-scope proof](candidate-history-scope-native.json) reports the canonical
Make target on a real initialized minimal consumer: old `--all` rejects the
unrelated fixture; new `HEAD` passes that case, rejects a finding removed from
a merged ancestor, and refuses shallow history before Go execution. Its initial
incomplete-fixture setup failure remains preserved separately. These scalar
records contain no RSA bytes or cloned Git repository.

That proof names exact Make file hashes and the original source base. It is
not final review or native CI for the repaired candidate. Changing this executed
gate input prevents relabelling earlier whole-workflow proof. Root owns the
repair review, final candidate integration and CI; consumer repository/GHCR/
A→B→A external authority remains pending. The original local B and recovery
reviews keep their accepted scope.
