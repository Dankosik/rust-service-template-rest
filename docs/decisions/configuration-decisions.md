# Configuration decisions

`crates/config` owns the keys, defaults, decoding, admission rules, and
messages. This record keeps only rejected alternatives and reopen conditions.

| Choice / Rejected alternative | Decisive non-code reason | Reopen condition |
| --- | --- | --- |
| `config` (config-rs) with serde over `figment`, `confique`, `schematic`, or `smart-config` | config-rs is maintained and the most used; figment has had no release since May 2024; the schema-first crates are pre-1.0 or small. The loader already adds what they would bring: value-free rejections, strict scalar decoders, and secret admission. | config-rs loses maintenance, or a schema-first crate reaches 1.0 with value-free errors and secret parameters. |
| Separate input and admitted types, built by one admission step that reports every violation | Operators fix a configuration in one restart instead of one per error; consumers no longer hold checks for states admission already excluded. Decoding stays first-failure-only, because serde stops at its first error. | Decoding moves to a library that collects decode errors. |
| Role and composition checks stay at bootstrap (jobs-worker messaging consumer keys, jobs pool capacity, idempotency retention) | They depend on which binary runs and which operations it composes, which the loader does not know. | The loader gains a per-binary snapshot type that names its role. |
| No generated configuration reference (`schemars` JSON Schema, generated example TOML) | Section types and their doc comments are the reference; a committed generated file would have to be regenerated per template profile, and no editor or operator tool consumes a schema today. | An operator-facing tool or editor integration needs a schema, or derived services ask for a key reference outside the code. |
| `jiff` friendly durations (a `Span` with 24-hour days) over `humantime-serde` | `humantime` is unmaintained ([RUSTSEC-2026-0542](https://rustsec.org/advisories/RUSTSEC-2026-0542)); `jiff` is maintained and accepts the same unit and spacing forms. Months and years are refused because they have no fixed length. | `jiff` loses maintenance, or an accepted key needs a calendar-relative duration. |
