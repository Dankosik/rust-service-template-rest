# hotpath 0.28.4: supported MCP dependency repair

Upstream reviewed: 0.31.3

The published `hotpath-0.28.4.crate` archive has SHA-256
`92f8d56370b4cf47b04cf873315d2d0a226a3766ca74f5131521b7aa8bc3d41f` and records
upstream revision `5bbfbe099458074678b5f7758ad13a5710fa686e` (`crates/hotpath`).
Cooldown-eligible upstream releases through `0.31.2` still require `rmcp ^1.4`,
and `0.31.3` (`rmcp ^3.10`, published 2026-11-14) remains inside the 7-day
publish-age cooldown while the `0.28.4` profiler-version pin stays in effect.

Published `hotpath 0.28.4`'s optional MCP feature resolves `rmcp 1.x`, which is
affected by [GHSA-33f5-2c5q-wgwj](https://github.com/advisories/GHSA-33f5-2c5q-wgwj)
and [GHSA-9pj6-vhgr-3mwh](https://github.com/advisories/GHSA-9pj6-vhgr-3mwh),
and `rmcp` releases through `3.8.0` are affected by `RUSTSEC-2026-0457` (fixed
in `3.7.0`) and `RUSTSEC-2026-0462` (fixed in `3.9.0`, published 2026-11-05);
fixed `rmcp 3.9.0` cannot be selected by Cargo under `^1.4`. This override sets
`dependencies.rmcp.version = "=3.9.0"` in `Cargo.toml` and applies upstream
`0.31.3`'s three RMCP 3.x adaptations in `src/mcp_server.rs` (`ContentBlock as Content`,
`ServerConfig`, and `legacy_session_mode = true`) without activating in default
builds or changing profiler behavior.

Retire this vendor directory, root Cargo patch/exclusion, Docker context and
cooked-layer entries, and classifier case together when the profiler pin permits
a maintained `hotpath` release with a fixed `rmcp` dependency (or an official
`0.28.4`-compatible remedy ships) and `make profiling-check` passes without
`rmcp 1.x` in `Cargo.lock`.
