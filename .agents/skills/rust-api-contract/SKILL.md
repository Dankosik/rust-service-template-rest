---
name: rust-api-contract
description: "Observable contract. Use when a REST or OpenAPI change can alter status, body shape, error code, security, or client compatibility."
---

# Rust API Contract

**Observable contract.** Identify every distinction the affected client can observe: success and failure status, body shape or absence, media type, headers, defaults, security, ordering, and the outcome of a retry. Follow the change through both the runtime path and its published description.

In this service, the contract is code-first: `utoipa` annotations compose in `crates/service/src/api.rs`, `make openapi-generate` writes the committed `api/openapi/service.yaml`, and a test fails on drift. Do not create a second manual authority; update the owning annotations or types and inspect the generated diff. Operation and schema documentation becomes product output, so its meaning must agree with the served behavior.

Resolve security from the complete served contract, including inherited defaults and explicit overrides; an omitted declaration and an explicit public override can mean different things. Follow the retained authentication profile's rules and check the runtime enforcement path, because a declared scheme does not enforce authorization. Preserve existing public operations, and reject ambiguous or unsupported combinations through the current finalizer rather than introducing placeholder authentication. Optional authoring metadata must agree when present without becoming an additional authority.

Trace failures that happen before the handler as well as those it returns; extractors, admission, authentication, timeouts, fallbacks, and response streaming may have different owners. `crates/service-failure` owns the closed failure catalog and `crates/infra-http` owns its HTTP projection. Keep status, media type, schema, stable code, omission versus `null`, and sanitized details aligned at that boundary. An unused catalog entry does not by itself require a new route or response; a reachable changed outcome does require an accurate contract.

Judge compatibility from the affected consumer and the artifact it uses. Input and output changes can have different consequences: a new response alternative can break exhaustive consumers even when the server considers it additive. Preserve accepted ordering, defaults, absence semantics, and replay behavior. Use the existing compatibility checker and approved exception mechanism, with the required owner and removal condition, rather than treating a syntactically valid schema as proof of compatibility. Do not widen the API merely to make an implementation easier.

Report schema checks separately from runtime and consumer evidence; mentioning a status, client generator, or publication step does not make a new test per item mandatory.
