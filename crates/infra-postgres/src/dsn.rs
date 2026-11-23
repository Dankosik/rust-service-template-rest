use std::fmt;
use std::path::{Path, PathBuf};

use sqlx::ConnectOptions;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use url::Url;

use crate::credentials::read_password;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequiredComponent {
    Host,
    Port,
    User,
    Password,
    Database,
    SslMode,
}

impl fmt::Display for RequiredComponent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Host => "host",
            Self::Port => "port",
            Self::User => "user",
            Self::Password => "password",
            Self::Database => "database",
            Self::SslMode => "sslmode",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueryParameter {
    SslMode,
    SslRootCert,
}

impl fmt::Display for QueryParameter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SslMode => "sslmode",
            Self::SslRootCert => "sslrootcert",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AmbientVar {
    SslRootCert,
    SslCert,
    SslKey,
    Options,
}

impl AmbientVar {
    const ALL: [Self; 4] = [
        Self::SslRootCert,
        Self::SslCert,
        Self::SslKey,
        Self::Options,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::SslRootCert => "PGSSLROOTCERT",
            Self::SslCert => "PGSSLCERT",
            Self::SslKey => "PGSSLKEY",
            Self::Options => "PGOPTIONS",
        }
    }
}

impl fmt::Display for AmbientVar {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(transparent)]
pub struct DsnError(pub(crate) DsnErrorKind);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DsnErrorKind {
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
    Missing(RequiredComponent),
    #[error("postgres dsn must name one tcp host; unix sockets are not supported")]
    Socket,
    #[error("postgres dsn must name one tcp host; fallback hosts are not supported")]
    MultipleHosts,
    #[error("postgres dsn sslmode must be one of disable, require, verify-ca, verify-full")]
    SslMode,
    #[error(
        "postgres dsn requires sslmode=verify-full unless insecure development is explicitly allowed"
    )]
    ServerVerification,
    #[error("postgres dsn sslrootcert must be an absolute file path")]
    RootCertPath,
    #[error("postgres dsn sslrootcert requires sslmode verify-ca or verify-full")]
    RootCertWithoutVerification,
    #[error(
        "postgres dsn contains an unsupported parameter; only sslmode and sslrootcert are accepted"
    )]
    Parameter,
    #[error("postgres dsn parameter {0} must appear only once")]
    Repeated(QueryParameter),
    #[error("postgres dsn must be the only connection source; unset {0} in the environment")]
    Ambient(AmbientVar),
    #[error("postgres dsn must not carry a password when postgres.password_file is set")]
    PasswordSources,
    #[error("postgres password file could not be read ({0})")]
    PasswordFile(std::io::ErrorKind),
    #[error("postgres password file is empty")]
    PasswordFileEmpty,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SslPolicy {
    #[default]
    VerifyFull,
    AllowLocalDevelopment,
}

#[derive(Clone)]
pub struct Dsn {
    options: PgConnectOptions,
    password_file: Option<PathBuf>,
}

impl fmt::Debug for Dsn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dsn")
            .field("host", &self.host())
            .field("port", &self.port())
            .field("database", &self.database())
            .field("ssl_mode", &self.ssl_mode_name())
            .finish_non_exhaustive()
    }
}

impl Dsn {
    pub fn admit(raw: &str) -> Result<Self, DsnError> {
        Self::admit_with(raw, None, SslPolicy::VerifyFull)
    }

    pub fn admit_with(
        raw: &str,
        password_file: Option<&Path>,
        ssl_policy: SslPolicy,
    ) -> Result<Self, DsnError> {
        Self::admit_kind(raw, password_file, ssl_policy, |name| {
            std::env::var_os(name).is_some_and(|value| !value.is_empty())
        })
        .map_err(DsnError)
    }

    #[cfg(test)]
    pub(crate) fn admit_with_environment<F>(raw: &str, is_env_set: F) -> Result<Self, DsnError>
    where
        F: Fn(&str) -> bool,
    {
        Self::admit_kind(raw, None, SslPolicy::AllowLocalDevelopment, is_env_set).map_err(DsnError)
    }

