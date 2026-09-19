"""One-off branch authoring; removed before the pull request is opened."""
from pathlib import Path
import re


def replace(path, old, new, count=1):
    file = Path(path)
    text = file.read_text()
    if text.count(old) != count:
        raise SystemExit(f"{path}: expected {count} anchors, found {text.count(old)}")
    file.write_text(text.replace(old, new))


def section_dependency(path, section, name, features=()):
    declaration = f'{name} = {{ workspace = true'
    if features:
        declaration += ', features = [' + ', '.join(f'"{f}"' for f in features) + ']'
    declaration += ' }\n'
    file = Path(path)
    text = file.read_text()
    heading = f'[{section}]\n'
    if heading not in text:
        text += '\n' + heading
    start = text.index(heading) + len(heading)
    end = text.find('\n[', start)
    if end == -1:
        end = len(text)
    if re.search(rf'^{re.escape(name)}\s*=', text[start:end], re.M):
        raise SystemExit(f'{path}: {name} already declared in {section}')
    file.write_text(text[:start] + declaration + text[start:])


# One declaration per selected library. Only consumers select features;
# the recipe suite is a dev consumer, never a runtime facade.
versions = {
    'async-trait': '0.1.92',
    'serde_with': '3.23.0',
    'derive_more': '2.1.1',
    'itertools': '0.15.0',
    'axum-extra': '0.12.6',
    'indexmap': '2.14.2',
    'heck': '0.5.0',
    'bstr': '1.13.1',
    'regex': '1.13.1',
    'unicode-normalization': '0.1.25',
    'unicode-segmentation': '1.13',
    'strsim': '0.11.1',
    'fs-err': '3',
    'walkdir': '2.5',
    'camino': '1',
    'csv': '1',
    'base64': '0.23.1',
    'ipnet': '2',
    'semver': '1',
    'bytes': '1',
    'json-patch': '4',
    'uuid': '1',
    'rust_decimal': '1',
    'time': '0.3.55',
    'bon': '3.10.1',
    'validator': '0.21.0',
    'insta': '1.48.0',
    'proptest': '1.11.0',
    'backon': '1.6.0',
    'moka': '0.12.16',
    'wiremock': '0.6.5',
    'reqwest': '0.13.5',
}
block = '# Utility toolkit (docs/backend-utility-recipes.md).\n# Declarations are not runtime dependencies; each owner opts in below.\n'
block += ''.join(f'{name} = {{ version = "{version}", default-features = false }}\n' for name, version in versions.items())
replace('Cargo.toml', '# Build metadata\n', block + '\n# Build metadata\n')
replace('Cargo.toml', 'futures-util = { version = "0.3", default-features = false }\n', '')
replace('Cargo.toml', '# Runtime and transport\n', '# Runtime and transport\nfutures-util = { version = "0.3", default-features = false }\n')

section_dependency('crates/health/Cargo.toml', 'dependencies', 'async-trait')
section_dependency('crates/infra-postgres/Cargo.toml', 'dependencies', 'async-trait')
section_dependency('crates/infra-postgres/Cargo.toml', 'dependencies', 'strum', ('derive',))
section_dependency('crates/infra-http/Cargo.toml', 'dependencies', 'serde_with', ('std', 'macros'))
section_dependency('crates/infra-http/Cargo.toml', 'dependencies', 'derive_more', ('display',))
section_dependency('crates/infra-http/Cargo.toml', 'dev-dependencies', 'itertools', ('use_std',))
section_dependency('crates/infra-http/Cargo.toml', 'dev-dependencies', 'uuid', ('std',))
section_dependency('crates/infra-telemetry/Cargo.toml', 'dev-dependencies', 'futures-util', ('std',))

# Generate the exact same boxed, Send futures needed by dyn Probe.
health = 'crates/health/src/lib.rs'
replace(health, 'use std::future::Future;\nuse std::pin::Pin;\n', '')
replace(health, "pub trait Probe: Send + Sync + 'static {", "#[async_trait::async_trait]\npub trait Probe: Send + Sync + 'static {")
replace(health, "    fn check(&self) -> Pin<Box<dyn Future<Output = Result<(), ProbeError>> + Send + '_>>;", '    async fn check(&self) -> Result<(), ProbeError>;')
replace(health, '    impl Probe for Flaky {', '    #[async_trait::async_trait]\n    impl Probe for Flaky {')
replace(health, '    impl Probe for Hanging {', '    #[async_trait::async_trait]\n    impl Probe for Hanging {')
replace(health, """        fn check(&self) -> Pin<Box<dyn Future<Output = Result<(), ProbeError>> + Send + '_>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::Relaxed);
                if self.healthy.load(Ordering::Relaxed) {
                    Ok(())
                } else {
                    Err(ProbeError::new("connection refused"))
                }
            })
        }""", """        async fn check(&self) -> Result<(), ProbeError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.healthy.load(Ordering::Relaxed) {
                Ok(())
            } else {
                Err(ProbeError::new("connection refused"))
            }
        }""")
