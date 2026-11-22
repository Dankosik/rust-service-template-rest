# hyper-util 0.1.21: same-family TCP opportunity and scoped dial budgets

Upstream reviewed: 0.1.21

The published `hyper-util-0.1.21.crate` archive has SHA-256
`ddc03d96684f9226b8a787cdb71488417b53ab5ea8fdb1dac946cb9431cc8bff` and records
upstream revision `23a868965964c1d6bb1b94f30ba4c420a4bebe7c`.

When Happy Eyeballs is enabled, `src/client/legacy/connect/http.rs` starts
same-family TCP candidates concurrently under their divided connect timeout so a
stalled first address cannot starve a healthy sibling; the first connected
socket wins, losing attempts drop before TLS or protocol negotiation, all-failure
keeps the resolver-order first error, and disabling Happy Eyeballs preserves
serial dialing. `Cargo.toml` enables `futures-util/alloc` on `client-legacy`
(`Cargo.toml.orig` preserves the published manifest), and `.gitignore` adds
`!/Cargo.lock` so the standalone `just native-transport` lockfile
remains tracked.

The legacy client exposes mutable access to its call-local connector clone
without replacing its shared pool. An opted-in native connector captures a
scoped TCP timeout when called and retains it through DNS and candidate dialing.
The scope lets Smithy preserve that duration through deferred connector polls.
Connectors without the opt-in use their configured static timeout.

Retire this vendor directory, root Cargo patch/exclusion, and profile, Docker,
and native-regression carriers together when an upstream `hyper-util` release
provides equivalent same-family race, loser custody, and error-order semantics
plus a way to carry exact per-request TCP budgets through pooled dispatch and
background handoff. Verify those behaviors against the production dependency graph.
