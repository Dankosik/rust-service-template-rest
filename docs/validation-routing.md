# Validation Routing

[AGENTS.md](../AGENTS.md#validation-budget) owns ordinary local completion.
Run `make help` to see the verification commands available for this checkout
and its selected profiles. CI and `make verify` use the same
[surface classifier](../scripts/verify/surfaces.py).

## Policy rules beyond `make help`

- **Generated and embedded contracts:** Never hand-edit generated OpenAPI
  (`api/openapi/service.yaml`), generated protobuf Rust (`crates/grpc-contracts`),
  applied SQL migrations (`migrations/*.sql`), or checked `.sqlx/` query
  metadata; regenerate or roll forward through their canonical `make` targets.
- **Gate waivers:** Every exception in [`deny.toml`](../deny.toml),
  [`.gitleaks.toml`](../.gitleaks.toml), [`.gitleaksignore`](../.gitleaksignore),
  [`.jscpd.json`](../.jscpd.json), [`quality/architecture.json`](../quality/architecture.json),
  or inline lint directives requires exact scope (exact current path and
  detected bytes in `.gitleaks.toml`; keep historical paths out of
  `.gitleaks.toml`), a non-code reason, and a reopen/removal condition; never
  exclude whole source trees or `.ci` outputs.
- **CI vs CI-less repositories (`make plan` / `make verify`):** By default
  (`github-actions`), [`scripts/verify/runner.py`](../scripts/verify/runner.py)
  leaves heavy container/integration proofs and the source initializer matrix to
  CI (`status: partially_verified`), caching reusable local receipts under
  `<git-common-dir>/codex/verify` (`VERIFY_FORCE=1` bypasses reuse; failures
  never auto-clean caches). Outside CI, `make shellcheck` and the link portion
  of `make docs-check` use pinned Docker containers when Docker is running and
  fall back to host `shellcheck` and `lychee` when it is unavailable. Setting
  `REPOSITORY_CI ?= none` in
  [`make/service.mk`](../make/service.mk) runs heavy and CI-only checks locally
  under `ALLOW_HEAVY=1` and `ALLOW_FULL=1` (`ci_mode: none`).
- **Execution guards:** `ALLOW_FULL=1` and `ALLOW_HEAVY=1` are explicit opt-ins
  for full-repository and container/history-wide targets; CI sets `CI=true`, so
  never set `CI=true` locally.
