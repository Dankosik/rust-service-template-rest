---
name: rust-config
description: "One owner per key. Use when changing configuration keys, defaults, validation, secret sources, or file-versus-env precedence."
---

# Rust Config

**One owner per key.** Trace each setting from declaration and source precedence through decoding, validation, and runtime consumption, keeping defaults beside the section that owns their meaning and cross-section checks with the dependent owner.

**Loader authority.** Extend `crates/config` (`load.rs` and `secret_policy.rs`) as the single configuration authority: define section defaults on their section types, validate cross-section invariants in the dependent owner, and check process-wide relationships at composition.

**Input semantics.** Define exact behavior for omitted, blank, malformed, duplicate, and unknown inputs at the accepted boundary, ensuring blank overrides follow the explicit unset-versus-invalid contract rather than silently falling back to lower-precedence sources. Keep source selection, typed decoding (`humantime`, `bytesize`, domain types), and semantic validation distinct, adding new sources or stricter parsing only when required.

**Key lifecycle.** Require a real runtime consumer and either a justified default or mandatory supply for every key. Validate cross-field bounds and units before the dependent effect runs, verify downstream consumers and diagnostics when changing defaults or precedence, and pass immutable configuration snapshots from startup composition.

**Secret admission and redaction.** Register secret keys in `crates/config/src/secret_policy.rs` alongside `secrecy::SecretString` and approved variable carriers so key-name pre-scans, serde error paths, startup summaries, `Debug` impls, and telemetry all redact raw values, following `secret_policy.rs` for any SDK environment exception and reusing existing loader tests where coverage suffices.
