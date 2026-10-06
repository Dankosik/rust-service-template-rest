//! Connection-string admission.
//!
//! `sqlx` is permissive: every `PgConnectOptions` constructor starts from the
//! libpq environment (`PGHOST`, `PGSSLROOTCERT`, ...), falls back to
//! `.pgpass` when the URL has no password, accepts Unix sockets and
//! `allow`/`prefer` TLS fallback, and warns with key *and value* on a
//! parameter it does not know. The template's policy is one explicit,
//! self-contained URL: what the operator set is exactly what connects, and a
//! diagnostic never carries the value. This module checks the URL text before
//! `sqlx` sees it and then reads the driver's own interpretation back.
//!
//! The password is the one component with a second admitted source: a file
//! the platform rewrites when it rotates the credential. The URL then carries
//! no password, so there is still exactly one.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use url::Url;

/// Environment variables `sqlx` would still merge into an admitted URL
/// (`sqlx-postgres/src/options/mod.rs`). Every other libpq variable it reads
/// is overwritten by a component the URL must carry, so it cannot change
/// what connects and is not refused.
const AMBIENT_ENVIRONMENT: [&str; 4] = ["PGSSLROOTCERT", "PGSSLCERT", "PGSSLKEY", "PGOPTIONS"];

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
    #[error("postgres dsn sslrootcert must be an absolute file path")]
    RootCertPath,
    #[error("postgres dsn sslrootcert requires sslmode verify-ca or verify-full")]
    RootCertWithoutVerification,
    #[error(
        "postgres dsn parameter {0:?} is not supported; only sslmode and sslrootcert are accepted"
    )]
    Parameter(String),
    #[error("postgres dsn must be the only connection source; unset {0} in the environment")]
    Ambient(&'static str),
    #[error("postgres dsn must not carry a password when postgres.password_file is set")]
    PasswordSources,
    #[error("postgres password file could not be read ({0})")]
    PasswordFile(std::io::ErrorKind),
    #[error("postgres password file is empty")]
    PasswordFileEmpty,
}

/// An admitted connection string, ready to become connect options.
#[derive(Clone)]
pub struct Dsn {
    options: PgConnectOptions,
    password_file: Option<PathBuf>,
}

impl std::fmt::Debug for Dsn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dsn")
            .field("host", &self.host())
            .field("port", &self.port())
            .field("database", &self.database())
            .field("ssl_mode", &self.ssl_mode_name())
            .finish_non_exhaustive()
    }
}

impl Dsn {
    /// Admit `raw` against the template rules and the process environment.
    /// Occupancy of a non-empty ambient `PG*` variable is part of this call,
    /// not only the URL text.
    ///
    /// # Errors
    ///
    /// The first violated rule, without the offending value.
    pub fn admit(raw: &str) -> Result<Self, DsnError> {
        Self::admit_with(raw, None)
    }

    /// [`Dsn::admit`] with the password read from `password_file` when one
    /// is given (`postgres.password_file`). The URL must then carry no
    /// password; the file holds the password alone, and one trailing line
    /// break is not part of it.
    ///
    /// # Errors
    ///
    /// The first violated rule, without the offending value.
    pub fn admit_with(raw: &str, password_file: Option<&Path>) -> Result<Self, DsnError> {
        Self::admit_from(raw, password_file, |name| {
            std::env::var_os(name).is_some_and(|value| !value.is_empty())
        })
    }

    /// [`Dsn::admit`] with an explicit occupancy lookup, so the ambient rule
    /// can be tested without mutating the process environment. `true` means
    /// the named variable is set to a non-empty value.
    #[cfg(test)]
    pub(crate) fn admit_with_environment<F>(raw: &str, occupied: F) -> Result<Self, DsnError>
    where
        F: Fn(&str) -> bool,
    {
        Self::admit_from(raw, None, occupied)
    }

    fn admit_from<F>(raw: &str, password_file: Option<&Path>, occupied: F) -> Result<Self, DsnError>
    where
        F: Fn(&str) -> bool,
    {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(DsnError::Empty);
        }
        if let Some(name) = AMBIENT_ENVIRONMENT.into_iter().find(|name| occupied(name)) {
            return Err(DsnError::Ambient(name));
        }
        if !raw.starts_with("postgres://") && !raw.starts_with("postgresql://") {
            return Err(DsnError::Scheme);
        }
        let url = Url::parse(raw).map_err(|_| DsnError::Invalid)?;
        if url.fragment().is_some() {
            return Err(DsnError::Fragment);
        }
        require_components(&url, password_file.is_some())?;
        check_parameters(&url)?;

