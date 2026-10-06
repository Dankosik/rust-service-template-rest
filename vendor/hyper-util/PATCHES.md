# hyper-util 0.1.21: same-family TCP opportunity

This source is the published `hyper-util-0.1.21.crate` archive. Its archive
SHA256 is
`ddc03d96684f9226b8a787cdb71488417b53ab5ea8fdb1dac946cb9431cc8bff`;
the archive provenance revision is
`23a868965964c1d6bb1b94f30ba4c420a4bebe7c`.

`Cargo.toml.orig` preserves the normalized published manifest. The patched
manifest changes only `client-legacy` to enable the already declared
`futures-util/alloc` feature needed for `FuturesUnordered`. The runtime patch
is confined to `src/client/legacy/connect/http.rs`.

When the public Happy Eyeballs option is enabled, each existing same-family
candidate is started concurrently with its existing divided TCP timeout. The
first successful socket wins, and returning from the race drops every loser
before TLS or protocol opening. All-failure reporting keeps the resolver-order
first error. Family preference and fallback delay remain Hyper-owned. When the
public option is disabled, candidate scheduling remains serial.

The patch does not add a resolver, a timer, a task, retry policy, or public
connector API. It preserves hostname/IP identity, socket options and the
existing connector limits. The selected native regression is
`client::legacy::connect::http::tests::same_family_candidate_race_preserves_late_dns_and_caller_budget`:
it holds a first IPv4 candidate pending in a loopback listen queue, spends
caller preparation and delayed DNS time under one original cutoff, and requires
the healthy later address to progress before the static serial share. It also
checks that explicitly disabling Happy Eyeballs retains serial behavior.

Retire this vendor source when a published Hyper utility release provides the
same enabled-versus-serial behavior, loser custody and resolver-order failure
semantics, and the native regression passes against the assembled production
graph. Remove the root patch/exclusion and all profile, Docker and native-proof
carriers in that same reviewed change.

## Current source and standalone graph

The returned TCP future uses precise empty lifetime capture (`use<>`), matching
its owned socket/timeout state and retaining the published Rust 1.85 minimum.
The syntax was stabilized before that minimum; no toolchain or edition changed.
The standalone lock is deliberately Cargo-authored against the exercised
production normal/build closure. The native helper records relevant features,
source/checksums and test-only differences, then requires exact nonignored test
counts. Source/type-checking evidence is not a runtime pass.

- `src/client/legacy/connect/http.rs` SHA256: `95bcdce15cf1b91886905c674825f9c94a452f6af9fb81036707a43ef366bfe6`.
- `Cargo.toml` SHA256: `992e0fd2d64affaddd18ea2b650a1a63493b5972ba691e9a2f9d6030a803af24`.
- `Cargo.toml.orig` SHA256: `ecdace19ddd41ff8f9d3c2b87d4cdc3daa1c5820d64a268817f6431ab1ba856c`.
- `Cargo.lock` SHA256: `ebc98b1920e300c8c73e7c4c931ad8478e26dc1d6a8c5ce07b59dab969f795be`.
- Standalone lock before composition: `63eec12a60cf824c88c06c001dcfc4f211f3161599c9f8ebc2aa5d4ea7278311`.
