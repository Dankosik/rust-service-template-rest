# Goal
status: ready
Completion: The accepted outbound transport corrections and operator guidance are assembled, pass the matching local build/tests and documentation checks, and receive final independent delivery review. The separate PR is published with its actual selected CI results recorded; merge and deployment are excluded.
Global constraints: [Intent](intent.md), [Specification](spec.md), [Technical Design](design/transport.md), [planning execution boundary](tasks/execution-boundary.md), and the repository [Validation budget](../../AGENTS.md#validation-budget) govern all units. Code and tests are implemented together; validation and delivery review occur once after assembly.

## Tasks

- [x] T1: Admitted PostgreSQL destinations preserve identity and give every resolved TCP candidate an opportunity within the existing caller deadline.
  - Depends on: none
  - Provides: IPv6 normalization and SQLx candidate selection with unchanged pool/finality ownership.
  - Packet: [T1](tasks/T1-postgres.md)
  - Result: Implemented; verification pending assembled Completion. Native owner: `/root/transport_postgres`. Bounded locked compile-only feedback passed for T1/T3 and the sqlx-core lib tests; no tests executed.
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
  - Result: Implemented; verification pending assembled Completion. Native owner: `/root/transport_nats_s3`; candidate manifest SHA256 `def8338d744278c63c8d9c5876b96ea41d68d796db52aceaa7385c9bc22df27a`.
- [x] T5: S3's native TCP candidates share the existing connect budget without changing SDK retries or mutation finality.
  - Depends on: none; shared carrier mutation is exclusive with T4 as recorded in both packets.
  - Provides: Native Smithy timeout propagation and coherent object-storage vendor/profile/delivery custody.
  - Packet: [T5](tasks/T5-s3.md)
  - Result: Implemented; verification pending assembled Completion. Native owner: `/root/transport_nats_s3`; bounded candidate manifest SHA256 `e2300f72ced5a7df926e5d42737dd47830ad062167a1cb1f77ac2c13962265b5`. Locked metadata passed; T4 compile-only feedback was interrupted by ENOSPC without a code result.
- [x] T6: Outbound HTTP and Redis operator guidance accurately describes their preserved DNS, socket and trust-material lifetimes.
  - Depends on: none
  - Provides: Source-grounded documentation correction without transport changes.
  - Packet: [T6](tasks/T6-preserved-transports.md)
  - Result: Implemented; verification pending assembled Completion. Native owner: `/root/transport_preserved_docs`.
