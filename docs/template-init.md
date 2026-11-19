# Service Initialization

Initialize a clean template checkout once to assign service identity, retain only
the selected capability and agent-harness profiles, and delete `template/`:

```sh
cargo fetch --locked
make template-init \
  SERVICE_NAME=catalog-api \
  REPOSITORY=https://github.com/example/catalog-api \
  DESCRIPTION='Catalog API' \
  CODEOWNER=@example/platform \
  AGENT_HARNESS=claude
```

Optional profile flags (all default to `none`, except `AGENT_HARNESS=all`):
`DATABASE=postgres`, `AUTHN=oidc-jwt|oidc-introspection`, `OUTBOUND_HTTP=bounded`,
`OUTBOUND_AUTH=oauth2-client-credentials`, `GRPC=enabled`, `HTTP_IDEMPOTENCY=postgres`,
`JOBS=postgres`, `MESSAGING=nats-jetstream`, `OUTBOX=postgres`, `WEBHOOKS=durable`,
`INBOUND_WEBHOOKS=standard-webhooks`, `CACHE=redis`, `OBJECT_STORAGE=s3`,
`SERVICE_AUTH=jwt`, and `AGENT_HARNESS=core|codex|claude|qwen|cursor|grok|opencode|antigravity|all`.

`template/init.py` validates inputs, strips `# template:begin/end` blocks, applies
service identity, deletes `template/` and unselected profiles, prunes unused
`[workspace.dependencies]` and `Cargo.lock` offline, formats Rust code,
regenerates `api/openapi/service.yaml`, and materializes the selected agent
harness without staging or committing. After initialization the repository is a
standalone Rust service owning all its files directly.
