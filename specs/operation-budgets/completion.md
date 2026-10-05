# Operation budgets Completion

Status: final validation in progress. All T001 writers are joined. The root
owns `tasks.md`; this Lead owns local proof, repair, review and PR delivery.

The single [PR #252](https://github.com/Dankosik/rust-service-template-rest/pull/252)
is open as a draft against `main`. Build/tests and final review closure remain
pending; ready promotion waits for them. No merge, deployment or live-provider
conformance is authorized or claimed.

## Candidate

Base: `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`. Current source bundle: 62 outputs,
SHA256 `c193c24e812bf73a271baa4f530fd75a12b6dd49f89fb1fd5176f67b1976ac92`. The digest uses the method in the
[implementation result](implementation-result.md), comparing source paths to the
base and excluding `specs/`. This identifies code independently of receipt edits.

The first draft head `aba175690c0dd726f6e3b3f0be7ae789eb67e898` is retained as a
recovery identity. Its benign Planning-prose secret-scan false positive was
reworded by the root with unchanged meaning. The root authorized replacing only
this task's draft branch with an exact lease on that head; the replacement
`a9fb45ad79ad3097cdfed081b0a4db001a40eb10` was pushed successfully. No ignore,
policy exemption or other-ref mutation was added. The latest marker formatting
repair is being committed before the required build/tests run.

## Selected proof and current results

`make plan` selected workspace build/test because manifests changed, plus the
existing mixed-surface gates below. Commands use the pinned toolchain by
prepending `/Users/daniil/.cargo/bin` to the inherited PATH. All CPU-heavy work
uses `scripts/ci/validation-lock.sh`; Cargo is locked and its target is owned by
this task. Command-local dev/test debuginfo zero and incremental disabled limit
artifacts without changing optimization or debug assertions.

| Local check | Result and scope |
| --- | --- |
| `make changed-surfaces-check affected-crates-check validation-lock-self-test` | Passed in their recorded command groups. |
| `make verify-check quality-check-self-test` | Passed after retaining the inherited PATH, which supplies native npx. |
| `make architecture-check duplication-check unused-deps deny` | Passed. Existing redundant-ignore and registry-duplication warnings do not change admission. All 578 registry package versions/checksums remain unchanged. |
| `make fmt-check` | Passed; latest bearer marker was additionally checked after a second formatter pass. |
| `make lint` then `make lint-changed PKGS=infra-oauth2-client-credentials` | The workspace pass exposed only the final OAuth table-test line-count finding; the focused repair rerun passed. Earlier extractor/S3 lint defects had already been repaired. |
| `make shellcheck SHELL_FILES=scripts/ci/changed-surfaces.sh` | Passed. |
| `make dockerfile-check` | Passed, no warnings. |
| `make docs-check` | Passed: 1331 links, zero errors. Final receipt link changes will be checked when assembled. |
| `make secret-scan (against the comparison base recorded above)` | Passed on the rewritten draft: worktree and one-commit range, no findings. |
| `make template-quality-projections` | Pending final rerun; the old snapshot deadlock is repaired, and its next profile-marker refusal has a formatter-stable repair. |
| `make build` and `make test` | Pending; next required local execution. |

The original compile-only commands remain evidence for compilation only. No
behavior test result is inferred from them. The first observation checkpoint
for a long command is 60 seconds and follows actual stage/output thereafter.
No local heavy/full override or new validation environment is selected.

## Repairs and evidence preservation

The initial corrected-PATH projection attempt was stuck for over five minutes
in existing `_batch_blobs`: it wrote all SHA requests before draining Git output.
Baseline input is 48,954 bytes; the first draft is 49,692. Only this attempt's
hung Git child was terminated. Its failed receipt remains under the Git-common
`codex/template-init/attempt.RVTDLv` locator. The root routed a narrow C13 repair
into the ownership map. Native subprocess communication now feeds stdin and
drains stderr while stdout uses an anonymous temporary file; declared-length
blob parsing and one in-memory result dictionary remain. The repeated original
gate completed snapshot extraction and reached the next real marker refusal,
so snapshot completion is observed but full projection acceptance is still pending.

The first [independent integrated review](implementation-review.md) reported
R1/R2 profile-marker defects and no other surviving runtime defect. The guide's
nested marker is now a sibling block. The bearer match arm is short enough to
keep its closing marker on a full line after repeated rustfmt; a newline-only
repair had been folded back by the formatter. No auth/budget/finality policy
changed. The same reviewer will recheck these anchored repairs and the
root-routed snapshot repair, consuming the final local results before PASS.

## Remaining delivery

Finish matching build/tests, final projection proof and bounded review closure.
Update the existing draft, then mark it ready and obtain selected CI success on
the current head, including `required` and `codeql-required`. The initializer
runtime/canonical projection matrix, database/provider integrations, SQLx
metadata, runtime image/security and CodeQL retain their existing CI owners.
Instruction/schema checks apply only if the actual classifier selects them.
No external gate has been accepted from a draft deferral or a local result.
