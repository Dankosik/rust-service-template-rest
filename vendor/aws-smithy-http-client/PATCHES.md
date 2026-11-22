# aws-smithy-http-client 1.5.1 TCP timeout propagation and pooled request budgets

Upstream reviewed: 1.5.2

The published `aws-smithy-http-client-1.5.1.crate` archive has SHA-256
`aeca69f65362f7375891008ea525fd440ad6db1836458ac188c87b126804114e` from
[smithy-rs](https://github.com/smithy-lang/smithy-rs). Upstream `1.5.2` adjusts
connection-pool partitioning while still omitting TCP `connect_timeout`
propagation in `src/client.rs`.

`src/client.rs` forwards `HttpConnectorSettings::connect_timeout` to Hyper's TCP
connector so the shared `hyper-util` same-family candidate race bounds per-socket
TCP dials inside Smithy's outer DNS/TCP/TLS connect timer (`Cargo.toml` selects
the local `hyper-util` patch for `just native-transport`, and
`tests/regen-certificates.sh` qualifies cleanup globs for ShellCheck). SDK
retries, operation deadlines, and error classification remain unchanged.

The object storage Rustls client opts into `Builder::per_request_connect_timeout`.
This mode keys the legacy Hyper transport cache by the read setting and applies
the exact connect duration and selected runtime sleep to the call's connector
clone. The existing Hyper checkout and background warmup retain ownership of
that connector. The outer Smithy timer retains timeout classification. Only the
inner connector call and polls scope the native TCP timeout. Ordinary builders
keep their static transport settings.

Retire this vendor directory, root Cargo patch/exclusion, Docker context and
cooked-layer copy, and `object-storage` profile removal entries together when an
upstream release propagates TCP `connect_timeout` with equivalent candidate
fallback and error classification and supports varying exact connect durations
and runtime timers on a shared pool with equivalent dial custody.
