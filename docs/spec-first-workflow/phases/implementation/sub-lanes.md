# Sub-Lanes

Load only when an Acceptance-Unit Lead delegates parallel disjoint subtasks
inside one unit, or when a mutable `worker-agent` returns a lane result.

## Delegation Rules

- Implement directly whenever handoff costs more than it saves.
- Run parallel lanes only when accepted interfaces are stable, writable owners
  and exclusive locks are disjoint (and never edited concurrently by the Lead),
  and capacity permits. Apply [Agent Harness](../../../agent-harness.md) when
  dispatching, steering, or replacing a lane.
- **Shared Rust seams:** land boundary types, traits, signatures, and generated
  sources first and verify with `cargo check` before dependent lanes consume
  them. A consumer of a checked contract may start immediately; a consumer of
  implemented code starts once that code is integrated into the unit candidate.
- **Serial integration & escalation:** integrate lane mutations serially. A
  lane raises scope expansion or invalid accepted behavior/architecture with
  its Lead through `gap`.
- Join or stop every lane before returning `Implementation Result`, freezing
  the candidate, or releasing writable scope.

## Delegated Result V1

Every mutable delegated worker returns:

```text
result: <completed bounded outcome or no result>
changed_paths: <repository-relative paths or none>
proof: <checks actually run with their results, or none>
candidate: <identity when isolated or none>
gap: <remaining blocker or none>
```

This result is input to the Acceptance-Unit Lead, never acceptance, review, or
ledger state.
