//! Connection-string admission.
//!
//! `sqlx` is permissive: every `PgConnectOptions` constructor starts from the
//! libpq environment (`PGHOST`, `PGPASSWORD`, ...), falls back to `.pgpass`
//! when the URL has no password, accepts Unix sockets, `allow`/`prefer` TLS
//! fallback, TLS key and certificate files, and warns with key *and value*
//! on a parameter it does not know. The template's policy is one explicit,
//! self-contained URL: what the operator set is exactly what connects, and a
//! diagnostic never carries the value. This module enforces that before
//! `sqlx` sees the string.

use std::ffi::OsString;
use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use url::Url;

/// Environment variables `sqlx` reads while building connect options
/// (`sqlx-postgres/src/options/mod.rs` and `pgpass.rs`). A non-empty value in
/// any of them would merge into the connection behind the operator's back.
pub const AMBIENT_ENVIRONMENT: [&str; 13] = [
    "PGHOSTADDR",
    "PGHOST",
    "PGPORT",
    "PGUSER",
    "PGPASSWORD",
    "PGDATABASE",
    "PGSSLMODE",
    "PGSSLROOTCERT",
    "PGSSLCERT",
    "PGSSLKEY",
    "PGAPPNAME",
    "PGOPTIONS",
    "PGPASSFILE",
];

/// Why a connection string was refused. Messages name the rule, never the
/// value; a key name is quoted only when it is the operator's own parameter.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DsnError {
    #[error("postgres dsn is empty")]
    Empty,
    #[error("postgres dsn must use the postgres:// or postgresql:// URL form")]
    Scheme,
    #[error("postgres dsn could not be parsed as a URL (value redacted)")]
    Invalid,
    #[error("postgres dsn must not include a URL fragment")]
    Fragment,
    #[error("postgres dsn was admitted but is not usable as connect options (value redacted)")]
    Unusable,
    #[error("postgres dsn requires an explicit {0}")]
    Missing(&'static str),
    #[error("postgres dsn must name one tcp host; unix sockets are not supported")]
    Socket,
    #[error("postgres dsn must name one tcp host; fallback hosts are not supported")]
    MultipleHosts,
    #[error("postgres dsn sslmode must be one of disable, require, verify-ca, verify-full")]
    SslMode,
    #[error("postgres dsn uses an unsupported service or passfile source ({0})")]
    ServiceSource(String),
    #[error("postgres dsn uses an unsupported TLS file source ({0})")]
    TlsFileSource(String),
    #[error("postgres dsn parameter {0:?} is not supported; only sslmode is accepted")]
    Parameter(String),
    #[error("postgres dsn must be the only connection source; unset {0} in the environment")]
    Ambient(&'static str),
}

/// An admitted connection string, ready to become connect options.
#[derive(Clone)]
pub struct Dsn {
    options: PgConnectOptions,
    host: String,
    port: u16,
    database: String,
    ssl_mode: PgSslMode,
}

impl std::fmt::Debug for Dsn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dsn")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("ssl_mode", &self.ssl_mode_name())
            .finish_non_exhaustive()
    }
}

impl Dsn {
    /// Admit `raw` against the template rules and the process environment.
    ///
    /// # Errors
    ///
    /// The first violated rule, without the offending value.
    pub fn parse(raw: &str) -> Result<Self, DsnError> {
        Self::parse_with_environment(raw, |name| std::env::var_os(name))
    }

