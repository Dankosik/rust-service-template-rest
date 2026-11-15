---
name: rust-config
description: "One owner per key. Use when changing configuration keys, defaults, validation, secret sources, or file-versus-env precedence."
---

# Rust Config

**One owner per key.** Trace a configuration value from its declaration and sources through precedence, parsing, validation, and the runtime consumer. Keep each default and rule with the owner that knows its meaning, while making dependent validation explicit.

Use the current project's configuration authority: `crates/config` owns source order, namespaces, blank handling, secrets, and section conventions (`load.rs` and `secret_policy.rs` implement them and their tests state them). Defaults live beside their section type and may be derived or implemented; a rule spanning sections belongs to the dependent owner, and a process-level relationship belongs to composition. Do not copy defaults into a second registry or assume every invariant belongs in the loader.

Establish what omitted, empty, malformed, duplicated, and unknown input mean at the accepted boundary. An empty override may deliberately unset a value, or may be invalid; it must not silently become a lower-precedence fallback unless that is the contract. Preserve distinctions between source selection, decoding, and validation, use types that express the runtime meaning, and retain human-readable forms where the existing loader supports them. Do not add stricter parsing, a new source, or a compatibility fallback without a current requirement.

A new key needs a consumer and a justified default or explicit requirement to supply it. Validate the relationships the runtime relies on, including units and cross-field bounds, before the dependent effect, without turning every technical choice into a new setting. When an existing value or precedence changes, inspect the affected consumer, deployment assumptions, and failure diagnostic; a successful parse does not prove the value is usable. Follow the project's reload or snapshot policy rather than reading mutable ambient configuration from arbitrary call sites.

Treat secret storage, source admission, and diagnostic redaction as separate obligations. `secrecy::SecretString` alone neither registers a field in `crates/config/src/secret_policy.rs` key-name pre-scan nor prevents serde deserialization errors from echoing raw values; use a recognized key name and preserve approved variable carriers and redacted types. Keep startup summaries, `Debug`, errors, telemetry, and invalid-input paths within the same disclosure boundary, and respect `secret_policy.rs` for SDK environment exceptions rather than inferring permission from a provider default.

Reuse adequate loading and validation tests rather than adding a fresh pair of tests or a policy paragraph for every new field, and never create a parallel configuration framework.
