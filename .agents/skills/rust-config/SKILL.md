---
name: rust-config
description: "Use when adding, changing, or validating a configuration key, its default, a secret source, or the precedence between files and environment."
metadata:
  invocation: model
  kind: method
---

# Rust Config

One key has **one owner**: the section file that declares its type, default,
and validation together, in `crates/config/src/<section>.rs`.

Precedence is fixed by [Configuration Source Policy](../../../docs/configuration-source-policy.md):
code defaults → `--config` → `--config-overlay` in order → `APP__SECTION__KEY`.
Every section type carries `#[serde(deny_unknown_fields, default)]` and an
`impl Default`; that pair is what makes an undeclared key fail and a missing
key take its default. There is no second registry to update.

To add a key: declare the field with its serde attribute (`humantime_serde`
for `Duration`, `ByteSize` for sizes, `SecretString` for a credential, an
enum with `rename_all` for a closed vocabulary); set the default with the
reason for the value beside it; add the range or cross-field rule to the
section's `validate()` naming the dotted key in the message; add a loader
test in `load.rs` that sets the key through `APP__…` and a validation test
for a rejected value; add a non-secret example to `env/config/local.toml`
only when a developer should see it; update the policy document when
secret-source or runtime-budget behavior changes.

A rule spanning two sections belongs to the section that depends on the
other and takes it as a parameter; a rule that needs process structure (the
teardown tail) belongs to `bootstrap`, as `validate_grace_budget` does.

Secret-like keys (`is_secret_like_key`) may carry a value only through the
environment; the file pre-scan refuses anything else. Values keep their
human forms in files and environment alike; an empty environment value is an
explicit override and must fail validation rather than fall back.

Prefer explicit types over stringly values: config-rs coerces `"yes"` to
`true` or `1`, so a `bool`, an enum, or `SocketAddr` is what makes garbage
fail. Do not read `RUST_LOG`, `PORT`, or any other ambient variable directly;
the `APP__` namespace is the only application input, and the OpenTelemetry
SDK's own variables are the documented exception.

Complete when the key has a default, validation, a loader test, a
validation test, and a policy line, and `make test-package PKG=service-config`
passes.
