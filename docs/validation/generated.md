# Generated Contract Validation

Edit the canonical source and regenerate as implementation work. The
committed document is the reviewed authority; generated files are evidence,
never the edit owner.

| Authority | Source | Generate | Prove |
| --- | --- | --- | --- |
| OpenAPI (`api/openapi/service.yaml`) | `#[utoipa::path]` attributes and schema derives on the handlers | `make openapi-generate` | `make openapi-check` (Redocly lint plus the drift and contract tests) |

The service's local command and CI owners define ordinary drift and explicit
compatibility verification, including any base-document and approval policy.
The service's local HTTP architecture owns the contract workflow.
