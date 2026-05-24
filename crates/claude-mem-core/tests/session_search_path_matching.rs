//! `isDirectChild` / `normalizePath` tests (port of
//! `tests/services/sqlite/session-search-path-matching.test.ts`, fix for
//! issue #794 — "folder CLAUDE.md shows no recent activity").
//!
//! Validates the shared path-utils module used by SessionSearch (runtime
//! folder CLAUDE.md generation) and regenerate-claude-md (CLI regen).

use claude_mem_core::shared::paths::{is_direct_child, normalize_path};

// --- same path format -----------------------------------------------------

#[test]
fn true_for_direct_child_relative_paths() {
    assert!(is_direct_child("app/api/router.py", "app/api"));
}

#[test]
fn true_for_direct_child_absolute_paths() {
    assert!(is_direct_child(
        "/Users/dev/project/app/api/router.py",
        "/Users/dev/project/app/api"
    ));
}

#[test]
fn false_for_file_in_subdirectory_relative() {
    assert!(!is_direct_child("app/api/v1/router.py", "app/api"));
}

#[test]
fn false_for_file_in_subdirectory_absolute() {
    assert!(!is_direct_child(
        "/Users/dev/project/app/api/v1/router.py",
        "/Users/dev/project/app/api"
    ));
}

#[test]
fn false_for_unrelated_paths() {
    assert!(!is_direct_child("lib/utils/helper.py", "app/api"));
}

// --- mixed path formats (absolute folder, relative file) - fixes #794 -----

#[test]
fn true_when_absolute_folder_ends_with_relative_file_directory() {
    // Exact bug case from #794.
    assert!(is_direct_child(
        "app/api/router.py",
        "/Users/dev/project/app/api"
    ));
}

#[test]
fn true_for_deeply_nested_folder_match() {
    assert!(is_direct_child(
        "src/components/Button.tsx",
        "/home/user/project/src/components"
    ));
}

#[test]
fn false_for_subdirectory_of_matched_folder() {
    assert!(!is_direct_child(
        "app/api/v1/router.py",
        "/Users/dev/project/app/api"
    ));
}

#[test]
fn false_when_file_path_does_not_match_folder_suffix() {
    assert!(!is_direct_child(
        "lib/api/router.py",
        "/Users/dev/project/app/api"
    ));
}

// --- path normalization ---------------------------------------------------

#[test]
fn handles_windows_backslash_paths() {
    assert!(is_direct_child("app\\api\\router.py", "app\\api"));
}

#[test]
fn handles_mixed_slashes() {
    assert!(is_direct_child("app/api\\router.py", "app\\api"));
}

#[test]
fn handles_trailing_slashes_on_folder_path() {
    assert!(is_direct_child("app/api/router.py", "app/api/"));
}

#[test]
fn handles_double_slashes() {
    assert!(is_direct_child("app//api/router.py", "app/api"));
}

#[test]
fn collapses_multiple_consecutive_slashes() {
    assert!(is_direct_child("app///api///router.py", "app//api//"));
}

// --- edge cases -----------------------------------------------------------

#[test]
fn false_for_single_segment_file_path() {
    assert!(!is_direct_child("router.py", "/Users/dev/project/app/api"));
}

#[test]
fn false_for_empty_paths() {
    assert!(!is_direct_child("", "app/api"));
    assert!(!is_direct_child("app/api/router.py", ""));
}

#[test]
fn handles_root_level_folders() {
    assert!(is_direct_child("src/file.ts", "/project/src"));
}

#[test]
fn prevents_false_positive_on_partial_segment_match() {
    // "api" folder should not match "api-v2".
    assert!(!is_direct_child(
        "app/api-v2/router.py",
        "/Users/dev/project/app/api"
    ));
}

#[test]
fn handles_similar_folder_names_correctly() {
    // "components" should not match "components-old".
    assert!(!is_direct_child(
        "src/components-old/Button.tsx",
        "/project/src/components"
    ));
}

// --- normalizePath --------------------------------------------------------

#[test]
fn normalize_converts_backslashes() {
    assert_eq!(normalize_path("app\\api\\router.py"), "app/api/router.py");
}

#[test]
fn normalize_collapse_consecutive_slashes() {
    assert_eq!(normalize_path("app//api///router.py"), "app/api/router.py");
}

#[test]
fn normalize_removes_trailing_slashes() {
    assert_eq!(normalize_path("app/api/"), "app/api");
    assert_eq!(normalize_path("app/api///"), "app/api");
}

#[test]
fn normalize_handles_windows_unc_paths() {
    assert_eq!(
        normalize_path(r"\\server\share\file.txt"),
        "/server/share/file.txt"
    );
}

#[test]
fn normalize_preserves_leading_slash_for_absolute() {
    assert_eq!(normalize_path("/Users/dev/project"), "/Users/dev/project");
}
