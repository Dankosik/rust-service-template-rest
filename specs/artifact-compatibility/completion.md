# Artifact compatibility local delivery

Local acceptance: **Accepted**. Global Completion and publication remain pending
required CI evidence in the [ledger](tasks.md). No deployment or restore result
is claimed.

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

## Required next evidence

The parent owns the separate PR and actual selected CI results:

- Full canonical projections and the existing initializer runtime matrix.
- Source image build/lifecycle/security/SBOM where `runtime_image` selects it.
- Serial initialized artifact graphs **1, 7, 47, 65**, each built once, with the
  same immutable image ID passed to filesystem, lifecycle, native inventory,
  vulnerability and SBOM gates. Actual binary extraction/native report assumptions
  remain unobserved locally.
- Existing selected messaging, cache, object-storage and OAuth integration
  leaves; applicable CI security/online audits and required aggregate success.

The image-job limit is a forecast: 55 minutes for five images at the existing
approximately 11-minute baseline, 15 for shared public initialization, 10 for
native gates and 10 for setup/slack. The first selected cold CI run must measure
that 90-minute budget; exceeding it reopens the measured bottleneck before any
budget or scope increase. No source/derived image build, workspace Rust build,
provider operation, commit or push was performed by this delivery owner.
