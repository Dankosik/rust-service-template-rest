---
name: rust-config
description: "One owner per key. Use when a Rust service configuration key, default, validation rule, secret source, or file-versus-environment precedence needs implementation or review."
---

# Rust Config

**One owner per key.** A configuration key is declared, defaulted, and validated in one section file of the config crate, and nowhere else. Trace the key from that file through the loader to the value the runtime reads. Honor supplied requirements and preserve settled choices outside the requested change; resolve only what the task leaves open.

Precedence is fixed: code defaults, then the base file, then overlay files in order, then the APP-prefixed environment with double underscores between path segments. Every section type denies unknown fields and derives its defaults, which is what makes an undeclared key fail startup and a missing key take its default; there is no second registry to maintain. Values keep their human forms in files and environment alike: durations such as eight seconds, sizes such as one mebibyte, booleans, and enums by their documented spelling. An empty environment value is an explicit override that flows into validation; it never falls back.

To add a key, declare the field with the serde attribute its type needs, set the default with the reason for the value beside it, add the range or cross-field rule to the section's validate function with a message that names the dotted key, and add a loader test that sets the key through the environment plus a validation test for a rejected value. A rule spanning two sections belongs to the section that depends on the other and takes it as a parameter; a rule that needs process structure, such as the teardown tail against the grace period, belongs to bootstrap. Update the local baseline file only for a non-secret value a developer should see, and the configuration policy document whenever secret-source or runtime-budget behavior changes.

Secret-like keys carry a value only through the environment; the file pre-scan refuses anything else, and an empty placeholder in a file is the documented way to show the key exists. Secret fields use the secret string type so debug output and the startup summary redact them.

Prefer explicit types over strings: the loader coerces "yes" into true or one, so a boolean, an enum, or a socket address is what makes garbage fail. Do not read ambient variables such as RUST_LOG or PORT directly; the APP namespace is the only application input, and the OpenTelemetry SDK's own variables are the documented exception.

For review, explain the precedence, default, or secret-source risk without editing. For implementation, finish when the key has a default, validation, a loader test, a validation test, and a policy line, and the config crate's tests pass. Do not add a new source, format, or override mechanism because one would be convenient.
