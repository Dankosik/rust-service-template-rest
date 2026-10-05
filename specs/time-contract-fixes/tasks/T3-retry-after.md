# T3 — Round Retry-After up over the full Duration domain

Outcome:
Replace fractional-second flooring in Problem rendering with the exact
whole-second ceiling and existing minimum of one, including Duration values
whose ceiling is 18446744073709551616 seconds.

Consumes:
- [Specification: HTTP Retry-After](../spec.md#http-retry-after).
- [Design: Retry-After rendering](../design/technical-design.md#retry-after-rendering).
- [Problem response owner](../../../crates/infra-http/src/problem.rs).

Provides:
- Correct integer delay-seconds rendering without panic, wrap, omission or
  a hint shorter than the requested delay.

Boundary:
Use the reviewed checked whole-second increment, existing integer HeaderValue
construction and static decimal overflow value. Preserve response status, body,
content type and absent-header behavior. Keep its rustdoc consistent. No
general conversion abstraction, new schema, status or problem code.

Mutable owners:
- `crates/infra-http/src/problem.rs` renderer, relevant tests and rustdoc.
- This packet's implementation details and chosen final-validation commands.

Exclusive locks:
- none.

Final validation:
- Claim: Supplied retry durations render their exact ceiling with minimum one
  across the complete Duration input range and preserve the rest of the response.
- Checks: The ledger's consolidated matching build and relevant tests; no
  additional runtime requirement. The Lead chooses concrete cases and commands.
- Observable: Rendered Problem response headers and unchanged response contract.

Reopen if:
System Design if the resolved HeaderValue construction cannot express the
selected result; Specification for any requested wire-contract change.

## Implementation result

```text
unit: T3
verdict: Implemented
candidate: bounded working-tree diff in crates/infra-http/src/problem.rs and this packet
provides: exact Retry-After ceiling with minimum one, including u64::MAX plus a fraction; unverified
next_owner: root delivery owner for assembled final validation and review
```

Code and documentation: `Problem::into_response` performs the reviewed checked
whole-second increment, preserves integer HeaderValue construction for ordinary
values, and uses the exact static decimal on overflow. `Problem::retry_after`
rustdoc now states upward rounding. Response construction and absent-header
behavior remain unchanged.

Test owner: `retry_after_rounds_up_to_whole_seconds_with_a_minimum_of_one` in
the same file extends the existing table with independent decimal expectations
for zero, one nanosecond, just below/at/above one second, 1999 ms, exact two
seconds, the largest successful increment, exact u64::MAX seconds, u64::MAX
plus one nanosecond, and Duration::MAX. It retains the absent-header assertion.
Existing `renders_problem_json_with_optional_members` owns status, body,
content-type and extension preservation. No production seam or duplicate test
framework was added. The credible regressions are flooring, off-by-one rounding,
overflow/panic and overflow saturation; the old table explicitly expected the
incorrect flooring and lacked fractional maximum-duration input.

Selected final commands: the root's consolidated `make build` and relevant
`infra-http` package tests (`make test-package PKG=infra-http`, or inclusion in
the assembled `make test` route), plus `make docs-check` for packet links.
No build, test or review was run by this Lead; regression failure/pass evidence
belongs to final validation. Scope is released; this Lead remains available for
T3 repairs.