replace(health, """        fn check(&self) -> Pin<Box<dyn Future<Output = Result<(), ProbeError>> + Send + '_>> {
            Box::pin(std::future::pending())
        }""", """        async fn check(&self) -> Result<(), ProbeError> {
            std::future::pending().await
        }""")
file = Path(health)
text = file.read_text()
start = text.index('        if self.tx.borrow().evaluation.is_none() {', text.index('    pub async fn refresh_until('))
end = text.index('\n    async fn evaluate(', start)
text = text[:start] + '''        // Cancel covers both the timer and an in-flight probe. An already
        // cancelled token never polls the work; no detached task is created.
        let _ = cancel.run_until_cancelled(async {
            if self.tx.borrow().evaluation.is_none() {
                let _ = self.refresh(policy).await;
            }
            let mut ticker = tokio::time::interval(policy.interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first evaluation was done above or by startup admission.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let before = self.reader().verdict();
                let _ = self.refresh(policy).await;
                let after = self.reader().verdict();
                if before.is_ok() != after.is_ok() {
                    match &after {
                        Ok(()) => tracing::info!("readiness recovered"),
                        Err(reason) => tracing::warn!(%reason, "readiness lost"),
                    }
                }
            }
        }).await;
    }
''' + text[end:]
# Explicit regression coverage: no eager work after cancellation and the
# original sequential first-error policy, not accidental fan-out.
end = text.rfind('\n}')
text = text[:end] + '''
    #[tokio::test(start_paused = true)]
    async fn an_already_cancelled_refresher_never_checks_a_probe() {
        let (readiness, _, calls) = flaky(true);
        let cancel = CancellationToken::new();
        cancel.cancel();
        readiness.refresh_until(policy(), cancel).await;
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(readiness.reader().verdict(), Err(NotReady::NotEvaluated));
    }

    #[tokio::test]
    async fn a_failed_probe_does_not_start_the_next_probe() {
        let first_calls = Arc::new(AtomicU32::new(0));
        let second_calls = Arc::new(AtomicU32::new(0));
        let readiness = Readiness::new(vec![
            Box::new(Flaky {
                healthy: Arc::new(AtomicBool::new(false)),
                calls: first_calls.clone(),
            }),
            Box::new(Flaky {
                healthy: Arc::new(AtomicBool::new(true)),
                calls: second_calls.clone(),
            }),
        ]);
        assert!(readiness.refresh(policy()).await.is_err());
        assert_eq!(first_calls.load(Ordering::Relaxed), 1);
        assert_eq!(second_calls.load(Ordering::Relaxed), 0);
    }
''' + text[end:]
file.write_text(text)

probe = 'crates/infra-postgres/src/probe.rs'
replace(probe, 'use std::future::Future;\nuse std::pin::Pin;\n\n', '')
replace(probe, 'impl Probe for PostgresProbe {', '#[async_trait::async_trait]\nimpl Probe for PostgresProbe {')
replace(probe, """    fn check(&self) -> Pin<Box<dyn Future<Output = Result<(), ProbeError>> + Send + '_>> {
        Box::pin(async move {
            let mut conn = self.pool.acquire().await.map_err(|err| probe_error(&err))?;
            conn.ping().await.map_err(|err| probe_error(&err))
        })
    }""", """    async fn check(&self) -> Result<(), ProbeError> {
        let mut conn = self.pool.acquire().await.map_err(|err| probe_error(&err))?;
        conn.ping().await.map_err(|err| probe_error(&err))
    }""")