    /// [`Dsn::parse`] with an explicit environment lookup, so the ambient
    /// rule can be tested without mutating the process environment.
    ///
    /// # Errors
    ///
    /// The first violated rule, without the offending value.
    pub fn parse_with_environment<F>(raw: &str, environment: F) -> Result<Self, DsnError>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(DsnError::Empty);
        }
        if let Some(name) = AMBIENT_ENVIRONMENT
            .into_iter()
            .find(|name| environment(name).is_some_and(|value| !value.is_empty()))
        {
            return Err(DsnError::Ambient(name));
        }
        if !raw.starts_with("postgres://") && !raw.starts_with("postgresql://") {
            return Err(DsnError::Scheme);
        }
        let url = Url::parse(raw).map_err(|_| DsnError::Invalid)?;
        if url.fragment().is_some() {
            return Err(DsnError::Fragment);
        }

        let host = url.host_str().unwrap_or_default();
        if host.is_empty() {
            return Err(DsnError::Missing("host"));
        }
        // The url crate keeps the host percent-encoded; `sqlx` decodes a
        // leading `/` as a socket directory.
        if host.starts_with('/') || host.starts_with("%2F") || host.starts_with("%2f") {
            return Err(DsnError::Socket);
        }
        if host.contains(',') {
            return Err(DsnError::MultipleHosts);
        }
        let port = match url.port() {
            Some(port) if port != 0 => port,
            _ => return Err(DsnError::Missing("port")),
        };
        if url.username().is_empty() {
            return Err(DsnError::Missing("user"));
        }
        if url.password().is_none_or(str::is_empty) {
            return Err(DsnError::Missing("password"));
        }
        let database = url.path().trim_start_matches('/');
        if database.is_empty() || database.contains('/') {
            return Err(DsnError::Missing("database"));
        }

        let mut ssl_mode = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "sslmode" if ssl_mode.is_none() => ssl_mode = Some(parse_ssl_mode(&value)?),
                "service" | "servicefile" | "passfile" => {
                    return Err(DsnError::ServiceSource(bounded(&key)));
                }
                "sslcert" | "ssl-cert" | "sslkey" | "ssl-key" | "sslrootcert" | "ssl-root-cert"
                | "ssl-ca" | "sslpassword" | "sslcrl" => {
                    return Err(DsnError::TlsFileSource(bounded(&key)));
                }
                // Includes a repeated `sslmode`.
                _ => return Err(DsnError::Parameter(bounded(&key))),
            }
        }
        let ssl_mode = ssl_mode.ok_or(DsnError::Missing("sslmode"))?;

        // Every rule held and the environment is clean, so what `sqlx`
        // parses is exactly what the operator wrote.
        let options = PgConnectOptions::from_str(raw).map_err(|_| DsnError::Unusable)?;
        Ok(Self {
            options,
            host: host.to_owned(),
            port,
            database: database.to_owned(),
            ssl_mode,
        })
    }

    /// Connect options for `sqlx`, before the template's session defaults.
    #[must_use]
    pub(crate) fn connect_options(&self) -> PgConnectOptions {
        self.options.clone()
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn database(&self) -> &str {
        &self.database
    }

    /// The admitted `sslmode`, as the operator spelled it.
    #[must_use]
    pub fn ssl_mode_name(&self) -> &'static str {
        match self.ssl_mode {
            PgSslMode::Disable => "disable",
            PgSslMode::Require => "require",
            PgSslMode::VerifyCa => "verify-ca",
            PgSslMode::VerifyFull => "verify-full",
            // Refused at admission; kept exhaustive for the compiler.
            PgSslMode::Allow => "allow",
            PgSslMode::Prefer => "prefer",
        }
    }
}

/// `allow` and `prefer` negotiate TLS per attempt, so two connections from
/// one pool could differ in what they protect; the policy admits only modes
/// with one outcome.
fn parse_ssl_mode(value: &str) -> Result<PgSslMode, DsnError> {
    match value {
        "disable" => Ok(PgSslMode::Disable),
        "require" => Ok(PgSslMode::Require),
        "verify-ca" => Ok(PgSslMode::VerifyCa),
        "verify-full" => Ok(PgSslMode::VerifyFull),
        _ => Err(DsnError::SslMode),
    }
}

