# Artifact compatibility completion

Local acceptance: **Accepted**, including the bounded Trivy, build-context and
Git snapshot repairs below.
[PR #250](https://github.com/Dankosik/rust-service-template-rest/pull/250) is
published. Global Completion is **Accepted** for candidate
`a04e8aae2ddb4cc8a64de1f073529dd242f7b3f2`, with successful selected CI and
CodeQL recorded below. The [ledger](tasks.md) owns subsequent archival and
cleanup publication. No deployment or restore result is claimed.

## Candidate and review

- Branch: `codex/artifact-compatibility-20261005`.
- Base/HEAD during local work: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
- Reviewed worktree fingerprint before this evidence record:
  `675ce19a912dd72600f3b8ed5cf2c8a1db5d1795b1bdc2936bc9c537b0183b3f`.
- Independent integrated Implementation Review: **PASS, local scope only**;
  no surviving findings. Fresh native `reviewer-agent`, Astra/high,
  `/root/artifact_derived_images/integrated_review`; bounded repairs rechecked by
  that same reviewer. The reviewer inspected all four units and their integration.
- [Specification](spec.md) and [Design](design/design.md) remain the accepted
  outcome and boundary. Cargo manifests, lock projection, dependency/toolchain
  versions and runtime/provider choices are unchanged.

## Local evidence

The selected `make plan` route has no Rust workspace build or test suite.
All new helper/test files were included in the changed-file inventory. The
refreshed plan succeeds with no unclassified paths.

**There is no successful aggregate `make verify` receipt.** The initial run
failed at a stale self-test assertion; final acceptance uses its actually passed
leaves plus repaired scoped results below, under the Evidence Contract.

| Command or boundary | Actual result and reuse scope |
| --- | --- |
| `make tools-check` | PASS; 17 pins and Dockerfile/toolchain agreement. |
| `make changed-surfaces-check` | PASS; source/derived routing. Subsequent defaulted reads preserve reset booleans; repaired routing was also exercised by planner/verify cases and reviewed. |
| `make affected-crates-check`, `make validation-lock-self-test` | PASS; unchanged owners reused. |
| `bash scripts/ci/verify.sh --self-test` | PASS after requiring both initializer and artifact CI obligations in the partial receipt. |
| `python3 scripts/ci/initializer-matrix.py --self-test` | PASS; canonical artifact subsets and retained-path narrowing. |
| `bash scripts/ci/template-init-check.sh --self-test` | PASS, including graph selection, fixed image forwarding, fail-fast recording and source-copy failure propagation. |
| `python3 scripts/tests/image-inputs-check.py` | Ten cases initially passed; repaired `Coverage.test_docs_gate_refuses_before_link_checker` passed separately (1 case, 4.091 s). Fixture now supplies actual Make prerequisites, so rejection reaches the intended coverage gate before Docker. |
| `python3 scripts/tests/runtime-image-inventory.py --native-conversion` | PASS, 11 cases, 33.850 s. Actual pinned offline Trivy conversion preserves each application and shared dependency graph. Later provenance/import guards preserve identical fixture data, so this native result is reused. |
| `make quality-check-self-test` | PASS, 12 cases, 11.942 s, including native checker fixtures. |
| `make template-quality-projections` | PASS for minimal, retained, outbound-only and inbound-only projections, including projected image-input coverage. |
| `make duplication-check`, `make architecture-check` | PASS; unchanged Rust graph and clone-admission inputs reused. |
| `python3 scripts/tests/template-owned-purity.py --repo .` | PASS after the portable fixture imports canonical provenance; 59 manifest entries. |
| `make actionlint` | PASS. |
| `make zizmor` | PASS in pinned offline mode; online audits remain CI-owned. |
| Scoped `make shellcheck SHELL_FILES=...` | PASS across all five changed shell scripts, with focused reruns after T3 repairs. |
| `make dockerfile-check` | PASS: BuildKit `--check`, no warnings; this did not build an image. |
| `make docs-check` | PASS: source input policy plus 1,338 links, 591 unique, zero errors. This record receives a separate scoped link check. |
| `git diff --check` | PASS. |

The original verifier attempt is retained at
`/Users/daniil/Projects/Opensource/rust-service-template-rest/.git/codex/verify/attempt-dc33806530be.tJZMv0`.
Its passed leaves are not relabelled as a passing aggregate. Scoped local logs:
`/tmp/artifact-remaining-validation.log`, `/tmp/artifact-final-plan.log`,
`/tmp/artifact-actionlint.log`, `/tmp/artifact-docs-check.log`,
`/tmp/artifact-zizmor.log`, `/tmp/artifact-dockerfile-check.log`,
`/tmp/artifact-shellcheck-repair.log`, and
`/tmp/artifact-snapshot-shellcheck.log`.
Native conversion and T1/T2 focused repair results were returned through their
actual execution records; no synthetic raw log was produced for them.

## Repairs and invalidated attempts

Final validation repaired fixture setup, the partial-receipt expectation,
ShellCheck diagnostics, portable provenance custody and test-created Python
bytecode. The new portable test disables bytecode before its import; an isolated
import check passed without generating cache files. Only the three identified
task-created bytecode files and an empty cache directory were removed.

Host ENOSPC exposed an existing source-snapshot defect: failures inside command
substitution could be followed by a Git commit of an incomplete source. A real
first copied file and a forced second-copy failure demonstrated that false
success before repair. Snapshot construction now explicitly propagates failed
inventory, directory, link, copy and Git operations. The same oracle passes after
repair with exit 28 before Git creation. Before/after logs are
`/tmp/artifact-snapshot-before-repair.log` and
`/tmp/artifact-snapshot-after-repair.log`.

Full local canonical projections were an additional local attempt, not an
original user requirement or a selected local `make plan` leaf. The delivery
owner reconciled that boundary: four local quality projections plus purity and
behavior guards establish local closure; full canonical coverage remains a
mandatory CI obligation. No CI gate is waived.

- `attempt.BbzWO0` (partial candidate `c1641f4...`) is invalidated by ENOSPC and
  incomplete source construction; it supplies no projection proof.
- `attempt.TrmS5O` (candidate `e3c5ffab...`) is an incomplete withdrawn retry;
  it supplies no full canonical projection proof.

Both attempts remain under the Git-common `codex/template-init` directory.
Only owned validation processes were stopped. Owned snapshot directories were
removed after matching their recorded revisions; the preflight clone was removed
after matching its origin. Foreign processes, worktrees and caches were preserved.
The withdrawn retry used the existing Python 3.14
[CPU-count control](https://docs.python.org/3.14/using/cmdline.html#envvar-PYTHON_CPU_COUNT),
which controls the default
[process pool size](https://docs.python.org/3.14/library/concurrent.futures.html#concurrent.futures.ProcessPoolExecutor),
without changing selected cases. It is not acceptance evidence.

## First CI observation and bounded repair

[CI run 37344976261](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37344976261)
completed against PR head `47c97430120acbeb5c31c1697464b9f32261feb7`.
All selected initializer runtime parts (the 65-graph inventory), canonical
projections, source suites/purity, integrations, quality, delivery, documentation,
security and secrets jobs passed. The separate
[CodeQL required gate](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37344976318/job/111882938052)
also passed; Rust analysis was intentionally skipped for this changed surface.
These are results for that head, not for a future repair commit.

The [source image job](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37344976261/job/111881286858)
built the image, passed lifecycle and stopped cleanly in 15 seconds. Its actual
`app.commit` was the PR merge revision
`9127759ec7e34845074693385dec6ba4a91d725a`, distinct from the PR head above.
Native inventory admission observed all three `/service`, `/migrate` and
`/jobs-worker` graphs. Security conversion then failed with
`unknown flag: --ignore-unfixed`. Source SBOM and the four derived images were
skipped; the `required` job correctly failed. The pipeline is **failed**.
The relevant source-image log is retained at `/tmp/artifact-ci-image-47c9743.log`.

The repair changes only `scripts/ci/runtime-image-scan.sh` and its existing
inventory tests. Security mode passes `--ignore-unfixed` to native `trivy image`,
where status filtering changes vulnerability findings without narrowing
`Packages`; `--list-all-pkgs=true` remains. Package graphs and severity categories
are preserved for admission; native `convert` applies HIGH/CRITICAL and exit-code
policy using supported flags. SBOM scanning retains all statuses. This preserves
the accepted inventory and fixable-vulnerability policy without a version,
exception or gate change; pinned
[command registration](https://github.com/aquasecurity/trivy/blob/v0.74.0/pkg/commands/app.go)
and [filter implementation](https://github.com/aquasecurity/trivy/blob/v0.74.0/pkg/result/filter.go)
are the native authorities.

The T2 owner proved the regression fails with the old helper's unsupported flag,
then passed **12 native cases** and scoped pinned ShellCheck after repair. The
new oracle replays the production converter arguments: fixed HIGH/CRITICAL fail,
MEDIUM passes, and unfiltered unfixed HIGH fails as a negative control showing
that status policy must be applied by the scanner. Unchanged local checks were
not repeated. The same integrated reviewer returned **PASS** for this bounded
local delta, with no findings. Reviewed two-file binary-diff SHA256 atop the head
above: `f58223686dc47ad831bdec2417de87cf32ec4b8f3c9adbaa99721a48de73a10d`.
No successful aggregate `make verify` or repaired CI result is claimed.

## Derived context failure and native snapshot correction

At PR head `d19adc26bb964c0cda66789bb2cfe31183d9a477`,
[CI run 37348825749](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37348825749)
passed all selected non-image jobs and CodeQL required. The source image now
passed lifecycle, security and SBOM, including native admission of all three
entrypoint graphs. Its `app.commit` was merge revision
`1a8796ec455eae1fc38969ecc61c17296390268b`.
The [image job](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37348825749/job/111895350585)
then failed graph 1 at `cargo chef prepare`: minimal initialization removed the
optional test helper library and fixture binary, while Docker excluded the
remaining `test/tests/` targets. The filtered workspace consequently had a
manifest with no targets. No derived image completed.

The repair admits the real `test/tests/` tree and covers `test/tests/**` in both
Railway watch forms. It adds no placeholder crate/target and changes no Cargo
manifest, dependency, toolchain, release flag or lock projection. Tests remain
uncompiled by the selected package/bin image builds. The existing source runner's
optional `--image-context` diagnostic uses the canonical minimum projector,
[native BuildKit local export](https://docs.docker.com/build/exporters/local-tar/)
and pinned Cargo target-discovery metadata. Its `--no-deps` selection matches
[cargo-chef 0.1.78 prepare with an existing lock and no member filter](https://github.com/LukeMathWalker/cargo-chef/blob/v0.1.78/src/skeleton/mod.rs).
This is file/metadata proof, not a runtime image build or a replacement matrix.

That diagnostic exposed a local duplex-pipe hang in the shared Git snapshot
helper. The old code timed out under a native 4,097-file snapshot; on installed
Python 3.14.3/macOS, `communicate(input=...)` also blocked. Samples showed both
Python's stdin write and Git's stdout write blocked (`PIPE_BUF=512`). The final
repair feeds the same OIDs through a stdlib `TemporaryFile` and runs the same
`git cat-file --batch`, retaining object-ID, kind, length, delimiter and exit-status
validation. Stdlib process handling drains both outputs and waits for child exit.
The bounded cost is one short-lived OID request file proportional to object count
and a buffered native response; no persistent carrier, custom thread/selector,
new format or version change was introduced. Reopen on measured memory/temp-I/O
pressure or changed native format, rather than silently weakening validation.

| Focused evidence | Result |
| --- | --- |
| Original actual Docker-filtered graph-1 context | Cargo metadata failed 101 with `no targets specified`; `/tmp/artifact-context-before.log`. |
| Repaired canonical graph-1 context | PASS: six real utility test targets; projected lock hash `9b5b16c98492cb9754c9e1a6981aab0b1e37a1d98bf1e28b3f7e372d589be67c` unchanged. `/tmp/artifact-context-after.log`, receipt `attempt.MZQgii`, private source candidate `f9535609d3aaf77a4c4e1ba1b98a2a12811da1f2`. |
| Native committed snapshot before/after | Old request-pipe implementation timed out at 30 s; repaired helper recovered all 4,097 exact committed files despite dirty working-tree bytes in 4.091 s. `/tmp/artifact-batch-before.log`, `/tmp/artifact-batch-after.log`. |
| Existing preflight failure/target preservation | PASS, 3.503 s; `/tmp/artifact-context-preflight.log`. |
| Watch coverage behavior | PASS, 11 cases, 3.628 s, including removal of the new watch family; `/tmp/artifact-context-watch-tests.log`. |
| Portable purity, Python syntax, scoped ShellCheck, Dockerfile check | PASS; 59 portable entries and no BuildKit warnings. |
| Scoped Railway documentation check | PASS, zero errors; `/tmp/artifact-context-docs.log`. |

The same integrated reviewer returned **PASS** for the seven-file delta atop
`d19adc26bb964c0cda66789bb2cfe31183d9a477`; binary-diff SHA256
`be183c9f0fe242c745db58658a81ed4bfad72df7a981a3e8fd15c583d0fae9ae`.
No surviving finding remains locally. Only owned hanging readers were stopped;
their private snapshots were removed after matching recorded revisions, and empty
native-context output directories were removed. Failed attempts remain recorded.
No full local image, four-image matrix or Rust runtime suite was run for this
repair. Actual rebuilt source and all four derived images remain CI obligations.

## Main documentation merge

The pending merge joins our `e9bf7276d194bf29015612518a48a6f5e147c5f7`
with main `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`, from common base
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`. Its delta contains only
`docs/authentication.md`, `docs/outbound-machine-authentication.md`,
`docs/cache.md` and `docs/production-contract.md`. T4 resolved/staged the two
cache/production-contract conflicts; the authentication documents merged cleanly.

Bounded three-way review by the same independent reviewer: **PASS**, no findings.
Incoming freshness, cancellation, capacity and trust-age limitations coexist
with the accepted invalidation, authoritative-cache custody and fenced recovery
obligations. Profile markers survive and the merge introduces no stronger
guarantee. Reviewed staged-diff SHA256:
`7ed9beaf7b5820337c9572171175191151a95db758c80438adb95ab323cbb65d`.

The scoped four-document `make docs-check MARKDOWN_FILES=...` exited 0:
32 total links, 30 unique, zero errors; `/tmp/artifact-merge-docs.log`.
The staged diff check passed and no unmerged paths remain. Runtime, scripts and
CI are unchanged, so their accepted local evidence is reused. No Rust suite,
image or profile matrix was repeated. Local merge acceptance is **Accepted**;
root retains the merge commit, push and fresh selected CI. Previous CI results
remain evidence only for their original revisions.

## Required classifier admission

At head `ef1ce57d42febfa93edbd152f4aeef4bdd978a6c`, initial
[CodeQL run 37362352139](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37362352139)
had classifier job `111939730527` cancelled without executing a step after the
hosted runner was not acquired. Nevertheless, aggregate job `111945040319`
passed while its rejection step and analyses were skipped. This is an observed
admission failure, not evidence that CodeQL ran. No retrospective claim is made
about the hidden `needs.*` values. Root separately confirmed a
[GitHub Actions runner-assignment incident](https://www.githubstatus.com/incidents/3q1yb5m7ltvb);
other jobs cancelled before acquiring a runner also represent unexecuted proof,
not project test failures. Root's native failed-job retry on the same head then
completed attempt 2 successfully: classifier, Actions analysis and
`codeql-required` passed, with Rust intentionally unselected. That legitimate
retry does not erase the initial false admission or prove the unpublished guard.

Both `codeql-required` and `ci.required` now use the same native guard:

```text
always() && (needs.changes.result != 'success' || contains(needs.*.result, 'failure') || contains(needs.*.result, 'cancelled'))
```

The successful classifier is a prerequisite for treating later skips as
intentional. Missing, skipped, failed or cancelled classification refuses;
`always()` makes that predicate explicit instead of relying on an implicit
status condition. Existing profile/draft/analysis selectors and selected-image
checks remain intact. The
[needs result contract](https://docs.github.com/en/actions/reference/workflows-and-actions/contexts#needs-context)
and [status-function contract](https://docs.github.com/en/actions/reference/workflows-and-actions/expressions#status-check-functions)
are the platform authorities.

Yq performed the scalar updates. Scoped actionlint and diff checks passed;
`/tmp/artifact-classifier-actionlint.log`. The same independent reviewer returned
**PASS** for the two-workflow delta, SHA256
`401ea2dc1ffc7ff2660f584da69ea35b84f5c3d601c504bedb0833275e07a65a`.
Local delta acceptance is **Accepted**. No new job, evaluator, framework, provider
operation or local matrix was introduced. Root's failed-job rerun and ongoing
source CI consume their immutable earlier head; this local result does not
replace missing executions or establish final-head CI success. Cleanup remains
pending proven closeout and a clean worktree under its existing owner.

## Completed source and derived image proof

[Image job 111940305508](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37362475341/job/111940305508)
succeeded for PR head `ef1ce57d42febfa93edbd152f4aeef4bdd978a6c`.
Its checkout/source revision was the GitHub PR merge commit
`f477ba7df2dbe95e1363f4ea3b918dca45a1c543`, **not** that PR head.
The private fixed initializer candidate was
`9f60aecfc5ea326991b3f33120d7aa8f1a324721`.

[Native image-proof artifact 11368661051](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37362475341/artifacts/11368661051)
was read from `/tmp/artifact-compatibility-ci3-s4kbzd4c`.
Receipt `rust-service-template-rest/rust-service-template-rest/.git/codex/template-init/attempt.2GGH5p`
ends with `state=passed`; its SHA256 is
`09a390f9bf3cd09c6f663ab32dcb013504f4c76297546f424137c8832df7aa74`.
All **28 recorded log hashes** matched the downloaded bytes. Each selected graph
records successful public initialization, one build, fixed image identity,
lifecycle, security, SBOM and cleanup; command readback keeps that same image ID
across its gates.

| Graph | Initialized revision | Immutable Docker image ID | Native applications | Init/build seconds |
| --- | --- | --- | --- | --- |
| 1 | `3827a4b96b4986c7ce2d99719006bb2ba6a421b4` | `sha256:0f179a6898a2accefd0b1b889b380d5f9343ea7bdcc7484554a8b896a7c2f220` | service | 96 / 240 |
| 7 | `e3d098e8debfea47725d705b8ce6faabb4f43907` | `sha256:3ca4cb8dd1ac72b75822e5e21dadc9cb073ec842b21557385d86152dc70a4b28` | service, migrate | 28 / 316 |
| 47 | `1ea94440b4aeb171649837d9173cf2f69a7ba380` | `sha256:583ffa55fd586aad1a6945a4942f97738c9590276534e852cc4ba954e9b6584b` | service, jobs-worker | 4 / 325 |
| 65 | `d7fadf932eb6f84f052abc57659a7fa7a4d1b70d` | `sha256:8550bacb497ff1d09e5a234a9b1920bee7192605d3afb29c349a602a02fe716a` | service, migrate, jobs-worker | 53 / 564 |

SBOM image identities agree with the receipt, application sets match the retained
binaries, and dependency traversal reaches each selected package root without a
missing component reference. Security and SBOM logs admit the same per-binary
sets. Lifecycle logs report each initialized revision as `app.commit` and clean
15-second stops. Graphs 1/7 report no worker; graph 47 observes the no-handler
refusal and graph 65 the disabled-PostgreSQL refusal before provider I/O.
The lock hashes remain separately recorded per graph in the native receipt.

The source image also passed lifecycle, native three-binary admission, security
and SBOM. Its immutable image ID is
`sha256:097b978b0bbf3bf19191febf3e312da71bbe207b0dfbe3483d1a1865d933e780`;
`_temp/source.cdx.json` has SHA256
`d364ea87736ded2525491bea091c76f1080ca11b6a6f55c77dccbd8177a8c836`.
Derived SBOM SHA256 values are:

- Graph 1: `804cf2046cb77375cb2c202039d816b53443be99cd1addd6413957b763479e74`.
- Graph 7: `851660a139b536d00732fabbfe974a21c04aa7eec42e7888ab7fb2111fbf7ba7`.
- Graph 47: `45fd03fb73891e7c5d75e1f8db750af390ec15a53222559bccdf61174b7927c3`.
- Graph 65: `72a7503e2b0ad71ebf4d4be5d04644a24664c828fcec84238c8861cf0f9b08a5`.

The image job took **39m50s**, including a **9m55s** source build; the artifact
stage recorded **1,709s (28m29s)**. BuildKit/Cargo/Trivy caches were enabled.
This is one observed cache-enabled run, not a universal cold/no-cache bound.
Migration rehearsal was intentionally skipped because no SQL change selected it;
this image evidence claims no migration execution or live provider result.

The overall old run is **cancelled**, not passed. Two initializer matrix members
never acquired a runner; the final aggregate remained queued. After image proof
finished, root confirmed native force-cancellation of the remaining old run when
normal cancellation did not close it. The completed image job and artifact remain
valid at their recorded identities. CodeQL attempt 2 on the same old PR head
succeeded. No missing matrix execution is converted into a pass.

Local acceptance remains **Accepted** and the four retained-binary artifact seams
are now actually observed on the old revision above. Global Completion remains
pending the final candidate's selected CI. This readback ran no new build,
container, matrix or runtime test and modified only this evidence record.

## Selected-gate success repair after runner failures

[CI 37367400441, attempt 2](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37367400441/attempts/2)
on `469118c2f2d4681b1abec4f27939eebfdb0168b0` ended **failure**.
Several executed suites passed, but quality, docs, initializer webhooks-messaging,
image, delivery and OAuth jobs were cancelled after waiting for a hosted runner;
the reported annotations and zero executed steps are absent proof, not failing
project tests. Aggregate job `111972879879` acquired a runner at 21:03:44 UTC.
Its generic failed/cancelled rejection step was skipped even with `always()`;
its explicit selected-image success check failed. This demonstrates that the
wildcard rejection alone is insufficient. The hidden engine cause is not known.

The bounded repair keeps classification mandatory and adds explicit terminal
success checks for every selected CI gate and both selected CodeQL analyses.
Each check uses `always()`, mirrors its job's exact surface/event/draft predicate,
and rejects a named job result other than `success`. Intentional unselected and
draft-deferred skips remain accepted. The existing wildcard failure guard remains
additional protection, but acceptance no longer depends on it alone.
Integration and OAuth checks have profile-removal markers registered in the
existing inventory, so derived services remove the checks with the removed jobs.
No new job, schema, evaluator or framework was introduced. Runtime, Docker,
Cargo and artifact-input code is unchanged.

Yq parsed the workflow authorities. Mechanical comparison confirmed all **11 CI**
and **2 CodeQL** success predicates match their job selectors exactly after
whitespace normalization. Scoped actionlint and `git diff --check` passed;
`/tmp/artifact-selected-gates-actionlint.log` records the lint result.
The existing Cargo-free `make template-init-projections` passed against private
candidate `e3d57d3164a479ff01a6956a54e55abe2d836e61` from source revision
`469118c2f2d4681b1abec4f27939eebfdb0168b0`. Native receipt
`/Users/daniil/Projects/Opensource/rust-service-template-rest/.git/codex/template-init/attempt.iIdGso`
ends with `state=passed`: self-test 3s, canonical projections 160s, total 171s.
Their log SHA256 values are respectively
`b834b50b83e84d430428081ea3ef81d90d73b7620dc5b1e46e3beb33ac43e98d` and
`f83b995a5d2223dd067da8a884463a8f4030fb792e94b172bd2405fd371765de`.
The full captured output is `/tmp/artifact-selected-gates-projections.log`.
The retained independent reviewer returned **PASS**, with no findings, for the
fixed three-file workflow/inventory delta, SHA256
`18b4d5feff0d3d2624a38c1ca47c6b9d99b054567f7266aacb7edf2dd66d03a3`.
The reviewer independently read the terminal projection receipt and confirmed
all 13 selectors and both new profile-removal markers. Local repair acceptance
is **Accepted**. No CI retry or full local execution was run. Writers and local
validation processes are stopped; final publication remains with root.

## Final selected CI and artifact proof

Candidate PR head: `a04e8aae2ddb4cc8a64de1f073529dd242f7b3f2`.
Root's native terminal readback confirms:

- [CI 37375346236, attempt 1](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37375346236/attempts/1):
  **SUCCESS**, including aggregate job `112000425653` and every selected job;
  grpc was intentionally unselected.
- [CodeQL 37375346229, attempt 1](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37375346229/attempts/1):
  **SUCCESS**, including classification, Actions analysis and its aggregate;
  Rust was intentionally unselected.
- [Image job 111982891531](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37375346236/job/111982891531):
  **SUCCESS**, source and all four derived image gates. Its interval was
  2026-10-05 21:26:39–22:06:53 UTC, **40m14s**.

The selected-gate repair is present in this successful actual CI. The selected
success paths and intentional unselected skips ran; this success does not claim
a new forced cancellation experiment. Earlier cancelled executions remain failed
or absent proof at their original identities and are not converted into passes.

[Native image-proof artifact 11373931537](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37375346236/artifacts/11373931537)
was consumed from `/tmp/artifact-compatibility-final-0rcggimf`.
Receipt `rust-service-template-rest/rust-service-template-rest/.git/codex/template-init/attempt.RZaW5a`
ends with `state=passed`; SHA256
`6853f7595da3512599b3211d322857de350e636b32f098d765b701ce2474590e`.
All **28 recorded log hashes** match the downloaded bytes. The checkout/source
revision is `8bc1e85a1369a0a70d3f9c2fcfe130a6834b8f9d`, distinct from the PR head;
the initializer's private fixed candidate is
`3bcf41e3db8eae1b5db9e052f5ef86e311f3c22c`.

| Graph | Initialized revision | Immutable Docker image ID | Native applications | Init/build seconds |
| --- | --- | --- | --- | --- |
| 1 | `e11e5746a917921f825867199a4f4b1116aa5c7a` | `sha256:8122d5b9215d1a6c658a9f12a162158f20b93b30872912c33681d1d81b5ff8f9` | service | 111 / 246 |
| 7 | `952ac01016ca8607e7dae1c02e22f3244b1a3c83` | `sha256:f1798442f8e9e4acf57344c26a5611bdf9dfe822fe722cb57b1842701628f6db` | service, migrate | 33 / 326 |
| 47 | `7a21280b43332b83afe65120dfd11e77dca98c8c` | `sha256:de37e0315d61feca15fbcb299612f690d868af70437b74df292afac869d75d13` | service, jobs-worker | 4 / 327 |
| 65 | `1b4db3ed7c46b20340c6f69b62b2aca89f622cf4` | `sha256:eaf5c8c3f378f52c4934d0deb2183450249c4805f47a17058394d5a4a3d2c7f9` | service, migrate, jobs-worker | 57 / 559 |

Every graph records successful public initialization, build, identity, lifecycle,
security, SBOM and cleanup. Downstream commands reuse its immutable image ID.
Lifecycle readback matches each initialized revision in `app.commit` and observes
clean 15-second stops. Graph 47 observes the no-handler worker refusal; graph 65
observes the disabled-PostgreSQL worker refusal before dependency I/O.

The source SBOM identifies image
`sha256:3d4746983f86988da8db8875cb205bc3b4cf59be84ebf2fb5561a6bf00840502`
and the three applications service, migrate and jobs-worker. Each derived SBOM
identity matches its receipt. All five SBOM application sets match retained
entrypoints, each application reaches its expected Cargo package root, and every
traversed component reference resolves. Derived security and SBOM logs
independently report admission of those same per-binary sets. SBOM byte hashes are:

| SBOM | SHA256 |
| --- | --- |
| Source | `fe6997bf37e1eb2126c582ced1bcf7d07f51e606c372cd2a2ad82baffc96b508` |
| Graph 1 | `028c3a804f9231fe4e2daad65288a7a3931a9b1506b197d1c630f683a2d37c25` |
| Graph 7 | `510f181637e69c67986f61332635f49d8c2e7581524fc6278d769b09831effb6` |
| Graph 47 | `70f1cea2affb86ac2642827e2118a4d5454ced8b2c6178ba3f03fcc8da51a38b` |
| Graph 65 | `a332d0c6cc6c91f09ef9f62be0f51c1671ca3f39f18fbfe44ae5591701d7de63` |

The derived artifact stage recorded **1,748s (29m08s)**. Build logs contain
BuildKit cache hits; the complete image job fits the existing 90-minute limit.
These are measurements of this cache-enabled run, not a universal cold-build
bound. The receipt consumption started no build, test suite, container, CI retry
or provider operation. Earlier `ef1ce57d`/`f477ba7d` image observations retain
their own identities and scope.

## Completion result

- Unit: **Completion**.
- Verdict: **Accepted**, global candidate scope at
  `a04e8aae2ddb4cc8a64de1f073529dd242f7b3f2`.
- Evidence: previously accepted consolidated local checks and scoped repairs,
  successful final selected CI/CodeQL, and the native source/derived artifact
  readback above. No deployment, registry publication, live-provider recovery or
  restore execution is claimed.
- Review: integrated Implementation Review **PASS**, including the retained
  reviewer's bounded repair deltas; no surviving findings. No code changed while
  consuming this terminal evidence.
- Next owner: root records canonical acceptance, archives this completed result
  in Git, moves the two remaining durable decisions to their existing owners,
  and performs the authorized bundle/worktree cleanup. A subsequent cleanup or
  documentation publication creates a new head whose selected CI still needs
  its actual result. Acceptance here does not pre-approve that future candidate.

Only this Completion record was edited during final receipt consumption.
All readers and writers for this delivery result are stopped.
