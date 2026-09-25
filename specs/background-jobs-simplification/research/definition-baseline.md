# Definition evidence

Read on 2026-09-25 at base `4edd184ea3cc6b6fa2b225244700fce57150ae18`.
This is source research, not measured performance or database execution proof.
Stop condition: enough evidence to fix behavior and compatibility; Technical
Design owns SQL/API placement and any required discriminating mechanism probe.

| Claim | Evidence and limit | Decision effect |
| --- | --- | --- |
| Current lease is renewed, not timeout-derived. | `crates/infra-jobs/src/engine.rs` declares 30-second TTL, 10-second upkeep and 2-second cancellation margin; `claim.rs` passes TTL separately from kind policy. | Replace periodic renewals with per-kind timeout plus fixed margin. |
| Unknown commits currently drive attribution machinery. | `claim.rs` retains returned rows for reconcile; `lease.rs::reconcile` and `attempt.rs` interpret database states for reported outcomes. | Remove attribution reads from engine recovery; preserve truthful uncertainty. |
| Current sampling scans live populations. | `maintenance.rs::SAMPLE` groups all pending and running rows every 10 seconds per worker. A two-second statement timeout bounds elapsed wait, not input cost. | Bound sampling work and expose freshness/censoring. No measured speedup claim. |
| Old storage admitted inputs JSONB cannot represent. | The immutable `20260924000001_create_background_jobs.sql` uses bytea; `docs/architecture/async.md` explicitly accepts JSON containing escaped NUL. | An unconditional conversion is unsafe. Forward migration must refuse incompatible data atomically. |
| Enqueue and lifecycle constraints already exist. | `enqueue.rs`, `test/tests/jobs/enqueue.rs`, `jobs-worker/src/shutdown.rs`; teardown tail is 17 seconds including a two-second release stage. | Preserve conflict semantics; replace drain handling within existing total budgets unless Design proves a necessary budget change. |
| Future products are not yet present. | `docs/roadmap.md` stage 10 items 5–6 describe webhooks, JetStream and transactional outbox. | Record jobs reuse without adding those implementations or assuming a third binary exists. |

Primary external evidence:

- [River active rescue](https://riverqueue.com/blog/active-job-rescue), dated
  2026-07-28, distinguishes timeout-based rescue from Pro queue-level heartbeats.
  It supports the tradeoff, not a claim that River never heartbeats. Our fixed
  margin is a local choice, not River's default or an availability guarantee.
- [River maintenance](https://riverqueue.com/docs/maintenance-services) warns
  that rescue can overlap a still-running execution. Fencing protects the row;
  effect idempotency still belongs to the handler/provider boundary.
- [RDS failover](https://docs.aws.amazon.com/AmazonRDS/latest/UserGuide/Concepts.MultiAZ.Failover.html)
  reports typical Multi-AZ instance failover of 60–120 seconds and potentially
  longer recovery. This falsifies any general guarantee that a 30-second lease
  or 60-second safety margin survives failover; no RDS deployment is implied.
- [PostgreSQL 18 JSON types](https://www.postgresql.org/docs/18/datatype-json.html)
  documents UTF8 dependence, JSONB NUL rejection, numeric bounds, and
  normalization of whitespace, key order and duplicate keys. New input must be
  validated before SQL; migration must not assume all historical bytes convert.
- [River transactional completion](https://riverqueue.com/docs/transactional-job-completion)
  supports atomic business effects plus completion and explains the gap in
  ordinary out-of-band completion. It does not establish our fenced API's safety.
- [River snooze](https://riverqueue.com/docs/snoozing-jobs) distinguishes a
  requested future run from failure and refunds the attempt. We explicitly adopt
  that distinction while preserving ordinary retry accounting.
- Installed `opentelemetry` 0.32.0 primary source,
  `src/propagation/text_map_propagator.rs`, defines `inject_context` and
  `extract_with_context`; source is under the local Cargo registry.
  Use it instead of handwritten W3C formatting/parsing. Browser docs.rs access
  failed; resolved local source supplies version-exact API evidence.

Reuse the prior crate comparison under the common Git directory
`.git/claude/background-jobs/lanes/` (`river-precedent.md`, `apalis.md`,
`graphile.md`, `screen-2026-09-24.md`, `sqlx08-screen.md`). The first two were
read for this Definition. They are historical research, not newly executed
proof: apalis had important integration, feature, shutdown and contract costs.
The accepted scope simplifies the current engine, so no new queue framework or
dependency upgrade is required. Design must check resolved APIs before adding
features or dependencies; installed SQL randomness remains a viable alternative
to adding rand directly.

Unknowns retained for Design: exact indexed claim/observer query shape, safe
caller-transaction completion API, trace carrier representation and shared
lifecycle placement. They are mechanism decisions, not missing product intent.
Reopen Definition only if their evidence makes a contract below infeasible.
