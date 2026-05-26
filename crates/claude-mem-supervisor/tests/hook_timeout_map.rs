//! Regression coverage for AC #1 (issue #5):
//!
//! Locks the Rust installer's generated hook manifest to the TypeScript v12
//! `hooks.json` semantics. The TS file is the source of truth — we fixture
//! its relevant shape in this test and assert the Rust installer-generated
//! manifest matches structurally for every documented hook event the Rust
//! runtime owns.
//!
//! Notable scope decisions:
//!
//! - The TS `Setup` event is **owned by the TS plugin's `smart-install.js`**
//!   shim and has no Rust equivalent (the Rust runtime is installed by
//!   `claude-mem-supervisor::installer`, not a Setup hook). We assert the
//!   Rust manifest deliberately omits Setup rather than matching it.
//! - The TS `PreToolUse(Read)` `file-context` hook is **not yet ported** in
//!   the Rust runtime (no `file-context` event in `hooks::execute_hook`).
//!   We treat its absence as a known parity gap and pin it as a TODO — see
//!   the `rust_manifest_does_not_yet_include_file_context_hook` assertion
//!   below so a future port flips that test from "pinned absent" to
//!   "asserted present".
//! - TS uses `SessionStart` for both context injection AND the bootstrap
//!   `worker-service.cjs start` command. The Rust runtime starts the worker
//!   via supervisor lifecycle, not a hook — so we only compare the
//!   user-facing context hook.

use claude_mem_supervisor::installer::{run_install, InstallOptions};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

// Async-safe env lock matching the convention used by other supervisor
// tests (PR #12 cleaned up `clippy::await_holding_lock`-tripping
// `std::Mutex` guards — keep using `tokio::sync::Mutex<()>::const_new`).
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

/// Snapshot of the TS v12 `hooks.json` timeout map.
///
/// Source: `~/.claude/plugins/marketplaces/thedotmack/plugin/hooks/hooks.json`
/// at marketplace pin `claude-mem@thedotmack` v12.
///
/// We embed the timeout values inline rather than reading the TS file from
/// disk so the test is hermetic on CI (where the TS marketplace isn't
/// installed) and the diff in this file becomes the audit trail whenever
/// the TS plugin bumps a timeout.
///
/// Entries are `(event_name, matcher, hook_index, timeout_seconds)`. A
/// `None` matcher means "no matcher field" (TS shape for events that don't
/// filter on tool name).
const TS_V12_TIMEOUT_MAP: &[(&str, Option<&str>, usize, u64)] = &[
    // Setup: smart-install bootstrap — owned by TS plugin, no Rust port.
    ("Setup", Some("*"), 0, 300),
    // SessionStart: smart-install bootstrap (TS-only) + worker start
    // (Rust supervisor owns this) + context hook (Rust ports as
    // `context`).
    ("SessionStart", Some("startup|clear|compact"), 0, 300),
    ("SessionStart", Some("startup|clear|compact"), 1, 60),
    ("SessionStart", Some("startup|clear|compact"), 2, 60),
    ("UserPromptSubmit", None, 0, 60),
    ("PostToolUse", Some("*"), 0, 120),
    ("PreToolUse", Some("Read"), 0, 2000),
    ("Stop", None, 0, 120),
    ("SessionEnd", None, 0, 30),
];

/// Maps the Rust runtime's hook event names (as written by the installer)
/// to the TS event + matcher + index that own the user-facing timeout.
///
/// The Rust installer writes a single Rust hook per Claude event, so this
/// is the canonical Rust→TS pairing. The Setup and bootstrap entries are
/// intentionally not in this map (see file header).
struct EventParity {
    rust_event: &'static str,
    rust_matcher: Option<&'static str>,
    rust_timeout: u64,
    ts_event: &'static str,
    ts_matcher: Option<&'static str>,
    ts_hook_index: usize,
}

