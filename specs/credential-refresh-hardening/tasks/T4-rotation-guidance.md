# T4 — Honest cross-provider rotation and revocation guidance

Outcome:
Canonical guidance lets an operator distinguish published material, application
reread, authenticated new sessions, token expiry and effective revocation across
the existing provider owners.

Consumes:
- [Specification R2](../spec.md#r2-accurate-rotation-and-revocation-guidance)
  and [recommendation disposition](../spec.md#recommendation-disposition) — the
  accepted operator contract and preserved/deferred runtime recommendations.
- [D4](../design/technical-design.md#d4-canonical-documentation-mapping) — exact
  canonical documentation owners and profile-marker requirement.
- T1/T2/T3 implemented source and provider guides — implementation dependency
  for assembly of shared summaries; their final proof is not a coding gate.

Provides:
- Compact cross-provider rotation/navigation section in the existing
  configuration source policy and corrected canonical persistence, cache,
  jobs and TLS/session guidance, with consistent architecture summaries.

Boundary:
Carry all remaining R2 obligations: startup snapshots and named readers;
PostgreSQL cadence/last-good/password-only/lifetime limits and separate LISTEN
session; unchanged Redis read+AUTH envelope, rejected-byte retries, jitter and
conditional recovery bounds; owner-specific TLS load points, additive SQLx
roots and require versus verify-full; process-wide HTTP TLS roots; existing
sessions/resumption and lack of universal hot reload or revocation deadline.
Describe atomic publication, provider overlap, external projection plus
application recovery, sanitized verification with fresh authenticated work,
provider/session emergency controls and restart boundaries. Explain eventual
Kubernetes projection/no subPath refresh and external Vault/sidecar ownership.
Link T1-T3 guides rather than duplicating their provider schedules. Update only
contradictory integration/lifecycle summaries. No runtime change, new overview,
framework, telemetry axis, live rotation or new acceptance environment.

Mutable owners:
- `docs/configuration-source-policy.md`, `docs/architecture/persistence.md`,
  `docs/background-jobs.md`, `docs/cache.md`, `docs/grpc.md`,
  `docs/outbound-http.md`.
- Contradictory summaries only in `docs/architecture/integration.md` and
  `docs/architecture/runtime-lifecycle.md`.
- Misleading cadence/retirement comments only in
  `crates/infra-postgres/src/credentials.rs`; no executable Rust changes.
  This source-consistency repair implements the same accepted R2 wording.
- Preserve relevant template/profile markers and canonical cross-links.

Exclusive locks:
- none; T1-T3 own their provider guides and must finish before T4 assembly.

Final validation:
- Claim: Every accepted R2 material and session limit has its correct canonical
  owner, with no unsupported cutover, reload, readiness or revocation promise.
- Checks: Static source/guide consistency and consolidated docs-check at
  [ledger Completion](../tasks.md#completion-evidence). Review the assembled
  scope once; no new provider, database or TLS runtime exercise.
- Observable: An operator can follow the documented sequence without confusing
  file delivery with authenticated cutover or removal of trust with termination
  of existing sessions. This is source-qualified guidance, not observed rotation.

Reopen if:
D4 has an actual ownership contradiction, source contradicts an accepted
material contract, or changed operational behavior/policy is needed; return to
Technical Design or Specification respectively instead of silently adding runtime work.
