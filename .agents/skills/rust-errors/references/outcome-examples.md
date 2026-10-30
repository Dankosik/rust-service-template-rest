# Optional input and invalid input

Premise: a caller may omit a numeric setting, but a present value must parse as
`u16`. `Result<Option<_>, _>` preserves both facts: omission is `Ok(None)`, while
an empty, malformed or out-of-range value is `Err`. The cost is handling absence
and failure separately. Collapsing a failed parse with `.ok()` would silently
activate an omission policy for invalid input.

If the operation requires the setting, omission becomes `Missing`; the same
parser still preserves malformed input as `Invalid`. This changed requirement
reverses the optional policy without changing syntax acceptance. Conversely,
choose `Result<u16, _>` directly when no consumer needs the optional operation.
If a contract explicitly permits malformed input to use a default, that is a
different policy to name at its owning boundary rather than infer from `Option`.

Both policies below share the parser. The error source retains the parse cause;
its display adds context without including the submitted value. The assertions
exercise missing, empty, malformed, out-of-range and valid present input, so a
conversion of invalid input into absence or default would be observable.

```rust
use std::error::Error;
use std::fmt;
use std::num::ParseIntError;

fn optional_value(raw: Option<&str>) -> Result<Option<u16>, ParseIntError> {
    raw.map(str::parse::<u16>).transpose()
}

#[derive(Debug)]
enum RequiredValueError {
    Missing,
    Invalid(ParseIntError),
}

impl fmt::Display for RequiredValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("required value is missing"),
            Self::Invalid(_) => formatter.write_str("required value is invalid"),
        }
    }
}

impl Error for RequiredValueError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Missing => None,
            Self::Invalid(cause) => Some(cause),
        }
    }
}

fn required_value(raw: Option<&str>) -> Result<u16, RequiredValueError> {
    optional_value(raw)
        .map_err(RequiredValueError::Invalid)?
        .ok_or(RequiredValueError::Missing)
}

assert_eq!(optional_value(None).unwrap(), None);
assert_eq!(optional_value(Some("0")).unwrap(), Some(0));
assert_eq!(optional_value(Some("65535")).unwrap(), Some(65535));
assert!(matches!(required_value(None), Err(RequiredValueError::Missing)));
assert_eq!(required_value(Some("0")).unwrap(), 0);
assert_eq!(required_value(Some("65535")).unwrap(), 65535);
for invalid in ["", "abc", "65536"] {
    assert!(optional_value(Some(invalid)).is_err());
    let error = required_value(Some(invalid)).unwrap_err();
    assert!(matches!(&error, RequiredValueError::Invalid(_)));
    assert!(error.source().is_some());
}
```
