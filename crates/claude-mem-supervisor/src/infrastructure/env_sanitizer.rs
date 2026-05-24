//! Env var sanitizer (port of
//! `src/supervisor/env-sanitizer.ts`).
//!
//! Strips variables that would leak Claude Code's internal state into
//! spawned worker processes, while preserving the specific
//! `CLAUDE_CODE_*` vars the worker needs (e.g. OAuth token, git bash
//! path).
//!
//! Returns a new `HashMap` without mutating the input. Entries with
//! `None` values are dropped (matches Node's `undefined` semantics).

use std::collections::HashMap;

/// Prefixes whose vars are stripped unless explicitly allowed below.
const STRIP_PREFIXES: &[&str] = &["CLAUDECODE_", "CLAUDE_CODE_"];

/// Exact-match vars that are always stripped.
const STRIP_EXACT: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "MCP_SESSION_ID",
];

/// `CLAUDE_CODE_*` vars allowed to pass through even though their prefix
/// would otherwise strip them.
const ALLOWED_CLAUDE_CODE_VARS: &[&str] = &["CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CODE_GIT_BASH_PATH"];

/// Run the sanitize algorithm over `env`, returning a fresh `HashMap`
/// without the stripped keys and with `None` (undefined) values dropped.
pub fn sanitize_env(env: &HashMap<String, Option<String>>) -> HashMap<String, String> {
    let mut out = HashMap::with_capacity(env.len());
    for (k, v) in env {
        let some_value = match v {
            Some(v) => v,
            None => continue, // undefined values are dropped
        };
        if STRIP_EXACT.contains(&k.as_str()) {
            continue;
        }
        let prefixed = STRIP_PREFIXES.iter().any(|p| k.starts_with(p));
        if prefixed && !ALLOWED_CLAUDE_CODE_VARS.contains(&k.as_str()) {
            continue;
        }
        out.insert(k.clone(), some_value.clone());
    }
    out
}
