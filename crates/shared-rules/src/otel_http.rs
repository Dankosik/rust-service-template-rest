pub const OTHER_METHOD: &str = "_OTHER";

#[must_use]
pub fn request_method(method: &str) -> &'static str {
    match method {
        "CONNECT" => "CONNECT",
        "DELETE" => "DELETE",
        "GET" => "GET",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        "PATCH" => "PATCH",
        "POST" => "POST",
        "PUT" => "PUT",
        "QUERY" => "QUERY",
        "TRACE" => "TRACE",
        _ => OTHER_METHOD,
    }
}

static STATUS_CODE_DIGITS: [[u8; 3]; 900] = {
    let mut digits = [[0; 3]; 900];
    let mut index = 0;
    let decimal = b"0123456789";
    while index < digits.len() {
        let code = 100 + index;
        digits[index] = [
            decimal[code / 100],
            decimal[code / 10 % 10],
            decimal[code % 10],
        ];
        index += 1;
    }
    digits
};

static STATUS_CODES: [&str; 900] = {
    let mut codes = [""; 900];
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
        .get(usize::from(code.checked_sub(100)?))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::response_status_code;

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