    fn admit_kind<F>(
        raw: &str,
        password_file: Option<&Path>,
        ssl_policy: SslPolicy,
        is_env_set: F,
    ) -> Result<Self, DsnErrorKind>
    where
        F: Fn(&str) -> bool,
    {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(DsnErrorKind::Empty);
        }
        if let Some(var) = AmbientVar::ALL
            .into_iter()
            .find(|var| is_env_set(var.as_str()))
        {
            return Err(DsnErrorKind::Ambient(var));
        }
        if !raw.starts_with("postgres://") && !raw.starts_with("postgresql://") {
            return Err(DsnErrorKind::Scheme);
        }
        let url = Url::parse(raw).map_err(|_| DsnErrorKind::Invalid)?;
        if url.fragment().is_some() {
            return Err(DsnErrorKind::Fragment);
        }
        require_components(&url, password_file.is_some())?;
        check_parameters(&url)?;

        let options = PgConnectOptions::from_url(&url).map_err(|_| DsnErrorKind::Unusable)?;
        if ssl_policy == SslPolicy::VerifyFull
            && !matches!(options.get_ssl_mode(), PgSslMode::VerifyFull)
        {
            return Err(DsnErrorKind::ServerVerification);
        }
        if options.get_socket().is_some() {
            return Err(DsnErrorKind::Socket);
        }
        if options.get_host().contains(',') {
            return Err(DsnErrorKind::MultipleHosts);
        }
        let options = match url.host() {
            Some(url::Host::Ipv6(host)) => options.host(&host.to_string()),
            _ => options,
        };
        let options = match password_file {
            Some(path) => options.password(&read_password(path)?),
            None => options,
        };
        Ok(Self {
            options,
            password_file: password_file.map(Path::to_path_buf),
        })
    }

    #[must_use]
    pub fn password_file(&self) -> Option<&Path> {
        self.password_file.as_deref()
    }

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
        self.options.get_database().unwrap_or_default()
    }

    #[must_use]
    pub fn ssl_mode_name(&self) -> &'static str {
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

fn require_components(url: &Url, password_from_file: bool) -> Result<(), DsnErrorKind> {
    if url.host_str().is_none_or(str::is_empty) {
        return Err(DsnErrorKind::Missing(RequiredComponent::Host));
    }
    if url.port().is_none_or(|port| port == 0) {
        return Err(DsnErrorKind::Missing(RequiredComponent::Port));
    }
    if url.username().is_empty() {
        return Err(DsnErrorKind::Missing(RequiredComponent::User));
    }
    let url_password = url.password().is_some_and(|password| !password.is_empty());
    match (url_password, password_from_file) {
        (false, false) => return Err(DsnErrorKind::Missing(RequiredComponent::Password)),
        (true, true) => return Err(DsnErrorKind::PasswordSources),
        _ => {}
    }
    let database = url.path().trim_start_matches('/');
    if database.is_empty() || database.contains('/') {
        return Err(DsnErrorKind::Missing(RequiredComponent::Database));
    }
    Ok(())
}

