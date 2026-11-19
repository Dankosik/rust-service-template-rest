const MAX_KEY_BYTES: usize = 1024;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, derive_more::Debug)]
#[debug("ObjectKey {{ len: {} }}", _0.len())]
pub struct ObjectKey(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("object key does not match the portable key grammar")]
pub struct InvalidObjectKey;

impl ObjectKey {
    pub fn try_new(key: impl Into<String>) -> Result<Self, InvalidObjectKey> {
        let key = key.into();
        if key.len() > MAX_KEY_BYTES || key == "soap" {
            return Err(InvalidObjectKey);
        }
        let valid = key.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._~-".contains(&byte))
        });
        if valid {
            Ok(Self(key))
        } else {
            Err(InvalidObjectKey)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ObjectKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::str::FromStr for ObjectKey {
    type Err = InvalidObjectKey;

    fn from_str(key: &str) -> Result<Self, Self::Err> {
        Self::try_new(key)
    }
}

impl TryFrom<String> for ObjectKey {
    type Error = InvalidObjectKey;

    fn try_from(key: String) -> Result<Self, Self::Error> {
        Self::try_new(key)
    }
}

impl TryFrom<&str> for ObjectKey {
    type Error = InvalidObjectKey;

    fn try_from(key: &str) -> Result<Self, Self::Error> {
        Self::try_new(key)
    }
}

#[cfg(test)]
mod tests {
    use super::ObjectKey;

    #[test]
    fn keys_follow_the_portable_grammar() {
        for key in [
            "a",
            "pricing-evidence/v1/2026/09/29/abc_def.json",
            "parse-results/op-1.json",
            "a.b~c-d_e",
            "dir/soap",
            &"k".repeat(1024),
        ] {
            ObjectKey::try_new(key).unwrap_or_else(|_| panic!("{key:?} should be admitted"));
        }
        for key in [
            "",
            "/leading",
            "trailing/",
            "double//slash",
            "./dot",
            "a/../b",
            "space here",
            "unicode-é",
            "query?x",
            "percent%20",
            "soap",
            &"k".repeat(1025),
        ] {
            assert!(
                ObjectKey::try_new(key).is_err(),
                "{key:?} should be refused"
            );
        }
    }

    #[test]
    fn key_debug_hides_the_key() {
        let key: ObjectKey = "customer-42/invoice.pdf".parse().unwrap();
        assert_eq!(ObjectKey::try_from("customer-42/invoice.pdf").unwrap(), key);
        assert_eq!(
            ObjectKey::try_from("customer-42/invoice.pdf".to_owned()).unwrap(),
            key
        );
        assert_eq!(key.as_ref(), "customer-42/invoice.pdf");
        assert!(ObjectKey::try_new("a").unwrap() < key);
        let rendered = format!("{key:?}");
        assert!(!rendered.contains("customer"), "{rendered}");
    }
}
