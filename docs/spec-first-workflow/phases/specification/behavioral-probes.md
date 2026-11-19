# Behavioral Probes

Single source of truth for the five divergence axes probed during
[Specification](../specification.md) step 2 and verified during
[Specification Review](../specification-review.md).

1. **States & boundaries:** For each touched entity and input, which transitions
   are legal, forbidden, or idempotent on repeat? What changes at omitted vs
   `null` vs empty, zero/max bounds, units/identifiers, and exact expiry
   (`== expires_at`)?
2. **Races & precedence:** If two calls mutate the same entity concurrently,
   which wins and what does the loser see? If multiple conditions fail at once
   (auth + invalid payload + replay, env + file config), which wins? Must the
   next read observe the write immediately, or is bounded lag allowed?
3. **Failure, replay & promises:** On every relied-on response, status, event,
   or ack, can an observer see success before the promised effect holds
   (*false-positive success*) or miss a landed effect (*unobservable
   completion*)? How do retries, partial failures, auth-vs-not-found exposure,
   and secret/PII redaction behave?
4. **Blast radius & existing state:** What happens to **pre-existing persisted
   rows and in-flight work** at deploy time (backfill, lazy upgrade,
   grandfather, reject) and during mixed-version overlap? Which callers,
   consumers, boot paths, or optional profiles
   (`scripts/template/profiles.json`) must hold—and for any authorized breaking
   change, what do former callers see?
5. **Operational & quantitative** *(when triggered)*: Which invalid config or
   dependency failures reject startup vs degrade at runtime? Which operator or
   audit signals and labeled numeric targets (with workload envelope) apply?
