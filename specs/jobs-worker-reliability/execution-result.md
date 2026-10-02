# JW1 delivery state

Status: locally accepted; independent final review PASS; requested PR CI delivery remains outstanding.
No runtime acceptance or remote delivery is claimed yet.

Candidate: uncommitted worktree diff over
`67be869acea112af271ec8ba621cbc50ae9d36b7` on
`codex/jobs-worker-reliability-20261002`.
The ready Planning result SHA256 is
`8d0f79304f649460c35df54a2c1064dbefe5c66b7e579777848501c8387cd3a4`.

## Implemented outcome

All B1–B5/R1–R7 source, tests and canonical documentation are assembled.
The six disjoint writer lanes (attempt custody, maintenance, config projection,
worker CLI, operator tests and docs/profiles) are joined. No background writer
remains. Production SQL and CLI retain the reviewed public contract, full
attempt custody, retained failed work, exact recovery identity/history,
version fencing, bounded safe inspection and one union sample.

The two new migrations are
`20261002150001_add_background_job_recovery_history.sql` and
`20261002150002_index_failed_background_jobs.sql`. They are ordered above the
immutable base high-water mark; no old migration changed. Offline Cargo
metadata regeneration added only the already selected worker clap/serde_json
edges, without changing package versions. SQLx generation wrote eight entries
and removed two superseded entries.

The required generator exposed an existing macOS Bash 3.2 empty-array/nounset
failure. Its one-line conditional array expansion repair in
`scripts/ci/sqlx-prepare.sh` preserves both prepare/check modes.

## Current evidence

Every CPU-heavy command is serialized under the Git-common validation lock.
Coding feedback (not behavioral proof):

- `make sqlx-prepare`: passed against its disposable migrated PostgreSQL; both
  new migrations applied; workspace compilation took 2m03s; cleanup exited 0.
- `cargo check --locked -p service-config -p infra-jobs -p jobs-worker --tests`:
  passed, 27.14s.
- `cargo check --locked -p integration-tests --features integration --test jobs`:
  passed, 1m37s. The later two-line fixture repair is not behavioral proof.

Final local plan:

- `make fmt-check`: passed.
- `make migration-check`: append-only check and six migrate unit tests passed.
- `make deny`: advisories, licenses, bans and sources passed, with existing
  duplicate-version warnings.
- `make unused-deps`: passed; existing redundant rcgen ignore warning retained.
- `make secret-scan`: worktree and branch-range scans passed, no leaks.
- Pinned native ShellCheck 0.11.0 with `-x -- scripts/ci/sqlx-prepare.sh`: passed.
  The equivalent container target could not start after OrbStack stopped.
- `make docs-check`: passed after starting the existing OrbStack daemon;
  pinned Lychee reported 898 total, 379 unique, 771 OK, 0 errors. Log:
  Git-common `codex/jw1-evidence/docs-check.log`.
- `make build` and `make test`: passed sequentially under the Git-common lock,
  native session 42165 terminal exit 0, pinned Rust 1.98.1. Build took 34.00s;
  test compilation 1m24s. The log contains 816 passed, 0 failed, 1 existing
  ignored Go wire export fixture (CI supplies its Go-generated inputs) across 67 test summaries. Changed infra-jobs
  tests were 34/34 and shipped worker process tests 8/8. Log:
  Git-common `codex/jw1-evidence/local-rust.log`. The first attempt stopped
  before compilation because the shell PATH omitted installed Cargo; the
  successful attempt prepended `/Users/daniil/.cargo/bin` without changing
  toolchain or source. The previous temporary logs are unavailable and no
  result was inferred from them.

Selected heavy PostgreSQL/SQLx, provider, migration-image, profile initializer,
image and CodeQL gates belong to the ready PR's CI. They remain required for
completed remote delivery; no duplicate local matrix is added. No observed
query plan is claimed. Design's suggested EXPLAIN method is not substituted
with static reasoning or an ordinary suite pass.

## Final review and repair

One fresh independent Astra xhigh Implementation Review examined the assembled
candidate. It found one blocking fixture defect: failed-row metric fixtures
omitted `id` after the existing migration removed its default. The smallest
repair adds `id` and `gen_random_uuid()` to that INSERT in
`test/tests/jobs/process.rs`; no production behavior or reviewed interface
changed. The prior reviewer identity is unavailable after interruption.
No other surviving source finding was reported. Fresh independent reviewer
`/root/jobs_delivery_resume/final_review` (Astra xhigh) now examines the fixed
assembled delivery and fixture repair. The reviewer consumed terminal local results and returned PASS with no findings;
see `implementation-review.md`. Required CI results remain a separate obligation.

## Remaining delivery and cleanup

The selected local plan and independent review are complete.
Archive task provenance in a local commit. Canonical docs already own the
shipping behavior; apply Cleanup to the completed bundle and refresh only the
affected docs/identity evidence before the first push, so bundle deletion does
not create another CI run. Preserve evidence locators and archive commit.

Publish exactly one separate PR against main, attach it to the chat, and consume
actual exact-head `required`, `codeql-required`, and selected CI results. No
merge, deployment, account changes or live queue operations are authorized.
Keep the worktree for the root's final readback before workspace cleanup.


## Local Completion Result V1

```text
unit: Completion
verdict: Accepted (local scope only)
candidate: fixed assembled JW1 diff over 67be869acea112af271ec8ba621cbc50ae9d36b7
evidence: required local build/tests/docs and still-valid recorded static/source-generation checks passed
review: implementation-review.md, independent PASS with no findings
next_owner: delivery Lead; archive provenance, retire completed bundle, publish one ready PR and obtain selected CI
```

The overall requested PR delivery is not complete until selected CI returns
terminal results for the published head. No merge or live rollout is included.
