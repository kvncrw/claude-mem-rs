//! Platform-aware path resolution shared across the runtime.
//!
//! Resolution order (`claude_mem_home`):
//! 1. `CLAUDE_MEM_HOME`
//! 2. `<home>/.claude-mem`, where `<home>` is `USERPROFILE`,
//!    `HOMEDRIVE` + `HOMEPATH`, or `HOME` on Windows and `HOME` on Unix.
//! 3. `./.claude-mem` as a last-resort relative fallback.
//!
//! Each public helper has a `_with_env` variant that takes an env lookup
//! closure so tests can exercise both Unix and Windows resolution paths
//! without mutating process-global state.

use std::ffi::OsString;
use std::path::PathBuf;

/// Closure-compatible env lookup. `std::env::var_os` matches this shape.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// Returns the user's home directory using the platform-conventional env vars.
///
/// Falls back to `.` if nothing is set, matching the existing behaviour of
/// the helpers that this module replaces.
pub fn home_dir() -> PathBuf {
    home_dir_with_env(&|name| std::env::var_os(name))
}

pub fn home_dir_with_env(env: EnvLookup<'_>) -> PathBuf {
    // CLAUDE_MEM_HOME is honoured one level up; this helper is strictly the
    // OS-level home directory.
    if cfg!(windows) {
        if let Some(value) = env("USERPROFILE").filter(|v| !v.is_empty()) {
            return PathBuf::from(value);
        }
        if let (Some(drive), Some(path)) = (env("HOMEDRIVE"), env("HOMEPATH")) {
            if !drive.is_empty() && !path.is_empty() {
                let mut joined = OsString::from(drive);
                joined.push(path);
                return PathBuf::from(joined);
            }
        }
        if let Some(value) = env("HOME").filter(|v| !v.is_empty()) {
            return PathBuf::from(value);
        }
    } else if let Some(value) = env("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(value);
    }
    PathBuf::from(".")
}

/// Returns the canonical claude-mem state directory (typically
/// `~/.claude-mem`).
pub fn claude_mem_home() -> PathBuf {
    claude_mem_home_with_env(&|name| std::env::var_os(name))
}

pub fn claude_mem_home_with_env(env: EnvLookup<'_>) -> PathBuf {
    if let Some(value) = env("CLAUDE_MEM_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(value);
    }
    home_dir_with_env(env).join(".claude-mem")
}

/// Returns the data directory (SQLite, vector index, transcript state).
/// `CLAUDE_MEM_DATA_DIR` overrides; otherwise mirrors `claude_mem_home`.
pub fn claude_data_dir() -> PathBuf {
    claude_data_dir_with_env(&|name| std::env::var_os(name))
}

pub fn claude_data_dir_with_env(env: EnvLookup<'_>) -> PathBuf {
    if let Some(value) = env("CLAUDE_MEM_DATA_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(value);
    }
    claude_mem_home_with_env(env)
}

/// Default location of the worker PID file.
pub fn worker_pid_path() -> PathBuf {
    claude_mem_home().join("worker.pid")
}

/// Default location of the SQLite database file.
pub fn default_db_path() -> PathBuf {
    claude_data_dir().join("claude-mem.db")
}

/// Returns the Claude Code config directory (`~/.claude` or
/// `$CLAUDE_CONFIG_DIR`).
pub fn claude_config_dir() -> PathBuf {
    claude_config_dir_with_env(&|name| std::env::var_os(name))
}

pub fn claude_config_dir_with_env(env: EnvLookup<'_>) -> PathBuf {
    if let Some(value) = env("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(value);
    }
    home_dir_with_env(env).join(".claude")
}

/// Default transcript watcher config path.
pub fn transcript_config_path() -> PathBuf {
    claude_mem_home().join("transcript-watch.json")
}

/// Default transcript watcher state path.
pub fn transcript_state_path() -> PathBuf {
    claude_mem_home().join("transcript-watch-state.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| map.get(key).map(OsString::from)
    }

    fn empty_env() -> impl Fn(&str) -> Option<OsString> {
        |_| None
    }

    #[test]
    fn claude_mem_home_env_wins() {
        let env = env_map(&[
            ("CLAUDE_MEM_HOME", "/custom/mem"),
            ("HOME", "/home/me"),
            ("USERPROFILE", "C:\\Users\\Me"),
        ]);
        assert_eq!(claude_mem_home_with_env(&env), PathBuf::from("/custom/mem"));
    }

    #[test]
    fn data_dir_env_overrides_mem_home() {
        let env = env_map(&[
            ("CLAUDE_MEM_HOME", "/custom/mem"),
            ("CLAUDE_MEM_DATA_DIR", "/var/lib/claude-mem"),
        ]);
        assert_eq!(
            claude_data_dir_with_env(&env),
            PathBuf::from("/var/lib/claude-mem")
        );
    }

    #[test]
    fn data_dir_falls_back_to_mem_home() {
        let env = env_map(&[("CLAUDE_MEM_HOME", "/custom/mem")]);
        assert_eq!(claude_data_dir_with_env(&env), PathBuf::from("/custom/mem"));
    }

    #[test]
    fn empty_claude_mem_home_is_ignored() {
        // An empty env var should not produce a `""` PathBuf; we should fall
        // through to the platform home derivation.
        let env = env_map(&[("CLAUDE_MEM_HOME", ""), ("HOME", "/home/me")]);
        let resolved = claude_mem_home_with_env(&env);
        if cfg!(unix) {
            assert_eq!(resolved, PathBuf::from("/home/me/.claude-mem"));
        }
    }

    #[test]
    fn last_resort_fallback() {
        assert_eq!(
            claude_mem_home_with_env(&empty_env()),
            PathBuf::from(".").join(".claude-mem")
        );
    }

    #[test]
    fn config_dir_env_wins() {
        let env = env_map(&[("CLAUDE_CONFIG_DIR", "/etc/claude"), ("HOME", "/home/me")]);
        assert_eq!(
            claude_config_dir_with_env(&env),
            PathBuf::from("/etc/claude")
        );
    }

    // Platform-specific home derivation. These run via cfg gates so a Linux
    // CI run still exercises the Unix branch and would catch a HOME regression
    // without needing a Windows runner.
    #[cfg(unix)]
    #[test]
    fn unix_home_uses_home_var() {
        let env = env_map(&[("HOME", "/home/alice"), ("USERPROFILE", "C:\\Users\\Alice")]);
        assert_eq!(home_dir_with_env(&env), PathBuf::from("/home/alice"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_home_prefers_userprofile() {
        let env = env_map(&[("HOME", "/home/alice"), ("USERPROFILE", "C:\\Users\\Alice")]);
        assert_eq!(home_dir_with_env(&env), PathBuf::from("C:\\Users\\Alice"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_home_composes_homedrive_homepath() {
        let env = env_map(&[("HOMEDRIVE", "D:"), ("HOMEPATH", "\\Users\\Bob")]);
        assert_eq!(home_dir_with_env(&env), PathBuf::from("D:\\Users\\Bob"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_home_falls_back_to_home_for_posix_shells() {
        let env = env_map(&[("HOME", "C:\\msys64\\home\\alice")]);
        assert_eq!(
            home_dir_with_env(&env),
            PathBuf::from("C:\\msys64\\home\\alice")
        );
    }
}
