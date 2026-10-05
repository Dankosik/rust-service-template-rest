# Technical Design transition

status: ready

owner: Technical Design (`/root/optimization_design`)

result: [design.md](design.md), Git blob
`6865b530187045c4ee7bb61422282835bd5c4fd3`

review: [technical-design-review.md](technical-design-review.md), **PASS**;
receipt Git blob `8f281ba25a0fa3b537269badd8ce997fed345710`

movement_evidence: One selected mechanism and reconciled responsibility/file
map close the body ownership, public API compatibility, serialization,
duplicate ordering, failure, retention and proof boundaries. The fresh
independent reviewer found no surviving defects and confirmed Planning can
act without inventing mechanisms or contracts. The reviewed candidate was
`7942d207dcfe6bd81f95c8c70171963fdf3e74f4`; its only subsequent edit records
ready status and the PASS link. Shared Transition's unchanged semantic-scope
rule retains that verdict.

reopen_owner: none

next_owner: Planning, dispatched as a fresh phase actor by root

## Selected work and next action

Transfer collected HTTP `Bytes` into the existing winning receipt branch and
store it in `Incoming.body`. Add the owned `receive_bytes` entry point while
retaining `receive(&[u8])`, both over one private admission core. The private
carrier copies borrowed input only after arbitration, inside `Incoming::new`.
Keep Base64 annotations and the shared enqueue/preparation code unchanged.
No new dependency, feature, schema, setting, worker mechanism, or public body
abstraction is selected.

Planning preserves one bounded implementation outcome, then the required
remote validation/comparison and final independent review. The primary target
remains at least one complete large-fixture body length less measured
construction/preparation allocation turnover, with the Specification's
behavior and regression constraints. Full JSON-byte parity and actual savings
must still be proved; this design does not claim implementation or speedup.

Only the three Technical Design documents were written by this actor. No
production source or tests were edited; unrelated dirty work was preserved.
Only static reads, source/Git identity inspection, document edits and native
review ran. Local builds, make/docs checks, tests, services, load, hotpath,
Python and Node analysis remain prohibited. Root owns execution/infrastructure
on approved droplet `606044304` (`159.89.101.4`), under Intent's 8-hour cap;
no push, PR, production inputs, deployment or provider mutation is selected.

Reopen Technical Design for a mechanism/API/lifetime conflict or an unmet
allocation/regression target. Reopen Definition for changed behavior, scope
or success meaning. Missing infrastructure or external authority returns to
root. No user-owned question blocks continuation. This actor stops at the
reviewed phase boundary; the implementation request continues through root.

Workflow locators: [router](../../docs/spec-first-workflow.md),
[Technical Design Review](../../docs/spec-first-workflow/phases/technical-design-review.md),
[Review](../../docs/spec-first-workflow/shared/review.md),
[Transition](../../docs/spec-first-workflow/shared/transition.md), and
[Codex harness](../../docs/agent-harness/codex.md).
