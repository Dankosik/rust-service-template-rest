# Generated Contract Validation

Edit the canonical source and regenerate as implementation work. The
committed document is the reviewed authority; generated files are evidence,
never the edit owner.

| Authority | Source | Generate | Prove |
| --- | --- | --- | --- |
| OpenAPI (`api/openapi/service.yaml`) | `#[utoipa::path]` attributes and schema derives on the handlers | `make openapi-generate` | `make openapi-check` (Redocly lint plus the drift and contract tests) |

`make test` already includes the drift test, so a stale document fails the
ordinary suite at the changed line. Explicit compatibility verification uses
`make openapi-breaking BASE_OPENAPI=<file>` against a readable base document;
CI runs it on pull requests against the event's exact base SHA and consults
`api/openapi/breaking-changes-approvals.txt` for accepted breaks.
[HTTP Architecture](../architecture/http.md) owns the contract workflow.
