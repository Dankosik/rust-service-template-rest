# Decision-changing Technical Design evidence

Checked 2026-10-06 in the current worktree. This is provider/source/tool
feasibility evidence; no broker, database or performance experiment ran in this
phase. Current-state baseline remains [Definition research](current-state.md).

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
