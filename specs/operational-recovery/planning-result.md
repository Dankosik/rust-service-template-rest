# Planning result: operational recovery

```text
status: ready
owner: Planning
result: tasks.md and tasks/T1-process-progress.md through tasks/T4-build-context.md
review: planning-review.md; fresh independent Task Review / Readiness PASS, no findings
movement_evidence: closed inputs, four atomic outcomes, serial shared owners, one Completion owner; fresh independent review PASS
reopen_owner: none
next_owner: Implementation through existing continuation bound as LEDGER_ORCHESTRATOR
```

## Candidate and accepted inputs

Worktree: `/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-operational-recovery-20261006`.
Branch: `codex/operational-recovery-20261006`.
Source/instruction baseline: `699887b18594088a59bcc23a049d290d089f6da1`.
Planning writes only this result, [ledger](tasks.md), its four packets and the
phase review receipt. Source and upstream artifacts remain unchanged/uncommitted.

| Accepted input | SHA-256 |
| --- | --- |
| [Definition result](definition-result.md) | `c42d9709b372382ece6301ea3e724c4a0114320528cdeb4aae449430ccdc88f2` |
| [Technical Design result](technical-design-result.md) | `ca7b672a6e6516106b2f45bb66f5b405841551ed41f18ef409bb97c7a07b212f` |
| [System Design](design/system.md) | `574833fa0513e68bb3f532d4c751f6df38ffe7a1fa863df181bc269a8308c48f` |
| [Execution Design](design/execution.md) | `268178dd4d7235fc97b4169617c5ccaabe4a4420f24fbd1854fbb595f5a4be1f` |
| [Ownership Map](design/ownership.md) | `779b7db3931df36ead549bf6e6ab3b27b329d6f798b4657961b3b4993587320e` |

Specification/intent/research and Design review hashes remain those retained in
these accepted results. Current exact readback matches their recorded semantic
candidate; no upstream reopening was needed.

## Review and current plan identity

Fresh [Task Review / Readiness](planning-review.md) returned PASS with no
findings. The reviewed candidate was fixed by the manifest in that receipt.
After review, only the ledger draft-to-ready status and this result's status,
review/transition locators and current identity summary changed. No task packet,
Outcome, dependency, mechanism, writable owner or proof boundary changed;
shared Transition retains the verdict for that unchanged semantic scope.

| Current plan artifact | SHA-256 |
| --- | --- |
| [tasks.md](tasks.md) | `c3606c04eb41085647464df98f09fe2112b4fa8b5c4a4ee2c2f24f65fbd2efb5` |
| [tasks/T1-process-progress.md](tasks/T1-process-progress.md) | `340b86559d7bf652737d31d53c0deb87d82238079647553c1f6f24cf9cd48d46` |
| [tasks/T2-consumer-recovery.md](tasks/T2-consumer-recovery.md) | `603772e0848225e824e2276841fe58a914f29e7a6601e9e4f888136ebbe1a6c7` |
| [tasks/T3-validation-custody.md](tasks/T3-validation-custody.md) | `aa9f0638ced9fd71cfb5b73aa7e2eedc33e1690f62e3ab3b7d8379da8036a231` |
| [tasks/T4-build-context.md](tasks/T4-build-context.md) | `560ef551d38ba660df0c13e45b4f197989741de34a6088a137400e091626d907` |
| [planning-review.md](planning-review.md) | `22a1c07de3a1ff65ffc04ff26e6b33dc7c3b72589f717cbd4412b9bc57c1bcec` |

## Atomicity and coverage decision

Four independently consumable outcomes justify a persisted ledger. A single
packet would combine runtime policy, existing-behavior consumer evidence and
developer execution behavior. Splitting H, S and W would instead leave a new
health capability without complete process policy. Splitting L from D would
leave generation custody unable to know its existing external work ended.
Splitting B from V would leave newly selected cache/output inputs outside
compatible receipt identity. Keep each coupled set inside its outcome; useful
implementation lanes do not create acceptance units.

