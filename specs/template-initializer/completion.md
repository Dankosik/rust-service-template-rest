# Stage 9 local completion

Local Completion: **Accepted**, 2026-09-20. Initializer, profile closure and
committed-source sync satisfy the retained [specification](spec.md) and
[system design](design/system.md). Stage 9 remains in progress until landed-main
and required remote-CI evidence exists; no publication or deployment is claimed.

## Fixed candidate and review

- Baseline: `81bbd16b320c90d430522e69fc2d52a2aaa049de`.
- Tested and independently reviewed 72-path candidate:
  `0419a737573c086d809b54c0b9de534c0f2fc097867884cca51a77500818c7b0`.
- Candidate archive SHA256:
  `4a2f2f6f495030258ca8565d3f48d8c4ef3d419aab2924f312ea3da7d159d599`.
- Local manifest/archive locator: `.git/codex/stage9-candidates/<candidate>/`.
- Fresh reviewer `/root/stage9_t1/final_review` returned **PASS**: all eleven
  anchored code findings closed, all 51 final matrix log hashes verified.
  No broad review was restarted after repairs. Static instruction fixtures
  establish no measured model-behavior claim.
- After public command exit, all 72 candidate bytes/modes and baseline HEAD
  matched the fixed manifest. Subsequent closeout changes are reporting and
  execution-artifact cleanup only; executable inputs remain unchanged.

## Passing local proof

Logs below are host-local code locators under
`.git/codex/stage9-delivery/8fc8c5f/`, not links expected in a clean CI clone.
Every Cargo resolver/build/test invocation used the locked graph; CPU-heavy
commands ran serially through the existing validation lock.

| Claim | Effective command and result |
| --- | --- |
| Every supported output | `ALLOW_FULL=1 ALLOW_HEAVY=1 make -j1 template-init-check`: public/native exit **0**, 2179 s, `matrix-11.log`. All sixteen outputs were initialized and fixture-committed, then each ran actual `make build` and `ALLOW_FULL=1 ALLOW_HEAVY=1 make check`. |
| Source safety and sync | The same public run passed manifest purity (51 owners), initializer/refusal/replay safety and full/instructions-only composition canary. Local skills/settings and unrelated bytes were preserved; successful full adoption/parity and independent refusal reasons were exercised. |
| Ordinary source scope | `make build test lint` passed in `workspace-repair-01.log`; unchanged scoped Rust proof was retained. Formatting/unused-dependency and affected routing repairs have scoped receipts. `make -j1 check-instructions docs-check shellcheck actionlint zizmor` passed in `repaired-source-static-01.log` (8 s): 28 skills, 5 roles, zero link errors; Zizmor ran offline. Tool-pin and Dockerfile checks passed in `delivery-static-01.log`. |
| Real PostgreSQL | In the retained generated fixture, `ALLOW_HEAVY=1 REQUIRE_DOCKER=1 make test-integration-db` passed all fifteen PostgreSQL tests and retained utility tests (62 s), `retained-db-01.log`. |
| Image and live lifecycle | In that fixture, `ALLOW_HEAVY=1 make runtime-image-build RUNTIME_IMAGE=stage9-postgres-core:local` passed (233 s). `ALLOW_HEAVY=1 REQUIRE_DOCKER=1 make migration-validate RUNTIME_IMAGE=stage9-postgres-core:local RUNTIME_EXPECTED_COMMIT=a0785894c79c9c3189b77cac2d3efffcd8d256e1` passed (19 s): current empty embedded set and replay both `no_change`, expected commit/version observed, PostgreSQL pool open, ready, clean stop in 15 s within 45 s. Logs: `retained-image-build-01.log`, `retained-image-runtime-01.log`. |

