# Goal
status: done
Completion: Roadmap stage 10.2 is locally accepted: the default-off bounded outbound HTTP profile is usable when selected, absent when unselected, preserves auth policy and tracked DNS custody, and passes one consolidated final validation and required independent final delivery review. Canonical selection proof covers 96 projections and 12 runtime graphs. No remote delivery is included.
Global constraints: [Intent](intent.md), [Specification](spec.md), [system design](design/system.md), [ownership map](design/ownership.md), and [Planning Ledger Contract](../../docs/spec-first-workflow/phases/planning/ledger-contract.md) govern execution. Local authority only; preserve unrelated work. Tests are authored within the code task and executed at the assembled final-validation boundary. Do not multiply runtime builds by harness.

## Tasks

- [x] T1: A derived service can select a complete bounded public-HTTPS client profile with correct default removal and preserved auth behavior.
  - Depends on: none; ready Definition and Technical Design provide the consumed contracts.
  - Provides: complete outbound client, common egress DNS, observable inbound deadline, profile projection and adoption guidance, plus their implementation tests and updated existing proof runners.
  - Packet: [T1 — complete bounded outbound profile](tasks/T1-bounded-outbound-profile.md).
  - Implementation: `Implemented` on HEAD `098b4ab18dd5b2d158a94e126798d8cc429ad735` plus the 43-file working-tree candidate; all writers joined. This checkbox records code completion only.
  - Completion: locally `Accepted` for the fixed 43-file candidate fingerprint `f2d48c1a6a194e300bec23e336e5980b10333fef70f416a9e6a43500d1b4f064`; 261 unique source tests, 96 canonical projections, 12 runtime graphs (eight fresh plus four exact-tree-equivalent reused), and independent final review PASS. The original full initializer attempt remains failed at graph 11; scoped recovery receipts and equivalence are recorded in `.git/codex/outbound-http/delivery/completion-result.json`. No CI, PR, publication, or deployment is claimed.
