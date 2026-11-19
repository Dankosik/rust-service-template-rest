use std::marker::PhantomData;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use secrecy::SecretString;
use serde::de::{IntoDeserializer as _, Unexpected};
use serde::{Deserialize, Deserializer};
use serde_with::DeserializeAs;

pub(crate) struct StrictString;

impl<'de> DeserializeAs<'de, String> for StrictString {
    fn deserialize_as<D>(deserializer: D) -> Result<String, D::Error>
    where
        D: Deserializer<'de>,
    {
        string(deserializer)
    }
}

impl<'de> DeserializeAs<'de, PathBuf> for StrictString {
    fn deserialize_as<D>(deserializer: D) -> Result<PathBuf, D::Error>
    where
        D: Deserializer<'de>,
    {
        string(deserializer).map(PathBuf::from)
    }
}

fn string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringVisitor;

    impl serde::de::Visitor<'_> for StringVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a string")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(value.to_owned())
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(value)
        }
    }

    deserializer.deserialize_any(StringVisitor)
}

pub(crate) struct StrictBool;

impl<'de> DeserializeAs<'de, bool> for StrictBool {
    fn deserialize_as<D>(deserializer: D) -> Result<bool, D::Error>
    where
        D: Deserializer<'de>,
    {
        const EXPECTED: &str = "a boolean";
        serde_untagged::UntaggedEnumVisitor::new()
            .expecting(EXPECTED)
            .bool(Ok)
            .string(|text| {
                text.trim()
                    .parse()
                    .map_err(|_| serde::de::Error::invalid_value(Unexpected::Str(text), &EXPECTED))
            })
            .deserialize(deserializer)
    }
}

#[derive(Debug, Default)]
pub(crate) struct StrictEnum<T = ()>(pub(crate) T);

impl<'de, T> Deserialize<'de> for StrictEnum<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        StrictEnum::<()>::deserialize_as(deserializer).map(Self)
    }
}

impl<'de, T> DeserializeAs<'de, T> for StrictEnum
where
    T: Deserialize<'de>,
{
    fn deserialize_as<D>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = string(deserializer)?;
        T::deserialize(raw.as_str().into_deserializer())
    }
}

#[allow(
    dead_code,
    reason = "only optional webhook profiles require non-optional secrets"
)]
pub(crate) struct StrictSecret;

impl<'de> DeserializeAs<'de, SecretString> for StrictSecret {
    fn deserialize_as<D>(deserializer: D) -> Result<SecretString, D::Error>
    where
        D: Deserializer<'de>,
    {
        string(deserializer).map(SecretString::from)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Scalar<T> {
    Typed(T),
    Text(String),
}

impl<T: FromStr> Scalar<T> {
    fn into_count<E: serde::de::Error>(self) -> Result<T, E> {
        match self {
            Self::Typed(value) => Ok(value),
            Self::Text(value) => value.parse().map_err(|_| invalid_count()),
        }
    }
}

fn invalid_count<E: serde::de::Error>() -> E {
    E::invalid_value(
        Unexpected::Other("a malformed or out-of-range count"),
        &"an integer",
    )
}

pub(crate) struct StrictCount;

impl<'de, T> DeserializeAs<'de, T> for StrictCount
where
    T: Deserialize<'de> + FromStr,
{
    fn deserialize_as<D>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
    {
        Scalar::deserialize(deserializer)
            .map_err(|_| invalid_count::<D::Error>())?
            .into_count()
    }
}

fn invalid_ratio<E: serde::de::Error>() -> E {
    E::invalid_value(Unexpected::Other("a malformed number"), &"a number")
}

pub(crate) struct StrictRatio;

impl<'de> DeserializeAs<'de, f64> for StrictRatio {
    fn deserialize_as<D>(deserializer: D) -> Result<f64, D::Error>
    where
        D: Deserializer<'de>,
    {
        match Scalar::<f64>::deserialize(deserializer).map_err(|_| invalid_ratio::<D::Error>())? {
            Scalar::Typed(value) => Ok(value),
            Scalar::Text(value) => value.trim().parse().map_err(|_| invalid_ratio()),
        }
    }
}

const LISTEN_ADDR_FORM: &str = "an IP address and port, or :port; hostnames are not resolved";

pub(crate) struct BlankAsNone<T = StrictString>(PhantomData<fn() -> T>);

impl<'de, V, T> DeserializeAs<'de, Option<V>> for BlankAsNone<T>
where
    T: DeserializeAs<'de, V>,
{
    fn deserialize_as<D>(deserializer: D) -> Result<Option<V>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let Some(raw): Option<String> = Option::<StrictString>::deserialize_as(deserializer)?
        else {
            return Ok(None);
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        T::deserialize_as(trimmed.into_deserializer()).map(Some)
    }
}

pub(crate) struct BlankSecretAsNone;

impl<'de> DeserializeAs<'de, Option<SecretString>> for BlankSecretAsNone {
    fn deserialize_as<D>(deserializer: D) -> Result<Option<SecretString>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: Option<String> = Option::<StrictString>::deserialize_as(deserializer)?;
        Ok(raw.and_then(|value| (!value.trim().is_empty()).then(|| SecretString::from(value))))
    }
}

pub(crate) struct ListenAddr;

impl<'de> DeserializeAs<'de, SocketAddr> for ListenAddr {
    fn deserialize_as<D>(deserializer: D) -> Result<SocketAddr, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = string(deserializer)?;
        parse_listen_addr(&raw)
    }
}

