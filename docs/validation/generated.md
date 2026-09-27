# Generated Contract Validation

Edit the canonical source and regenerate as implementation work. The
committed document is the reviewed authority; generated files are evidence,
never the edit owner.

| Authority | Source | Generate | Prove |
| --- | --- | --- | --- |
| OpenAPI (`api/openapi/service.yaml`) | `#[utoipa::path]` attributes and schema derives on the handlers | `make openapi-generate` | `make openapi-check` (Redocly lint plus the drift and contract tests) |
<!-- template:begin grpc:docs-validation-grpc-generated -->
| Protobuf (`crates/grpc-contracts/src/generated`, embedded descriptor) | `api/proto/`, pinned `buf.lock` and locked `tools/grpc-codegen` | `make grpc-generate` | `make grpc-check`: Buf format/lint, repeat generation, committed drift and exact PR-base FILE compatibility |
<!-- template:end grpc:docs-validation-grpc-generated -->

The service's local command and CI owners define ordinary drift and explicit
compatibility verification, including any base-document and approval policy.
The service's local HTTP architecture owns the contract workflow.
