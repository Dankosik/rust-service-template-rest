# Planning review and movement evidence

Adapter: [Task Review / Readiness](../../docs/spec-first-workflow/phases/task-review-readiness.md)
under shared [Review](../../docs/spec-first-workflow/shared/review.md).

- Checkout: `main` at `ac6a6ffe248cf6dac5232438876c564030316ed1`, equal to
  `origin/main`. The only working-tree change is this untracked bundle.
- Ready inputs, unchanged from their transitions: `intent.md`,
  `research/synthesis.md`, `spec.md`, `design/system.md`,
  `design/ownership.md`, `design/ownership-review.md`,
  `technical-design-review.md`, `technical-design-transition.md`,
  `definition-review.md`, and `definition-transition.md`. The owner and the
  reviewer both verified all ten hashes.

Final result: **CONCERNS**, reached after one bounded delta recheck by the
same reviewer. That recheck's two non-blocking findings are discharged below
by the Planning owner, using exactly the smallest repairs the reviewer named.
Movement is permitted.

The reviewer, `ada0e5da5cb793ca8`, was a native `reviewer-agent` carrier
selected with the Opus model field, with fresh history and a read-only
boundary. It received:

- the fixed hashes;
- the adapter and the Planning owners;
- the ready inputs;
- the coordinator's facts: local-only authority, the two local heavy runs,
  custody under `.git/claude/http-idempotency/`, the carrier, and the machine;
- the stage-10.2 plan as evidence only;
- the rule that no network request may carry the user's email address or any
  other personal data.

## Planning author check

