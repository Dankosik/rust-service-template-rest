# hotpath 0.28.4: supported MCP dependency repair

This excluded dependency preserves the user's exact profiler version0.28.4.
It is copied from the published crates.io source:
https://static.crates.io/crates/hotpath/hotpath-0.28.4.crate,
archive SHA256 `92f8d56370b4cf47b04cf873315d2d0a226a3766ca74f5131521b7aa8bc3d41f`.
Published VCS revision `5bbfbe099458074678b5f7758ad13a5710fa686e`,
path `crates/hotpath`; the MIT license is declared by the published manifest.
The published macros dependency, build script, CLI sources and tests remain
unchanged. Registry's `.cargo-ok` cache marker, unused dependency Cargo.lock
and original unnormalized Cargo.toml.orig are omitted. The normalized manifest
is serialized through yq; its sole semantic change is
`dependencies.rmcp.version = "=2.1.0"`. The sole library source adaptation is
`model::{ContentBlock as Content, *}` in `src/mcp_server.rs`, preserving all
existing text-tool payloads through RMCP2.1's new content name.

## Why the override is necessary

The published optional dependency requires rmcp1.4 and resolves1.8.0. All1.x
versions are affected by high-severity
[OAuth resource spoofing](https://github.com/advisories/GHSA-33f5-2c5q-wgwj) and
[Streamable HTTP session leakage](https://github.com/advisories/GHSA-9pj6-vhgr-3mwh).
The [official2.0 release](https://github.com/modelcontextprotocol/rust-sdk/releases/tag/rmcp-v2.0.0)
fixes both; [2.1.0](https://github.com/modelcontextprotocol/rust-sdk/releases/tag/rmcp-v2.1.0)
also fixes the medium redirect header leak. GitHub's advisory query for
rmcp2.1.0 returned no applicable advisories on2026-10-05.

Supported Cargo resolution cannot choose2.x under hotpath's1.x requirement.
Upgrading hotpath violates the fixed0.28.4 measurement-tool constraint;
removing MCP loses the selected profiling capability. A narrow published-source
manifest override preserves those requirements without recreating the profiler,
forking rmcp or suppressing an advisory. The current release3.5.0 would add
unrelated major migration;2.1.0 is the smallest released fixed line checked.

The MCP endpoint continues to bind127.0.0.1, retains hotpath's optional token
middleware, tools and profiler identity. Default builds do not activate this
dependency. Compatibility must be established by locked feature builds and
an actual local-only MCP initialize/tools query on the remote CI runner.
Historical measurements use their original source/version; they are not
relabelled as measurements of this patched dependency graph.

## Delivery and retirement

The root Cargo patch, exclusion, Docker context/cooked source and classifier
carry this dependency on every retained template profile. The package is outside
the workspace and has no new application or public API responsibility.
Dependency Review and cargo-deny remain required. The generated merged lock
must remove rmcp1.x rather than keep it beside2.1.0.

Retire this package/patch/exclusion/image copy together when the user's profiler
version constraint permits a maintained release with a fixed rmcp dependency,
or an official0.28.4-compatible remedy becomes available. Keep the regression
proof for default/profile compilation and MCP compatibility. Root owns the
temporary patch; no automatic upstream publication is selected.
