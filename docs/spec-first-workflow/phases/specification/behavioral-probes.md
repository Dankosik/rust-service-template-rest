# Behavioral Probes

Exhaustive check for [Specification](../specification.md) (audited in [Design
Review](../design-review.md) and [Plan Review](../plan-review.md)). Apply all
six probes to every touched entity and external surface. A probe clears only
with a closed rule (`R<n>`), preserved guarantee (`G<n>`), user-owned fork, or a
verified primary-source reason why it does not apply.

1. **States & boundaries:** For each touched entity and input, which transitions
   are legal, forbidden, or idempotent on repeat? What changes at omitted vs
   `null` vs empty, zero/max bounds, units/identifiers, and exact expiry
   (`== expires_at`)?
2. **Races & precedence:** If two calls mutate the same entity concurrently,
   which wins and what does the loser see? If multiple conditions fail at once
   (auth + invalid payload + replay, env + file config), which wins? Must the
   next read observe the write immediately, or is bounded lag allowed?
3. **Promises, failure & replay:** On every relied-on response, status, event,
   or ack, can an observer see success before the promised effect holds
   (*false-positive success*) or miss a landed effect (*unobservable
   completion*)? How do retries, replays, timeouts, and partial failures
   behave?
4. **Trust, authorization & privacy:** Which actor/identity, tenant, or scope
   may invoke each entry seam or observe each resource, and how do
   unauthenticated, forbidden, and not-found responses differ? Can keys,
   cursors, or caches leak or collide across callers? What untrusted-input
   bounds and secret/PII redaction rules apply?
5. **Blast radius & existing state:** What happens to **pre-existing persisted
   rows and in-flight work** at deploy time (`backfill`, `lazy upgrade`,
   `grandfather`, `reject`) and during mixed-version overlap? Which callers,
   consumers, boot paths, or optional profiles (`template/profiles.json`) must
   hold—and for any authorized breaking change, what do former callers see?
6. **Operational & quantitative:** Which invalid config or dependency failures
   reject startup vs degrade at runtime? Which operator or audit signals and
   labeled numeric targets (with workload envelope) apply?
