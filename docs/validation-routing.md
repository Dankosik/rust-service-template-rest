# Validation Routing

[AGENTS.md](../AGENTS.md#validation-budget) owns ordinary local completion.
Run `make help` to see every command available for this checkout. CI routes
jobs via [`.github/actions/detect-changes/detect.py`](../.github/actions/detect-changes/detect.py).

## Policy rules beyond `make help`

- **Generated and embedded contracts:** Never hand-edit generated OpenAPI
  (`api/openapi/service.yaml`), generated protobuf Rust (`crates/grpc-contracts`),
  applied SQL migrations (`migrations/*.sql`), or checked `.sqlx/` query
  metadata; regenerate or roll forward through their canonical `make` targets.
- **Gate waivers:** Every exception in [`deny.toml`](../deny.toml),
  [`.gitleaks.toml`](../.gitleaks.toml), [`.gitleaksignore`](../.gitleaksignore),
  [`quality/architecture.json`](../quality/architecture.json),
  or inline lint directives requires exact scope (exact current path and
  detected bytes in `.gitleaks.toml`; keep historical paths out of
  `.gitleaks.toml`), a non-code reason, and a reopen/removal condition; never
  exclude whole source trees or `.ci` outputs.
- **Container linters locally:** `make shellcheck` and `make docs-check` use
  pinned Docker containers when Docker is running and fall back to host
  `shellcheck` / `lychee` when Docker is unavailable.
