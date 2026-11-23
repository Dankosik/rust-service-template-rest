# Service Initialization

Initialize a clean template checkout once to assign service identity, retain only
the selected capability and agent-harness profiles, and delete `template/`:

```sh
cargo fetch --locked
just template-init \
  --service-name catalog-api \
  --repository https://github.com/example/catalog-api \
  --description 'Catalog API' \
  --codeowner @example/platform \
  --agent-harness claude
```

Optional profile flags (all default to `none`, except `--agent-harness all`;
matching uppercase env vars also work):
`--database postgres`, `--authn oidc-jwt|oidc-introspection`,
`--outbound-http bounded`, `--outbound-auth oauth2-client-credentials`,
`--grpc enabled`, `--http-idempotency postgres`, `--jobs postgres`,
`--messaging nats-jetstream`, `--outbox postgres`, `--webhooks durable`,
`--inbound-webhooks standard-webhooks`, `--cache redis`,
`--object-storage s3`, `--clickhouse http`, `--service-auth jwt`, `--temporal sdk`, and
`--agent-harness core|codex|claude|qwen|cursor|grok|opencode|antigravity|all`.

`template/init.py` validates inputs against `template/profiles.json`, strips
`# template:begin/end` blocks, applies service identity, deletes `template/` and
unselected profiles, prunes unused `[workspace.dependencies]` and `Cargo.lock`
offline, formats Rust code, regenerates `api/openapi/service.yaml`, and
materializes the selected agent harness without staging or committing. After
initialization the repository is a standalone Rust service owning all its files
directly.
