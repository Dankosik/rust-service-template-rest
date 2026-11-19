# Evidence Contract

Evidence semantics and exceptions for local completion, verification, and
external readiness claims. Load for claim scope, reuse, additional required
proof, external readiness, or unavailable infrastructure; ordinary local work
uses [AGENTS.md](../../../AGENTS.md#validation-budget) without loading this file.

## Local Completion

The ordinary local criterion is owned by [AGENTS.md](../../../AGENTS.md#validation-budget).
It establishes sufficient local acceptance, not observation of every production
path. For mixed or specialized proof, [Validation Routing](../../validation-routing.md)
selects existing commands.

Unit tests must meaningfully check changed behavior in the existing style and
actually execute or have valid reusable results. Written tests alone, a selector
matching none of the required tests, skipped required cases, and execution
errors are not passes. Build affected deliverable entry points and required
variants, not every template profile. Reuse adequate existing coverage; do not
invent an exhaustive scenario matrix or re-prove unchanged behavior without a
relevant dependency change or concrete defect.

Once the local criterion and explicit local additions pass, record the results
and finish local verification. Report unrun optional scenarios only when they
materially limit a claim. Requested CI, production observations, deployments,
and releases need their own evidence and applicable authority.

## Required And Optional Proof

A product behavior requirement, a possible test method, a local acceptance
criterion, and a CI/release gate are different things. Additional mandatory
verification must trace to an explicit requirement of the accepted task or an
existing applicable external gate. That gate applies to its own action; it does
not require a duplicate local run. A file path, generic risk label, skill,
reviewer preference, selected command, or agent-authored packet does not expand
local acceptance. Loading a domain method supplies expertise, not new gates.

Preserve genuine user-established requirements, including explicitly requested
migration execution, runtime observation, green CI, or release.

Do not create temporary applications, smoke runners, compose stacks, remote
environments, provider sandboxes, permanent workflows, or validation matrices
solely to establish local completion. Small unit fixtures, existing test doubles,
and in-process router or service tests remain normal test authoring. Use existing suitable
tools for a concrete diagnostic question; do not promote the diagnostic into a
new completion requirement. New verification infrastructure needs an explicitly
accepted task requirement, not a broad security, integrity, or concurrency label.

If an optional check lacks Docker, a provider, access, or usable setup, record
only the material unverified scope and stop that path. Do not repair or provision
its environment, increase privileges, or enable heavy modes to remove the gap.
If a required build, unit test, or explicitly required verification cannot run,
report the exact missing input and incomplete scope truthfully. Continue useful
authorized work, but do not turn environment recovery into an unbounded project.

A reproducible or mechanically supported in-scope defect must be repaired,
including one found by an optional check. A missing optional observation,
speculative risk, style preference, or imaginable extra scenario is not such a
defect. Unrelated historical defects remain observations unless the accepted
outcome spans them. Never hide a real defect behind passing unit tests.

## Claim Scope

For a specific runtime or verification claim, evidence must be current, match
its scope, and reject the claimed wrong behavior or missing wiring. A unit-test
result establishes its tested boundary, not a live provider or production path.
Status, file presence, proposed commands, and implementation summaries alone do
not establish observed behavior. Report only what was exercised; accepting
local development does not require widening that empirical claim.

Before adopting a hard constraint, trace its source, scope, and units. Distinguish
requester policy, observed runtime settings, diagnostic limits, and agent
assumptions. Prior artifact acceptance does not establish that attribution.
Preserve actual safety and authority requirements.

## Design Proof

Specification closes behavior and its highest-boundary falsifier; Technical
Design supports the mechanism, invariants, and feasible proof. A proposed live
falsifier is not automatically a mandatory live acceptance scenario.
Evidence needed to resolve an actual mechanism choice remains with Technical
Design; do not claim measured existing behavior from design rationale alone.

## Execution Evidence

Attach commit or tree identity only across a checkout or integration boundary;
the current bounded diff is enough for local work. Retain actual commands,
inputs, environment, result, and exercised scope, using [Evidence Result
V1](#evidence-result-v1) for final verification claims.
[Build In Tight Vertical Slices](../phases/implementation/unit.md#2-build-in-tight-vertical-slices)
owns coding feedback; [Final Validation](../phases/implementation/final-validation.md)
owns the single assembled boundary.

Reuse passing scoped evidence when covered code, relevant dependencies, command,
inputs, and environment are unchanged. A new commit, unrelated documentation
edit, task handoff, or phase transition alone does not invalidate it. Check the
relevant delta and retain original candidate and scope; never relabel an old
result as a new run. When relevant equivalence is uncertain, rerun that affected
check, not unrelated proof.

The Delivery Owner selects one non-overlapping plan for the local criterion and
explicit additions. One result may support several claims. Running full
`make check` is not an automatic completion gate.
When expanded verification is explicitly required, select its matching canonical
leaves or aggregate without duplicating covered checks. Heavy execution still
needs its existing authorization and guards.

After a failed or interrupted run, retain its plan and completed scoped results.
Reconcile against the repair, running failed, invalidated, and not-yet-run
required checks only. Do not repeat the same failed attempt without changed
code, environment, or a discriminating diagnostic hypothesis. A focused repair
does not restart the whole plan, review, or task ledger.

Valid consolidated scoped evidence can establish local verification without an
aggregate receipt. An explicitly required exact-candidate aggregate, CI check,
fresh runtime observation, or release gate still needs its actual result.
Whole-candidate receipts retain the tool's exact base, candidate, plan, input,
and environment rules under Validation Routing; do not synthesize them from
partial results. The Orchestrator records acceptance without repeating proof.

When required final proof is unavailable, report `implementation complete; verification incomplete`
for that scope. Report material optional gaps as limitations. Carry verified
results into the [Completion Result](../interfaces/acceptance-result-v1.md#completion-result)
when reporting the verdict for the requested scope.

## Evidence Result V1

Return one record per claim:

```text
claim: <assertion>
candidate: <commit/tree/bounded diff identity>
scope: <package/files/behavior>
command: <exact command>
inputs: <changed paths or stable input identity>
environment: <relevant tags/services/toolchain>
result: pass | fail | blocked
duration: <wall time>
status: verified | partially_verified | not_verified
gap_or_next_owner: <none, unverified remainder, or owner>
```

A summary inherits the weakest status among the claims it covers. When required
proof cannot run, record the intended proof, reason, narrower evidence, and
unverified remainder.

[Execution Evidence](#execution-evidence) owns reuse eligibility for scoped
results and whole-candidate receipts. Keep a reused record's original candidate
and exercised scope. The Lead validates its applicability to the current claim;
a reviewer reuses valid evidence or runs a missing or distinct adversarial
falsifier.
