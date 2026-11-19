# Specification

Use when structured work lacks a reviewed behavioral contract. Own observable
behavior: what affected users, operators, and downstream systems can rely on.
**Observability litmus test:** if an external observer (client, operator,
downstream consumer, or black-box check) can tell two variants apart without
reading internals, Definition owns it; otherwise leave mechanism and placement
to Technical Design, slicing to Planning, and test implementation to
Implementation.

## Steps

1. **Ground in source truth.** Read `specs/<task>/intent.md`, prior
   `research/*.md`, and primary repository sources (`api/openapi.yaml`, code,
   migrations, config, tests, `docs/decisions/`) for every named or implied
   surface. Verify `intent.md` and Markdown claims about current behavior
   against actual code and callers.
   - Run supporting [Research](research.md) when an external contract,
     standard, or deep baseline question can change a behavioral rule (inline
     for one bounded lookup, or via `evidence-agent` for read-heavy discovery).
     Keep [Solution Discovery](research-branches.md#solution-discovery-evidence)
     in Technical Design unless feasibility changes observable behavior.
   - **No-delta or wrong-premise exit:** if primary sources prove every
     accepted obligation already holds, skip drafting and submit that evidence
     to step 5; if `intent.md` rests on a false premise about current behavior,
     return `status: blocked` (`reopen_owner: Intake`) with the verified
     discrepancy.
   - *Done when* every touched surface and consumer in the **blast radius** is
     verified in primary sources and no behavioral evidence gap remains.

2. **Pin vocabulary and resolve behavioral forks.** Adopt canonical nouns from
   code, `api/openapi.yaml`, and `docs/decisions/`; pin any new or overloaded
   term (`cancel` vs `delete`, `accepted` vs `completed`) before writing rules.
   Load only domain methods whose pressure changes observable behavior:
   - REST/OpenAPI client semantics -> `rust-api-contract`;
   - persistence truth, transactions, or migrations -> `rust-sqlx`;
   - failure semantics, problem codes, or degradation -> `rust-errors`;
   - trust, authorization, isolation, or sensitive data -> `rust-security`;
   - deadlines, retries, readiness, or drain behavior -> `rust-reliability`;
   - operator-visible config keys, defaults, precedence, or startup rejection -> `rust-config`;
   - requested operator probes, audit/telemetry contract, or log/trace redaction -> `rust-observability`;
   - requested CI gate, image contract, or deployment-profile behavior -> `rust-delivery-platform`.

   Decide business state transitions from `intent.md` and [Repository
   Architecture](../../repo-architecture.md). Disposition every material item
   in `intent.md` (including `Open questions` and delegated limits) and every
   surfaced fork:

   | Resolution source | Action |
   | --- | --- |
   | Primary authority (contract, standard, `docs/decisions/`, verified guarantee) | Adopt it and cite the primary locator. |
   | Delegated by [Intent](../interfaces/intent.md) or [Decision Ownership](../../../AGENTS.md#decision-ownership) | Decide within stated limits (or state the invariant if multiple valid options exist), using Research as evidence, with a concrete `Reopen if` trigger. |
   | Minor gap preserving requester meaning | Record a labeled bounded assumption with an objective `Reopen if` trigger. |
   | Requester-owned choice (outcome, business policy, unauthorized breaking change, money/legal/external effects) | Finish all independent branches to collect the full frontier of user-owned blockers in one pass, save resolved rules in `spec.md` at `status: blocked`, and return [Transition Result V1](../shared/transition.md#transition-result-v1) (`status: blocked`, `reopen_owner: Intake`) without review—giving primary evidence, options, user consequences, and a recommendation per blocker. |

   - *Done when* every `intent.md` item maps to accepted behavior (`R<n>`),
     deliberately unchanged behavior (`G<n>`), a non-goal, or a collected
     Intake blocker, with zero silently resolved forks.

3. **Persist draft `spec.md`.** Write `specs/<task>/spec.md` at `status: draft`
   using [Specification V1](../interfaces/spec-v1.md) through
   [Artifacts](../shared/artifacts.md). Outline actors, baseline, delta,
   preserved guarantees, and non-goals first, then expand each materially
   affected rule into an addressable `R<n>` using [Rule Precision](#rule-precision).

4. **Run the Specification Dry Run (before review).** Attack the `draft`
   against all four checks and close gaps in place:
   - **Malicious compliance:** construct a literal implementation passing every
     `R<n>` that still misses `intent.md`'s outcome or `Success signal`; close
     every loophole.
   - **Deletion test (mechanism leak):** for each inferred clause, ask *"If
     Technical Design omitted this and met the external outcome another way,
     what accepted guarantee would break?"* If none, delete it or relax it to
     the observable invariant.
   - **Two-implementations & interaction test:** trace interacting rules across
     end-to-end and unhappy-path scenarios (precedence, partial failure,
     retry/replay, invalid transitions, legacy callers); close any fork where
     two engineers could ship different observable results, or state the
     permitted invariant.
   - **Cold-start & proof-boundary check:** walk `intent.md`, `spec.md`, and
     cited links with zero chat history. Confirm Design and Planning need no
     product guesswork and no `Falsifier` prescribes tests, fixtures, layers,
     commands, or unmandated live gates
     ([Evidence Contract](../shared/evidence-contract.md#required-and-optional-proof)).

5. **Submit for independent review.** Route the fixed `draft` `spec.md` (or
   no-delta evidence) to a fresh `reviewer-agent` running [Specification
   Review](specification-review.md) under [Review](../shared/review.md). Repair
   spec-local `FAIL` findings in place and rerun affected step 4 checks; return
   `status: blocked` with `reopen_owner` in [Transition Result
   V1](../shared/transition.md#transition-result-v1) for upstream gaps.

6. **Finalize and hand off.** After review returns `PASS` or `CONCERNS`, return
   [Transition Result V1](../shared/transition.md#transition-result-v1)
   (`status: ready`, `owner: Definition`) and stop—never begin Technical
   Design here:
   - **No-delta result:** `result: <verified no-delta evidence>`, `next_owner:
     none`, without persisting `spec.md`.
   - **Specification result:** persist any bounded `CONCERNS` obligation into
     `Carried concerns`, flip `status: ready` in `spec.md`, and set `result:
     specs/<task>/spec.md`, `next_owner: Technical Design` (or `Planning` when
     Technical Design is untriggered).

## Rule Precision

Close these four high-divergence seams wherever two implementations could differ:

- **Unhappy paths & boundaries:** invalid or repeated transitions, rejection
  and error observables (including auth vs not-found exposure), absence and
  defaults, identifiers/units/bounds, and idempotency, replay, or
  partial-failure outcomes.
- **Precedence & cross-rule interactions:** when two conditions hold at once
  (auth failure + invalid input, replay + expiry, env + file config), state
  which rule wins.
- **Existing behavior & compatibility (blast radius):** compare current vs
  target outcomes per affected consumer scenario. Separate verified supported
  guarantees from incidental details. For any changed or removed guarantee,
  state what authorizes the change, what former callers observe, and whether
  mixed-version overlap compatibility is required (leaving rollout mechanics to
  Technical Design).
- **Promise boundaries (signals & finality):** whenever an actor relies on a
  response, status, event, or ack to act, close both failure modes:
  - *False-positive success:* could the actor see success before the promised
    effect holds?
  - *Unobservable completion:* if completion must be discoverable, could the
    effect land while the actor cannot tell?