fn parse_listen_addr<E: serde::de::Error>(value: &str) -> Result<SocketAddr, E> {
    let trimmed = value.trim();
    match trimmed.strip_prefix(':') {
        Some(port) => format!("0.0.0.0:{port}").parse(),
        None => trimmed.parse(),
    }
    .map_err(|_| E::invalid_value(Unexpected::Str(value), &LISTEN_ADDR_FORM))
}

pub(crate) struct StrictDuration;

impl<'de> DeserializeAs<'de, Duration> for StrictDuration {
    fn deserialize_as<D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        decoded_duration(&string(deserializer)?, "a duration")
    }
}

pub(crate) fn decoded_duration<E: serde::de::Error>(
    text: &str,
    expected: &str,
) -> Result<Duration, E> {
    parse_duration(text).ok_or_else(|| E::invalid_value(Unexpected::Str(text), &expected))
}

fn parse_duration(text: &str) -> Option<Duration> {
    let span: jiff::Span = text.trim().parse().ok()?;
    let signed = span
        .to_duration(jiff::SpanRelativeTo::days_are_24_hours())
        .ok()?;
    Duration::try_from(signed).ok()
}

pub(crate) fn display_duration(duration: Duration) -> String {
    jiff::fmt::friendly::SpanPrinter::new().unsigned_duration_to_string(&duration)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[rstest::rstest]
    #[case::port_only(":8080", "0.0.0.0:8080")]
    #[case::ipv6("[::1]:9000", "[::1]:9000")]
    #[case::ipv4("127.0.0.1:8080", "127.0.0.1:8080")]
    #[case::trimmed("  :8080  ", "0.0.0.0:8080")]
    fn socket_addr_accepts_supported_forms(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(
            parse_listen_addr::<serde::de::value::Error>(input).unwrap(),
            expected.parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn socket_addr_rejects_hostnames() {
        #[serde_with::serde_as]
        #[derive(Debug, Deserialize)]
        struct Probe {
            #[serde_as(as = "ListenAddr")]
            #[expect(dead_code, reason = "the probe only checks that deserialization fails")]
            addr: SocketAddr,
        }

        let err = toml::from_str::<Probe>("addr = \"localhost:8080\"").unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("localhost:8080"), "{rendered}");
        assert!(rendered.contains(LISTEN_ADDR_FORM), "{rendered}");
    }

    #[test]
    fn strict_deserializers_reject_scalar_coercion() {
        #[derive(Debug, Deserialize, PartialEq, Eq)]
        #[serde(rename_all = "lowercase")]
        enum Kind {
            Alpha,
            Beta,
        }

        #[serde_with::serde_as]
        #[derive(Debug, Deserialize)]
        struct Probe {
            #[serde(default)]
            #[serde_as(as = "BlankAsNone")]
            path: Option<String>,
            #[serde(default)]
            #[serde_as(as = "Option<StrictBool>")]
            flag: Option<bool>,
            #[serde(default)]
            #[serde_as(as = "Option<StrictEnum>")]
            kind: Option<Kind>,
            #[serde(default)]
            #[serde_as(as = "Option<StrictDuration>")]
            ttl: Option<Duration>,
            #[serde(default)]
            #[serde_as(as = "BTreeMap<_, StrictSecret>")]
            secrets: BTreeMap<String, SecretString>,
        }

        let valid: Probe = toml::from_str(
            "path = \"/tmp/key\"\nflag = \" true \"\nkind = \"alpha\"\nttl = \"5s\"\n[secrets]\nk = \"v\"\n",
        )
        .unwrap();
        assert_eq!(valid.path.as_deref(), Some("/tmp/key"));
        assert_eq!(valid.flag, Some(true));
        assert_eq!(valid.kind, Some(Kind::Alpha));
        assert_eq!(valid.ttl, Some(Duration::from_secs(5)));
        assert_eq!(valid.secrets.len(), 1);

        for invalid in [
            "path = false\n",
            "flag = 1\n",
            "flag = \"yes\"\n",
            "kind = false\n",
            "kind = \"gamma\"\n",
            "ttl = 5\n",
            "ttl = \"later\"\n",
            "[secrets]\nk = 7\n",
        ] {
            assert!(toml::from_str::<Probe>(invalid).is_err(), "{invalid}");
        }
    }
}