# The whitelist still rejects prefer/allow, case variants and duplicates.
dsn = 'crates/infra-postgres/src/dsn.rs'
replace(dsn, '#[derive(Clone, Copy, Debug, PartialEq, Eq)]\nenum AdmittedSslMode {', '#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::EnumString, strum::IntoStaticStr)]\n#[strum(serialize_all = "kebab-case")]\nenum AdmittedSslMode {')
replace(dsn, '''impl AdmittedSslMode {
    fn name(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Require => "require",
            Self::VerifyCa => "verify-ca",
            Self::VerifyFull => "verify-full",
        }
    }
}

''', '')
replace(dsn, '        self.ssl_mode.name()\n', '        self.ssl_mode.into()\n')
replace(dsn, '''    match value {
        "disable" => Ok(AdmittedSslMode::Disable),
        "require" => Ok(AdmittedSslMode::Require),
        "verify-ca" => Ok(AdmittedSslMode::VerifyCa),
        "verify-full" => Ok(AdmittedSslMode::VerifyFull),
        _ => Err(DsnError::SslMode),
    }''', '    value.parse().map_err(|_| DsnError::SslMode)')

problem = 'crates/infra-http/src/problem.rs'
replace(problem, '''/// `Serialize` is manual via [`Code::as_str`]: `InternalServerError`'s wire
/// token is `internal_error`, not serde `snake_case`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, strum::VariantArray)]''', '''/// `Display` and serialization delegate to [`Code::as_str`], not to the
/// Rust variant name: `InternalServerError` stays `internal_error` on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, strum::VariantArray,
         derive_more::Display, serde_with::SerializeDisplay)]
#[display("{}", self.as_str())]''')
replace(problem, '''impl Serialize for Code {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

''', '')
replace(problem, '#[derive(Clone, Debug, Serialize, ToSchema)]\n#[serde(deny_unknown_fields)]\npub struct Problem {', '#[serde_with::skip_serializing_none]\n#[derive(Clone, Debug, Serialize, ToSchema)]\n#[serde(deny_unknown_fields)]\npub struct Problem {')
replace(problem, '    #[serde(skip_serializing_if = "Option::is_none")]\n', '', 3)
replace(problem, '        let mut seen = std::collections::HashSet::new();\n', '        use itertools::Itertools;\n        assert!(Code::ALL.iter().map(|code| code.as_str()).all_unique());\n')
replace(problem, '            assert!(seen.insert(code.as_str()), "{} duplicated", code.as_str());\n', '            assert_eq!(code.to_string(), code.as_str());\n')

# Preserve the one-poll assertion, not block_on or another runtime.
tracing = 'crates/infra-telemetry/src/tracing.rs'
replace(tracing, '        use std::future::Future as _;\n        use std::task::{Context, Poll, Waker};\n', '        use futures_util::FutureExt;\n')
replace(tracing, '''        let mut export = std::pin::pin!(async {
            use opentelemetry_sdk::trace::SpanExporter as _;
            exporter.export(Vec::new()).await
        });
        let mut cx = Context::from_waker(Waker::noop());
        let Poll::Ready(result) = export.as_mut().poll(&mut cx) else {
            panic!("blocking OTLP client must finish in one poll");
        };''', '''        let result = {
            use opentelemetry_sdk::trace::SpanExporter as _;
            exporter.export(Vec::new()).now_or_never()
                .expect("blocking OTLP client must finish in one poll")
        };''')
replace('crates/infra-telemetry/src/metrics.rs', '''        tokio::select! {
            () = cancel.cancelled() => {},
            () = reporter => {},
        }''', '        let _ = cancel.run_until_cancelled(reporter).await;')

# Sorting assertions without maintaining auxiliary mutable vectors.
router = 'crates/infra-http/src/router.rs'
replace(router, '    use health::{Readiness, RefreshPolicy};\n', '    use health::{Readiness, RefreshPolicy};\n    use itertools::Itertools;\n')
replace(router, '''        let mut paths: Vec<_> = document.paths.paths.keys().cloned().collect();
        paths.sort_unstable();
        let mut expected: Vec<_> = HEALTH_PROBE_ROUTES.iter().map(|&p| p.to_owned()).collect();
        expected.sort_unstable();
        assert_eq!(paths, expected);''', '''        assert_eq!(
            document.paths.paths.keys().map(String::as_str).sorted().collect_vec(),
            HEALTH_PROBE_ROUTES.iter().copied().sorted().collect_vec(),
        );''')

