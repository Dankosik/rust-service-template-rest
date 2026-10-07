# Decision-changing Technical Design evidence

Checked 2026-10-06 in the current worktree. This is provider/source/tool
feasibility evidence; no broker, database or performance experiment ran in this
phase. Current-state baseline remains [Definition research](current-state.md).
The later Implementation evidence below carries its own candidate and proof boundary.

| Question | Primary evidence | Design consequence |
| --- | --- | --- |
| Do stream limits include headers? | Pinned [NATS2.15 stream source](https://github.com/nats-io/nats-server/blob/v2.15.0/server/stream.go#L6176) checks header bytes and payload together against `maxMsgSize`; current `wire::encoded_header_bytes` calculates NATS framing and every value | Reserve actual required DLQ additions over admitted source total bytes |
| Is the total bound sufficient? | Pinned NATS2.15 `stream.go` additionally refuses `len(hdr) > math.MaxUint16` (`NewJSStreamHeaderExceedsMaximumError`), independently of the total limit. T2 Technical Review exposed a valid long-subject counterexample | T3 separately bounds transfer headers H at 65,535; exact repository wire framing and required metadata supply both calculations |
| What gets transferred? | Current [`Delivery::dead_letter`](../../../crates/infra-messaging/src/consumer.rs) copies first identity and trace values, replaces publication ID and adds original subject, reason, expected stream; [`Registry::routed`](../../../crates/infra-messaging/src/registry.rs) exposes concrete routes inside the crate | Registry-aware admission can bound normal subject overhead without a new public knob or stricter wire grammar |
| Are ACK/persistence fields available? | Locked [`stream::Config`](../../../vendor/async-nats/src/jetstream/stream.rs) carries `no_ack`, `max_message_size`, storage and `persist_mode`; [provider persistence definition](https://github.com/nats-io/nats-server/blob/v2.15.0/server/stream.go#L173) distinguishes default from explicit asynchronous mode | Native fields suffice; no upgrade or wrapper; carry PR #239 source/DLQ file/default admission |
| Can deletion compare exact record identity? | Native [`Stream::delete_message`](../../../vendor/async-nats/src/jetstream/stream.rs) sends sequence; pinned [delete request/handler](https://github.com/nats-io/nats-server/blob/v2.15.0/server/jetstream_api.go) has sequence/no_erase and no CAS | Require maintained lifecycle fence; never claim a final read closes the race |
| Can stream creation identity replace a fence? | [Restore source](https://github.com/nats-io/nats-server/blob/v2.15.0/server/stream.go#L8959) can recover creation time. `Nats-Stream-Identity` is checked on direct/sourcing consumer creation, not message delete in the pinned API source | Creation timestamp, subject identity or a local/KV lock cannot protect a delayed delete against replacement |
| How to read selected DLQ? | Native [`get_raw_message`](../../../vendor/async-nats/src/jetstream/stream.rs) is leader-backed; existing [`restore_dead_letter`](../../../crates/infra-messaging/src/wire.rs) hashes actual DLQ coordinates for deterministic redrive and retains raw payload | Native exact-sequence read + full saved record; no cursor-based selection or new identity algorithm |
| How to resolve racing unknown COMMIT? | Current [Tx truth](../../../docs/architecture/persistence.md#transaction-truth), PostgreSQL18 [INSERT conflict arbitration](https://www.postgresql.org/docs/18/sql-insert.html), and [unique checks](https://www.postgresql.org/docs/18/index-unique-checks.html) | Unique receipt insertion arbitrates before a later READ COMMITTED receipt read; plain read absence is insufficient |
| Real snapshots available? | Official [NATS backup/restore](https://docs.nats.io/learn/backup-recovery/stream-backup-restore) covers messages/config/consumer positions and restore into absent stream; PostgreSQL18 [pg_dump](https://www.postgresql.org/docs/18/backup-dump.html) supplies consistent per-database backup | Quiescence supplies coherent cross-store boundary; deliberately different archives exercise mismatches |
| Installed archive tooling compatible? | Local NATS CLI v0.5.0 help: `backup stream`, `backup restore stream`, `backup validate`; backup inspection/validation specifically supports server2.15+; old `stream backup/restore` aliases report deprecation | Use modern native commands; pin/version-readback CLI in a CI run instead of assuming current docs match another installed binary |
| Existing binary/example hosts? | `integration-tests` owns recipes; `test/src/bin/jobs_worker_fixture.rs` invokes public `jobs_worker::run`; package dependencies include typed events, messaging, SQLx and fixture libraries. T1 independent review found `Registration` runs before `admit_pool` and exposes no pool | Reuse real worker with one narrowly added post-admission registry factory; do not open a second pool or invent worker lifecycle |
| Local resource feasibility? | Coordinator readback: Docker29.4, NATS CLIv0.5.0, PostgreSQL tools, hyperfine; host16GiB, loaded; task filesystem4.8GiB free on 460GiB | Bounded fixture data and memory; no assumed fresh full build; existing CI is an authorized proof alternative |

The bounded specialist `/root/messaging_design/dlq_identity` independently
checked the delete/restore semantics with `gpt-6-astra`, `xhigh`, read-only.
Its selected mechanism is native operations inside an enforced original-store
lifecycle fence. It rejected read/fingerprint/delete and expiring local/KV
locks because delayed server requests survive caller timeout/crash. An isolated
snapshot deletion cannot prove retirement of an original live DLQ. These
claims were checked against the named source before design adoption.

The [Definition B4 disposition](../definition-transition.md#b4-provider-driven-clarification)
retains the ready specification without behavior change: the owned fixture may
enforce this maintenance precondition; an adopter owns equivalent deployment
custody; unavailable custody must refuse rather than weaken exact-record safety.

Tracing evidence only locates the pressure: current tests install a local
`Records` subscriber and a second dispatcher, while the region assertion counts
one callback carrying the Amazon field. This phase neither reproduces the flake
nor accepts the cached-interest hypothesis. Implementation must discriminate
capture from instrumentation and preserve assertions.

No new general-purpose crate is selected: this task composes already resolved
libraries and provider tools. Provider deletion's missing CAS is an operating
boundary, not a reason to fork NATS or invent a delivery engine. No performance
claim, live topology certificate or production RPO/RTO follows from this file.

## Native bounded pull completion repair, 2026-10-07

Final-validation run `37555899230` observed this on candidate
`a06125d7050d0150c4c657b8b568f83d5ee77aae`: the R3/TLS source held one
376-byte record, its outbox intent was completed, and effect/DLQ counts were
zero. The durable had consumer sequence 5, stream sequence 1, ACK floor zero,
one pending ACK and no unallocated record. Both tracked workers were running;
untruncated logs contained five closed
`pull_errors.batch_receive.batch_completed` observations. The retained artifact
is `11455486179`, SHA256
`98878667c968c9cf9c99d4de00099eb1f3b37387e6abf6e08d67cc5aaa4258e3`;
its evidence member is
`native/rehearsal/archive-source/evidence/1791336049096792065-effect-failure.json`.
This is an observed pre-handler transport failure, not evidence of a receipt or
schema defect. B4 passed; B2 failed and B5 did not execute on that candidate.

Pinned [NATS 2.15 delivery ordering](https://github.com/nats-io/nats-server/blob/v2.15.0/server/consumer.go#L5588)
calls `deliverMsg`, then sends `409 Batch Completed` when the message quota is
exhausted but byte allowance remains. [Replicated delivery](https://github.com/nats-io/nats-server/blob/v2.15.0/server/consumer.go#L5835)
queues ACKed data until quorum while that status can enter the outbound queue
immediately. The [native Batch](../../../vendor/async-nats/src/jetstream/consumer/pull.rs)
previously treated every 409 as terminal, so the adapter dropped the subscription
before the delayed data arrived and retried with source custody retained.

The dependency comparison checked the installed source and official upstream on
2026-10-07:

| Candidate | Supported behavior and gap | Decision |
| --- | --- | --- |
| Existing `batch()` / `fetch()` | Both use the same native `Batch` polling and terminate on this status; request builders have no completion-policy extension | Preserve the bounded one-shot API and repair its native owner |
| Native continuous `Stream` / `messages()` | Handles status accounting, but automatically replenishes when either message or byte allowance reaches half; R3 status accounting can start another pull before the first data arrives | Reject: conflicts with reserved free slots and the single outstanding batch |
| Published async-nats upgrade | [Registry](https://crates.io/api/v1/crates/async-nats) still lists 0.50.0, published 2026-07-20, as latest; inspected upstream [pull source at 92f7f72](https://github.com/nats-io/nats.rs/blob/92f7f72ed7a028a1075681bd2ec0caa81a32e069/async-nats/src/jetstream/consumer/pull.rs) retains the same Batch branch. That recent commit fixes continuous-stream request-limit handling, not this case | No released or inspected upstream correction to adopt; no version/feature/lock change |
| Same-version native patch | Consume only typed 409 with exact description `Batch Completed`, then poll the same receiver while retaining the outstanding count and original watchdog | Selected; existing dependency owner maintains the narrow delta and retirement proof |

The official [Go 1.53.1 definition](https://github.com/nats-io/nats.go/blob/v1.53.1/jetstream/errors.go#L293)
identifies this as the full batch sent with bytes left. Its continuous
[pending accounting](https://github.com/nats-io/nats.go/blob/v1.53.1/jetstream/pull.go#L720)
keeps allocated deliveries outstanding. Its one-shot Fetch still treats the
status as terminal. [NATS CLI 0.5 consumer-next](https://github.com/nats-io/natscli/blob/v0.5.0/cli/consumer_command.go#L2293)
requests one message without `MaxBytes`; copying that path would remove the
accepted byte cap.

The read-only specialist
`/root/t1_pull_budget_repair/batch_completion_semantics` confirmed that the sole
NATS 2.15 completion emitter has `Nats-Pending-Messages: 0`. Pending bytes here
are unused allowance; they are not bytes still owed. With N requested and R
received, keep N−R without a new header parser or counter. Poll again in the
same call so buffered data stays visible to `next().now_or_never()` during drain;
the existing Tokio receiver supplies cooperative scheduling.

Partial byte cutoff K<N produces the distinct `Message Size Exceeds MaxBytes`
status and retains its existing error disposition. Silent exact-byte exhaustion
is a separate pre-existing case: without an expiry it can still wait
indefinitely. This repair does not invent an expiry or change no-wait/404,
408, source closure, other 409 statuses, or the existing watchdog. The adapter
keeps its finite local pull deadline, size cap, identity checks and settlement.
Reopen if a supported server emits this exact completion with a positive
unallocated count, or partial-byte draining/no-expiry completion becomes an
accepted requirement.

Deterministic native cases are authored for buffered completion before/between
data, delayed data without a watchdog, preserved partial/empty/error/closure
dispositions, and retention of the original watchdog. They use native types and
existing channel constructors, with no server, new runner or production test
seam. Neither those cases nor the repaired R3 path have run in this task lane;
assembled delivery owns their outcome and the existing broker/Go/fixture proof.
The compatibility and source-custody record is
[vendor PATCHES](../../../vendor/async-nats/PATCHES.md#native-batch-completion-repair).
