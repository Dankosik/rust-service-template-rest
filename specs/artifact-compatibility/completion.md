# Artifact compatibility local delivery

Local acceptance: **Accepted**, including the bounded Trivy, build-context and
Git snapshot repairs below.
[PR #250](https://github.com/Dankosik/rust-service-template-rest/pull/250) is
published. Global Completion remains pending repaired CI evidence in the
[ledger](tasks.md). No deployment or restore result is claimed.

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

## Required next evidence

The parent owns committing/pushing the latest reviewed repair and the next selected
CI run. It must establish successful rebuilt source-image gates and serial initialized
artifact graphs **1, 7, 47, 65**, each built once, with one immutable image ID
consumed by filesystem, lifecycle, native inventory, vulnerability and SBOM gates.
None of the four derived image proofs has **completed**. Existing selected CI checks and
the required aggregate must also pass for the repaired candidate; earlier passed
jobs retain only their original head and scope. No local image or full matrix
rerun substitutes for this outstanding CI evidence.

The image-job limit is a forecast: 55 minutes for five images at the existing
approximately 11-minute baseline, 15 for shared public initialization, 10 for
native gates and 10 for setup/slack. The first selected cold CI run must measure
that 90-minute budget; exceeding it reopens the measured bottleneck before any
budget or scope increase. No source/derived image build, workspace Rust build,
provider operation, commit or push was performed by this delivery owner.
