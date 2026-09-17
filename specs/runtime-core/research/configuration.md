# Lane report: runtime configuration, CLI flags, secrets, durations

Raw research lane output, 2026-09-17. Versions from the crates.io API,
maintenance figures from the GitHub API; the critical loader behaviours
(unknown keys, `__` nesting, malformed names, empty values, coercion,
durations, byte sizes, secret redaction) were executed in a scratch project
with Rust 1.98.1, `config 0.15.25` and `figment 0.10.19`. Decisions are
consolidated in [synthesis.md](synthesis.md).

Requirement legend: R1 typed/immutable via serde · R2 layering + `APP__`/`__`
env · R3 strict unknown keys + malformed env names · R4 durations/byte sizes ·
R5 empty env = explicit · R6 secrets · R7 cross-field validation.

## Loaders

| Crate | Latest (release) | Maintenance | Fit |
|---|---|---|---|
| `config` (config-rs) | 0.15.25 (2026-06-26) | rust-cli/config-rs: push 2026-09-15, 3 210 stars, MSRV 1.85, edition 2024; backends `toml 1.0`, `yaml-rust2 0.11`, `serde_json`, ron, ini, json5 | R1 yes. R2 yes: `Environment::with_prefix("APP").separator("__")` maps `APP__HTTP__ADDR` to `http.addr`; ordered file sources, last wins. R3 `deny_unknown_fields` rejects unknown file and env keys (verified; maintainer confirms in #450); malformed `APP____X` surfaces only as `unknown field ".x"`, so add a small pre-scan for a readable error. R4 values stay strings, so `humantime_serde` and `bytesize` work from files and env (verified `"100ms"`, `"4 MiB"`). R5 yes: empty env kept unless `.ignore_empty(true)` (verified). R6 not built in; `SecretString` deserializes; pre-scan files. R7 not built in. Gotchas: env keys lowercased (#340), lenient coercion (`APP__DB__POOL=yes` → `u32 = 1`, verified), unknown-field errors carry the parent key but not the source. |
| `figment` | 0.10.19 (2024-05-17) | SergioBenitez/Figment: push 2024-09-13; issue #148 "Maintenance status?" answered 2026-04-18 with intent to continue, no release since; depends on `toml ^0.8` and deprecated `serde_yaml ^0.9` (PRs #154/#155 open). Forks: `figment2` 0.11.5, `compote` 0.3.0 | R1, R2 yes (`merge` = last wins; profiles opt-in). R3 `deny_unknown_fields` works and the error names the provider (verified), but malformed names are silently dropped (`Env::iter` filters keys with an empty segment; verified `APP____X` → `Ok`). R4 from env, values are parsed TOML-style first. R5 yes with provenance. R6 footgun: verified `APP__DB__DSN=123456` → `expected a string` (#17 closed by design). Metadata: yes, per-key provenance. |
| `confique` | 0.4.0 (2025-10-27) | push 2025-10-27, 223 stars; `yaml` feature pulls deprecated `serde_yaml` | Per-field `#[config(env = "...")]`, no prefix/`__` scheme (R2 poor fit); unparsable empty env treated as unset (R5 conflict); good at generating commented templates |
| `twelf` | 0.15.0 (2024-03-11) | push 2024-05-27 | Env via `envy`, flat; dormant |
| `envconfig` | 0.11.1 (2025-12-10) | active | Env only |
| `envy` | 0.4.2 (2021) | stale | Env only, flat |
| `serde-env` | 0.3.0 (2026-04-15) | small | Env only |
| `clap` | 4.6.7 (2026-09-14) | very active | `--config PathBuf`, repeatable ordered `--config-overlay Vec<PathBuf>` (`ArgAction::Append`) |

## Supporting crates

| Crate | Latest (release) | Notes |
|---|---|---|
| `secrecy` | 0.10.3 (2024-10-09); monorepo pushed 2026-09-09, release pending (#1377) | `SecretString = SecretBox<str>`; `Debug` prints `SecretBox<str>([REDACTED])` (verified); `serde` feature gives `Deserialize` only; `FromStr` unreleased (#1314); `forbid(unsafe_code)`, zeroize on drop |
| `humantime-serde` | 1.1.1 (2022-03-11) | ~200 LOC shim over `humantime ^2`; verified `"8s"`, `"100ms"`, rejects bare `"8"` |
| `humantime` | 2.4.0 (2026-07-02) | Now under chronotope; healthy again |
| `jiff` | 0.2.37 (2026-09-12) | `SignedDuration` parses ISO 8601 and friendly forms; heavier |
| `duration-str` | 0.21.0 (2026-03-03) | Accepts arithmetic (`"3m + 13s"`), more surface than wanted |
| `bytesize` | 2.7.0 (2026-08-02) | `serde` feature: string or integer (verified `"4 MiB"`) |
| `byte-unit` | 5.2.6 (2026-09-13) | Equivalent; used by Meilisearch and Loco |
| `garde` | 0.23.0 (2026-05-23) | Derive validation; cross-field needs `custom` closures |
| `validator` | 0.21.0 (2026-07-27) | `schema(function)` for struct-level rules; errors keyed by field |
| `serde_with` | 3.23.0 (2026-09-07) | Not required |
| `toml` | 1.1.6+spec-1.1.0 (2026-09-10) | Backend of config-rs; handy for the secret pre-scan (`toml::Table`) |

## Recommendation

`config` (config-rs 0.15) as the layering engine, `clap` for the two loader
flags, serde attributes for strictness and defaults, `humantime-serde` +
`bytesize` for human values, `secrecy::SecretString` for secrets, hand-written
`validate()` for cross-field rules. Two pre-scans (~25 lines, `std::env` +
`toml::Table`) cover what no crate provides: malformed env names and secrets
in files.

Why config-rs over figment: the only actively released option; meets R3–R5
out of the box; figment silently drops malformed env names and turns
numeric-looking env secrets into integers; config-rs is the dominant service
idiom (Zero To Production uses `File(base) + File({env}) +
Environment::with_prefix("APP").separator("__")`). Figment's provenance
advantage matters less once files are pre-scanned and validation names keys.

Why hand-written `validate()`: the rules are few and cross-field; a plain
function yields the exact operator message without a derive.

Verified sketch shape:

```rust
#[derive(clap::Parser)]
struct Cli {
    #[arg(long, value_name = "PATH")] config: PathBuf,
    #[arg(long = "config-overlay", value_name = "PATH")] config_overlay: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Http {
    pub addr: SocketAddr,
    #[serde(with = "humantime_serde")] pub request_timeout: Duration,
    pub max_body: ByteSize,
}

pub fn load(cli: &Cli) -> anyhow::Result<AppConfig> {
    reject_malformed_env("APP__")?;
    let mut b = config::Config::builder();
    for path in std::iter::once(&cli.config).chain(&cli.config_overlay) {
        reject_file_secrets(path)?;
        b = b.add_source(config::File::from(path.clone()).format(config::FileFormat::Toml).required(true));
    }
    let cfg: AppConfig = b
        .add_source(config::Environment::with_prefix("APP").separator("__"))
        .build()?
        .try_deserialize()?;
    cfg.validate()?;
    Ok(cfg)
}
```

## File format

TOML as the primary and default format; YAML optional, not default.

- TOML is the ecosystem convention (`Cargo.toml`, `.cargo/config.toml`,
  `rustfmt.toml`); `toml` 1.1.6 tracks spec 1.1.0 and is the backend of
  config-rs, figment, confique and Meilisearch (`deny_unknown_fields`).
- YAML has no canonical maintained serde crate in 2026: `serde_yaml`
  0.9.34+deprecated (archived, `unsafe-libyaml` archived); `serde_yml`
  0.0.13 repo archived, RUSTSEC-2025-0068 marks all versions unsound and
  unmaintained; `serde_yaml_ng` 0.10.0 (2024-05) still on archived
  `unsafe-libyaml`; `serde_norway` 0.9.42 (2024-12) on its own fork;
  `saphyr-serde` is a 0.0.0 placeholder; `serde-saphyr` 1.3.0 (2026-09-16)
  is the most active pure-Rust option but single-maintainer and outside the
  saphyr project; `serde_yaml2` "incomplete" per RustSec.
- If YAML parity is required, config-rs's `yaml` feature (`yaml-rust2`
  0.13.0, pure Rust, active) adds no extra YAML serde crate.
- Vector still depends on deprecated `serde_yaml`; Loco aliases
  `serde_yaml_ng` and hand-merges YAML values.

## Production idioms

| Project | Approach |
|---|---|
| Zero To Production | config-rs: `base.yaml` + `{env}.yaml` + `Environment::with_prefix("APP").prefix_separator("_").separator("__")`, `secrecy` |
| Meilisearch | clap derive with `env = "MEILI_*"`; TOML via `toml::from_str` with `deny_unknown_fields`; `byte-unit`, `humantime`, `secrecy` |
| Vector | Custom loader (TOML/YAML/JSON, `${VAR}` interpolation) |
| Loco.rs | `config/{env}.yaml` merged by hand as `serde_yaml_ng::Value` |

## Risks

1. config-rs lenient coercion (`"yes"` → `true`/`1`): prefer explicit types
   (`bool`, enums, `SocketAddr`, `Duration`).
2. config-rs lowercases env keys (#340); fields must not contain `__`.
3. Unknown-field errors lack the source; pre-scans and outer context
   compensate.
4. `Environment` collects every `APP__*` var: an unrelated `APP__FOO` fails
   startup, which R3 asks for; document for operators.
5. `secrecy` has no `Serialize`: defaults via `#[serde(default)]` +
   `impl Default`, not `Config::try_from(&Default)`.
6. `humantime-serde` is stable but unmaintained (2022); a 10-line
   `deserialize_with` over `humantime::parse_duration` replaces it if needed.
7. Line/column diagnostics are lost through value trees; errors are key-path
   based.
8. Not executed: confique `layer_attr(serde(deny_unknown_fields))`,
   garde/validator output, `figment2`, Shuttle/cargo-generate survey.

## Sources

crates.io API for every crate above and figment/figment2 dependency lists;
GitHub repos and issues: SergioBenitez/Figment (#148, #17, #119, #121, #154,
#155; `src/providers/env.rs`), rust-cli/config-rs (#450, #340, #531, #543;
`src/env.rs`, `src/de.rs`, `CHANGELOG.md`), LukasKalbertodt/confique,
bnjjj/twelf, greyblake/envconfig-rs, softprops/envy, Xuanwo/serde-env,
iqlusioninc/crates (#1314, #1377), jean-airoldie/humantime-serde,
chronotope/humantime, BurntSushi/jiff, baoyachi/duration-str, fundu-rs/fundu,
bytesize-rs/bytesize, magiclen/byte-unit, jprochazk/garde, Keats/validator,
jonasbb/serde_with, toml-rs/toml, clap-rs/clap, dtolnay/serde-yaml (archived),
acatton/serde-yaml-ng, sebastienrousseau/serde_yml (archived),
cafkafk/serde-norway, zim32/serde_yaml2, saphyr-rs/saphyr (#1, #66),
bourumir-wyngs/serde-saphyr, Ethiraric/yaml-rust2, lmmx/figment2; RustSec
RUSTSEC-2025-0068, RUSTSEC-2023-0075, RUSTSEC-2024-0320; production sources:
LukeMathWalker/zero-to-production `src/configuration.rs`,
meilisearch `crates/meilisearch/src/option.rs`, vectordotdev/vector
`src/config/format.rs`, loco-rs/loco `src/config/mod.rs`. Local proof of
concept executed under `/tmp` (outside the repository).
