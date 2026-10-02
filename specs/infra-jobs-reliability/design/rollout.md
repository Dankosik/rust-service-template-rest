# Release closure and proof boundary

The immutable implementation foundation is named in [System Design](system.md);
this sequence applies to that foundation plus corrections D1–D6. Source adoption
is not a deployed result.

This is the sequence consumed by Implementation and the eventual adopter
runbook. No deployment, live migration, queue recovery or deletion is authorized
by this design phase or the current delivery request.

## Affected graph and mixed versions

One application image carries the service producer, migrator and jobs-worker.
The worker may compose ordinary handlers, the reserved outbox publisher and
NATS consumer; one PostgreSQL database owns job/sequence/history state. NATS
topology and consumer effect-deduplication storage are unchanged external
neighbors. The operator mode is the same jobs-worker executable using only an
admitted PostgreSQL route; it connects neither to NATS nor to worker listeners.

The schema changes are additive: old enqueue/claim/outcome statements omit the
new recovery_history column and tolerate it, while new code requires the complete new
embedded migration history before admission. The current migrator's history
rule admits later applied versions only when the embedded known prefix matches;
do not remove that check or add runtime schema repair. Confirm that existing
history contract in its real-DB proof rather than guessing from schema shape.

Schema compatibility does not make mixed worker behavior safe. Old workers
still delete failed rows and free capacity before completion persistence. Consequently all old
retention/claim owners must be stopped and replaced before recovery commands
are enabled or the failure-custody guarantee is relied on. No recovery under
mixed old/new worker owners is supported. Archive-before-reset preserves cycle
history without changing ordinary failure writers, but does not neutralize old
retention owners or grant them the corrected capacity guarantees. Producers can continue using their compatible enqueue contract;
service startup remains governed by the embedded history rule. A mixed fleet
is a temporary rollout state, not an accepted custody/recovery operating mode.

## Ordered gates

| Owner/node | Prerequisite | Action | Success signal | Distinct safe failure | Horizon/budget | Recovery | Proof/readback |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Delivery candidate | Reviewed design and completed implementation | Run ordinary build/tests and existing selected DB, metadata, migration, outbox and profile routes; assembled independent review. | Exact candidate receipts and no surviving in-scope defect. | Pending/missing required CI or failed proof remains incomplete. | Existing validation owners only. | Repair smallest current owner; no extra full matrix. | Existing local/CI receipts tied to candidate. |
| Adopter worker fleet | Separate deployment authority; inventory of every process that can retain/claim from target DB | Drain/stop all old workers, including replicas, one-off workers and outbox-only instances; hold recovery disabled operationally. | Deployment inventory shows no old retention/claim owner remains. | Unknown old owner means no custody guarantee and no supported recovery. | Existing per-process grace, no new availability SLO. | Keep stopped until a compatible new worker is ready; preserve queue. | Target fleet/process inventory; already deleted rows remain unrecoverable. |
| Migrator: archive column | Complete prior canonical history and stopped old worker owners | Apply additive column migration through existing migrator. | Acknowledged migration history and expected JSONB type and empty-array default. | Lock/deadline/checksum refusal stops rollout without schema repair. | Existing 15s lock/2min transactional statement and configured migration deadline. | Retry acknowledged pending migration; roll forward. | History checksum, column definition and legacy failure rows preserved. |
| Migrator: failed index | Column migration acknowledged | Run concurrent partial-index migration. | Migration history plus valid expected index definition. | Failed concurrent build may leave invalid index; current runner refuses restart until cleanup. | Existing migration_deadline sized by target operator. | Authorized operator drops invalid index concurrently, then reruns; never mark an invalid build applied. | `indisvalid`, exact index definition, migration terminal record. IF NOT EXISTS alone is not definition proof. |
| New binaries/operator | Both migrations complete; old worker owners absent | Admit new code through current history/session/config policy. Use payload-free inspect/pages before any mutation. | New worker readiness or acknowledged operator read; no provider is needed for operator read. | Pending/mismatch/unavailable admission returns safe nonzero and opens no queue loop. | Existing startup budgets; one-shot operation 12s. | Leave worker stopped or restore compatible fixed candidate; inspect known state. | Admitted image identity and process/command records; target-only proof is deployment-owned. |
| Custody/observation | Compatible new workers only | Resume ordinary/publisher work; inspect retained failed rows and registered-union metrics. | Failed rows retained, complete sample timestamp, no new capacity beyond full-attempt bound. | Sampling failure retains old timestamp; zero or >30s is unobserved/stale. | 10s cadence, capped1000 meaning; no completion deadline promised. | Diagnose DB/service failure without deleting intent. | Existing logs/metrics plus explicit bounded operator traversal. |
| Optional recovery | Separate authority for that exact live identity and action; compatible handlers restored; effects reconciled | Inspect, then one id/kind/version redrive or explicit discard. | Acknowledged DB receipt only; later handler/broker effects need separate observation. | Missing/stale/live-key conflict/known failure/unknown remain distinct. | 2s statement, 12s operation; no retry loop. | Inspect same identity after unknown; never refresh token automatically. | Payload-free receipt and subsequent state; absence never attributes uncertain discard. |

The last transparent binary-rollback state is before relying on retained failed
custody or executing recovery with the new cycle archive. Additive schema need
not be removed on rollback. Reintroducing an old worker after that point can
delete unresolved work and reintroduce early capacity release; keep such workers
stopped and roll forward to a repaired compatible release. Returning to old
workers would require a separately accepted preservation/reconciliation plan,
not a silent rollback. Dropping the new column/index is not part of this task.

Backup restore is a separate recovery operation: include the queue, history and
sequence consistently; invalidate saved pre-restore operator tokens/commands;
restore handlers capable of reading outstanding kinds/payload versions;
reconcile already-applied effects; inspect then recover individual identities.
Finite broker deduplication does not cover indefinite manual replay. Never
rename/delete unknown kinds automatically or claim that payload poison is
fixed by resetting attempts.

## Candidate proof without multiplying dimensions

Implementation first adopts the immutable foundation serially and applies only
D1–D6 plus actual defects found within the existing proof boundary. It uses the
existing Rust build/test route once for its changed
packages/manifests, generates SQLx metadata from the canonical migrated schema,
and uses the current PostgreSQL integration target for transaction/lock/commit/
retention/paging claims. Migration source/append-only and image migration
rehearsal keep their existing owners. The outbox target proves unchanged stored
publication identity through recovery; the existing worker process target
proves operator isolation and process-union observation. Template projections
cover jobs retained, jobs absent with messaging retained, and current outbox
representatives using the factored CI runtime graph. Unchanged harness/database
dimensions do not each repeat an entire build.

This phase ran no builds, tests, migration, broker, benchmark, deployment or
live queue command. Earlier 45 baseline unit/process tests are source-baseline
context only and cannot prove the future implementation. Ready design is a
planning input; candidate acceptance still requires the existing final
validation and independent assembled review. Deployment and recovery receipts
remain outside the PR-completion claim.
