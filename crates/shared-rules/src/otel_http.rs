const OTHER_METHOD: &str = "_OTHER";

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HttpMethod {
    Connect,
    Delete,
    Get,
    Head,
    Options,
    Patch,
    Post,
    Put,
    Query,
    Trace,
    Other,
}

impl HttpMethod {
    pub const COUNT: usize = Self::Other as usize + 1;

    #[must_use]
    pub fn parse(method: &str) -> Self {
        match method {
            "CONNECT" => Self::Connect,
            "DELETE" => Self::Delete,
            "GET" => Self::Get,
            "HEAD" => Self::Head,
            "OPTIONS" => Self::Options,
            "PATCH" => Self::Patch,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "QUERY" => Self::Query,
            "TRACE" => Self::Trace,
            _ => Self::Other,
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "CONNECT",
            Self::Delete => "DELETE",
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::Patch => "PATCH",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Query => "QUERY",
            Self::Trace => "TRACE",
            Self::Other => OTHER_METHOD,
        }
    }

    #[must_use]
    pub const fn span_name_prefix(self) -> &'static str {
        match self {
            Self::Other => "HTTP",
            _ => self.as_str(),
        }
    }
}

const MIN_STATUS_CODE: usize = 100;
const STATUS_CODE_COUNT: usize = 900;

static STATUS_CODE_DIGITS: [[u8; 3]; STATUS_CODE_COUNT] = {
    let mut digits = [[0; 3]; STATUS_CODE_COUNT];
    let mut index = 0;
    let decimal = b"0123456789";
    while index < digits.len() {
        let code = MIN_STATUS_CODE + index;
        digits[index] = [
            decimal[code / 100],
            decimal[code / 10 % 10],
            decimal[code % 10],
        ];
        index += 1;
    }
    digits
};

static STATUS_CODES: [&str; STATUS_CODE_COUNT] = {
    let mut codes = [""; STATUS_CODE_COUNT];
    let mut index = 0;
    while index < codes.len() {
        codes[index] = match str::from_utf8(&STATUS_CODE_DIGITS[index]) {
            Ok(code) => code,
            Err(_) => panic!("status digits are ASCII"),
        };
        index += 1;
    }
    codes
};

#[must_use]
pub fn response_status_code(code: u16) -> Option<&'static str> {
    STATUS_CODES
        .get(usize::from(code).checked_sub(MIN_STATUS_CODE)?)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::{HttpMethod, response_status_code};

    #[test]
    fn http_methods_round_trip_and_index_within_count() {
        let known = [
            (HttpMethod::Connect, "CONNECT"),
            (HttpMethod::Delete, "DELETE"),
            (HttpMethod::Get, "GET"),
            (HttpMethod::Head, "HEAD"),
            (HttpMethod::Options, "OPTIONS"),
            (HttpMethod::Patch, "PATCH"),
            (HttpMethod::Post, "POST"),
            (HttpMethod::Put, "PUT"),
            (HttpMethod::Query, "QUERY"),
            (HttpMethod::Trace, "TRACE"),
        ];
        assert_eq!(known.len() + 1, HttpMethod::COUNT);
        for (slot, (method, text)) in known.into_iter().enumerate() {
            assert_eq!(HttpMethod::parse(text), method);
            assert_eq!(method.as_str(), text);
            assert_eq!(method.span_name_prefix(), text);
            assert_eq!(method.index(), slot);
        }
        assert_eq!(HttpMethod::Other.index(), HttpMethod::COUNT - 1);
        assert_eq!(HttpMethod::Other.as_str(), "_OTHER");
        assert_eq!(HttpMethod::Other.span_name_prefix(), "HTTP");
        for unknown in ["", "get", "PURGE", "_OTHER"] {
            assert_eq!(HttpMethod::parse(unknown), HttpMethod::Other, "{unknown}");
        }
    }

    #[test]
    fn every_three_digit_status_is_its_decimal_text() {
        for code in 100..=999_u16 {
            assert_eq!(response_status_code(code), Some(code.to_string().as_str()));
        }
    }

    #[test]
    fn a_code_outside_three_digits_has_no_value() {
        for code in [0, 99, 1000, u16::MAX] {
            assert_eq!(response_status_code(code), None, "{code}");
        }
    }
}
