# Technical Design result: operational recovery

```text
status: ready
owner: Technical Design
result: design/system.md; design/execution.md; design/ownership.md
review: design/technical-review.md, repaired-candidate Review Result V1 PASS; design/ownership-review.md, three compatible PASS lenses
movement_evidence: R1-R4 mechanisms, exact ownership, alternative dispositions, required inputs and proving boundaries are closed; TD-1 repaired and fresh independent review passed; no user-owned input remains
reopen_owner: none
next_owner: Planning
```

## Authoritative candidate

Worktree: `/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-operational-recovery-20261006`.
Branch: `codex/operational-recovery-20261006`.
Source/instruction baseline: `699887b18594088a59bcc23a049d290d089f6da1`.
This phase's artifacts remain uncommitted. No source or Definition file changed.

| Artifact | SHA-256 |
| --- | --- |
| [System mechanism](design/system.md) | `574833fa0513e68bb3f532d4c751f6df38ffe7a1fa863df181bc269a8308c48f` |
| [Execution mechanism](design/execution.md) | `268178dd4d7235fc97b4169617c5ccaabe4a4420f24fbd1854fbb595f5a4be1f` |
| [Ownership Map V1](design/ownership.md) | `779b7db3931df36ead549bf6e6ab3b27b329d6f798b4657961b3b4993587320e` |
| [Rust Ownership panel](design/ownership-review.md) | `b03ddb94023d22dc93387cb2ec351ab4c36b2b2a2def1a867a5e9572136b7521` |
| [Technical Design Review](design/technical-review.md) | `06d6abb7d08a9fa05d671112c96f79e551185be4c0274a513d209240e6159bed` |

Accepted inputs remain unchanged:
[Definition result](definition-result.md)
`c42d9709b372382ece6301ea3e724c4a0114320528cdeb4aae449430ccdc88f2`;
[Intent](intent.md)
`4072afd2863e3138405eb669ff5d48374f052f92d6485fd945e83f818f9d6428`;
[Specification](spec.md)
`ced137a1a4d2b29311cbba94b02790d30077803eb6f9dbe9945ed25218c12ab7`;
[Research](research/baseline.md)
`178e2e3a45cee7fffd367a90033ecd0d625b07522fc72358719ab3d09909a638`.

## Decisions enabling Planning

1. Health's existing watch state serializes arm, completed publication, strict
   expiry and stop. Both roots arm after successful startup readiness admission,
   register an independent tracked observer, and preserve first primary failure
   through their current teardown. Late completion cannot erase an armed gap;
   generic unarmed health and ordinary failed/timed-out rounds remain recoverable.
   Defaults, transport bodies/codes and the current 18.5-second tail remain.
2. Existing real-PostgreSQL integration proof owns two separate instance
   compositions/pools and actual HTTP/gRPC useful work, Check/Watch and absent-
   diagnostics connection-cap recovery. This proves local state isolation,
   not two independent schedulers or fleet capacity. Actual service/worker
   process owners separately prove mandatory arm/stop/failure/grace behavior.
   No shipped business route, new fixture binary or test framework is selected.
3. A permanent sibling kernel guard serializes the current validation protocol;
   its admission file is temporary so normal release permits older clients to
   run. A pre-exec gate, generation identity, verified nesting and retained
   interrupted custody prevent unsafe stale/release/cancellation paths. Waiting
   is monotonic, visible and bounded; no FIFO daemon is introduced.
4. Existing Docker/Compose validation owners explicitly record pending work
   before detached admission and acknowledge scoped terminal evidence afterward.
   The same generation record holds these narrow tickets. Cleanup failures
   cannot disappear behind shell success, release admission or produce a pass.
   This closes review finding TD-1 without transferring cleanup authority.
5. Existing build/verifier paths gain opt-in command-scoped sccache 0.18.0,
   explicit verified task-local binary provisioning, a joined foreground server,
   resource diagnostics and compatible receipt identity. Missing or conflicting
   explicit cache mode refuses before expensive work. No global installation,
   Cargo/security setting, target sharing or cache deletion is selected.
6. Delivery uses a new PR. Its description names #254's already-adopted health
   bytes and stronger pool lifecycle, and identifies this PR as carrying #243's
   remaining timing/topology guidance plus the accepted operational delta.
   No force-push, closing #243, main merge or deployment is assumed.

The [ownership map](design/ownership.md) names exact source and script owners.
Planning may sequence implementation and validation without choosing requester
meaning, lifecycle authority, mechanism, dependency direction or evidence owner.
Tests, fixtures, assertions and focused commands remain Implementation work.

## Review and bounded repair

Three fresh Rust Ownership reviewers passed disjoint execution, boundaries and
cohesion lenses on one candidate. Their scope stayed unchanged by the later
script-only repair; the panel result records that exact identity refresh.

The initial fresh Technical Design reviewer found only TD-1: current Docker/
Compose cleanup does not supply the terminal acknowledgement E1 required. System
Design repaired the mechanism and source ownership. Because that repair introduces
the missing internal interface, a fresh reviewer inspected the corrected
candidate and returned PASS with no surviving findings. It verified all design
hashes before and after review and consumed the retained Rust panel.

Current review owner:
`/root/operational_continuation/technical_design/technical_review_repaired`.
Both Technical Design reviewers used fresh native `reviewer-agent` dispatch
with `gpt-6-astra` / `xhigh`; panel reviewers used `high`. The native tool accepted
these settings and reported running/completed lifecycle; it has no separate
effective-model readback field. Review evidence is source/static contract proof,
not an implementation, runtime, activation or CI result.

## Delivery activation and evidence boundary

The continuation owner explicitly accepted the distinction between current-
protocol safety and normal legacy interoperability. The new candidate does not
claim to repair a race between two unmodified old reclaimers. The final delivery
owner uses the candidate entrypoint for task-owned admissions and read-only
establishes completion/absence of already-admitted legacy work before shared-path
use. Contested/unreconciled legacy state prohibits shared activation; it neither
authorizes interrupting another runner nor requires upgrading every checkout.
Scoped implementation, isolated lock proof, PR and required CI continue while
that precise local limitation remains. No shared V2 path was activated here.

This phase performed CodeGraph/current-source inspection, official native-tool
contract inspection, static consistency and relative-link/fragment/whitespace
checking of its artifacts (PASS). It ran no build, test, runtime experiment,
container, compiler-cache installation, commit, push or remote mutation. The
complete repository docs-check and assembled source/CI validation remain with
the final delivery owner; the artifact check is not claimed as those commands.

Implementation retains typecheck-first iteration, worktree-owned target,
the existing Git-common validation path and serial CPU-heavy commands. Reuse
adequate built candidates and matching receipts; assembled required proof and
actual-head `required`/`codeql-required` CI remain necessary for delivery. Old
#254/#255 evidence does not prove this follow-up.

Reopen only the smallest owner when implementation exposes an infeasible state,
completion input, supported host/configuration or profile boundary. Changed
observable behavior goes to Specification; changed source/provider facts to
Research; placement refinement to Rust Ownership; mechanism/custody/activation
to System Design or its named delivery owner. There is no unresolved requester
decision and no reason to stop before Planning.
