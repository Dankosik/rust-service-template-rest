# Consumer lifecycle Completion

Status: local A and B content admitted; integrated delivery is **not Accepted**.
The native historical recovery is proved locally. The comparable final CI result
and consumer publication/runtime observations remain incomplete. Root owns the ledger, source integration and
external effects; `/root/consumer_preparation` owns these Completion records
and the separately owned local consumers. `tasks.md` is not edited here.

## Fixed local artifacts

| Artifact | Actual identity and scope |
| --- | --- |
| Template used for accepted B | `4ba38a78a228fcf30e0ed1d9a98b8431ac561740`, tree `ba7fd5d499779b5bca8087c18b1a8946aea0d7aa` |
| Current local implementation/recovery candidate | `fb33186d0eb1fab14ff37d071a4e2d6eac9bb45c`, tree `726913f5d37482676a0c50363c97569f53e335c3`; later source-only assertion and evidence closure preserve B runtime content |
| Pristine captured consumer A | `384815a674ea9e8f08f8f2e3d6b43a968f249ccd`, reproduced from historical source `2cb871895b9edd018205fc98223477e269fce2e9` |
| Accepted A | `ac09d5af7737d6db34e6bf9eaa7a718b5807bcf3`, tree `cd6484b195dbfee020bf585ed286193695d7fa46`; parents corrected consumer `d31d212c16d8f0c83c4dbf98130bc653036dacb1` and captured baseline `e67f0437318b6350b006271f381c9101de97483f` |
| Authored B input | `8c970447fde62a1702478557ac0ef87f20152385`, accepted A plus the authored greeting/version evolution |
| Final resolved B | `1bd0e1d15c4891f2b5c45b93aad4380ff58f8bbe`, content tree `4a661c2d7618f1d9b45617d8a394950145528228` |
| Newly rendered B baseline | `0edc900b04a389a39f702b62fc81bec3271cd5f5`, tree `5b4cd8f16b969016c39b24b6e79f2c5a0e4ae195`, parent captured B0 `e67f043…` |
| Accepted B | `f3fc351768f58290a9f7fc257ce69dab149e0f04`, tree `2a914ba0aa7c8b066d960fe9e62ef2c46170ced6`; parents resolved B `1bd0e1d…` and pristine B1 `0edc900…` |
| Current minimal consumer | `5ab843f099ac16df87c3d4da58139d4619450d9e`; original full initialization `32f469707cdf1514fece814b23af82e6fa18b788` remains reachable |

The original [preparation receipt](consumer-preparation.md) retains the initial
render identities and recipe. No later repair relabels those executions.

The durable consumer repository is
`/Users/daniil/Projects/consumer-lifecycle/20261006/lifecycle-demo`.
`main` and `codex/release-a-source` retain accepted A;
`codex/release-b-source` contains accepted B and is the clean checked-out branch.
Neither acceptance changed runtime content: the native seals changed only
`template.upgrade.json`, and the clean originals integrated by explicit local
fetch and fast-forward. No consumer remote exists or was written.

The final isolated accepted candidate is
`/Users/daniil/Projects/consumer-lifecycle/20261006/lifecycle-demo-upgrade-b-final`.
Native generation, resolution, review, validation, seal and integration records
are in `completion-records/upgrade-b-final/`; A's corresponding records are in
`adoption-a-final/`. Earlier failed/adopted-content and superseded B attempts
were aborted and preserved, including the actual failures and their proof.

## Local proof and reuse

| Check | Observed result | Boundary |
| --- | --- | --- |
| Full final B preparation | PASS, 60.812 s | Actual public rendering at 4ba with locked offline inputs, pinned toolchain, explicit generation target and admitted line-table debug setting |
| Final B conflict resolution | One blank-line-only documentation conflict | Both sides remove the same inapplicable gRPC paragraphs; same three native stage blobs as the earlier reviewed attempt; no migration issues |
| B build and workspace tests | PASS, 50.156 s and 457 tests in 97.111 s at R626a | Exactly 895 other tree entries match final R1bd; only portable Make, verification fixture and ownership manifest differ. Two ignored tests remain explicitly limited. No new binary identity is inferred |
| B docs, actionlint and migration history | PASS at R626a | Unchanged document, workflow, migration and checker objects admit reuse; actual commands/log hashes stay bound to their original execution |
| Final source verification fixture | PASS, 40.859 s at 4ba | Source-only groups execute only with the real source capability |
| Actual minimal verification fixture | PASS, 30.677 s at 5ab843f | Real pruned consumer, without source-only helpers or PostgreSQL Make file |
| Causal old-Make counterexample | Expected FAIL, 3.494 s | Current none-profile fixture with the old unconditional recipe fails at missing `image-results.py`; repaired recipe permits absence and rejects an injected present-helper failure |
| Changed pinned ShellCheck | PASS, 2.636 s | Actual repaired `verify.sh` and native carrier |
| Current minimal build and affected service tests | PASS, 43.052 s and 30 tests in 41.442 s | Closes the earlier service predicate correction's matching-proof gap; no repeated full workspace/profile matrix |
| A admission proof | PASS with scoped reuse | 457 workspace tests, then 30 affected service tests/build/strict lint after its helper correction; native history scan, routing and same-fixture/new-commit negative evidence are separate |
| Updater protocol | Six passing cases | Retains actual execution/input hashes; reviewer confirmed unchanged relevant inputs at the reviewed source |

