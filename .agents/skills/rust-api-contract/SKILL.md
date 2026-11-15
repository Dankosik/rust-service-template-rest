---
name: rust-api-contract
description: "Observable contract. Use when a REST or OpenAPI change can alter status, body shape, error code, security, or client compatibility."
---

# Rust API Contract

**Observable contract.** Identify every client-visible distinction—success and failure status, body shape or absence, media type, headers, defaults, security, ordering, and retry outcome—and trace the change through both runtime execution and the published OpenAPI document.

**Code-first authority.** Keep `utoipa` annotations and types composed in `crates/service/src/api.rs` as the single source of truth, regenerate `api/openapi/service.yaml` with `make openapi-generate`, and inspect the schema diff alongside the drift test. Align operation and schema doc comments with actual served behavior because they ship as product documentation.

**Security declarations.** Evaluate security across inherited defaults and explicit route overrides, verifying both the OpenAPI scheme and the runtime enforcement path under the active authentication profile. Preserve intentionally public operations and route unsupported or ambiguous auth combinations through the existing finalizer.

**Pre-handler and handler errors.** Trace rejections across extractors, admission, authentication, timeouts, fallbacks, and streaming as well as handler `Result` paths. Keep status, Problem media type, schema, stable code, field omission versus `null`, and sanitized details aligned between `crates/service-failure` and `crates/infra-http`, documenting every reachable changed outcome.

**Compatibility and evidence.** Judge breaking risk from consumer impact—remembering that new response variants can break exhaustive clients—while preserving ordering, defaults, absence semantics, and idempotency replay rules. Validate compatibility through the repository's breaking-change checker (recording any approved exception with its owner and removal condition), keep the API surface scoped to consumer needs, and report schema checks separately from runtime and consumer proof.