fn check_parameters(url: &Url) -> Result<(), DsnErrorKind> {
    let mut sslmode_verifies = None;
    let mut has_root_cert = false;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "sslmode" => {
                if sslmode_verifies.is_some() {
                    return Err(DsnErrorKind::Repeated(QueryParameter::SslMode));
                }
                sslmode_verifies = Some(match value.as_ref() {
                    "disable" | "require" => false,
                    "verify-ca" | "verify-full" => true,
                    _ => return Err(DsnErrorKind::SslMode),
                });
            }
            "sslrootcert" => {
                if has_root_cert {
                    return Err(DsnErrorKind::Repeated(QueryParameter::SslRootCert));
                }
                if !Path::new(value.as_ref()).is_absolute() {
                    return Err(DsnErrorKind::RootCertPath);
                }
                has_root_cert = true;
            }
            _ => return Err(DsnErrorKind::Parameter),
        }
    }
    let verifies_certificate =
        sslmode_verifies.ok_or(DsnErrorKind::Missing(RequiredComponent::SslMode))?;
    if has_root_cert && !verifies_certificate {
        return Err(DsnErrorKind::RootCertWithoutVerification);
    }
    Ok(())
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
        for (authority, expected) in [
            ("[::1]", "::1"),
            ("[2001:0db8:0:0:0:0:0:1]", "2001:db8::1"),
            ("127.0.0.1", "127.0.0.1"),
            ("db.internal", "db.internal"),
        ] {
            for mode in ["disable", "require", "verify-ca", "verify-full"] {
                let dsn = parse(&format!(
                    "postgresql://a%40pp:p%40ss%2Fword@{authority}:6543/app?sslmode={mode}"
                ))
                .unwrap();
                let options = dsn.connect_options();
                assert_eq!(options.get_host(), expected);
                assert_eq!(dsn.port(), 6543);
                assert_eq!(dsn.database(), "app");
                assert_eq!(dsn.ssl_mode_name(), mode);
                assert_eq!(options.get_username(), "a@pp");
            }
        }
    }

    #[tokio::test]
    async fn admitted_ipv6_reaches_the_native_tls_handshake() {
        use sqlx::ConnectOptions;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
        let dsn = parse(&format!(
            "postgres://app:pw@[::1]:{}/app?sslmode=verify-full",
            listener.local_addr().unwrap().port()
        ))
        .unwrap();
        let options = dsn.connect_options();
        let peer = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut ssl_request = [0; 8];
            socket.read_exact(&mut ssl_request).await.unwrap();
            assert_eq!(ssl_request, [0, 0, 0, 8, 4, 210, 22, 47]);
            socket.write_all(b"S").await.unwrap();
            let mut record_header = [0; 5];
            socket.read_exact(&mut record_header).await.unwrap();
            assert_eq!(record_header[0], 22);
            assert_eq!(record_header[1], 3);
            assert!(u16::from_be_bytes([record_header[3], record_header[4]]) > 0);
        };
        let (connected, ()) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(options.connect(), peer)
        })
        .await
        .expect("IPv6 TCP and TLS opening are bounded");
        assert!(connected.is_err());
    }

    #[test]
    fn every_component_is_required() {
        let cases = [
            ("", DsnErrorKind::Empty),
            ("   ", DsnErrorKind::Empty),
            ("host=h port=5432 user=u", DsnErrorKind::Scheme),
            ("mysql://app:pw@h:5432/app", DsnErrorKind::Scheme),
            (
                "postgres://app:pw@h:5432/app?sslmode=require#frag",
                DsnErrorKind::Fragment,
            ),
            (
                "postgres://app:pw@:5432/app?sslmode=require",
                DsnErrorKind::Invalid,
            ),
            (
                "postgres:///app?sslmode=require",
                DsnErrorKind::Missing(RequiredComponent::Host),
            ),
            (
                "postgres://app:pw@h/app?sslmode=require",
                DsnErrorKind::Missing(RequiredComponent::Port),
            ),
            (
                "postgres://app:pw@h:0/app?sslmode=require",
                DsnErrorKind::Missing(RequiredComponent::Port),
            ),
            (
                "postgres://:pw@h:5432/app?sslmode=require",
                DsnErrorKind::Missing(RequiredComponent::User),
            ),
            (
                "postgres://app@h:5432/app?sslmode=require",
                DsnErrorKind::Missing(RequiredComponent::Password),
            ),
            (
                "postgres://app:@h:5432/app?sslmode=require",
                DsnErrorKind::Missing(RequiredComponent::Password),
            ),
            (
                "postgres://app:pw@h:5432?sslmode=require",
                DsnErrorKind::Missing(RequiredComponent::Database),
            ),
            (
                "postgres://app:pw@h:5432/app/extra?sslmode=require",
                DsnErrorKind::Missing(RequiredComponent::Database),
            ),
            (
                "postgres://app:pw@h:5432/app",
                DsnErrorKind::Missing(RequiredComponent::SslMode),
            ),
        ];
        for (raw, expected) in cases {
            assert_eq!(parse(raw).err(), Some(DsnError(expected)), "{raw}");
        }
        for (raw, label) in [
            ("postgres:///app?sslmode=require", "host"),
            ("postgres://app:pw@h/app?sslmode=require", "port"),
            ("postgres://:pw@h:5432/app?sslmode=require", "user"),
            ("postgres://app@h:5432/app?sslmode=require", "password"),
            ("postgres://app:pw@h:5432?sslmode=require", "database"),
            ("postgres://app:pw@h:5432/app", "sslmode"),
        ] {
            assert_eq!(
                parse(raw).unwrap_err().to_string(),
                format!("postgres dsn requires an explicit {label}"),
                "{raw}"
            );
        }
    }

    #[test]
    fn sockets_and_fallback_hosts_are_refused() {
        assert_eq!(
            parse("postgres://app:pw@%2Fvar%2Frun%2Fpostgresql:5432/app?sslmode=disable").err(),
            Some(DsnError(DsnErrorKind::Socket))
        );
        assert_eq!(
            parse("postgres://app:pw@h1,h2:5432/app?sslmode=disable").err(),
            Some(DsnError(DsnErrorKind::MultipleHosts))
        );
        assert_eq!(
            parse("postgres://app:pw@h1:5432,h2:5432/app?sslmode=disable").err(),
            Some(DsnError(DsnErrorKind::Invalid))
        );
    }

    #[test]
    fn only_deterministic_ssl_modes_are_admitted() {
        for mode in ["disable", "require", "verify-ca", "verify-full"] {
            let raw = format!("postgres://app:pw@h:5432/app?sslmode={mode}");
            let dsn = parse(&raw).unwrap();
            assert_eq!(dsn.ssl_mode_name(), mode);
            let production =
                Dsn::admit_kind(&raw, None, SslPolicy::VerifyFull, |_| false).map_err(DsnError);
            if mode == "verify-full" {
                assert!(production.is_ok());
            } else {
                assert_eq!(
                    production.unwrap_err(),
                    DsnError(DsnErrorKind::ServerVerification)
                );
            }
        }
        for mode in ["allow", "prefer", "", "REQUIRE", "bogus"] {
            assert_eq!(
                parse(&format!("postgres://app:pw@h:5432/app?sslmode={mode}")).err(),
                Some(DsnError(DsnErrorKind::SslMode)),
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
                Some(DsnError(DsnErrorKind::RootCertWithoutVerification)),
                "{mode}"
            );
        }
        for path in ["rds-ca.pem", "", "system"] {
            assert_eq!(
                parse(&format!("{base}?sslmode=verify-full&sslrootcert={path}")).err(),
                Some(DsnError(DsnErrorKind::RootCertPath)),
                "{path}"
            );
        }
    }

    #[test]
    fn every_other_parameter_is_refused() {
        let base = "postgres://app:pw@h:5432/app?sslmode=verify-full&sslrootcert=/ca.pem";
        for (key, param) in [
            ("sslmode", QueryParameter::SslMode),
            ("sslrootcert", QueryParameter::SslRootCert),
        ] {
            let err = parse(&format!("{base}&{key}=x")).unwrap_err();
            assert_eq!(err, DsnError(DsnErrorKind::Repeated(param)), "{key}");
            assert_eq!(
                err.to_string(),
                format!("postgres dsn parameter {key} must appear only once"),
                "{key}"
            );
        }
        for key in [
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
                Some(DsnError(DsnErrorKind::Parameter)),
                "{key}"
            );
        }
    }

    #[test]
    fn only_a_variable_the_url_cannot_override_refuses_the_dsn() {
        for (name, var) in [
            ("PGSSLROOTCERT", AmbientVar::SslRootCert),
            ("PGSSLCERT", AmbientVar::SslCert),
            ("PGSSLKEY", AmbientVar::SslKey),
            ("PGOPTIONS", AmbientVar::Options),
        ] {
            let err =
                Dsn::admit_with_environment(VALID, |candidate| candidate == name).unwrap_err();
            assert_eq!(err, DsnError(DsnErrorKind::Ambient(var)), "{name}");
            assert_eq!(
                err.to_string(),
                format!(
                    "postgres dsn must be the only connection source; unset {name} in the environment"
                ),
                "{name}"
            );
        }
        for name in ["PGHOST", "PGPORT", "PGUSER", "PGPASSWORD", "PGSSLMODE"] {
            let result = Dsn::admit_with_environment(VALID, |candidate| candidate == name);
            assert!(result.is_ok(), "{name}");
        }
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
    )]
    fn a_password_file_is_the_only_password_source_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("password");
        let without_password = "postgres://app@db.internal:5432/app?sslmode=require";
        let admit = |raw: &str| {
            Dsn::admit_kind(raw, Some(&file), SslPolicy::AllowLocalDevelopment, |_| {
                false
            })
            .map_err(DsnError)
        };

        assert_eq!(
            admit(without_password).err(),
            Some(DsnError(DsnErrorKind::PasswordFile(
                std::io::ErrorKind::NotFound
            )))
        );
        std::fs::write(&file, "\n").unwrap();
        assert_eq!(
            admit(without_password).err(),
            Some(DsnError(DsnErrorKind::PasswordFileEmpty))
        );
        std::fs::write(&file, vec![b'x'; 16 * 1024]).unwrap();
        assert!(admit(without_password).is_ok());
        std::fs::write(&file, vec![b'x'; 16 * 1024 + 1]).unwrap();
        assert_eq!(
            admit(without_password).err(),
            Some(DsnError(DsnErrorKind::PasswordFile(
                std::io::ErrorKind::InvalidData
            )))
        );
        std::fs::write(&file, "s3cret\n").unwrap();
        assert_eq!(
            admit(VALID).err(),
            Some(DsnError(DsnErrorKind::PasswordSources))
        );
        let dsn = admit(without_password).unwrap();
        assert_eq!(dsn.password_file(), Some(file.as_path()));
        assert_eq!(parse(VALID).unwrap().password_file(), None);
        assert!(!format!("{dsn:?}").contains("s3cret"));
        let ipv6 = admit("postgres://app@[::1]:6543/app?sslmode=verify-full").unwrap();
        assert_eq!(ipv6.host(), "::1");
        assert_eq!(ipv6.password_file(), Some(file.as_path()));
        assert_eq!(ipv6.ssl_mode_name(), "verify-full");

        std::fs::write(&file, " p w \r\n").unwrap();
        assert_eq!(read_password(&file).unwrap().as_str(), " p w ");
        #[cfg(unix)]
        {
            let mounted = dir.path().join("mounted");
            std::os::unix::fs::symlink(&file, &mounted).unwrap();
            assert_eq!(read_password(&mounted).unwrap().as_str(), " p w ");
        }
        dir.close().unwrap();
    }

    #[test]
    fn diagnostics_and_debug_never_carry_credentials() {
        let dsn = parse(VALID).unwrap();
        let rendered = format!("{dsn:?}");
        assert!(rendered.contains("db.internal"));
        assert!(!rendered.contains("s3cret"));
        assert!(!rendered.contains("app:"));
        for raw in [
            "postgres://app:s3cret@h:5432/app?sslmode=prefer",
            "postgres://app:s3cret@h:5432/app?sslmode=require&sslkey=s3cret",
            "postgres://app:pw@h:5432/app?sslmode=require&s3cret=x",
            "postgres://app:s3cret@h/app",
        ] {
            let error = parse(raw).unwrap_err();
            let message = format!("{error} {error:?}");
            assert!(!message.contains("s3cret"), "{message}");
        }
    }
}