/// A parameter key for a diagnostic: the operator's own spelling, capped so
/// a pathological string cannot flood a log line.
fn bounded(key: &str) -> String {
    const MAX: usize = 32;
    if key.chars().count() <= MAX {
        key.to_owned()
    } else {
        let mut cut: String = key.chars().take(MAX).collect();
        cut.push('…');
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "postgres://app:s3cret@db.internal:5432/app?sslmode=require";

    fn no_env(_: &str) -> Option<OsString> {
        None
    }

    fn parse(raw: &str) -> Result<Dsn, DsnError> {
        Dsn::parse_with_environment(raw, no_env)
    }

    #[test]
    fn a_complete_url_is_admitted() {
        let dsn = parse(VALID).unwrap();
        assert_eq!(dsn.host(), "db.internal");
        assert_eq!(dsn.port(), 5432);
        assert_eq!(dsn.database(), "app");
        assert_eq!(dsn.ssl_mode_name(), "require");
        let options = dsn.connect_options();
        assert_eq!(options.get_host(), "db.internal");
        assert_eq!(options.get_port(), 5432);
        assert_eq!(options.get_username(), "app");
        assert_eq!(options.get_database(), Some("app"));
        assert!(options.get_socket().is_none());
    }

    #[test]
    fn postgresql_scheme_and_ipv6_hosts_are_admitted() {
        let dsn = parse("postgresql://app:pw@[::1]:6543/app?sslmode=disable").unwrap();
        assert_eq!(dsn.host(), "[::1]");
        assert_eq!(dsn.port(), 6543);
    }

    #[test]
    fn percent_encoded_credentials_are_decoded_by_the_driver() {
        let dsn = parse("postgres://app:p%40ss%2Fword@h:5432/app?sslmode=disable").unwrap();
        assert_eq!(dsn.connect_options().get_username(), "app");
    }

    #[test]
    fn every_component_is_required() {
        let cases = [
            ("", DsnError::Empty),
            ("   ", DsnError::Empty),
            ("host=h port=5432 user=u", DsnError::Scheme),
            ("mysql://app:pw@h:5432/app", DsnError::Scheme),
            (
                "postgres://app:pw@h:5432/app?sslmode=require#frag",
                DsnError::Fragment,
            ),
            (
                "postgres://app:pw@:5432/app?sslmode=require",
                DsnError::Invalid,
            ),
            (
                "postgres://app:pw@h/app?sslmode=require",
                DsnError::Missing("port"),
            ),
            (
                "postgres://app:pw@h:0/app?sslmode=require",
                DsnError::Missing("port"),
            ),
            (
                "postgres://:pw@h:5432/app?sslmode=require",
                DsnError::Missing("user"),
            ),
            (
                "postgres://app@h:5432/app?sslmode=require",
                DsnError::Missing("password"),
            ),
            (
                "postgres://app:@h:5432/app?sslmode=require",
                DsnError::Missing("password"),
            ),
            (
                "postgres://app:pw@h:5432?sslmode=require",
                DsnError::Missing("database"),
            ),
            (
                "postgres://app:pw@h:5432/?sslmode=require",
                DsnError::Missing("database"),
            ),
            (
                "postgres://app:pw@h:5432/app/extra?sslmode=require",
                DsnError::Missing("database"),
            ),
            ("postgres://app:pw@h:5432/app", DsnError::Missing("sslmode")),
        ];
        for (raw, expected) in cases {
            assert_eq!(parse(raw).err(), Some(expected), "{raw}");
        }
    }

    #[test]
    fn sockets_and_fallback_hosts_are_refused() {
        assert_eq!(
            parse("postgres://app:pw@%2Fvar%2Frun%2Fpostgresql:5432/app?sslmode=disable").err(),
            Some(DsnError::Socket)
        );
        assert_eq!(
            parse("postgres://app:pw@h1,h2:5432/app?sslmode=disable").err(),
            Some(DsnError::MultipleHosts)
        );
        // `h1:5432,h2:5432` is not a URL at all.
        assert_eq!(
            parse("postgres://app:pw@h1:5432,h2:5432/app?sslmode=disable").err(),
            Some(DsnError::Invalid)
        );
    }

    #[test]
    fn only_deterministic_ssl_modes_are_admitted() {
        for mode in ["disable", "require", "verify-ca", "verify-full"] {
            let dsn = parse(&format!("postgres://app:pw@h:5432/app?sslmode={mode}")).unwrap();
            assert_eq!(dsn.ssl_mode_name(), mode);
        }
        for mode in ["allow", "prefer", "", "REQUIRE", "bogus"] {
            assert_eq!(
                parse(&format!("postgres://app:pw@h:5432/app?sslmode={mode}")).err(),
                Some(DsnError::SslMode),
                "{mode}"
            );
        }
    }

    #[test]
    fn side_channel_parameters_are_refused_by_kind() {
        let base = "postgres://app:pw@h:5432/app?sslmode=require";
        for key in ["service", "servicefile", "passfile"] {
            assert_eq!(
                parse(&format!("{base}&{key}=x")).err(),
                Some(DsnError::ServiceSource(key.to_owned())),
                "{key}"
            );
        }
        for key in ["sslcert", "sslkey", "sslrootcert", "ssl-ca", "sslpassword"] {
            assert_eq!(
                parse(&format!("{base}&{key}=x")).err(),
                Some(DsnError::TlsFileSource(key.to_owned())),
                "{key}"
            );
        }
        for key in [
            "host",
            "hostaddr",
            "port",
            "dbname",
            "user",
            "password",
            "options",
            "application_name",
            "statement-cache-capacity",
            "target_session_attrs",
        ] {
            assert_eq!(
                parse(&format!("{base}&{key}=x")).err(),
                Some(DsnError::Parameter(key.to_owned())),
                "{key}"
            );
        }
        assert_eq!(
            parse(&format!("{base}&sslmode=disable")).err(),
            Some(DsnError::Parameter("sslmode".to_owned()))
        );
    }

    #[test]
    fn a_long_parameter_key_is_cut_in_the_diagnostic() {
        let key = "k".repeat(80);
        let Err(DsnError::Parameter(reported)) = parse(&format!(
            "postgres://app:pw@h:5432/app?sslmode=require&{key}=x"
        )) else {
            panic!("expected a parameter error");
        };
        assert_eq!(reported.chars().count(), 33);
        assert!(reported.ends_with('…'));
    }

    #[test]
    fn a_non_empty_ambient_variable_refuses_the_dsn() {
        for name in AMBIENT_ENVIRONMENT {
            let result = Dsn::parse_with_environment(VALID, |candidate| {
                (candidate == name).then(|| OsString::from("set"))
            });
            assert_eq!(result.err(), Some(DsnError::Ambient(name)), "{name}");
        }
    }

    #[test]
    fn an_empty_ambient_variable_is_ignored() {
        let result = Dsn::parse_with_environment(VALID, |_| Some(OsString::new()));
        assert!(result.is_ok());
    }

    #[test]
    fn a_fragment_is_refused_as_its_own_rule() {
        let err = parse("postgres://app:pw@h:5432/app?sslmode=require#frag").unwrap_err();
        assert_eq!(err, DsnError::Fragment);
        let message = err.to_string();
        assert!(message.contains("fragment"), "{message}");
        assert!(!message.contains("parsed as a URL"), "{message}");
    }

    #[test]
    fn diagnostics_and_debug_never_carry_credentials() {
        let dsn = parse(VALID).unwrap();
        let rendered = format!("{dsn:?}");
        assert!(rendered.contains("db.internal"), "{rendered}");
        assert!(!rendered.contains("s3cret"), "{rendered}");
        assert!(!rendered.contains("app:"), "{rendered}");
        for raw in [
            "postgres://app:s3cret@h:5432/app?sslmode=prefer",
            "postgres://app:s3cret@h:5432/app?sslmode=require&sslkey=s3cret",
            "postgres://app:s3cret@h/app",
        ] {
            let message = parse(raw).unwrap_err().to_string();
            assert!(!message.contains("s3cret"), "{message}");
        }
    }
}
