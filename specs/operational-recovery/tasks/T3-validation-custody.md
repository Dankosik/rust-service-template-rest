# T3 — Safe validation generation and external-work custody

Outcome:
Replace unsafe PID/directory deletion and boolean nesting with current-protocol
exclusive generation admission whose existing owners retain command and named
Docker/Compose work until scoped terminal evidence, with bounded visible waiting.

Consumes:
- [R4](../spec.md#r4-delivery-and-evidence-economy),
  [E1](../design/execution.md#e1-kernel-exclusion-plus-generation-custody) and
  [Ownership L/D/P/V](../design/ownership.md) — accepted mechanism and finite callers.
- [Technical Design activation disposition](../technical-design-result.md#delivery-activation-and-evidence-boundary)
  — current-protocol safety and normal legacy interoperability are separate claims.

Provides:
- Existing lock entrypoint/helper, pre-exec gate, generation/nested/recovery
  custody, bounded owner/wait feedback, finite pending/terminal ticket seam,
  and all named existing external-work caller acknowledgements.
- Make/verifier/portable/classifier integration consumed by T4; intermediate
  uncached execution remains a complete valid validation-custody outcome.

Boundary:
L plus D is one outcome: admission cannot become safely consumable before all
existing escaping work owners acknowledge terminal state. Preserve the public
entrypoint, 900-second deadline/exit 75, immutable sibling guard and ephemeral
regular admission file. Interrupted unknown custody stays quarantined. Retain
owner-specific cleanup; no generic resource manager, new runner/daemon, PID-only
reclaim, global Docker action or interruption of unrelated work. Outside a
validation generation existing fixture owners retain normal scoped cleanup.
Cache/context mechanism remains T4; T3 includes only verifier propagation
necessary to prevent success/receipts when custody is incomplete.

Mutable owners:
- `scripts/ci/validation-lock.sh`, new `validation-lock.py`, existing lock
  self-test and `template-init-check.sh` verified nested admission.
- D's exact finite list in Ownership: `scripts/lib/compose-postgres.sh`,
  `scripts/ci/test-integration-{db,messaging,cache,object-storage,oauth}.sh`,
  `sqlx-prepare.sh`, `migration-validate.sh`, `runtime-image-{build,check}.sh`,
  `runtime-progress-proof.sh` (all script shorthand paths under `scripts/ci/`).
- `make/template.mk` current foreground Docker leaves and lock composition;
  `scripts/ci/verify.sh` incomplete-custody result/receipt refusal only, with
  existing applicable self-tests. Preserve workload inputs/oracles and CI gates.
- Existing template metadata/profile projection owner and `template-owned.paths`
  where necessary to retain helper/caller paths; `scripts/ci/changed-surfaces.sh`
  and its current self-test owner for validation-system classification.
- `docs/build-test-and-development-commands.md`, `docs/validation-routing.md`,
  `docs/validation/delivery.md` for lock/ticket/nesting/activation usage and limits.

Exclusive locks:
- L/D/Make/verifier command-custody contract, then released to dependent T4.
- Existing profile/projection/classifier bundle shared serially with T2/T4;
  wait only for that writer, with no per-task proof prerequisite.
- Shared CPU/execution admission where needed; isolated lock proof uses isolated
  task-owned paths and never activates contested Git-common state.

Final validation:
- Claim: Current callers exclude competing owners, including interruption,
  generation changes and verified nesting; queued cancelled/timed-out work never
  starts; pending external tickets prevent premature release/pass; legacy normal
  admission remains interoperable at its explicitly narrower boundary.
- Checks: Existing lock/script/verifier/classifier and selected CI owners under
  current delivery routing, once assembled. Implementation authors discriminating
  cases/commands; unchanged runtime oracles are retained without a new matrix.
- Observable: Token-specific custody and scoped terminal acknowledgements,
  bounded safe owner/wait diagnostics and truthful incomplete attempts. The final
  delivery owner owns shared activation; contested legacy state keeps its precise
  local limitation while isolated proof/PR/CI continue.

Reopen if:
A governed effect escapes the finite named terminal owners, native host/flock
contracts fail, or evidence invalidates generation/activation safety. Return to
System Design or its named activation owner. Do not convert unknown completion
into success or request a global upgrade.

## Implementation notes

Lead: `/root/operational_continuation/t3_validation_custody`.
The Lead owns L and joins the strict `daemon_owners` and
`entrypoint_integration` execution lanes before returning T3. Both original
implementations and the same-brief corrections below are now joined; source
ownership is released to T4.

Coding feedback assumption (one bounded scenario): on this macOS host, Python's
new-session gate plus Bash command substitution and a changed working directory
must preserve the generation's session/group identity for internal ticket calls.
T4's command-context integration consumes this same inherited-custody boundary;
discovering a host/launch incompatibility after that wiring would require
substantial rework. The discriminating scenario uses one isolated task-owned
admission path, begins and positively completes one synthetic no-effect ticket
through the actual entrypoint, then returns a known failure. Expected outcome:
exit 7 preserved, admission removed only after terminal custody, permanent guard
retained. It executes no Docker, Cargo, test suite, shared-path activation, or
runtime service. This is coding feedback, not T3 acceptance or final proof.

Observed coding-feedback result: the actual entrypoint, one `bash -eu -c`
payload changing into its temporary directory, command-substitution
`--ticket-begin make-docs-check fixture-no-effect`, matching
`--ticket-complete ... container-absent fixture-no-effect`, and `exit 7`
returned 7. Admission was absent, the permanent guard remained, and the retained
generation was `completed` with one terminal ticket. The isolated fixture was
removed after owned completion. The enclosing bounded Python probe exited 0;
no shared Git-common path was activated. This resolves only the named launch/
inheritance assumption. Later source additions were static self-test coverage,
safe diagnostics and verifier identity propagation, not new runtime observations.

Implemented boundary:

- The existing shell entrypoint delegates to the stdlib helper. Its permanent
  sibling flock guard serializes an exclusive temporary regular admission file,
  token/inode checks, generation metadata, pre-exec GO, verified nesting,
  cancellation and explicit scoped reconciliation. Unknown custody and legacy
  directories are never reclaimed by PID absence.
- Finite current Compose/container/build owners now begin before effects and
  acknowledge only scoped positive completion. Failed commands with terminal
  custody preserve their failure and release. Ambiguous build/cleanup results
  retain pending tickets and return incomplete custody; no builder adoption or
  global stop/prune was introduced.
- Make's foreground Docker leaves use unique scoped names. The verifier stages
  results until supervisor completion, refuses incomplete reuse, and requires
  custody protocol 2 for reuse. Its public candidate/plan identity is passed to
  lock diagnostics; direct commands hash HEAD plus dirty source. Nested verifies
  retain pending attempts without publishing a reusable receipt.
- Initializer nesting, portable helper copying, validation-system classification
  and existing delivery/command documentation were integrated. T1/T2 source and
  profile metadata were preserved; no additional metadata change was needed.

Test authorship follows the test-audit authoring gate at the existing lock and
verifier self-test owners. Missing baseline risks are boolean/forged nesting,
queued cancellation/timeout, current/legacy admission, stale reconciliation,
supervisor death, live group after direct-child exit, unknown metadata, pending
external effects, wrong-owner/idempotent acknowledgements, and receipt reuse or
publication before custody completion. Fixtures use actual process/session/pipe
boundaries and isolated paths; they add no production test toggle or runner.
The self-tests and baseline/control observations are authored but unexecuted.
Completion owns `make validation-lock-self-test`, `make verify-check` and the
existing classifier/projection/Docker/script routes, once assembled.

Actual static feedback: Python `ast.parse` accepted the helper and the changed
embedded script without importing or executing them; `bash -n` accepted all
changed shell sources and the execution lane's five extracted Make shell
bodies. No build, test suite, lint/ShellCheck,
aggregate verification, independent review, commit, push or remote mutation ran.
The previously reported disk limit remains a final-validation limitation, not
an implementation blocker.

Integration found and repaired one further causal defect before handoff: positive absence
readback alone cannot close a ticket when the initial Docker/Compose submission
was interrupted before a confirmed response/identity. Both existing-owner lanes
now retain that confirmation or native CID and leave ambiguous startup pending.
This preserves ordinary failed commands with positively terminal work while
refusing unknown daemon completion. It is same-scope coding repair; no runtime
gate or new runtime scenario was executed. The existing lock self-test now
contains real Compose-owner calls against controlled native outcomes: unknown
startup plus empty readback remains pending; confirmed startup plus ordinary
failed workload releases with the failure preserved. Runtime-progress reuses
its existing four native launch IDs and final driver disposition; early missing
identity, interruption and timeout remain unknown. Its Rust source, frozen
inputs and runtime oracles were not changed.

Both execution lanes returned native completed state after these repairs; no
descendant remains active. Final assembled proof and shared activation remain
with the Delivery owner.

```text
unit: T3
verdict: Implemented
candidate: baseline699887b18594088a59bcc23a049d290d089f6da1 plus the bounded 21-path source/docs diff and this packet; exact file-hash manifest returned to the Orchestrator
provides: L/D generation custody, finite external-work tickets, Make/verifier/initializer/projection/classifier integration and authored self-tests; unverified
next_owner: T4
```
