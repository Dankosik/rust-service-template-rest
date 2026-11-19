# T5 — Prepare equivalent remote comparison and evidence capture

Outcome:
One task-local experiment overlay can drive the same public call workloads on the original baseline and later candidates, isolate fixture and caller work, and export raw observations and resource identities for the applied comparison protocol.

Depends on:
- none

Sources:
- [R7 and R8](../spec.md#r7--conditional-alternatives), [D7 and D8](../design/design.md#d7--isolate-conditional-changes-from-the-base-candidate), [comparison protocol](../design/comparison.md) — required workloads, isolated variants, authority, saved evidence and stopping conditions.
- `crates/infra-oauth2-client-credentials/src/credentials.rs::{Registration,RefreshDriver,ServiceTokens,ExchangeTokens}` and `src/{http_client,grpc}.rs`; `crates/infra-outbound-http/src/lib.rs::{Client,PreparedRequest}`; `crates/infra-outbound-grpc/src/client.rs::{Client,PreparedCall}` — existing callable seams on baseline and candidate. `crates/infra-outbound-http/src/transport.rs::platform_tls_config` owns HTTP trust at the public TLS path.
- `rustls-platform-verifier@0.7.1/src/verification/others.rs` and `rustls-native-certs@0.8.4/src/lib.rs::load_native_certs` — the locked Linux verifier loads roots from `SSL_CERT_FILE` before the public HTTP client config is initialized. `crates/infra-outbound-grpc/src/client.rs::{ClientSecurity,ClientTlsMaterial,GrpcConnector::call}` — public gRPC fixture CA input and required h2 ALPN.
- `crates/infra-outbound-http/src/{lib,policy,transport}.rs::{buffer_response,Origin::admits,build_transport}`, `crates/shared-rules/src/uri.rs::http_origin_port`, `vendor/hyper-util/src/client/legacy/client.rs::{Client::try_send_request,extract_domain,Builder::set_host}` — variant surfaces, configured Host and unchanged URI/pool/retry behavior.
- `hyper@1.12.0/src/proto/h1/io.rs::MemRead::read_mem`, `bytes@1.12.1/src/bytes_mut.rs::BytesMut::split_to`, `http-body-util@0.1.5::Limited` — visible bytes and backing storage are different bounds. `parking_lot@0.12.5::RwLock` and the existing service-cache owner in `credentials.rs` — shared selection versus exclusive scheduling.
- `Cargo.toml` release and dependency settings, `Cargo.lock`, `rust-toolchain.toml` — equivalent locked toolchain and build inputs.

Provides:
- Reusable comparison overlay for matched remote proof and bounded teardown

Boundary:
- In scope: `specs/outbound-call-perf/experiment/` only. Prepare a shared driver and fixtures, comparison controls and off-host collection/recovery procedure. Match builds and instrumentation. Use a synthetic CA and identity with a SAN matching each loopback URI. Start each Linux caller with the fixture CA in `SSL_CERT_FILE` before creating the public HTTPS token and resource clients. Configure the gRPC fixture CA through `ClientSecurity::Tls` with `ClientTlsMaterial::ca_certificate_pem` and serve h2 ALPN. Make active refresh distinguishable by a positively observed replacement grant with its response held while the old token remains eligible. Support each remaining workload and variant in the accepted comparison, including raw counts, CPU and p99, response work, live retained memory or connections where relevant. Keep experimental switches and source overlays outside production configuration and the locked product graph.
- Out of scope: Creating or contacting the cloud target, running any performance measurement, editing shipping code, and adopting variants. The Delivery Owner executes comparison and teardown on the one authorized Droplet and routes any chosen product change to its owning surface. Unconditional first-frame retention is comparison-only under the accepted memory bound. Fixed-URL pool identity stays unchanged pending the D7 reopen condition.

Mutable owners:
- `specs/outbound-call-perf/experiment/` comparison preparation.

Exclusive locks:
- none

Final validation:
- Observable claim: Starting from either frozen source tree, the prepared overlay drives identical public call workloads with attributable caller CPU and explicit offered/completed, provider, resource, and status or byte receipts. It distinguishes all four single/64-way service-hit windows before and during positively confirmed early refresh, and separates demand waiting, exchange, HTTP and gRPC scenarios and variant-specific resource evidence. The base checkout has no such experiment entry point. Preserve R8 equivalence and G1–G5 workload boundaries.
- Additional proof: For Completion, the Delivery Owner records all five R7 dispositions and runs the R8 [matched comparison and verified teardown](../design/comparison.md) after assembled review. Retain raw and summarized evidence off the target.

Reopen if:
- Intake owns an unavailable CPU-optimized target or an off-host cleanup path or budget that fails the pre-create bounds. Technical Design owns a demonstrated mixed-authority workload with compatible connection evidence, or a simple source-verified enforcement of the retained-response backing bound with qualifying matched evidence.
