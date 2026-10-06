# Consumer lifecycle native rehearsal

This source-only procedure observes one synthetic PostgreSQL/jobs/JetStream
composition across the historical jobs custody transition and a native restore.
It uses actual compiled actors from `67be869acea112af271ec8ba621cbc50ae9d36b7`
and `2cb871895b9edd018205fc98223477e269fce2e9`. A passing receipt applies only to
that pair, fixture, provider versions and single-node topology.

The existing [jobs custody contract](background-jobs.md),
[messaging settlement contract](durable-messaging.md) and
[PostgreSQL validation owner](validation/postgres.md) remain authoritative.
Production recovery additionally needs the service's accepted
[Production Contract](production-contract.md). Local elapsed times do not set
production RTO/RPO, backup retention, loss tolerance or cluster guarantees.

## Run and retained inputs

Run once from the source repository, under its final-validation owner:

```bash
ALLOW_HEAVY=1 make consumer-lifecycle-check OUTPUT=/absolute/new/rehearsal-directory
```

Use a new directory on a volume with at least 8 GiB available. The existing
Docker daemon, Compose supporting `!override`, NATS CLI with `backup stream`,
`backup validate` and `backup restore stream`, and both historical Rust
toolchains must already be usable. The target refuses missing capacity or
capabilities; it does not install a daemon, upgrade a provider, clear caches or
provision a runner. PostgreSQL's dump/restore tools run inside the pinned
PostgreSQL container, so their version matches the server. No production
endpoint, credential or data is accepted by this invocation.

The target is deliberately absent from ordinary validation aggregates and
initializer matrices. Its integration test is ignored under ordinary Cargo
discovery. The explicit target selects that exact ignored test, requires one
executed passing case and requires its terminal `completed.json`; zero executed
tests or a missing receipt fails the target.

Preparation makes two independent local Git clones from the fixed commits and
runs each source's own full public initializer with this identical tuple:

- Service `lifecycle-demo`, repository input
  `https://github.com/Dankosik/rust-consumer-lifecycle-demo`, description
  `Synthetic consumer lifecycle rehearsal.`, codeowner `@Dankosik`.
- PostgreSQL database and jobs, NATS JetStream messaging and PostgreSQL outbox.
  All other capabilities are `none`; the agent harness is `core`.
- Each initialized tree is committed locally before overlay application.
  `actors.json` records upstream/pristine commits and tree, generated lock hash,
  toolchain, Cargo target directory and build command, identical overlay SHA-256
  and each executable SHA-256.

The only overlay is
[`test/examples/consumer_lifecycle_actor.rs`](../test/examples/consumer_lifecycle_actor.rs).
No production source, dependency, lockfile or embedded migration changes after
initialization. The fixture creates its synthetic intent/effect tables outside
migration bookkeeping. Both builds run serially with `--locked`. Each historical
source uses its own `OUTPUT/build/old` or `OUTPUT/build/new` Cargo target for
initialization, actor compilation and executable copying, keeping artifacts from
the two source graphs separate. The carrier checks for other modified/untracked
files and for lockfile drift.

This distinction matters for the historical migration rule. The corrected
framework's two added migration versions exceed the old embedded maximum.
A real consumer with later migrations might refuse unknown versions inside its
own embedded range; this drill does not promise rolling compatibility there.

## Finite operational sequence

1. The old migrator initializes the source database. The old worker admits its
   schema and completes `job-old`. The corrected worker must exit with the
   missing-embedded-migrations refusal before it reports readiness.
2. The corrected migrator applies its additive migrations while the old worker
   remains active. The old process completes `job-old-expanded`, proving actual
   old queries still work. Old/new processes overlap for `job-overlap`.
3. The parent sends SIGTERM to the sole old retention owner and waits for its
   successful exit. Custody admission then refuses any old-worker launch. This
   is an explicit operator check: the old binary has no fleet custody detector.
4. Corrected workers create a retained failure, redrive it through the existing
   transactional operator API, and fail the next cycle. The fixture ages that
   failed row beyond the old seven-day retention horizon and runs corrected
   retention. The exact row and its recovery history must remain.