        // The text holds every component and only known parameters, so what
        // `sqlx` parses is what the operator wrote. The remaining rules read
        // the driver's own interpretation instead of repeating its parser.
        let options = PgConnectOptions::from_str(raw).map_err(|_| DsnError::Unusable)?;
        if options.get_socket().is_some() {
            return Err(DsnError::Socket);
        }
        if options.get_host().contains(',') {
            return Err(DsnError::MultipleHosts);
        }
        // Without a password in the URL the driver took one from
        // `PGPASSWORD` or `.pgpass`; the file's value replaces it, so neither
        // ever connects.
        let options = match password_file {
            Some(path) => options.password(&read_password(path)?),
            None => options,
        };
        Ok(Self {
            options,
            password_file: password_file.map(Path::to_path_buf),
        })
    }

    /// The file the password came from, when it did not come from the URL.
    #[must_use]
    pub fn password_file(&self) -> Option<&Path> {
        self.password_file.as_deref()
    }

    /// Connect options for `sqlx`, before the template's session defaults.
    #[must_use]
    pub(crate) fn connect_options(&self) -> PgConnectOptions {
        self.options.clone()
    }

    #[must_use]
    pub fn host(&self) -> &str {
        self.options.get_host()
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.options.get_port()
    }

    #[must_use]
    pub fn database(&self) -> &str {
        // `require_components` refused a URL without a database.
        self.options.get_database().unwrap_or_default()
    }

    /// The admitted `sslmode`, as the operator spelled it.
    #[must_use]
    pub fn ssl_mode_name(&self) -> &'static str {
        // `allow` and `prefer` never pass admission; they are named only to
        // keep the match total.
        match self.options.get_ssl_mode() {
            PgSslMode::Disable => "disable",
            PgSslMode::Allow => "allow",
            PgSslMode::Prefer => "prefer",
            PgSslMode::Require => "require",
            PgSslMode::VerifyCa => "verify-ca",
            PgSslMode::VerifyFull => "verify-full",
        }
    }
}

/// The password a platform wrote to `path`.
#[allow(
    clippy::disallowed_methods,
    reason = "startup PostgreSQL admission owns the initial password read; refresh uses async IO"
)]
fn read_password(path: &Path) -> Result<String, DsnError> {
    let content =
        std::fs::read_to_string(path).map_err(|err| DsnError::PasswordFile(err.kind()))?;
    password_from(content)
}

/// The password in a password file's content: the whole file, minus one
/// trailing line break that editors and `echo` add.
pub(crate) fn password_from(mut password: String) -> Result<String, DsnError> {
    if password.ends_with('\n') {
        password.pop();
        if password.ends_with('\r') {
            password.pop();
        }
    }
    if password.is_empty() {
        return Err(DsnError::PasswordFileEmpty);
    }
    Ok(password)
}

/// Every component the driver would otherwise take from the environment,
/// `.pgpass`, or its own defaults. With a password file the URL carries no
/// password instead.
fn require_components(url: &Url, password_from_file: bool) -> Result<(), DsnError> {
    if url.host_str().is_none_or(str::is_empty) {
        return Err(DsnError::Missing("host"));
    }
    if url.port().is_none_or(|port| port == 0) {
        return Err(DsnError::Missing("port"));
    }
    if url.username().is_empty() {
        return Err(DsnError::Missing("user"));
    }
    match (url.password().is_none_or(str::is_empty), password_from_file) {
        (true, false) => return Err(DsnError::Missing("password")),
        (false, true) => return Err(DsnError::PasswordSources),
        _ => {}
    }
    let database = url.path().trim_start_matches('/');
    if database.is_empty() || database.contains('/') {
        return Err(DsnError::Missing("database"));
    }
    Ok(())
}

