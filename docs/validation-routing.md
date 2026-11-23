# Validation Routing

[AGENTS.md](../AGENTS.md#validation-budget) owns ordinary local completion.
Run `just` to see every command available for this checkout (including
`just lint-changed`, `just test-changed`, and `just test-budget`), and
`mise install` to install the pinned CLI tools from [`mise.toml`](../mise.toml).
CI routes jobs via [`.github/actions/detect-changes/detect.py`](../.github/actions/detect-changes/detect.py)
and scopes PR Rust checks via [`scripts/checks/rust_selection.py`](../scripts/checks/rust_selection.py).

## Policy rules beyond `just`

- **Generated and embedded contracts:** Never hand-edit generated OpenAPI
  (`api/openapi/service.yaml`), generated protobuf Rust (`crates/grpc-contracts`),
  applied SQL migrations (`migrations/*.sql`), or checked `.sqlx/` query
  metadata; regenerate or roll forward through their canonical `just` recipes.
- **Gate waivers:** Every exception in [`deny.toml`](../deny.toml),
  [`.gitleaks.toml`](../.gitleaks.toml), [`.gitleaksignore`](../.gitleaksignore),
  [`quality/architecture.json`](../quality/architecture.json),
  or inline lint directives requires exact scope (exact current path and
  detected bytes in `.gitleaks.toml`; keep historical paths out of
  `.gitleaks.toml`), a non-code reason, and a reopen/removal condition; never
  exclude whole source trees or `.ci` outputs.