Before expanding the packet, the owner applied the
[atomicity gate](../../docs/spec-first-workflow/phases/task-review-readiness.md#atomicity-gate)
as a self-check:

- **One Outcome.** The Specification names one deliverable, the selectable
  profile, with properties of both its selected and its default output. No
  layer can be consumed or accepted alone:
  - The initializer refuses markers missing from the inventory
    (`_apply_markers` in `scripts/lib/template_init.py`), so the runtime pack
    without the profile machinery breaks every initialization.
  - An unmarked pack would be retained in `DATABASE=none` graphs, which lack
    `infra-postgres`, so they would not build.
  - The machinery without the pack selects an empty profile.
- **No different gate.** No part waits on an external effect. The request
  stops before push, and every check is local final validation.
- **Conditional branches not triggered.** Design probes 1–7 (system design
  section 14) retired the integration uncertainties. No mechanical contract
  fans out beyond one bounded, valid task.

Readiness dry run: a written walk of T1 from dispatch through final
validation against current sources, without launching tests or services.

- **Consumed code exists as the design states:**
  - `infra_postgres::{in_tx, in_tx_with, TxOptions, Isolation, retryable}`
    and the two commit-error variants;
  - `infra_http::{protect, VerifiedPrincipal}` and `RequestDeadline::at`,
    whose `request-budget` marker is always selected with an authentication
    engine;
  - `infra_bearerauthn::Verifier::disabled` and the introspection
    `test_support` fixture;
  - `config::app::occupied_string`.
- **The anchors hold:**
  - `prepare_auth` returns a verifier that `serve` discards;
  - the contract is built after `readiness.refresh`;
  - `api.rs:157` sits inside the authentication test-route region.
- **The carriers need nothing new:**
  - `test-integration-db.sh` forwards `--test` arguments;
  - `crates/config` already depends on `humantime` and `humantime-serde`;
  - the workspace `members` glob admits the new crate;
  - `.dockerignore` admits `crates/` and `migrations/`.
- **The non-obvious surfaces are named in the packet:**
  - the generated contract, which the initializer regenerates per selection;
  - the candidate allowlist, since the runner copies tracked files plus the
    allowlist only;
  - the `integration` build variant.
- **Environment at planning time:**
  - Docker 29.4.0 was reachable;
  - 104 GiB of disk was free, and `target/` held 3.6 GiB;
  - `go` and `npx` were present;
  - the pinned `cargo-deny`, `cargo-shear`, and `zizmor` builds existed;
  - `d24d173` is a local ancestor of `HEAD`.

  These are observations, not gates.

The owner then fixed one defect before the first candidate. The mutable
owners had named several locks without a writer (the manifests, the
inventory file, and the generated contract), and two READMEs sat under two
owners. Each lock now has exactly one writer, and the owners are pairwise
disjoint.

## Review of c1

```text
candidate: specs/http-idempotency/tasks.md
  d5c8136ccf0b95befe3473c4acc541a593006246054ce7f182e52adf5753d5ec;
  specs/http-idempotency/tasks/T1-http-idempotency-profile.md
  04fa1a7096d83b59d8ce54f75dbb45088d961883ed66d243766494d8034ebdcc
  (verified before and after)
verdict: CONCERNS
findings: F1 (non-blocking; repair owner Planning)
evidence_boundary: read-only written walkthrough against the Planning owners,
  AGENTS.md, the Evidence Contract, Implementation and Implementation Review,
  the Claude Code adapter and carriers, the ready inputs, and current sources
  (the make files, verify.sh, changed-surfaces.sh, template-init-check.sh and
  its allowlist, template_init.py and template_state.py,
  test-integration-db.sh, ci.yml, .redocly.yaml, .gitleaks.toml, api.rs,
  bootstrap/mod.rs, infra-http, infra-postgres, test/). No make target,
  build, test, container, subagent, or network use.
reopen_owner: none
```

**F1: lint, formatting, and zizmor were mislabeled "CI-owned".** AGENTS.md
defines CI-owned as the steps `make verify` marks CI-owned. `verify.sh`'s
`ci_owned()` marks only heavy steps and the matrix. `make fmt-check`,
`make lint`, and `make zizmor` are local route steps, so the plan misstated
their status. The actionlint/zizmor asymmetry followed from the same label.
The finding was non-blocking: not running these steps is allowed, but
because push is excluded, clippy, rustfmt, and zizmor would never have run
before handover.

The coordinator sent evidence to the Planning owner while the review was
running: AGENTS.md routes a mixed-surface change through `make plan`, and
`verify.sh` runs `fmt-check` and `lint` locally. The owner forwarded it to
the reviewer as evidence, not as a directive, without changing the
candidate. The reviewer tested it independently and anchored F1 on it.

Attempted falsifiers, from the reviewer's table:

| Falsifier | Result |
| --- | --- |
| Atomicity: more than one outcome, or a layer that needs planned companions | No defect; one task, as in the 10.2 precedent |
| A hidden decision or unavailable input for the next action | No defect; Docker and `d24d173` are gated at final acceptance only |
| The packet's claims about current code | All hold |
| A changed surface without an owner | None found |
| New files silently left out of the matrix | Covered; all six new paths are named |
| Heavy runs once with one shared target | Holds with an absolute `CARGO_TARGET_DIR` |
| The one-shot `none` equality | Feasible with the runner's identity values; the longest service name is 56 characters |
| The four unreferenced components against `openapi-check` | Pass; Redocly only warns |
| Links in the bundle | All resolve |
| Custody across restarts | Consistent; bundle Cleanup leaves `.git/claude/` intact |
| The three judgments the brief named | Lint, formatting, and zizmor mislabeled (F1); `unused-deps` and `actionlint` traced; roadmap bookkeeping consistent |

## Repair: c2

The owner judged the evidence and chose to run the route, not only to correct
the label. AGENTS.md's validation budget gives a mixed-surface change the
route that `make plan` prints, and the review left the choice to Planning.
The repair changed four hunks:

- the ledger's Completion wording;
- the packet's Checks, which now name the route's local steps and keep the
  heavy runs once;
- a replacement for the "CI-owned" bullet;
- one observable line.

Delta: SHA-256 `842cfe56dffa3bd66df57d1ea9dd07fb4faccd90d3e07b02c6ef95102c3b5f89`.
The repair left the Outcome, Boundary, owners, locks, custody, final review,
and reopen conditions unchanged, so it qualified for a bounded delta recheck.

## Bounded delta recheck of c2

```text
candidate: specs/http-idempotency/tasks.md
  6736cfa32db5726cc7c7d51a037c99dcd3accd07d55404c1f9dc14fea0d06574;
  specs/http-idempotency/tasks/T1-http-idempotency-profile.md
  96bc00d6917c267cefcc506f87d685e9431523c074b3e812e327ea37e9a67172
  (verified before and after; the supplied delta matches a direct c1-to-c2
  diff)
verdict: CONCERNS
findings: F1 closed; F2 and F3 (non-blocking, introduced by the repair;
  repair owner Planning)
evidence_boundary: read-only recheck of F1 and the reasoning the four hunks
  affect: verify.sh (routing, step order, run loop), changed-surfaces.sh,
  git-changed-paths.sh, ci.yml, and one non-mutating
  `scripts/ci/verify.sh --plan --files` call over representative final paths.
  No make target, build, test, container, subagent, or network use.
reopen_owner: none
```

- **F2: the route list was incomplete.** The new migration also selects the
  runtime-image surface under the postgres profile. That adds the local
  `make dockerfile-check` and the CI-owned `make container-security`, and c2
  named neither. c2's own rule ("every local step runs") would then have
  disagreed with its list.
- **F3: the `ALLOW_FULL=1 make verify` option conflicted with three owners.**
  - Implementation requires cheap failures first and says one failure must
    not hide independent diagnostics; `verify.sh` runs the matrix before the
    Rust steps and stops at the first failure.
  - The Evidence Contract reruns only invalidated checks; a failed verify
    reruns every step, including the matrix.
  - The ownership map adds no other aggregate.

Attempted falsifiers that found no defect:

- the delta is the complete change;
- F1's label and asymmetry are gone;
- no step runs twice;
- the stated verify facts are accurate;
- the route requirement traces to AGENTS.md, not to an invented gate;
- both named additions are sourced;
- the links resolve.

## Owner discharge of F2 and F3: c3

The owner applied exactly the smallest repairs the reviewer named:

- **F2: corrected against the planner.** The owner ran the same read-only
  `scripts/ci/verify.sh --plan --files` over the planned final path list. It
  printed 18 local steps, including `make dockerfile-check`, and 5 CI-owned
  steps: `make template-init-check`, `make test-integration-db`, the runtime
  image build, `make migration-validate`, and `make container-security`. The
  packet now lists them and states that `make plan`'s output for the fixed
  candidate is authoritative. `make container-security` joins the CI-owned
  image steps under "Not run locally".
- **F3: the `make verify` option is withdrawn**, the alternative the reviewer
  offered. Every command runs once, one by one, cheap and CPU steps before the
  heavy runs, with one result record each. This restores c1's cheap-first rule
  for all execution, and leaves no aggregate receipt and no frozen ledger.

The delta is packet-only, four hunks, SHA-256
`973430f88d35ecab64d2c16083e09be8d2671ca68db14df6730a19eae2701a7b`; `tasks.md`
did not change. The discharge narrows the plan, adds no behavior, owner,
interface, or risk, and changes nothing the reviewer did not anchor. No
further review cycle runs, for three reasons:

- CONCERNS permits movement;
- the one bounded delta recheck has been used;
- a fresh reviewer is reserved for a recheck that still fails.

The Definition owner discharged C1 the same way in
[definition-review.md](definition-review.md#owner-discharge-of-c1-and-the-lifecycle-edit).

## Lifecycle edit and final identities

After the discharge, the owner changed only the ledger's `status` line from
`draft` to `ready`. Under Transition's unchanged-semantic-scope rule, the
CONCERNS verdict with F1–F3 closed applies to these identities (SHA-256):

- `tasks.md`: `2888849b1f788e43d360ec2093df751d63c254a26b949e5519bc344cc4a4e741`
- `tasks/T1-http-idempotency-profile.md`: `17f39541cd98f97513f6e7c97a8250f55926a569ee335f5b9e21cfb6a48cc47d`

`make docs-check` passed on every candidate (c1, c2, and c3; 768 links, 0
errors). It is a static link check, not implementation evidence. Planning
edited only this bundle. It ran `make docs-check` and the read-only plan mode
of `verify.sh`, and nothing was built, tested, or started. No
implementation, runtime, database, initializer, CI, or deployment result is
claimed.
