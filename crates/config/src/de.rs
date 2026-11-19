use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;
use serde::de::{IntoDeserializer as _, Unexpected};

#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
pub(crate) struct StrictString(#[serde(deserialize_with = "string")] pub(crate) String);

pub(crate) fn string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
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

pub(crate) fn optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<StrictString>::deserialize(deserializer)?.map(|value| value.0))
}

pub(crate) fn boolean<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct BoolVisitor;

    impl serde::de::Visitor<'_> for BoolVisitor {
        type Value = bool;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a boolean")
        }

        fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(value)
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            match value.trim() {
                "true" => Ok(true),
                "false" => Ok(false),
                _ => Err(E::invalid_value(Unexpected::Str(value), &self)),
            }
        }
    }

    deserializer.deserialize_any(BoolVisitor)
}

#[allow(dead_code, reason = "only optional profiles use optional booleans")]
pub(crate) fn optional_boolean<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct StrictBool(#[serde(deserialize_with = "boolean")] bool);

    Ok(Option::<StrictBool>::deserialize(deserializer)?.map(|value| value.0))
}

#[derive(Debug, Default)]
pub(crate) struct StrictEnum<T>(pub(crate) T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for StrictEnum<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        string_enum(deserializer).map(Self)
    }
}

pub(crate) fn string_enum<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    let raw = string(deserializer)?;
    T::deserialize(raw.as_str().into_deserializer())
}

#[allow(dead_code, reason = "only optional profiles use optional enums")]
pub(crate) fn optional_string_enum<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Ok(Option::<StrictEnum<T>>::deserialize(deserializer)?.map(|value| value.0))
}

#[allow(
    dead_code,
    reason = "only optional webhook profiles require non-optional secrets"
)]
pub(crate) fn secret<'de, D>(deserializer: D) -> Result<SecretString, D::Error>
where
    D: serde::Deserializer<'de>,
{
    string(deserializer).map(SecretString::from)
}

#[allow(dead_code, reason = "only inbound webhooks decode a secret map")]
pub(crate) fn secret_map<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, SecretString>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct StrictSecret(#[serde(deserialize_with = "secret")] SecretString);

    let raw = BTreeMap::<String, StrictSecret>::deserialize(deserializer)?;
    Ok(raw.into_iter().map(|(key, value)| (key, value.0)).collect())
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

pub(crate) fn unsigned_count<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + FromStr,
{
    Scalar::deserialize(deserializer)
        .map_err(|_| invalid_count::<D::Error>())?
        .into_count()
}

pub(crate) fn optional_unsigned_count<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + FromStr,
{
    Option::<Scalar<T>>::deserialize(deserializer)
        .map_err(|_| invalid_count::<D::Error>())?
        .map(Scalar::into_count)
        .transpose()
}

fn invalid_ratio<E: serde::de::Error>() -> E {
    E::invalid_value(Unexpected::Other("a malformed number"), &"a number")
}

pub(crate) fn ratio<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Scalar::<f64>::deserialize(deserializer).map_err(|_| invalid_ratio::<D::Error>())? {
        Scalar::Typed(value) => Ok(value),
        Scalar::Text(value) => value.trim().parse().map_err(|_| invalid_ratio()),
    }
}

const LISTEN_ADDR_FORM: &str = "an IP address and port, or :port; hostnames are not resolved";

pub(crate) fn blank_as_none_with<'de, D, T>(
    deserializer: D,
    parse: impl FnOnce(&str) -> Result<T, D::Error>,
) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    optional_string(deserializer)?
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(parse)
        .transpose()
}

pub(crate) fn blank_as_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: From<String>,
{
    blank_as_none_with(deserializer, |trimmed| Ok(T::from(trimmed.to_owned())))
}

pub(crate) fn blank_secret_as_none<'de, D>(
    deserializer: D,
) -> Result<Option<SecretString>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = optional_string(deserializer)?;
    Ok(raw.and_then(|value| (!value.trim().is_empty()).then(|| SecretString::from(value))))
}

pub(crate) fn listen_addr<'de, D>(deserializer: D) -> Result<SocketAddr, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = string(deserializer)?;
    parse_listen_addr(&raw)
}

pub(crate) fn optional_listen_addr<'de, D>(deserializer: D) -> Result<Option<SocketAddr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    blank_as_none_with(deserializer, parse_listen_addr)
}

fn parse_listen_addr<E: serde::de::Error>(value: &str) -> Result<SocketAddr, E> {
    let trimmed = value.trim();
    match trimmed.strip_prefix(':') {
        Some(port) => format!("0.0.0.0:{port}").parse(),
        None => trimmed.parse(),
    }
    .map_err(|_| E::invalid_value(Unexpected::Str(value), &LISTEN_ADDR_FORM))
}

pub(crate) fn duration<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    decoded_duration(&string(deserializer)?, "a duration")
}

#[allow(
    dead_code,
    reason = "removing both OIDC profiles leaves no optional duration fields"
)]
pub(crate) fn optional_duration<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    optional_string(deserializer)?
        .map(|text| decoded_duration(&text, "a duration"))
        .transpose()
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
        #[derive(Debug, Deserialize)]
        struct Probe {
            #[serde(deserialize_with = "listen_addr")]
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

        #[derive(Debug, Deserialize)]
        struct Probe {
            #[serde(default, deserialize_with = "blank_as_none")]
            path: Option<String>,
            #[serde(default, deserialize_with = "optional_boolean")]
            flag: Option<bool>,
            #[serde(default, deserialize_with = "optional_string_enum")]
            kind: Option<Kind>,
            #[serde(default, deserialize_with = "optional_duration")]
            ttl: Option<Duration>,
            #[serde(default, deserialize_with = "secret_map")]
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