| Obligation | Plan disposition | Independent acceptable result |
| --- | --- | --- |
| R1 and root part of R3; H/S/W | T1 | Both real roots consume serialized health progress failure with current cleanup and diagnostics-independent policy. |
| R2 and consumer/topology part of R3; C | T2 | Existing real-database runner proves actual useful-work recovery and local isolation through retained consumers. It uses current adapters without consuming T1's new arming API. |
| R4 safe admission/cancellation/waiting; L/D | T3 | Current validation commands retain generation and finite external-work custody through their own terminal owners, usable in ordinary uncached mode. |
| R4 cache/resources/reuse; B/V | T4 after T3 | Build context and sole verifier receipt writer agree on effective inputs and resource outcomes. |
| P portability/classifier integration | Named deltas in T2/T3/T4 | Canonical metadata precedes derived projections; shared owner is serial. No separate half-implemented projection task. |
| G runtime/persistence/#243 timing-topology | T1; consumer scope in T2 | Existing operator guidance follows actual runtime/provider contracts. |
| G build/command/validation guidance | T3 then T4 | Usage tracks the completed existing entrypoints and their limitations. |
| Prior #254 health/pool/logging and #255 iteration policy | Preserved; no reimplementation | Source-supported baseline disposition remains in Research and Specification. |
| New PR, #243 disposition, actual-head CI, independent source review | Completion | One final delivery owner establishes requested delivery; no artificial test/review ledger task. |

## Readiness walkthrough and execution custody

The next Leads can load their packets and accepted inputs directly in this
checkout. T1 can extend H and compose both roots inside one owner; T2 can use
existing adapters/runner and retained profile contracts; T3 can implement E1 and
its finite D callers without a missing architecture decision. Their final test
techniques and commands are normal Implementation work. T4 consumes integrated
T3 code, including incomplete-custody propagation, before extending the same
Make/verifier owners. Current contracts already close T4's design decisions.

The initial logical frontier is T1/T2/T3. Admit T1 and T2; T3 obtains shared
profile metadata after T2 joins, then T4 follows integrated T3. This bounded
serial-owner schedule prevents overlap without making test results or review
prerequisites for implementation. Root process tests belong to T1, database
consumer compositions to T2. The ledger records native identities at actual
dispatch and one replaceable implementation result per unit. No speculative
worktree, persisted wave, external environment or fixture setup was created.

Chosen carrier: existing continuation coordinator binds as
`LEDGER_ORCHESTRATOR` under the Codex adapter and becomes sole ledger writer.
Fresh general-purpose Acceptance-Unit Leads own the units; any disjoint workers
remain their implementation lanes. After all implemented code is assembled and
all writers join, one Operational-recovery Delivery Lead receives Completion.
That owner selects consolidated current validation, repairs, independent source
review and authorized commit/push/new PR/CI; root records the returned verdict.
The Planning actor does not retain implementation or acceptance ownership.

## Proof and authority boundary

Planning used artifact/current-owner reads and static relative-link/fragment/
whitespace consistency inspection. It ran no Rust build, test, runtime probe,
container, cache install, commit, push or remote mutation. The full repository
docs-check, ordinary build/tests, required runtime observations and actual-head
CI remain in assembled Completion. The task's baseline and plan bytes are not
implementation proof.

The accepted legacy-activation disposition remains explicit: candidate entrypoint
for task-owned admissions; read-only completion/absence check for already-admitted
legacy work; contested state prevents shared activation while isolated proof,
PR and CI continue with its precise local limitation. No global upgrades,
future-old-caller absence proof or unrelated runner termination is added.
Task-local verified sccache provisioning/native observation follows E2; no global
machine/cache mutation is authorized. Reviewable PR authority excludes main
merge, deployment/production reads and closure or force-push of #243.

Reopen only the smallest invalidated upstream mechanism, ownership or behavior;
otherwise preserve this plan and continue available implementation. A missing
implementation test case/command or a later optional environment is not a
Planning input gap. A genuinely missing required final result remains visible
in Completion without manufacturing success or stopping independent code.
