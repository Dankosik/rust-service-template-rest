//! Decode failures rendered without a value a variable supplied.

use crate::de::VALUE_FREE;

/// Sections whose values stay out of a decode failure whichever source set
/// them; their `Debug` output redacts the same trust inputs.
const VALUE_FREE_SECTIONS: &[&str] = &[
    // template:begin client-integrations:load-value-free-section
    "integrations",
    // template:end client-integrations:load-value-free-section
];

/// Render a decode failure without a value a variable supplied.
///
/// Variables are the only secret source, and config-rs and serde quote the
/// value they refuse, sometimes lowercased, trimmed, or parsed. So a failure
/// at a key a variable sets is rebuilt from its key and its expected form,
/// never from the decoder's text, and so is one inside a section of
/// [`VALUE_FREE_SECTIONS`]. Only a message that names keys alone (an unknown
/// or missing field) or carries [`VALUE_FREE`] is shown as written, as is a
/// failure at any other key only a file sets. `variable_keys` are the dotted
/// keys the variables of this load set, and `carrier` names where those
/// variables come from.
pub(super) fn describe_rejection(
    error: &config::ConfigError,
    variable_keys: &[String],
    carrier: &str,
) -> String {
    let (key, rejection) = classify(error);
    // `urls[0]` is an element of the value at `urls`.
    let value_key = key.map(|key| key.split_once('[').map_or(key, |(head, _)| head));
    // A failure that names no key may come from any source.
    let variable_may_set = value_key.is_none_or(|key| {
        variable_keys
            .iter()
            .any(|variable_key| is_at_or_under(variable_key, key))
    });
    let in_value_free_section = value_key.is_some_and(|key| {
        VALUE_FREE_SECTIONS
            .iter()
            .any(|section| is_at_or_under(key, section))
    });
    let hide_value = variable_may_set || in_value_free_section;
    let place = key.map_or_else(String::new, |key| format!(" for key `{key}`"));
    let origin = if variable_may_set {
        format!(" in {carrier}")
    } else {
        String::new()
    };
    match rejection {
        Rejection::Expected(expected) if hide_value => {
            format!("invalid value, expected {expected}{place}{origin}")
        }
        Rejection::Unexplained if hide_value => format!("invalid value{place}{origin}"),
        Rejection::AsWritten | Rejection::Expected(_) | Rejection::Unexplained => {
            error.to_string().replace(VALUE_FREE, "")
        }
    }
}

/// What a decode failure may say about the value it refused.
enum Rejection<'a> {
    /// The message holds no value.
    AsWritten,
    /// The message may quote the value; this is the form it expected.
    Expected(&'a str),
    /// The message may quote the value and names no expected form.
    Unexplained,
}

/// The key a decode failure names, and what its message may be trusted for.
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
            if message.starts_with(VALUE_FREE)
                || KEYS_ONLY.iter().any(|form| message.starts_with(form)) =>
        {
            (None, Rejection::AsWritten)
        }
        // serde ends these forms with its own `, expected <form>`; a value
        // quoted earlier in the message cannot follow the last one.
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

/// Whether the dotted `path` is `key` or lies beneath it.
fn is_at_or_under(path: &str, key: &str) -> bool {
    path.strip_prefix(key)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}