const PARITY_MATRIX: &[EventParity] = &[
    EventParity {
        rust_event: "SessionStart",
        rust_matcher: Some("startup|clear|compact"),
        rust_timeout: 60,
        ts_event: "SessionStart",
        ts_matcher: Some("startup|clear|compact"),
        // The user-facing claude-mem context hook is index 2 in the TS
        // SessionStart array (0 = smart-install bootstrap,
        // 1 = worker-service start, 2 = context).
        ts_hook_index: 2,
    },
    EventParity {
        rust_event: "UserPromptSubmit",
        rust_matcher: None,
        rust_timeout: 60,
        ts_event: "UserPromptSubmit",
        ts_matcher: None,
        ts_hook_index: 0,
    },
    EventParity {
        rust_event: "PostToolUse",
        rust_matcher: Some("*"),
        rust_timeout: 120,
        ts_event: "PostToolUse",
        ts_matcher: Some("*"),
        ts_hook_index: 0,
    },
    EventParity {
        rust_event: "Stop",
        rust_matcher: None,
        rust_timeout: 120,
        ts_event: "Stop",
        ts_matcher: None,
        ts_hook_index: 0,
    },
    EventParity {
        rust_event: "SessionEnd",
        rust_matcher: None,
        rust_timeout: 30,
        ts_event: "SessionEnd",
        ts_matcher: None,
        ts_hook_index: 0,
    },
];

/// Mirror of `installer_transcripts::isolate_env` — overrides HOME on every
/// platform AND USERPROFILE + clears HOMEDRIVE/HOMEPATH on Windows. Codex
/// flagged two P1s on PR #11 for missing the Windows arms; preserve them
/// here so this test passes on the Windows matrix slot.
struct IsolatedEnv {
    _temp: tempfile::TempDir,
    prior_home: Option<std::ffi::OsString>,
    prior_claude_config: Option<std::ffi::OsString>,
    prior_systemd_user_dir: Option<std::ffi::OsString>,
    prior_launch_agents_dir: Option<std::ffi::OsString>,
    prior_windows_tasks_dir: Option<std::ffi::OsString>,
    #[cfg(windows)]
    prior_userprofile: Option<std::ffi::OsString>,
    #[cfg(windows)]
    prior_homedrive: Option<std::ffi::OsString>,
    #[cfg(windows)]
    prior_homepath: Option<std::ffi::OsString>,
}

impl IsolatedEnv {
    fn new() -> (Self, PathBuf) {
        let temp = tempfile::TempDir::new().unwrap();
        let home_path = temp.path().to_path_buf();
        let claude_dir = home_path.join(".claude");

        let prior_home = std::env::var_os("HOME");
        let prior_claude_config = std::env::var_os("CLAUDE_CONFIG_DIR");
        let prior_systemd_user_dir = std::env::var_os("CLAUDE_MEM_SYSTEMD_USER_DIR");
        let prior_launch_agents_dir = std::env::var_os("CLAUDE_MEM_LAUNCH_AGENTS_DIR");
        let prior_windows_tasks_dir = std::env::var_os("CLAUDE_MEM_WINDOWS_TASKS_DIR");
        #[cfg(windows)]
        let prior_userprofile = std::env::var_os("USERPROFILE");
        #[cfg(windows)]
        let prior_homedrive = std::env::var_os("HOMEDRIVE");
        #[cfg(windows)]
        let prior_homepath = std::env::var_os("HOMEPATH");

        std::env::set_var("HOME", &home_path);
        std::env::set_var("CLAUDE_CONFIG_DIR", &claude_dir);
        std::env::set_var(
            "CLAUDE_MEM_SYSTEMD_USER_DIR",
            home_path.join(".config/systemd/user"),
        );
        std::env::set_var(
            "CLAUDE_MEM_LAUNCH_AGENTS_DIR",
            home_path.join("Library/LaunchAgents"),
        );
        std::env::set_var(
            "CLAUDE_MEM_WINDOWS_TASKS_DIR",
            home_path.join("AppData/Roaming/claude-mem"),
        );
        #[cfg(windows)]
        {
            std::env::set_var("USERPROFILE", &home_path);
            std::env::remove_var("HOMEDRIVE");
            std::env::remove_var("HOMEPATH");
        }

        let guard = Self {
            _temp: temp,
            prior_home,
            prior_claude_config,
            prior_systemd_user_dir,
            prior_launch_agents_dir,
            prior_windows_tasks_dir,
            #[cfg(windows)]
            prior_userprofile,
            #[cfg(windows)]
            prior_homedrive,
            #[cfg(windows)]
            prior_homepath,
        };
        (guard, home_path)
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        restore("HOME", self.prior_home.take());
        restore("CLAUDE_CONFIG_DIR", self.prior_claude_config.take());
        restore(
            "CLAUDE_MEM_SYSTEMD_USER_DIR",
            self.prior_systemd_user_dir.take(),
        );
        restore(
            "CLAUDE_MEM_LAUNCH_AGENTS_DIR",
            self.prior_launch_agents_dir.take(),
        );
        restore(
            "CLAUDE_MEM_WINDOWS_TASKS_DIR",
            self.prior_windows_tasks_dir.take(),
        );
        #[cfg(windows)]
        {
            restore("USERPROFILE", self.prior_userprofile.take());
            restore("HOMEDRIVE", self.prior_homedrive.take());
            restore("HOMEPATH", self.prior_homepath.take());
        }
    }
}

