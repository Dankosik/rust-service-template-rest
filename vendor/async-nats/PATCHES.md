# async-nats 0.50.0: bounded native ownership and pull completion

Upstream reviewed: 0.52.0

The published `async-nats-0.50.0.crate` archive has SHA-256
`d83a251fa1a4c9d0fe6e816b7acd60549e473e08d14f27a1d992c2675abff05f` and records
upstream revision `9b382a2a01b5404cd66bee6c2b4f0c82c9943063`. Upstream
`0.51.0`–`0.52.0` ship only the `ConnectInfo` secret redaction
([`4e70300`](https://github.com/nats-io/nats.rs/commit/4e70300dcd7542441a303af2dae846a7c757a98d));
the ownership, recovery, and pull-completion repairs remain unmerged.

This source patches `src/{lib,client,connector,options,tls}.rs`,
`src/jetstream/context.rs`, and `src/jetstream/consumer/pull.rs` for:
- adaptive multiplexer pruning of abandoned request senders (upstream draft
  [PR 1629 at `7db17cf`](https://github.com/nats-io/nats.rs/blob/7db17cf15830a1a65e7ba73cecda65aee72b1ea7/async-nats/src/lib.rs),
  [issue 1617](https://github.com/nats-io/nats.rs/issues/1617)) and `ConnectInfo`
  `Debug` secret redaction;
- bounded JetStream publish ACK receiver/permit custody across caller
  cancellation plus passive `Context::in_flight_publishes()` occupancy reads;
- optional `ConnectOptions::initial_connect_deadline(...)`, async native root-CA
  loading, and runner close-lease custody restricted to live clients and
  subscribers so detached `UNSUB` cleanup cannot pin a full reconnect queue;
- non-terminal `409 Batch Completed` handling in `pull::Batch` when NATS
  allocates the full count before replicated deliveries arrive.

Retire this vendor directory, root Cargo patch/exclusion, `messaging`
profile/Docker/classifier carriers, and vendor fixture `.gitleaks.toml` rules
together when a published `async-nats` release provides equivalent request
pruning, ACK/permit custody, passive occupancy observation (never replaced with
permit acquisition or `wait_for_acks`), bounded transport/close-lease recovery,
and reordered `Batch Completed` handling, passing
`make native-transport-regressions` and messaging integration proofs.
