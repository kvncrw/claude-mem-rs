//! env-sanitizer tests (port of `tests/supervisor/env-sanitizer.test.ts`,
//! 156 lines, 9 cases).

use claude_mem_supervisor::infrastructure::env_sanitizer::sanitize_env;
use std::collections::HashMap;

fn env(pairs: &[(&str, Option<&str>)]) -> HashMap<String, Option<String>> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.map(|s| s.to_string())))
        .collect()
}

#[test]
fn strips_variables_with_claudecode_prefix() {
    let result = sanitize_env(&env(&[
        ("CLAUDECODE_FOO", Some("bar")),
        ("CLAUDECODE_SOMETHING", Some("value")),
        ("PATH", Some("/usr/bin")),
    ]));
    assert!(!result.contains_key("CLAUDECODE_FOO"));
    assert!(!result.contains_key("CLAUDECODE_SOMETHING"));
    assert_eq!(result.get("PATH").map(String::as_str), Some("/usr/bin"));
}

#[test]
fn strips_claude_code_prefix_but_preserves_allowed() {
    let result = sanitize_env(&env(&[
        ("CLAUDE_CODE_BAR", Some("baz")),
        ("CLAUDE_CODE_OAUTH_TOKEN", Some("token")),
        ("HOME", Some("/home/user")),
    ]));
    assert!(!result.contains_key("CLAUDE_CODE_BAR"));
    assert_eq!(
        result.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str),
        Some("token")
    );
    assert_eq!(result.get("HOME").map(String::as_str), Some("/home/user"));
}

#[test]
fn strips_exact_match_variables() {
    let result = sanitize_env(&env(&[
        ("CLAUDECODE", Some("1")),
        ("CLAUDE_CODE_SESSION", Some("session-123")),
        ("CLAUDE_CODE_ENTRYPOINT", Some("hook")),
        ("MCP_SESSION_ID", Some("mcp-abc")),
        ("NODE_PATH", Some("/usr/local/lib")),
    ]));
    assert!(!result.contains_key("CLAUDECODE"));
    assert!(!result.contains_key("CLAUDE_CODE_SESSION"));
    assert!(!result.contains_key("CLAUDE_CODE_ENTRYPOINT"));
    assert!(!result.contains_key("MCP_SESSION_ID"));
    assert_eq!(
        result.get("NODE_PATH").map(String::as_str),
        Some("/usr/local/lib")
    );
}

#[test]
fn preserves_allowed_variables() {
    let result = sanitize_env(&env(&[
        ("PATH", Some("/usr/bin:/usr/local/bin")),
        ("HOME", Some("/home/user")),
        ("NODE_PATH", Some("/usr/local/lib/node_modules")),
        ("SHELL", Some("/bin/zsh")),
        ("USER", Some("developer")),
        ("LANG", Some("en_US.UTF-8")),
    ]));
    assert_eq!(
        result.get("PATH").map(String::as_str),
        Some("/usr/bin:/usr/local/bin")
    );
    assert_eq!(result.get("HOME").map(String::as_str), Some("/home/user"));
    assert_eq!(
        result.get("NODE_PATH").map(String::as_str),
        Some("/usr/local/lib/node_modules")
    );
    assert_eq!(result.get("SHELL").map(String::as_str), Some("/bin/zsh"));
    assert_eq!(result.get("USER").map(String::as_str), Some("developer"));
    assert_eq!(result.get("LANG").map(String::as_str), Some("en_US.UTF-8"));
}

#[test]
fn returns_a_new_object_without_mutating_original() {
    let mut original = HashMap::new();
    original.insert("PATH".into(), Some("/usr/bin".into()));
    original.insert("CLAUDECODE_FOO".into(), Some("bar".into()));
    original.insert("KEEP".into(), Some("yes".into()));
    let original_copy = original.clone();

    let result = sanitize_env(&original);

    // Original unchanged.
    assert_eq!(original, original_copy);
    // Stripped vars gone from result.
    assert!(!result.contains_key("CLAUDECODE_FOO"));
    assert_eq!(result.get("PATH").map(String::as_str), Some("/usr/bin"));
}