fn restore(key: &str, value: Option<std::ffi::OsString>) {
    match value {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

fn read_generated_hook_manifest(home: &Path) -> Value {
    let manifest_path = home
        .join(".claude")
        .join("plugins")
        .join("marketplaces")
        .join("kvncrw")
        .join("plugin")
        .join("hooks")
        .join("hooks.json");
    let text = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|_| panic!("expected hook manifest at {}", manifest_path.display()));
    serde_json::from_str(&text).expect("hook manifest must be valid JSON")
}

fn rust_hook_timeout(manifest: &Value, event: &str, matcher: Option<&str>) -> u64 {
    let entries = manifest["hooks"][event]
        .as_array()
        .unwrap_or_else(|| panic!("missing Rust hook entries for event {event}"));
    let entry = entries
        .iter()
        .find(|entry| match matcher {
            Some(expected) => entry["matcher"].as_str() == Some(expected),
            None => entry["matcher"].is_null() || entry["matcher"].as_str().is_none(),
        })
        .unwrap_or_else(|| {
            panic!("no Rust hook entry for {event} with matcher {matcher:?}, got {entries:?}")
        });
    let hooks = entry["hooks"]
        .as_array()
        .expect("hook entry must have a `hooks` array");
    hooks[0]["timeout"]
        .as_u64()
        .expect("Rust hook timeout must be a u64")
}

fn rust_hook_command(manifest: &Value, event: &str, matcher: Option<&str>) -> String {
    let entries = manifest["hooks"][event].as_array().unwrap();
    let entry = entries
        .iter()
        .find(|entry| match matcher {
            Some(expected) => entry["matcher"].as_str() == Some(expected),
            None => entry["matcher"].is_null() || entry["matcher"].as_str().is_none(),
        })
        .unwrap();
    entry["hooks"][0]["command"].as_str().unwrap().to_owned()
}

fn ts_hook_timeout(event: &str, matcher: Option<&str>, hook_index: usize) -> u64 {
    TS_V12_TIMEOUT_MAP
        .iter()
        .find(|(e, m, i, _)| *e == event && *m == matcher && *i == hook_index)
        .map(|(_, _, _, timeout)| *timeout)
        .unwrap_or_else(|| panic!("no TS fixture entry for {event}/{matcher:?}/index {hook_index}"))
}

fn install_and_read_manifest() -> (IsolatedEnv, Value) {
    let (guard, _home) = IsolatedEnv::new();
    let bin = PathBuf::from("/usr/local/bin/claude-mem-rs-test");
    let report = run_install(InstallOptions {
        ide: Some("claude-code".into()),
        yes: true,
        dry_run: false,
        bin_path: Some(bin),
    })
    .expect("installer must succeed against isolated HOME");
    assert!(
        report.failed.is_empty(),
        "installer failures: {:?}",
        report.failed
    );
    let manifest = read_generated_hook_manifest(guard._temp.path());
    (guard, manifest)
}

#[tokio::test]
async fn rust_hook_manifest_timeouts_match_ts_v12_per_event() {
    let _guard = ENV_LOCK.lock().await;
    let (_env, manifest) = install_and_read_manifest();

    for parity in PARITY_MATRIX {
        let rust_timeout = rust_hook_timeout(&manifest, parity.rust_event, parity.rust_matcher);
        let ts_timeout = ts_hook_timeout(parity.ts_event, parity.ts_matcher, parity.ts_hook_index);
        assert_eq!(
            rust_timeout, parity.rust_timeout,
            "Rust timeout drift for {}: manifest has {}, parity matrix expected {}",
            parity.rust_event, rust_timeout, parity.rust_timeout
        );
        assert_eq!(
            rust_timeout, ts_timeout,
            "TS↔Rust timeout drift on {}/{:?}: Rust {} != TS {}",
            parity.rust_event, parity.rust_matcher, rust_timeout, ts_timeout
        );
    }
}

#[tokio::test]
async fn rust_hook_manifest_emits_one_entry_per_rust_event() {
    let _guard = ENV_LOCK.lock().await;
    let (_env, manifest) = install_and_read_manifest();
    let hooks = manifest["hooks"]
        .as_object()
        .expect("hooks must be a JSON object");

    // The Rust installer should emit exactly the five Rust-owned events.
    // Anything else means a regression (drift between installer and
    // PARITY_MATRIX above).
    let mut rust_event_names: Vec<&str> = hooks.keys().map(String::as_str).collect();
    rust_event_names.sort();
    assert_eq!(
        rust_event_names,
        vec![
            "PostToolUse",
            "SessionEnd",
            "SessionStart",
            "Stop",
            "UserPromptSubmit",
        ],
        "Rust manifest emitted unexpected event set"
    );
}

#[tokio::test]
async fn rust_manifest_commands_reference_hook_claude_code_subcommands() {
    let _guard = ENV_LOCK.lock().await;
    let (_env, manifest) = install_and_read_manifest();
    let pairs: &[(&str, Option<&str>, &str)] = &[
        ("SessionStart", Some("startup|clear|compact"), "context"),
        ("UserPromptSubmit", None, "session-init"),
        ("PostToolUse", Some("*"), "observation"),
        ("Stop", None, "summarize"),
        ("SessionEnd", None, "session-complete"),
    ];
    for (event, matcher, expected_subcommand) in pairs {
        let command = rust_hook_command(&manifest, event, *matcher);
        assert!(
            command.contains(" hook claude-code "),
            "{event} command missing platform tag: {command}"
        );
        let suffix = format!(" hook claude-code {expected_subcommand}");
        assert!(
            command.ends_with(&suffix),
            "{event} command does not end with `{suffix}`, got: {command}"
        );
    }
}

#[tokio::test]
async fn rust_manifest_does_not_yet_include_file_context_hook() {
    // TS PreToolUse(Read) -> `file-context` is **not yet ported** in the
    // Rust runtime — `claude_mem_supervisor::hooks::execute_hook` has no
    // matching event arm and would return `unknown hook event:
    // file-context` if Claude Code dispatched it.
    //
    // This is a pinned parity gap. When the Rust runtime grows a
    // `file-context` handler this test will need to flip from "asserted
    // absent" to "asserted present" alongside a new PARITY_MATRIX entry.
    //
    // TODO(parity): port PreToolUse(Read) -> file-context to Rust.
    let _guard = ENV_LOCK.lock().await;
    let (_env, manifest) = install_and_read_manifest();
    assert!(
        manifest["hooks"]["PreToolUse"].is_null(),
        "Rust manifest unexpectedly has PreToolUse; if file-context was ported, \
         update PARITY_MATRIX and remove this guard"
    );
}

#[tokio::test]
async fn rust_manifest_omits_ts_only_setup_bootstrap() {
    // TS `Setup` runs `smart-install.js` (300s); the Rust installer owns
    // its own install lifecycle and must NOT emit a Setup hook because
    // claude-mem-rs is installed via `claude-mem install`, not via a
    // Claude Code Setup hook handing off to a node bootstrap script.
    let _guard = ENV_LOCK.lock().await;
    let (_env, manifest) = install_and_read_manifest();
    assert!(
        manifest["hooks"]["Setup"].is_null(),
        "Rust manifest unexpectedly contains a Setup hook (TS-only event)"
    );
}
