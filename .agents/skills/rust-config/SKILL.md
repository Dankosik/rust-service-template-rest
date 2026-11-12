---
name: rust-config
description: "One owner per key. Use when a Rust service configuration key, default, validation rule, secret source, or file-versus-environment precedence needs implementation or review."
---

# Rust Config

**One owner per key.** Trace a configuration value from its declaration and sources through precedence, parsing, validation, and the runtime consumer. Keep each default and rule with the owner that knows its meaning, while making dependent validation explicit. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Use the current project's configuration authority. In this service, `crates/config` owns source order, namespaces, blank handling, secrets, and section conventions: `load.rs` and `secret_policy.rs` implement them and their tests state them. Defaults live beside their section type and may be derived or implemented. A rule spanning sections belongs to the dependent owner; a process-level relationship belongs to composition. Do not copy those defaults into a second registry or assume that every invariant belongs in the loader.

Establish what omitted, empty, malformed, duplicated, and unknown input mean at the accepted boundary. An empty override may deliberately unset a value, or may be invalid; it must not silently become a lower-precedence fallback unless that is the contract. Preserve distinctions between source selection, decoding, and validation. Use types that express the runtime meaning and retain human-readable forms where the existing loader supports them. Do not add stricter parsing, a new source, or a compatibility fallback without a current requirement.

A new key needs a consumer and a justified default or explicit requirement to supply it. Validate the relationships the runtime relies on, including units and cross-field bounds, before the dependent effect. Do not turn every technical choice into a new setting. When an existing value or precedence changes, inspect the affected consumer, deployment assumptions, and failure diagnostic; a successful parse does not prove the value is usable. Follow the project's reload or snapshot policy rather than reading mutable ambient configuration from arbitrary call sites.

Treat secret storage, source admission, and diagnostic redaction as separate obligations. A secret wrapper does not ensure that a file pre-scan recognizes a new field, nor that decoding errors avoid echoing its raw value. This service's pre-scan recognizes key names; use a recognized name and preserve its approved variable carriers and redacted type. Keep startup summaries, Debug, errors, telemetry, and invalid-input paths within the same disclosure boundary. The current policy also owns SDK environment exceptions; do not infer permission from a provider's default behavior.

For review, explain a concrete precedence, default, validation, or disclosure consequence without editing. For implementation, extend the current owner, update policy when its governed behavior changes, and use focused loading or validation proof for the changed risk. Reuse adequate tests instead of requiring a fresh pair of tests or a policy paragraph for every field. Report which source and failure cases were exercised; do not create a parallel configuration framework or broaden the project's validation budget.
