//! Secrets mounted as files.

/// The value a secret file holds: its text without the trailing line breaks
/// (`\n`, and `\r` from a Windows line end) that editors, `echo`, and
/// heredocs add. Shell command substitution, `$(< file)`, likewise drops
/// every trailing newline. Every other byte, spaces included, is kept.
#[must_use]
pub fn value(mut text: String) -> String {
    let length = text.trim_end_matches(['\r', '\n']).len();
    text.truncate(length);
    text
}

#[cfg(test)]
mod tests {
    use super::value;

    #[test]
    fn trailing_line_breaks_are_not_part_of_the_value() {
        for text in [
            "s3cret",
            "s3cret\n",
            "s3cret\r\n",
            "s3cret\n\n",
            "s3cret\r",
            "s3cret\r\n\r\n",
        ] {
            assert_eq!(value(text.to_owned()), "s3cret", "{text:?}");
        }
    }

    #[test]
    fn every_other_byte_is_kept() {
        assert_eq!(value(" p w \r\n".to_owned()), " p w ");
        assert_eq!(value("line\nbreak\n".to_owned()), "line\nbreak");
        assert_eq!(value("tab\t\n".to_owned()), "tab\t");
        assert_eq!(value("\n\r\n".to_owned()), "");
    }
}
