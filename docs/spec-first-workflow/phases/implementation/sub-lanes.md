# Sub-Lanes

Load only when an Acceptance-Unit Lead delegates parallel disjoint subtasks
inside one unit, or when a mutable `worker-agent` returns a lane result.

## Delegation Rules

- Implement directly whenever handoff costs more than it saves.
- Once interfaces are stable, apply [Nested Execution](../../../agent-harness.md#nested-execution) for scope, capacity, integration and lifetime.
- **Shared Rust seams:** land boundary types, traits, signatures, and generated
  sources first and verify with `cargo check` before dependent lanes consume
  them. A consumer of a checked contract may start immediately; a consumer of
  implemented code starts once that code is integrated into the unit candidate.
- Report an out-of-scope change or invalidated contract to the Lead in `gap`.

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