Matrix source snapshot: `c21f6a72618f90b37d309ada390fbc82e409598c`.
Receipt: `.git/codex/template-init/attempt.7N4AgS`; per-command logs:
`.git/codex/template-init/attempt-logs.OsrEaq/`. The receipt records each choice,
command, terminal result, output revision and log hash. Supported choices were
`none`/`postgres` × `core`, `codex`, `claude`, `qwen`, `cursor`, `grok`,
`opencode`, `all`. Installed Cargo was on PATH; existing `target/` and
`.git/tools/` caches were explicitly reused without deleting them.

Database/image fixture commit: `a0785894c79c9c3189b77cac2d3efffcd8d256e1`.
Image ID: `sha256:b8b19f79ab2adde901eed0bd8e42f88265eb6c700a257f3e17276a8e4a2c5f48`.
The final matrix's PostgreSQL/core output is
`3bee44c7c9f053392efb93a7275b195bb60d7209`. `retained-runtime-equivalence.json`
records 82 identical Git blobs/modes covering Rust/Cargo/tests/config/OpenAPI,
migrations, Dockerfile/toolchain and DB/runtime proof scripts. The build-helper
version-extraction repair separately covers both Cargo package-ID forms; the
exercised renamed-package form produces the same package/version in both
helpers. Reused results retain their original fixture identity.

Earlier failed attempts are not acceptance receipts. Matrix 10 passed its cells
but failed its outer command after a live Bash entry file was edited; matrix 11
reran the complete public command with its runner immutable until exit.
`matrix-10-result.md` preserves that scheduling lesson. Metrics record baseline
HEAD, so patched identity comes from the explicit manifests and receipts above.

## First dispatched ledger

Root bound `LEDGER_ORCHESTRATOR` and dispatched fresh Acceptance-Unit Lead
`/root/stage9_t1` using gpt-6-astra/xhigh. Dispatch packet SHA256:
`a7136e46a6ef9b3a6e6a79b3fa4a48441c1cbd9b6a54ddc66e4811852b328e96`.
The five descendants (`engine`, `profile`, `harness`, `proof`, `sync`) joined
before the Lead returned **Implemented** candidate
`8fc8c5f57b7b8532e84f1d694deff2eaaed408ef737ff3a308c48c8ed23a1223`
(70 paths; archive SHA256
`138a797d52759f5ab70a5d94fff7666a3f07a083cb678d208c097d2910df6851`).
Root verified custody and integrated the bytes serially in the same checkout,
then assigned this Lead the single final validation boundary. These observed
native dispatch/return/integration events close the carried harness exercise;
they are separate from behavioral proof. Root recorded local Completion
Accepted, then removed `tasks.md`, `tasks/T1-derived-service-lifecycle.md` and
the empty `tasks/` directory under
[Cleanup](../../docs/spec-first-workflow/shared/cleanup.md).

## Delivery boundary

At local acceptance on 2026-09-20, no staging, commit, push, merge, publication,
deployment or stage-10 work had occurred. Local acceptance established neither
landed source nor remote CI.

Delivery was authorized on 2026-09-22 through a pull request. Before preparing
that change, all retained executable inputs and modes matched the fixed
72-path candidate above; differences were the roadmap closeout and the removed
execution task. The retained specification/design/research and this completion
record accompany the implementation. A delivery-preparation repair disables
bytecode output in `check-skills.py`: importing the profile helper had created
an untracked `__pycache__` file that made the next `make plan` fail closed.
The instruction check and subsequent planning are rerun for this delta; remote
CI validates the assembled delivery candidate. The first remote run also exposed
a sync-canary fixture commit that depended on global Git identity. Path-scoped
fixture commits now supply their own test identity; the canary is rerun with
global/system Git configuration and ambient author/committer identity disabled.

[The roadmap](../../docs/roadmap.md#stage-9-template-initializer-profiles-and-sync)
closes stage 9 only when the delivery pull request lands on `main` after its
exact head passes `required` and `codeql-required`, including the selected
initializer matrix and sync suites. GitHub retains the pull request, check runs
and merge identity as remote delivery evidence. Publication, deployment and
stage-10 work are outside this delivery. Reopen only an invalidated accepted
decision or a newly requested outcome.