The B object comparison is `upgrade-b-final/resolved-reuse-comparison.json`.
It covers Rust, manifests, lock, toolchain, SQLx, migrations, configuration,
contract, workflow and all other unaffected objects. Git/VCS metadata and final
published binary identity are separate. New portable controls have fresh proof
under `repair-4ba/`; earlier setup failures under `repair-a385/`, `repair-6525/`
and `repair-006d/` remain failures, not passes.

Earlier source purity, routing/recorder, docs, original minimal workspace,
source strict-Clippy and storage diagnostic records remain under `local/` with
their actual candidate scopes. The isolated storage diagnostic and 52-case
parallel run passed, but no flake-removal claim is made. Original shell/workflow
failures and actual delegated repair results are both retained.

## Independent review

The single fresh reviewer is
`/root/consumer_preparation/integrated_final_review`, retained for bounded repairs.
The initial result identified the duplicate initializer environment and prohibited
portable helper ownership. Both findings closed in the bounded recheck.

The reviewer independently gave **PASS for local B content admission**, verified
B's clean content tree, baseline ancestry, the 895 equal objects and all three
new tooling files against the rendered baseline. Its result is retained in
`integrated-review-b-local-recheck.md` and bound into B's accepted metadata.

The bounded T2 assertion repair is now proved by the real canonical recovery.
`integrated-review-final-local.md` records **local implementation/B PASS** with
no surviving candidate-caused findings, and **NEEDS_PARENT for global Completion**
only for C2 and externally gated R2/R3. Earlier failed findings remain preserved
at their actual candidates.

## Native historical recovery

The first 8cf canonical invocation failed before execution because Make-exported
initializer selectors duplicated explicit flags. Its 2.205 s record remains under
`native-recovery/`; no providers or actors started in that attempt.

The repaired 4ba invocation ran for 271.122 s and built both actual historical
actors from `67be869acea112af271ec8ba621cbc50ae9d36b7` and
`2cb871895b9edd018205fc98223477e269fce2e9`, using their own public initializers,
pinned 1.98.1/1.99.0 toolchains, identical overlay and separate target directories.
The corrected worker correctly exited 1 before readiness on the old schema and
logged `postgres migration history: embedded migrations are pending` to stdout.
The fixture's stderr-only assertion then failed before native archive/restore.
The narrow assertion repair subsequently passed on source
`fb33186d0eb1fab14ff37d071a4e2d6eac9bb45c`.

Actual evidence is retained at
`/Users/daniil/Projects/consumer-lifecycle/20261006/native-recovery-f4ba-20261007`;
command and custody records are in `completion-records/native-recovery-4ba/`.
No actor processes remain. Both exact named Compose projects stopped all four
containers with exit 0 and preserve their four named volumes. No archive or
`completed.json` exists. No additional cleanup was performed by Completion.

The fresh canonical fb33186 run **passed** in 209.845 s, selecting exactly one
case with 1 passed, 0 failed, 0 ignored. Its evidence directory is
`/Users/daniil/Projects/consumer-lifecycle/20261006/native-recovery-ffb33186-20261007`;
command and terminal readback are under `completion-records/native-recovery-fb33186/`.
All five native archive hashes match (16,459 bytes); restored database, broker
state/consumer positions and role/session settings reconcile. Claim generation
advances 9→11; replay reaches the handler twice with one durable effect. Pending
and outstanding ACK counts finish at zero. The retained failed job and its
recovery history survive; `event-dead-letter` remains explicit in DLQ.

Observed versions are PostgreSQL 18.6, NATS 2.15.0 and CLI 0.5.0. Measured local
fence-to-ready is 1,459 ms; restore-start-to-durable-completion is 1,192 ms.
Successful-attempt containers, volumes and actor processes are absent after
native cleanup. The failed 4ba attempt remains separately preserved. Historical
refusal is not a published A→B→A rollback, and these local timings do not
establish production RTO/RPO or multi-node guarantees.

## Native CI comparison and remaining external work

The reviewed [CI design](design/ci.md) selects normal imports with the shared
cooked-stage cache observed absent, retaining intra-run reuse and a narrow
measured claim. The old warm-only plan is preserved as retired. Root owns the
active comparison plan and the three evidence files released for committed link
closure; Completion does not edit those released files concurrently.

