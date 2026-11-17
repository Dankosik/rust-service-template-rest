pub(super) fn describe_rejection(
    error: &config::ConfigError,
    variable_sources: &[(String, &'static str)],
) -> String {
    let (key, rejection) = classify(error);
    let value_key = key.map(|key| key.split_once('[').map_or(key, |(head, _)| head));
    let mut sources = variable_sources
        .iter()
        .filter(|(path, _)| value_key.is_none_or(|key| is_at_or_under(path, key)))
        .map(|(_, source)| *source);
    let first = sources.next();
    let hide_value = value_key.is_none() || first.is_some();
    let place = key.map_or_else(String::new, |key| format!(" for key `{key}`"));
    let origin = match first {
        Some(source) if sources.all(|other| other == source) => format!(" in {source}"),
        Some(_) => " in the environment or the secrets directory".to_owned(),
        None => String::new(),
    };
    match rejection {
        Rejection::Expected(expected) if hide_value => {
            format!("invalid value, expected {expected}{place}{origin}")
        }
        Rejection::Unexplained if hide_value => format!("invalid value{place}{origin}"),
        Rejection::AsWritten | Rejection::Expected(_) | Rejection::Unexplained => error.to_string(),
    }
}

enum Rejection<'a> {
    AsWritten,
    Expected(&'a str),
    Unexplained,
}

fn classify(error: &config::ConfigError) -> (Option<&str>, Rejection<'_>) {
    const KEYS_ONLY: [&str; 3] = ["unknown field ", "missing field ", "duplicate field "];
    const QUOTES_VALUE: [&str; 3] = ["invalid type: ", "invalid value: ", "unknown variant "];
    match error {
        config::ConfigError::At { error, key, .. } => {
            let (inner_key, rejection) = classify(error);
            (key.as_deref().or(inner_key), rejection)
        }
        config::ConfigError::Type { expected, key, .. } => {
            (key.as_deref(), Rejection::Expected(expected))
        }
        config::ConfigError::NotFound(_) => (None, Rejection::AsWritten),
        config::ConfigError::Message(message)
            if KEYS_ONLY.iter().any(|form| message.starts_with(form)) =>
        {
            (None, Rejection::AsWritten)
        }
        config::ConfigError::Message(message)
            if QUOTES_VALUE.iter().any(|form| message.starts_with(form)) =>
        {
            let expected = message.rsplit_once(", expected ");
            (
                None,
                expected.map_or(Rejection::Unexplained, |(_, expected)| {
                    Rejection::Expected(expected)
                }),
            )
        }
        _ => (None, Rejection::Unexplained),
    }
}

fn is_at_or_under(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use serde::de::{Error, Unexpected};

    use super::*;

    const VALUE: &str = "hunter2";

    fn describe(error: &config::ConfigError) -> String {
        describe_rejection(error, &[("http.addr".to_owned(), "the environment")])
    }

    #[test]
    fn serde_value_forms_keep_the_expected_form_and_drop_the_value() {
        for error in [
            <config::ConfigError as Error>::invalid_type(Unexpected::Str(VALUE), &"a boolean"),
            <config::ConfigError as Error>::invalid_value(Unexpected::Str(VALUE), &"a boolean"),
        ] {
            let message = error.to_string();
            assert!(message.contains(VALUE), "serde quotes the value: {message}");
            assert_eq!(
                describe(&error),
                "invalid value, expected a boolean in the environment"
            );
        }
        let variant = <config::ConfigError as Error>::unknown_variant(VALUE, &["json", "text"]);
        assert_eq!(
            describe(&variant),
            "invalid value, expected `json` or `text` in the environment"
        );
    }

    #[test]
    fn serde_key_forms_are_shown_as_written() {
        for error in [
            <config::ConfigError as Error>::unknown_field("levl", &["level"]),
            <config::ConfigError as Error>::missing_field("level"),
            <config::ConfigError as Error>::duplicate_field("level"),
        ] {
            assert_eq!(describe(&error), error.to_string());
        }
    }

    #[test]
    fn an_unrecognised_form_hides_its_text() {
        let error = <config::ConfigError as Error>::invalid_length(3, &VALUE);
        assert_eq!(describe(&error), "invalid value in the environment");
    }
}