/// Admit `sslmode` (once, with a mode that has one TLS outcome) and an
/// optional `sslrootcert`; refuse every other key before `sqlx` can log its
/// value.
///
/// `allow` and `prefer` negotiate TLS per attempt, so two connections from
/// one pool could differ in what they protect. `sslrootcert` names the CA
/// file of a private certificate authority (RDS, Cloud SQL, an in-house CA);
/// `sqlx` adds it to the bundled webpki roots and ignores it under
/// `require`, so it is admitted only where it is actually verified.
fn check_parameters(url: &Url) -> Result<(), DsnError> {
    let mut verifies_certificate = None;
    let mut has_root_cert = false;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "sslmode" if verifies_certificate.is_none() => {
                verifies_certificate = Some(match value.as_ref() {
                    "disable" | "require" => false,
                    "verify-ca" | "verify-full" => true,
                    _ => return Err(DsnError::SslMode),
                });
            }
            "sslrootcert" if !has_root_cert => {
                if !Path::new(value.as_ref()).is_absolute() {
                    return Err(DsnError::RootCertPath);
                }
                has_root_cert = true;
            }
            _ => return Err(DsnError::Parameter(bounded(&key))),
        }
    }
    let verifies_certificate = verifies_certificate.ok_or(DsnError::Missing("sslmode"))?;
    if has_root_cert && !verifies_certificate {
        return Err(DsnError::RootCertWithoutVerification);
    }
    Ok(())
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

    fn parse(raw: &str) -> Result<Dsn, DsnError> {
        Dsn::admit_with_environment(raw, |_| false)
    }

    #[test]
    fn a_complete_url_is_admitted() {
        let dsn = parse(VALID).unwrap();
        assert_eq!(dsn.host(), "db.internal");
        assert_eq!(dsn.port(), 5432);
        assert_eq!(dsn.database(), "app");
        assert_eq!(dsn.ssl_mode_name(), "require");
        let options = dsn.connect_options();
        assert_eq!(options.get_username(), "app");
        assert!(options.get_socket().is_none());
    }

    #[test]
    fn postgresql_scheme_and_ipv6_hosts_are_admitted() {
        let dsn = parse("postgresql://app:pw@[::1]:6543/app?sslmode=disable").unwrap();
        assert_eq!(dsn.host(), "[::1]");
        assert_eq!(dsn.port(), 6543);
    }

    #[test]
    fn percent_encoded_username_is_decoded_by_the_driver() {
        let dsn = parse("postgres://a%40pp:p%40ss%2Fword@h:5432/app?sslmode=disable").unwrap();
        assert_eq!(dsn.connect_options().get_username(), "a@pp");
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
            ("postgres:///app?sslmode=require", DsnError::Missing("host")),
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
    fn a_private_ca_file_is_admitted_only_where_it_is_verified() {
        let base = "postgres://app:pw@h:5432/app";
        let ca = "sslrootcert=/etc/ssl/rds-ca.pem";
        for mode in ["verify-ca", "verify-full"] {
            let dsn = parse(&format!("{base}?sslmode={mode}&{ca}")).unwrap();
            assert_eq!(dsn.ssl_mode_name(), mode);
        }
        for mode in ["disable", "require"] {
            assert_eq!(
                parse(&format!("{base}?sslmode={mode}&{ca}")).err(),
                Some(DsnError::RootCertWithoutVerification),
                "{mode}"
            );
        }
        for path in ["rds-ca.pem", "", "system"] {
            assert_eq!(
                parse(&format!("{base}?sslmode=verify-full&sslrootcert={path}")).err(),
                Some(DsnError::RootCertPath),
                "{path}"
            );
        }
    }

    #[test]
    fn every_other_parameter_is_refused_by_name() {
        let base = "postgres://app:pw@h:5432/app?sslmode=verify-full&sslrootcert=/ca.pem";
        for key in [
            "sslmode",
            "sslrootcert",
            "ssl-root-cert",
            "ssl-ca",
            "sslcert",
            "sslkey",
            "sslpassword",
            "service",
            "passfile",
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
    fn only_a_variable_the_url_cannot_override_refuses_the_dsn() {
        for name in AMBIENT_ENVIRONMENT {
            let result = Dsn::admit_with_environment(VALID, |candidate| candidate == name);
            assert_eq!(result.err(), Some(DsnError::Ambient(name)), "{name}");
        }
        for name in ["PGHOST", "PGPORT", "PGUSER", "PGPASSWORD", "PGSSLMODE"] {
            let result = Dsn::admit_with_environment(VALID, |candidate| candidate == name);
            assert!(result.is_ok(), "{name}");
        }
    }

    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
    )]
    fn a_password_file_is_the_only_password_source_when_set() {
        let dir = std::env::temp_dir().join(format!("dsn-password-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("password");
        let without_password = "postgres://app@db.internal:5432/app?sslmode=require";
        let admit = |raw: &str| Dsn::admit_from(raw, Some(&file), |_| false);

        assert_eq!(
            admit(without_password).err(),
            Some(DsnError::PasswordFile(std::io::ErrorKind::NotFound))
        );
        std::fs::write(&file, "\n").unwrap();
        assert_eq!(
            admit(without_password).err(),
            Some(DsnError::PasswordFileEmpty)
        );
        std::fs::write(&file, "s3cret\n").unwrap();
        assert_eq!(admit(VALID).err(), Some(DsnError::PasswordSources));
        let dsn = admit(without_password).unwrap();
        assert_eq!(dsn.password_file(), Some(file.as_path()));
        assert_eq!(parse(VALID).unwrap().password_file(), None);
        assert!(!format!("{dsn:?}").contains("s3cret"));

        // Only one trailing line break is dropped; the rest is the password.
        std::fs::write(&file, " p w \r\n").unwrap();
        assert_eq!(read_password(&file).unwrap(), " p w ");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn diagnostics_and_debug_never_carry_credentials() {
        let dsn = parse(VALID).unwrap();
        let rendered = format!("{dsn:?}");
        // The assertions carry no message: a failure must not print the rendering.
        assert!(rendered.contains("db.internal"));
        assert!(!rendered.contains("s3cret"));
        assert!(!rendered.contains("app:"));
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
