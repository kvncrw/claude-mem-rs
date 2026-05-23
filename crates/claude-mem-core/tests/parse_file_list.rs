//! `parse_file_list` tests (port of
//! `tests/services/sqlite/parse-file-list.test.ts`).
//!
//! Validates safe JSON array parsing for `files_read`/`files_modified`
//! DB columns that may contain legacy bare path strings instead of JSON
//! arrays.

use claude_mem_core::db::observations::files::parse_file_list;

#[test]
fn returns_empty_for_none() {
    assert!(parse_file_list(None).is_empty());
}

#[test]
fn returns_empty_for_empty_string() {
    assert!(parse_file_list(Some("")).is_empty());
}

#[test]
fn parses_normal_json_array() {
    assert_eq!(
        parse_file_list(Some(r#"["/a/b.ts","/c/d.ts"]"#)),
        vec!["/a/b.ts", "/c/d.ts"]
    );
}

#[test]
fn wraps_bare_unix_path() {
    assert_eq!(
        parse_file_list(Some("/Users/foo/bar.go")),
        vec!["/Users/foo/bar.go"]
    );
}

#[test]
fn wraps_bare_windows_path() {
    assert_eq!(
        parse_file_list(Some(r"C:\Users\foo\bar.ts")),
        vec![r"C:\Users\foo\bar.ts"]
    );
}

#[test]
fn invalid_json_treated_as_single_element() {
    assert_eq!(
        parse_file_list(Some("not valid json {")),
        vec!["not valid json {"]
    );
}

#[test]
fn wraps_json_scalar_string() {
    assert_eq!(
        parse_file_list(Some(r#""single-file.ts""#)),
        vec!["single-file.ts"]
    );
}

#[test]
fn empty_json_array_is_empty() {
    assert!(parse_file_list(Some("[]")).is_empty());
}

#[test]
fn drops_non_string_elements_in_array() {
    assert_eq!(
        parse_file_list(Some(r#"["/real/path.ts",42,null]"#)),
        vec!["/real/path.ts"]
    );
}