Old diagnostic run 37530417527 succeeded with 23 jobs for source 8cf/control 3fa.
Its 47 downloaded proof files are retained outside Git at
`/Users/daniil/Projects/consumer-lifecycle/20261006/ci-old-control-proof-8cf53b`.
Six successful measurement records and 28 stage-log hashes match; each cooked
stage actually compiled, while derived non-cook stages retain local cache hits.
The 9,465 observed native job-seconds are not a charge calculation. No final-source
or scheduling-improvement claim is inferred from that old run.

Fresh final serial run 37536152461 uses control
`d1c510ce2f79ac7db514f0f779541a855400c3a0`, parent 4ba; only its scheduling
workflow differs. All 1,360 non-scheduling objects and 698 Docker inputs match.
Its docs job failed because three linked local receipts were not committed;
the terminal native result is FAIL for docs and `required`, while image-source,
relay and image aggregate succeeded. It is not an admitted whole-gate timing arm.
Root committed the three missing receipt files with the source-only T2 assertion
repair as fb33186. An exact Git-archive snapshot of its committed tree passed
canonical docs-check: 343 tracked Markdown files, 2,125 links and zero errors,
without access to the original untracked files. Root owns the explicitly bounded
replacement serial and previously unstarted split after prior-run terminal join,
cache/quota preflight and intact comparable proof. No old-control bridge is admitted.

A/B local source preparation is now concrete for root's external proposal.
New consumer repository/settings/refs, GHCR publication, signature/provenance/SBOM
and digest verification, and observed distinct-digest A→B→A service/worker runs
remain pending matching authority and execution. The [proposed external
envelope](design/release-recovery.md#proposed-external-envelope) remains the owner.
Local content admission, native template CI and the historical actor drill do
not substitute for those consumer release results.

The one admitted replacement serial run `37541180687` completed successfully
with all 23 jobs on control
`fbada6da0c3d118942b91073c2582688d7aca637`, whose sole parent is final source
`fb33186d0eb1fab14ff37d071a4e2d6eac9bb45c`. Native readback confirms only
`ci.yml` differs; all 1,363 non-scheduling objects and 698 Docker-context objects
match. The changed source-only test is included in the Docker context, so its
new context hash is recorded rather than copied from the prior candidate.
`ci/replacement-serial-control-readback.json` and dedicated fb33186 manifests
retain the input readback. `ci/replacement-serial-prerequisite-verdict.json`
records intact first-arm proof and admission of the one previously unstarted
Split. Its success alone does not establish C2.

All local implementation, consumer admission, native proof and review processes
have joined. Local Completion artifacts are ready for root custody; C2 and R2/R3
remain the only global outcome gaps. No consumer external write was performed.

The actual Split run `37546407132` on unchanged source fb33186 is terminal
FAILURE: 21 of 23 jobs succeeded, while `secrets` and `required` failed. Both
image lanes and image aggregate succeeded, with native artifact IDs/digests and
command/log/SBOM hashes verified. The scan consumed a new unrelated remote
branch: root established that both finding commits are outside source/control
ancestry and the scan changed from 451 to 456 commits. No raw credential content
is copied here. Root owns the explicit complete-HEAD-history correction,
including shallow refusal, while broad all-ref auditing stays separate.

C2 is incomplete. Actual image builders used different runner image versions,
and six selected jobs had different Cargo cache keys and hit/miss outcomes.
Creation-to-`required` elapsed time was 2,497 s serial and 1,788 s split; these are
confounded diagnostic observations, not scheduling-improvement proof. Assigned
runner intervals were 9,894 s and 10,121 s, respectively; all five related runs
account for 43,271 s. This is not a charge calculation or all development CI.
[The terminal analysis](completion-records/ci/c2-analysis.md) retains definitions,
actual lane intervals, cache outcomes, artifact identity and storage limitations.
No additional benchmark follows automatically, and ordinary draft PR CI cannot
substitute for C2 or a later repaired-candidate result.

The root-owned canonical history-target proof passed in an initialized minimal
consumer: unrelated refs no longer change candidate admission, a removed finding
in merged ancestry still fails, and shallow history refuses before Go execution.
[Its scalar evidence](completion-records/ci/candidate-history-scope-native.json)
preserves exact Make hashes and the initial incomplete-fixture setup failure.
The focused repair review and native CI for the integrated candidate remain
parent-owned; earlier B/recovery review is not relabelled.

The refreshed curation proposal keeps compact reports and failures in Git while
full native logs, archives, binaries, consumer patches and fixture repositories
remain external with hash/path custody. No files were staged or committed by
this Completion evidence update. R2/R3 external consumer authority is unchanged.