#[test]
fn handles_empty_env() {
    let result = sanitize_env(&HashMap::new());
    assert!(result.is_empty());
}

#[test]
fn skips_entries_with_undefined_values() {
    let result = sanitize_env(&env(&[
        ("DEFINED", Some("value")),
        ("UNDEFINED_KEY", None),
    ]));
    assert_eq!(result.get("DEFINED").map(String::as_str), Some("value"));
    assert!(!result.contains_key("UNDEFINED_KEY"));
}

#[test]
fn combines_prefix_and_exact_removal_in_single_pass() {
    let result = sanitize_env(&env(&[
        ("PATH", Some("/usr/bin")),
        ("CLAUDECODE", Some("1")),
        ("CLAUDECODE_FOO", Some("bar")),
        ("CLAUDE_CODE_BAR", Some("baz")),
        ("CLAUDE_CODE_OAUTH_TOKEN", Some("oauth-token")),
        ("CLAUDE_CODE_SESSION", Some("session")),
        ("CLAUDE_CODE_ENTRYPOINT", Some("entry")),
        ("MCP_SESSION_ID", Some("mcp")),
        ("KEEP_ME", Some("yes")),
    ]));
    assert_eq!(result.get("PATH").map(String::as_str), Some("/usr/bin"));
    assert_eq!(result.get("KEEP_ME").map(String::as_str), Some("yes"));
    assert!(!result.contains_key("CLAUDECODE"));
    assert!(!result.contains_key("CLAUDECODE_FOO"));
    assert!(!result.contains_key("CLAUDE_CODE_BAR"));
    assert_eq!(
        result.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str),
        Some("oauth-token")
    );
    assert!(!result.contains_key("CLAUDE_CODE_SESSION"));
    assert!(!result.contains_key("CLAUDE_CODE_ENTRYPOINT"));
    assert!(!result.contains_key("MCP_SESSION_ID"));
}

#[test]
fn preserves_claude_code_git_bash_path_through_sanitization() {
    let result = sanitize_env(&env(&[
        (
            "CLAUDE_CODE_GIT_BASH_PATH",
            Some("C:\\Program Files\\Git\\bin\\bash.exe"),
        ),
        ("PATH", Some("/usr/bin")),
        ("HOME", Some("/home/user")),
    ]));
    assert_eq!(
        result.get("CLAUDE_CODE_GIT_BASH_PATH").map(String::as_str),
        Some("C:\\Program Files\\Git\\bin\\bash.exe")
    );
    assert_eq!(result.get("PATH").map(String::as_str), Some("/usr/bin"));
    assert_eq!(result.get("HOME").map(String::as_str), Some("/home/user"));
}

#[test]
fn selectively_preserves_only_allowed_claude_code_vars() {
    let result = sanitize_env(&env(&[
        ("CLAUDE_CODE_OAUTH_TOKEN", Some("my-oauth-token")),
        ("CLAUDE_CODE_GIT_BASH_PATH", Some("/usr/bin/bash")),
        ("CLAUDE_CODE_RANDOM_OTHER", Some("should-be-stripped")),
        ("CLAUDE_CODE_INTERNAL_FLAG", Some("should-be-stripped")),
        ("PATH", Some("/usr/bin")),
    ]));
    assert_eq!(
        result.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str),
        Some("my-oauth-token")
    );
    assert_eq!(
        result.get("CLAUDE_CODE_GIT_BASH_PATH").map(String::as_str),
        Some("/usr/bin/bash")
    );
    assert!(!result.contains_key("CLAUDE_CODE_RANDOM_OTHER"));
    assert!(!result.contains_key("CLAUDE_CODE_INTERNAL_FLAG"));
    assert_eq!(result.get("PATH").map(String::as_str), Some("/usr/bin"));
}
