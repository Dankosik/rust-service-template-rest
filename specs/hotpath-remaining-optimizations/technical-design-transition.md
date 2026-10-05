# Technical Design transition

status: ready

owner: Technical Design (System / Integration Design and Rust Code / Ownership Design)

result: [Technical Design](design.md), Git blob
`841d53b5fe557866329924ff7703c0369b939f0f`.

review: [Independent Technical Design Review and ownership panel](technical-design-review.md),
all PASS, no findings; Git blob `f6ce6d8714e59c824c0f89ac4cdf92a37800f0ec`.
The fixed reviewed design was blob `3704053dcfc2512c2137179c12401682300e55f3`;
only its ready status and receipt link changed after review. The semantic
decisions, accepted inputs, ownership and risk/proof boundaries are unchanged.

movement_evidence: Every required area has a concrete mechanism or a bounded
rejection with causal evidence and a required final disposition. Payload uses
existing streaming Base64/Serde APIs; spans preserve static values and evaluate
name formatting separately; metrics reuse fixed typed numeric status strings.
SQL candidates are explicitly rejected on duplicate/wake/transaction/driver
constraints, including an independently examined gate scheduling counterexample;
SQL remains an evidence-bearing area with required protocol-count observation,
not a claimed speedup. Pool sizing has actual-budget inputs, bounded candidates
4/8/16, worker/dedicated/overlap accounting, and a measured synthetic selection
rule. Exact Rust file owners, unchanged public/generated boundaries, adoption
and rollback meaning, and proof owners are fixed. Planning can order the
implementation and measurements without inventing mechanism or permission.

reopen_owner: none

next_owner: Planning, through a fresh phase actor selected by root.

## Authoritative inputs and custody

[Intent](intent.md) remains blob `8d80c7be65b1aa16df3770de30b671052ab6b04b`;
[Specification](spec.md) remains blob `82c39b3adfa32073c719dc70513c39b171b2dea8`.
Definition is not reopened. Root reported immutable baseline source custody
on the approved host at `/root/remaining/evidence/baseline-source.tar.gz`,
SHA-256 `aae64f3624831b7ef5a60491810d812a3034511f346b7811de2b88d6b0c0e2e7`.
Source and Cargo.lock identities matched the design. This archive receipt is
root-owned operational evidence, not a new performance measurement by this
phase actor. [Operations](operations.md) owns the complete current machine,
cost, execution and cleanup record.

The approved host is droplet `606085569`, `46.101.222.57`; the eight-hour
limit ends `2026-10-05T01:57:04Z`. Root alone owns SSH, provisioning and all
execution. Builds/tests/services/database/load/hotpath and executable result
reduction or docs checks remain confined there, serial with CPU-heavy work.
No new authority is granted by this transition.

## Open execution evidence and next action

Planning retains the independent candidate dispositions, shared final
validation and original-baseline assembled comparison. Implementation chooses
concrete test cases and commands under the existing validation owners. Net
allocation/performance gains, ordinary-release non-regression, required real
PostgreSQL behavior proof, actual connection budget/topology, full protocol
counts and final independent delivery review are not established yet. Root
is preparing baseline ordinary/profile binaries and will provide the synthetic
budget readback and warmed low-rate protocol evidence when ready.

The SQL no-supported-optimization result becomes final only after its bounded
required evidence closes; an unavailable trace remains blocked, and an
unexplained removable exchange reopens the SQL design. A payload growth or
telemetry-parity failure follows its existing design reopen/rejection rule.
No source/code optimization has been accepted by this phase transition.

This actor wrote only `design.md`, `technical-design-review.md` and this
transition. It performed source/document/Git inspection, primary-library
research and fresh read-only specialist/review delegation. It ran no local
build, test, service, database, benchmark, script-based reduction or executable
docs check and made no production/test/infrastructure change. Static review
is complete; executable docs validation joins root's remote final-validation
boundary. All descendants returned and their bounded results were consumed.

Workflow authority read in the current checkout: [router](../../docs/spec-first-workflow.md),
[System / Integration Design](../../docs/spec-first-workflow/phases/system-integration-design.md),
[Rust Code / Ownership Design](../../docs/spec-first-workflow/phases/rust-code-ownership-design.md),
[Review](../../docs/spec-first-workflow/shared/review.md),
[Transition](../../docs/spec-first-workflow/shared/transition.md),
[Agent Harness](../../docs/agent-harness.md) and its [Codex adapter](../../docs/agent-harness/codex.md).
The design applied matching Rust performance, SQLx, observability, ownership,
structural, security, reliability and Tokio constraints, without adding phase
or per-task proof gates.

Stop this actor here, before Planning. Root continues the accepted outcome;
missing external evidence returns to root, changed requester meaning to
Definition, and mechanism/ownership conflicts to Technical Design.
