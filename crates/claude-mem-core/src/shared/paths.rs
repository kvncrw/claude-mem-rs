use std::path::{Path, PathBuf};

/// `~/.claude-mem/` root directory.
pub fn claude_mem_home() -> PathBuf {
    if let Ok(p) = std::env::var("CLAUDE_MEM_HOME") {
        return PathBuf::from(p);
    }
    let home = dirs_home();
    home.join(".claude-mem")
}

pub fn claude_mem_db_path() -> PathBuf {
    claude_mem_home().join("claude-mem.db")
}

pub fn claude_mem_logs_dir() -> PathBuf {
    claude_mem_home().join("logs")
}

pub fn claude_mem_settings_path() -> PathBuf {
    claude_mem_home().join("settings.json")
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

#[allow(dead_code)]
fn is_absolute_or_explicit(p: impl AsRef<Path>) -> bool {
    p.as_ref().is_absolute()
}
