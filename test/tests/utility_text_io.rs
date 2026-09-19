//! Text and file recipes. File work is synchronous and belongs off async worker threads.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::LazyLock;

use bstr::ByteSlice;
use heck::{ToKebabCase, ToSnakeCase};
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

#[test]
fn names_and_text_patterns_use_existing_utilities() {
    assert_eq!("XMLHttpRequest".to_snake_case(), "xml_http_request");
    assert_eq!("UserProfile".to_kebab_case(), "user-profile");
    static JOB_ID: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^job-([0-9]+)$").unwrap());
    let captures = JOB_ID.captures("job-123").unwrap();
    assert_eq!(&captures[1], "123");
    assert!(JOB_ID.captures("job-123-extra").is_none());
    assert_eq!(strsim::levenshtein("config", "confg"), 1);
}

#[test]
fn byte_strings_do_not_require_lossy_utf8_conversion() {
    let input = &b"ok\ninvalid:\xff\n"[..];
    assert!(std::str::from_utf8(input).is_err());
    assert_eq!(
        input.lines().collect::<Vec<_>>(),
        [b"ok".as_slice(), b"invalid:\xff".as_slice()]
    );
    assert_eq!(input.find_byte(0xff), Some(11));
}

#[test]
fn normalization_and_graphemes_have_explicit_meanings() {
    let decomposed = "e\u{301}";
    assert_ne!(decomposed, "é");
    assert_eq!(decomposed.nfc().collect::<String>(), "é");
    assert_eq!(decomposed.graphemes(true).count(), 1);
    assert_eq!("👨‍👩‍👧‍👦".graphemes(true).count(), 1);
    // Do not change an identifier's normalization or a public length contract
    // simply because these utilities are available.
}

#[test]
fn recursive_file_operations_keep_errors_and_relative_paths() {
    let dir = tempfile::tempdir().unwrap();
    fs_err::create_dir(dir.path().join("nested")).unwrap();
    fs_err::write(dir.path().join("top.txt"), "top").unwrap();
    fs_err::write(dir.path().join("nested/child.txt"), "child").unwrap();
    // Collect errors before filtering entries; filter_map(Result::ok) hides failures.
    let entries = walkdir::WalkDir::new(dir.path())
        .follow_links(false)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let files = entries
        .iter()
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.path().strip_prefix(dir.path()).unwrap().to_owned())
        .sorted()
        .collect_vec();
    assert_eq!(
        files,
        [
            std::path::PathBuf::from("nested/child.txt"),
            std::path::PathBuf::from("top.txt")
        ]
    );
    assert_eq!(
        fs_err::read_to_string(dir.path().join("top.txt")).unwrap(),
        "top"
    );
    let missing = dir.path().join("missing.txt");
    let error = fs_err::read_to_string(&missing).unwrap_err();
    assert!(error.to_string().contains("missing.txt"));
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn utf8_paths_are_an_explicit_contract_not_a_lossy_conversion() {
    let path = camino::Utf8Path::new("exports").join("report.csv");
    assert_eq!(path.file_name(), Some("report.csv"));
    assert_eq!(path.parent(), Some(camino::Utf8Path::new("exports")));
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_are_rejected_when_converted_to_utf8_paths() {
    use std::os::unix::ffi::OsStringExt;
    let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![0xff]));
    assert_eq!(
        camino::Utf8PathBuf::from_path_buf(path.clone()).unwrap_err(),
        path
    );
}

#[test]
fn spooled_files_roll_to_disk_without_a_custom_memory_disk_state_machine() {
    let mut file = tempfile::spooled_tempfile(8);
    file.write_all(b"hello").unwrap();
    assert!(!file.is_rolled());
    file.write_all(b" world!").unwrap();
    assert!(file.is_rolled());
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut output = String::new();
    file.read_to_string(&mut output).unwrap();
    assert_eq!(output, "hello world!");
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
struct ImportRow {
    id: u64,
    label: String,
}

#[test]
fn csv_preserves_commas_quotes_and_embedded_newlines() {
    let rows = [
        ImportRow {
            id: 1,
            label: "comma, and \"quote\"".into(),
        },
        ImportRow {
            id: 2,
            label: "two\nlines".into(),
        },
    ];
    let mut writer = csv::Writer::from_writer(Vec::new());
    for row in &rows {
        writer.serialize(row).unwrap();
    }
    let encoded = writer.into_inner().unwrap();
    let decoded = csv::Reader::from_reader(encoded.as_slice())
        .deserialize::<ImportRow>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(decoded, rows);
}
