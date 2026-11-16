# hotpath 0.28.4: supported MCP dependency repair

Upstream reviewed: 0.31.2

The published `hotpath-0.28.4.crate` archive has SHA-256
`92f8d56370b4cf47b04cf873315d2d0a226a3766ca74f5131521b7aa8bc3d41f` and records
upstream revision `5bbfbe099458074678b5f7758ad13a5710fa686e` (`crates/hotpath`).
Cooldown-eligible upstream releases through `0.31.2` still require `rmcp ^1.4`
while the `0.28.4` profiler-version pin stays in effect.

Published `hotpath 0.28.4`'s optional MCP feature resolves `rmcp 1.x`, which is
affected by [GHSA-33f5-2c5q-wgwj](https://github.com/advisories/GHSA-33f5-2c5q-wgwj)
and [GHSA-9pj6-vhgr-3mwh](https://github.com/advisories/GHSA-9pj6-vhgr-3mwh);
fixed `rmcp 2.1.0` cannot be selected by Cargo under `^1.4`. This override sets
`dependencies.rmcp.version = "=2.1.0"` in `Cargo.toml` and adapts the renamed
content import in `src/mcp_server.rs` without activating in default builds or
changing profiler behavior.

Retire this vendor directory, root Cargo patch/exclusion, Docker context and
cooked-layer entries, and classifier case together when the profiler pin permits
a maintained `hotpath` release with a fixed `rmcp` dependency (or an official
`0.28.4`-compatible remedy ships) and `make profiling-check` passes without
`rmcp 1.x` in `Cargo.lock`.
