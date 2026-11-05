---
name: rust-api-contract
description: "Observable contract. Use when a Rust service REST change can alter what a deployed client distinguishes across status, body shape, error code, security, or compatibility, or when the OpenAPI document and the handlers must stay in agreement."
---

# Rust API Contract

**Observable contract.** Identify every distinction the affected client can observe: success and failure status, body shape or absence, media type, headers, defaults, security, ordering, and the outcome of a retry. Follow the change through both the runtime path and its published description. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Establish the contract's source of truth before editing. Some projects generate a document from code and others generate code from a document; do not create a second manual authority. In this service, [HTTP architecture](../../../docs/architecture/http.md#the-contract) owns the code-first utoipa composition, generated OpenAPI file, finalization, and regeneration commands. Update the owning annotations or types and inspect the generated diff. Operation and schema documentation becomes product output, so its meaning must agree with the served behavior.

Resolve security from the complete served contract, including inherited defaults and explicit overrides. An omitted declaration and an explicit public override can mean different things. Follow the retained authentication profile's rules and check the runtime enforcement path; a declared scheme does not enforce authorization. Preserve existing public operations, and reject ambiguous or unsupported combinations through the current finalizer rather than introducing placeholder authentication. Optional authoring metadata must agree when present without becoming an additional authority.

Trace failures that happen before the handler as well as those it returns. Extractors, admission, authentication, timeouts, fallbacks, and response streaming may have different owners. In this service, service-failure owns the closed failure catalog and infra-http owns its HTTP projection. Keep status, media type, schema, stable code, omission versus null, and sanitized details aligned at that boundary. An unused catalog entry does not by itself require a new route or response; a reachable changed outcome does require an accurate contract.

Judge compatibility from the affected consumer and the artifact it uses. Input and output changes can have different consequences: a new response alternative can break exhaustive consumers even when the server considers it additive. Preserve accepted ordering, defaults, absence semantics, and replay behavior. Use the existing compatibility checker and approved exception mechanism, with the required owner and removal condition, rather than treating a syntactically valid schema as proof of compatibility. Do not widen the API merely to make an implementation easier.

For review, state the old and accepted observables, the client consequence, and a concrete example that could falsify agreement without editing. For implementation, regenerate the governed artifact and use the project's selected contract checks. Reuse adequate coverage and add proof for material changed outcomes at the actual mounted boundary, including failure paths the handler alone cannot observe. Report schema checks separately from runtime and consumer evidence. A new test per status, client generator, or publication step is not required solely because this method mentions it.
