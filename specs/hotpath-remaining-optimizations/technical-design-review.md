# Technical Design review

Fixed candidate: [design.md](design.md), Git blob
`3704053dcfc2512c2137179c12401682300e55f3`.
Authority: [spec.md](spec.md), Git blob
`82c39b3adfa32073c719dc70513c39b171b2dea8`.

## Rust Ownership Review panel

The panel follows [Rust Ownership Review](../../docs/spec-first-workflow/rubrics/rust-ownership-review.md)
and shared [Review](../../docs/spec-first-workflow/shared/review.md). All three
fresh independent read-only reviewers used the same fixed candidate and native
`gpt-6-astra`, `high` settings. No lane edited files or ran executable proof.

### Responsibility and execution paths

Reviewer: `/root/remaining_design/ownership_responsibility`.

```text
candidate: design.md blob3704053dcfc2512c2137179c12401682300e55f3
verdict: PASS
findings: none
evidence_boundary: ownership panel lens 1; current source, six recorded identities, ready specification
reopen_owner: none
```

Attempted falsifiers: moving serialization into duplicate admission or generic
jobs policy; ambiguous durable/commit/wake ownership; representation helpers
taking over telemetry policy or response-body lifetime; synthetic sizing
creating a second config/lifecycle authority. Current CodeGraph source traces
and the design preserve the existing owners for each path.

### Package, dependency, visibility and generated boundaries

Reviewer: `/root/remaining_design/ownership_boundaries`.

```text
candidate: design.md blob3704053dcfc2512c2137179c12401682300e55f3
verdict: PASS
findings: none
evidence_boundary: ownership panel lens 2; current modules/manifests, architecture, source identities
reopen_owner: none
```

Attempted falsifiers: required public seams, missing dependencies or reverse
edges, profile-dependent production APIs, subscriber/recorder composition
moving into helpers, and generated OpenAPI/SQL metadata needing manual edits.
Existing private modules and declared dependencies support the proposed
changes, and no selected production change alters a generated boundary.

### File cohesion, names and proof placement

Reviewer: `/root/remaining_design/ownership_cohesion`.

```text
candidate: design.md blob3704053dcfc2512c2137179c12401682300e55f3
verdict: PASS
findings: none
evidence_boundary: ownership panel lens 3; current declarations, existing proof locations and identities
reopen_owner: none
```

Attempted falsifiers: unnecessary file splitting or utility placement,
confusion between span-method values and normalized metric labels, duplicate
proof, and test-only exports. The adapter belongs beside `Incoming`; HTTP
helpers belong beside their emitters. Existing in-file tests and black-box
webhook database tests own material gaps without a new test surface.

### Panel synthesis

All three non-overlapping lenses PASS on the same candidate, with no
incompatible ownership decisions or findings. The phase owner accepts this
panel result as ownership-review evidence only. It does not establish code
correctness, measured benefit or delivery completion.

## Broader Technical Design Review

Reviewer: `/root/remaining_design/technical_review`, fresh read-only native
`gpt-6-astra`, `xhigh`, through
[Technical Design Review](../../docs/spec-first-workflow/phases/technical-design-review.md).
The reviewer consumed the three panel receipts without repeating their lenses.

```text
candidate: design.md blob3704053dcfc2512c2137179c12401682300e55f3
verdict: PASS
findings: none
evidence_boundary: complete fixed Technical Design; ready Intent/Specification; current source and resolved library mechanisms; three ownership receipts
reopen_owner: none
```

Independently attempted falsifiers and results:

- Payload: byte parity, padding, optional fields, duplicate bypass, generic
  validation order and bounded memory. Current source and resolved libraries
  support the adapter; net allocation benefit remains remote evidence.
- HTTP: extension methods, route whitespace, string-to-display visitors,
  complete status range and retained allocations. Tracing/JSON visitors preserve
  formatted string meaning; OpenTelemetry preserves borrowed Cow values; the
  typed table uses the resolved public status API for all legal values.
- SQL: receipt/job fusion violates duplicate preparation bypass; the due-only
  enqueue/notify gate introduces the documented wake suppression and unique-key
  cycle; resolved SQLx release hooks cannot skip the driver's subsequent ping.
  Commit-delivered notification semantics support the rejection. Protocol
  predictions remain clearly separated from the required observation, and
  acquire/transaction/release accounting is explicit.
- Pool: reservations, replica overlap, dedicated listeners, worker minima and
  code-versus-pool confounding are accounted for. Assumed inputs remain labeled
  and require actual settings/topology before an admissible measured selection.
- Completeness and feasible proof: every area has a candidate or bounded
  rejection, proof and reopen boundary. Ordinary controls and the final
  assembled comparison expose interactions without multiplying full builds
  across a Cartesian matrix. No downstream behavior or authority invention
  is required.

All six recorded source identities matched. The reviewer made no edits, ran
no executable validation or protocol capture, claimed no performance result,
and performed no acceptance or phase movement. The phase owner changed only
the design's ready status and review link after this PASS; the reviewed
semantic scope and decisions are unchanged. Remote execution remains the
Implementation/delivery owner's evidence boundary.
