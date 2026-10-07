# Goal
status: ready
Completion: The accepted outbound transport corrections and operator guidance are assembled, pass the matching local build/tests and documentation checks, and receive final independent delivery review. The separate PR is published with its actual selected CI results recorded; merge and deployment are excluded.
Global constraints: [Intent](intent.md), [Specification](spec.md), [Technical Design](design/transport.md), [planning execution boundary](tasks/execution-boundary.md), and the repository [Validation budget](../../AGENTS.md#validation-budget) govern all units. Code and tests are implemented together; validation and delivery review occur once after assembly.

## Tasks

- [x] T1: Admitted PostgreSQL destinations preserve identity and give every resolved TCP candidate an opportunity within the existing caller deadline.
  - Depends on: none
  - Provides: IPv6 normalization and SQLx candidate selection with unchanged pool/finality ownership.
  - Packet: [T1](tasks/T1-postgres.md)
  - Result: Implemented after the test-only `Box::pin` repair for Clippy's large future diagnostic. Native owner: `/root/transport_postgres`; current PostgreSQL test SHA256 `97916f8ac7ef1cd361227bf0173ed86d1d809968a6f01440358d9f0d711aece4`. At published `7a337196d0cd44b998a4bb19ff1816402523a8cc`, build, workspace tests, native SQLx 3/3 and real database integration passed; the repaired lint and current test await selected CI. Production code, deadline and assertions are unchanged by this repair.
- [x] T2: A reusable lazy gRPC client applies its native cooperative five-second dial timeout and can subsequently reconnect without renewing that deadline across idle polling.
  - Depends on: none
  - Provides: Native bounded lazy connector, regression coverage and accurate gRPC guidance.
  - Packet: [T2](tasks/T2-grpc.md)
  - Result: Implemented against the reviewed cooperative gRPC clarification; verification pending assembled Completion. Native owner: `/root/transport_grpc`; bounded four-file manifest SHA256 `d00d8b9e8b5cd7006c4c8e8416df83b0ba7eb995c06d0e05a4e513e386397a3b`.
- [x] T3: Authentication-provider connection attempts finish within two seconds under the existing three-second total deadline.
  - Depends on: none
  - Provides: Native connection sub-budget, preserved auth policy and accurate provider guidance.
  - Packet: [T3](tasks/T3-auth.md)
  - Result: Implemented; verification pending assembled Completion. Native owner: `/root/transport_auth`.
- [x] T4: NATS recovery and termination are bounded by their existing deadlines and discovery stays within the configured trust boundary.
  - Depends on: none; shared carrier mutation is exclusive with T5 as recorded in both packets.
  - Provides: Native attempt/close ownership, TLS-first configuration, coherent messaging vendor/profile/delivery custody and operator migration guidance.
  - Packet: [T4](tasks/T4-nats.md)
  - Result: Implemented after bounded CI repair; verification pending selected post-repair CI. Native owner: `/root/transport_nats_s3`; repaired five-file manifest SHA256 `698fae3678864230da698e06d0bd94a3750e67053ab63aec47e680ad2b3b07ea`. The separately reviewed security/profile repair digest is `9a96214db0de04bb31e411d9c59ecc12e4fac932ebd7049d30bd60398268ad68`.
- [x] T5: S3's native TCP candidates share the existing connect budget without changing SDK retries or mutation finality.
  - Depends on: none; shared carrier mutation is exclusive with T4 as recorded in both packets.
  - Provides: Native Smithy timeout propagation and coherent object-storage vendor/profile/delivery custody.
  - Packet: [T5](tasks/T5-s3.md)
  - Result: Implemented after the refused-port fixture repair; verification pending selected post-repair CI. Native owner: `/root/transport_nats_s3`; two-file manifest SHA256 `50dd8d5b418ef3b941cce53814ad50a30b8dd1ea3e9ed4a74e1599ad215d1c4c`. Corrected native filter passed 1/1; removing only timeout propagation failed the healthy-second scenario. Exact restoration and rebuilt positive receipt: `/tmp/transport-smithy-negative-control.json`. Production timeout code and classification assertions are unchanged by this repair.
- [x] T6: Outbound HTTP and Redis operator guidance accurately describes their preserved DNS, socket and trust-material lifetimes.
  - Depends on: none
  - Provides: Source-grounded documentation correction without transport changes.
  - Packet: [T6](tasks/T6-preserved-transports.md)
  - Result: Implemented; verification pending assembled Completion. Native owner: `/root/transport_preserved_docs`.
