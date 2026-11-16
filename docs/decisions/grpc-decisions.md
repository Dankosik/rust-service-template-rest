# Native gRPC decisions

`crates/infra-grpc`, `crates/grpc-contracts`, `crates/config`, and `Cargo.toml`
own the supported API, limits, metric names, and dependency pins (with the
client status hook in [the tonic patch](../../vendor/tonic/PATCHES.md)). This
record keeps only rejected alternatives, protocol constraints, measurements, and
reopen conditions.

| Choice / Rejected alternative | Decisive evidence | Reopen condition |
| --- | --- | --- |
| Tonic `Routes` on the shared HTTP listener over `tonic::transport::Server` | Reuses one bounded accept loop, connection cap, protocol sniff, and graceful drain instead of maintaining a parallel listener. | A gRPC listener policy cannot be expressed on the shared Hyper listener. |
| Handler validation + `tonic-types` (`google.rpc.ErrorInfo`) over `protovalidate` or a generated policy proxy | Avoids a 0.x CEL runtime and duplicate dispatch proxy when cross-language proto constraint evaluation is not required; `RESOURCE_EXHAUSTED` prevents retry storms that `UNAVAILABLE` triggers. | Cross-language declarative proto constraints become an accepted requirement. |
| Committed `tonic_prost_build` over Buf descriptor sets (with imports) vs `build.rs`, remote Buf plugins, or `prost-reflect` | Avoids codegen during ordinary builds and external plugin registries; `prost` panics on imported messages if `--exclude-imports` strips their descriptors. | Stock codegen cannot emit a required trait surface. |
| 2 KiB initial codec buffers over Tonic's 8 KiB default or 1 KiB | On 2,000 held bidi streams (DigitalOcean c-4), peak server RSS dropped from 20.9 MB to 12.4 MB with no CPU regression; 1 KiB cost +1.4% instructions on 1 KiB messages. | Typical contract messages exceed 2 KiB, or Tonic changes buffer growth. |
| Fixed HTTP/2 receive windows (8 MiB connection / 2 MiB stream) + `TCP_NODELAY` over adaptive windows | `h2` resets connections with `ENHANCE_YOUR_CALM` after ~140 unread frames < 256 B under the 65,535 B initial adaptive window; `TCP_NODELAY` cut 4 KiB unary latency on a 10 ms RTT loopback from 61 ms to 10.5 ms. | Bandwidth-delay product exceeds 2 MiB/stream, or `h2` decouples small-frame reset budgets from initial window size. |
| Bounded request-body drain on auth/shed rejection over immediate reply | Answering before reading in-flight `DATA` causes `h2` stream resets; past 1,024 resets Hyper sends `GOAWAY` (measured 16–1,250 collateral failed calls per 5 s auth-failure flood vs 0 with bounded drain). | Hyper exposes server reset-stream retention or stops counting late `DATA` toward `GOAWAY`. |
| Thin server `tracing` span + `grpc-ecosystem` Prometheus series over `tracing_opentelemetry_instrumentation_sdk` | JSON logging serializes span fields on creation and every `Span::record`; the thin span cut 15% of instructions and 20 allocations per unary call while preserving standard `go-grpc-middleware` dashboard queries. | OpenTelemetry `rpc.*` metric/span conventions reach stability and replace `grpc-ecosystem` dashboards. |
| Client HTTP/2 keepalive PING at 60 s (active calls only), single channel per service/mesh address, no compression, startup-read PEM | Faster idle pings trigger `GOAWAY too_many_pings` on `grpc-go` / `grpc-java` / C-core servers; service meshes own endpoint discovery; same-network compression rarely repays CPU. | Direct headless DNS balancing, payload compression, or sub-restart cert reload becomes required. |