harden = 'crates/infra-http/src/harden.rs'
replace(harden, '        assert_eq!(id.len(), 36, "generated UUIDv4: {id}");', '        let parsed = uuid::Uuid::parse_str(id).expect("generated request ID is a UUID");\n        assert_eq!(parsed.get_version_num(), 4);\n        assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122);')
replace(harden, '        assert_eq!(id.to_str().unwrap().len(), 36);', '        let parsed = uuid::Uuid::parse_str(id.to_str().unwrap()).unwrap();\n        assert_eq!(parsed.get_version_num(), 4);\n        assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122);')

# The existing test package hosts executable recipes. They are not new
# runtime dependencies of the service or a fabricated business feature.
replace('test/Cargo.toml', 'description = "Database-backed proof for the PostgreSQL profile; runs only with the `integration` feature and a live DATABASE_URL."', 'description = "Executable utility recipes and opt-in database-backed PostgreSQL proof."')
replace('test/Cargo.toml', '# `make test` builds this crate with the feature off, so no test here needs\n# Docker; `scripts/ci/test-integration-db.sh` turns it on with a live server.', '# Utility recipes run with `make test` and need no Docker or external service.\n# PostgreSQL tests alone require `integration` and a live DATABASE_URL.')
recipe_features = {
    'itertools': ('use_std',), 'indexmap': ('std',),
    'serde': ('std', 'derive'), 'serde_json': ('std',),
    'serde_with': ('std', 'macros'), 'derive_more': ('display', 'as_ref'),
    'axum': ('json', 'query'), 'axum-extra': ('query', 'typed-header', 'with-rejection'),
    'axum-test': (), 'infra-http': (),
    'futures-util': ('std', 'async-await'),
    'tokio': ('macros', 'rt-multi-thread', 'time', 'sync', 'io-util', 'test-util'),
    'tokio-util': ('io', 'rt'), 'bytes': ('std',),
    'heck': (), 'bstr': ('std',), 'regex': ('std', 'unicode', 'perf'),
    'unicode-normalization': ('std',), 'unicode-segmentation': (), 'strsim': (),
    'fs-err': (), 'walkdir': (), 'tempfile': (), 'camino': (), 'csv': (),
    'base64': ('std',), 'ipnet': ('std',), 'semver': ('std',),
    'json-patch': (), 'uuid': ('std', 'serde', 'v4'),
    'rust_decimal': ('std', 'serde-with-str'),
    'time': ('std', 'serde', 'serde-well-known', 'formatting', 'parsing', 'macros'),
    'bon': (), 'validator': ('derive',), 'insta': ('json',),
    'proptest': ('std',), 'backon': ('tokio-sleep',),
    'moka': ('future',), 'wiremock': (), 'reqwest': ('json',),
}
for name, features in recipe_features.items():
    section_dependency('test/Cargo.toml', 'dev-dependencies', name, features)

# Keep the current guide accurate and retain its conditional safety policies.
guide = 'docs/backend-library-selection.md'
replace(guide, '## Adopted for existing code\n', '## Production utility toolkit\n\n[Executable utility recipes](backend-utility-recipes.md) records the full\nutility selection, runnable cases, actual source migrations and alternatives.\nThe recipes are normal tests in the existing test package; only production\nconsumers add normal dependencies. `Probe` now uses `async-trait`, cancellation\nuses `tokio-util`, and `Code` serialization uses `derive_more` plus `serde_with`\nwithout changing its wire identity.\n\n## Adopted for existing code\n')
replace(guide, 'Do not derive serde `snake_case` Serialize: the wire token comes from `as_str`', 'Do not derive serde `snake_case` Serialize: `SerializeDisplay` delegates through `Display` to `as_str`')
replace(guide, 'A few repeated `skip_serializing_if` attributes alone are not a reason to add it.', 'It is now used for `Code` serialization and omission of optional `Problem` fields; the recipe suite also exercises nested adapters and PATCH states.')
replace(guide, 'The first operation owns this integration; no unused generic validator,\nplaceholder DTO or validation dependency is added to the health-only scaffold.', 'The first operation owns production integration. The executable HTTP recipe\nexercises `validator` as a dev-dependency; it does not publish a generic\nvalidator API, placeholder endpoint or authentication mechanism.')
replace(guide, 'These are adoption triggers, not unfinished work in this change. The owning\nfeature/profile introduces the dependency, implementation, focused tests and\nconfiguration only when its real requirement exists.', 'The recipe suite exercises the selected utility mechanisms without installing\na cache, HTTP provider, retry policy or new endpoint in the running service.\nThe owning feature/profile still supplies production policy and wiring.')
print('Source and manifest edits applied. Cargo must resolve the lockfile next.')