5. The real outbox publisher produces typed v1 events. The fixture consumer
   records each successful durable effect by logical ID in the same transaction
   as its delivery observation. An intentional permanent failure goes through
   the actual DLQ route. A further source event remains unconsumed; additional
   job and outbox intent remain pending in PostgreSQL.
6. All producers, workers, consumers and retention owners stop and join. The
   inspection pool closes. The native carrier refuses capture if PostgreSQL
   still has another client connection. `fenced.json` contains the complete
   finite database and stream manifest and the joined-owner inventory.
7. Take a whole-database custom-format `pg_dump`; retain its table-of-contents
   validation, password-free role DDL and role/extension/session settings.
   Back up source and DLQ with `nats backup stream ... --consumers`, then run
   `backup validate` for each archive. Hash the complete native archive file
   set and make it read-only.
8. Restore only into the separate empty database/broker. The same Compose
   initialization supplies the required `app` role, which is checked before
   `pg_restore --single-transaction --exit-on-error`. Restore both named streams
   with the native CLI; existing destination names are an error. Hashes must
   still match before and after restoration.
9. Compare every stored queue/history row, migration checksum, intent/effect,
   sequence value, stream message identity/payload/header digest and durable
   ACK/delivery position. Record stream/consumer creation identities separately.
   Sequence internals such as WAL preallocation are not logical restore state.
   No saved recovery token crosses this boundary: inspect the retained failed
   identity anew with the corrected operator.
10. Admit fresh corrected worker/consumer processes only after reconciliation.
    Pending jobs, unpublished outbox intent and the pending stream identity
    complete. The next actual job claim must use a generation above every
    saved generation. Republish `event-before` after the broker dedupe window;
    observe a second typed-handler delivery and the same single durable effect.

The final unresolved identity is intentionally explicit: `event-dead-letter`
remains in DLQ with no authorized effect. The drill does not discard, compensate
or silently redrive it. Unexpected missing intent/effect, conflicting meaning,
lost failed custody or inconsistent history prevents readmission.

## Evidence and failure custody

| Retained file | What it establishes |
| --- | --- |
| `actors.json`, `overlay/`, `consumers/`, `bin/` | Exact source/render/overlay/executable relationship |
| `actor-*.json`, `actor-*.stdout`, `actor-*.stderr` | Actual process PID/revision/action, admission, refusal and diagnostics |
| `resources.json`, `compose.yml`, `versions.txt` | Named disposable projects, loopback ephemeral bindings, exact observed server/tool versions |
| `fenced.json`, `restored.json` | Per-identity before/after state and creation identities |
| `archives/`, `archives.sha256.json`, `database.toc`, `roles.sql`, `*-settings.json` | Native archive custody, validation and restore prerequisites |
| `native-*.stdout`, `native-*.stderr`, `rehearsal.log` | Executed provider commands and selected test result |
| `completed.json` | Final identities, replay settlement, unresolved DLQ identity and measured intervals |

`fence_to_ready_ms` starts after all source owners and the inspection pool have
joined and ends when fresh restored worker and consumer report ready.
`restore_start_to_durable_completion_ms` includes native restoration,
reconciliation, pending work and the demonstrated replay. These are local
observations for this finite workload.

On success, cleanup removes only the two named disposable Compose projects and
their volumes. Source trees, binaries, manifests, logs and native archives remain
in the output directory for the owning task's retention decision. On failure,
cleanup stops the two projects and preserves their volumes and all evidence.
Read `resources.json` and the command logs before retrying. A partial NATS
restore remains fenced; retry restoration from unchanged archives into an empty
destination. PostgreSQL's single-transaction restore bound does not make NATS
restore transactional or the independent snapshots distributed-atomic.

The historical old-worker refusal is **not** a successful release rollback.
Published corrected A/B/A observations use separate source commits and verified
image digests. The source actor can enqueue the same finite event through
`enqueue event ID` and consume with `consume` while an actual published
`/jobs-worker` supplies outbox publication; record that worker's verified digest
and source separately. This actor is never a shipped image entrypoint.

The carrier, actor, test and this guide are removed by full initialization.
Existing linked guides remove their source-only link blocks in the same
projection. The final image retains only the existing service/migrator/worker
inventory.
