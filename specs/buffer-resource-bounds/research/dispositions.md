# Audit evidence and recommendation dispositions

Status: ready as supporting Definition evidence, 2026-10-05.
Source tree: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
These are source-derived facts, not allocation measurements or live provider proof.
The preceding audit supplies dependency/HTTP observations; the decision-changing
adapter defects below were reopened against this worktree. [Specification](../spec.md)
owns accepted changes; recommendations alone do not authorize new mechanisms.

| Audit surface / recommendation | Current authority and disposition |
| --- | --- |
| HTTP body collection, handler permits and timeout | `crates/infra-http/src/harden.rs`, `extract.rs`, `crates/config/src/http.rs`: document request limit and handler lifetime. Multi-frame coalescing can transiently retain original frames plus contiguous output; metadata and decoded values add to payload. No demonstrated business flow needs a replacement collector or blanket streaming conversion. S6 documentation; behavior unchanged. |
| Bytes slices/clones and Vec spare capacity | Document retained backing allocation versus visible length and selective copying only at a real long-lived ownership boundary. No blanket copy, shrink or generic ownership layer. |
| Outbound HTTP | `crates/infra-outbound-http`: existing sequential response collection and deadline/ceiling are adequate; intentional trailer discard stays. Document exact scope if needed, no new collector. |
| gRPC stream permits/deadlines | `docs/grpc.md`, `crates/infra-grpc`: active business streams hold admission through terminal status/drop; configured server timeout is opening-only and caller deadline can cover the entire call. S6 feature recipe, no universal whole-stream timeout. |
| gRPC codec/message limits | `crates/grpc-contracts/src/codec.rs` keeps stock `tonic_prost` and 2048/32768 buffer policy; audit identifies 4 MiB default decode, unlimited encode, compression off, and decoder capacity retained by completed Streaming owners. S6 supported per-service/client size examples and prompt drop; no codec fork or blanket cap. |
| Redis backlog | `crates/infra-cache/src/connection.rs:331` leaves native concurrency unset. Commands have timeouts and generation retirement; those bound time/generations, not fan-in count. S4 fixes outstanding command ownership/count. |
| Redis native controls | Resolved `redis 1.7.1`, `src/client.rs:319–374`: pipeline default 50, write boundary default 8 KiB, concurrency default None. `src/aio/multiplexed_connection.rs:773` holds concurrency guard in caller future, so drop releases it before driver cleanup. Reuse controls but prove cancellation ownership. Pipeline/write defaults are already finite; no gratuitous override. |
| Cache keys/values/server memory | `docs/cache.md`, `crates/infra-cache`: bytes-only adapter, feature-owned keys/value policy. S6 explains decoded allocations and server maxmemory/eviction. No arbitrary key/value limit or server configuration write. |
| Auth caches | `docs/authentication.md`, `docs/outbound-machine-authentication.md`: count/weight targets, expiry and shared values do not certify RSS or immediate eviction. Clarify docs if their wording overclaims; unchanged cache behavior and revocation/failure semantics. |
| Messaging consumer | `crates/infra-messaging/src/consumer.rs`, `crates/config/src/messaging.rs`: existing slots through settlement, source max-message validation and C*(M+8192)<=64 MiB protect the declared wire window. Preserve; decoded allocations and driver buffers are extra. |
| JSON preparation | `crates/infra-messaging/src/prepared.rs:49` and `crates/infra-jobs/src/enqueue.rs:264` fully allocate before checking. S3 fixes retained serialization output while preserving exact errors/fields/precedence. serde_json writer API plus std::io is the available mechanism family; no dependency addition or two-pass serializer. |
| Publisher backlog | `crates/infra-messaging/src/producer.rs` shares low-level publication with DLQ; `messaging.rs:199` sets timeouts but inherits native admission. async-nats 0.50.0 `jetstream/context.rs` defaults to 5000 ACK permits and backpressure=true (the method comment incorrectly says false); queued waiting callers may retain payload. S5 chooses finite immediate admission sized from existing wire scale and checks the receiving resource M/H for source/outbox publication. DLQ retains malformed source bytes under the separate broker size ceiling; its aggregate bound is not the ordinary 64 MiB target. Use native max_ack_inflight/backpressure controls before another queue. |
| Native NATS buffers / cancellation | async-nats 0.50.0 `options.rs` defaults sender capacity 2048 and subscription capacity 65536. The unpolled `PublishAckFuture::drop` transfer to its bounded acker is not a complete cancellation guarantee: Technical Design confirmed gaps in pending request-map lifetime and cancellation after polling the ACK future, corroborated by upstream issue 1617 / PR 1618. Stock native controls alone cannot satisfy S5. A narrow same-version source lifetime repair is admitted through the existing SQLx-style dependency-patch delivery pattern, retaining native Context admission, ACK parsing and automatic recovery. Technical Design owns the patch scope, source evidence, proof and removal condition. No dependency upgrade, replacement protocol or additional task/queue/memory manager is admitted; S5/S6 behavior is unchanged. |
| Prepared Bytes / outbox expansion | `prepared.rs`, `outbox.rs`: clones share payload; padded base64 and metadata fit the existing jobs ceiling. S3 preserves that accounting; S6 documents outside-result ownership. No new outbox format or copies. |
| S3 tail collection | `crates/infra-object-storage/src/download.rs:145`: Vec capacity uses original metadata.size, despite bytes() returning only remaining bytes. `remaining` plus held `last` is the actual unread payload. S1 fixes excess reservation and empty tail. |
| S3 nonstreaming replies and GET errors | Resolved aws-sdk-s3 1.150.0 `operation/put_object.rs:408–428` and `delete_object.rs:229–249` consume the loaded body before deciding whether 2xx contains Error XML; `get_object` returns None for error responses, and aws-smithy-runtime 1.15.0 orchestrator read_body collects before parsing. S2 therefore covers every currently used nonstreaming reply (PUT, HEAD object/bucket, DELETE) regardless of status, plus GET errors. Status-only interception would leave 2xx collection unbounded. Supported SDK interception is the mechanism candidate; successful GET remains streaming under max_object_bytes and EOF/checksum authority. |
| S3 final chunk and upload ExactLength | `download.rs:103–130`, `body.rs`, `docs/object-storage.md`: waiting for EOF/trailers/checksum is deliberate correctness, not a buffering defect. Preserve. |
| Slow or unpolled S3 consumers/results | Existing `docs/object-storage.md` already warns slots last through streaming and returned Bytes outlive them. S6 strengthens adopter lifecycle guidance where needed; no storage permit can bound completed results or force unpolled owners to drop. |

The repository's Cargo.lock is the dependency-version authority. Maintainer API
pages were attempted at versioned docs.rs URLs but were unavailable through the
web reader; the installed resolved crate source supplies the claims above. Useful
API references: [redis AsyncConnectionConfig](https://docs.rs/redis/1.7.1/redis/struct.AsyncConnectionConfig.html)
and [Smithy Intercept](https://docs.rs/aws-smithy-runtime-api/1.18.0/aws_smithy_runtime_api/client/interceptors/trait.Intercept.html).
No dependency was changed or tested. Reopen these facts if Cargo.lock or the cited
paths change, or a focused proof contradicts the described ownership.

The selected local values (S3 nonstreaming reply envelope 1 MiB, Redis 256 outstanding commands,
publication 64 MiB wire window) are explicit Definition policy choices anchored
in existing template scales, not externally mandated limits. No general hard-RSS
claim is admitted. Technical Design compares only supported controls, std/library
writer primitives, minimal adapter glue, and the confirmed same-version dependency
lifetime repair needed to meet the accepted behavior. Native APIs remain the first
choice; the confirmed async-nats gaps make the narrow source repair necessary. No
new general-purpose library or resource manager is warranted by this evidence.
