use super::config::{
    expand_home_path, TranscriptSchema, TranscriptWatchConfig, WatchSchema, WatchTarget,
};
use super::processor::{ProcessEntryStats, TranscriptEventProcessor};
use super::state::{load_state, save_state, TranscriptWatchState};
use crate::hooks::WorkerClient;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchRunStats {
    pub files_seen: usize,
    pub lines_processed: usize,
    pub matched_events: usize,
    pub session_inits: usize,
    pub observations: usize,
    pub summaries: usize,
    pub completions: usize,
}

pub struct TranscriptWatcher {
    config: TranscriptWatchConfig,
    state_path: PathBuf,
    state: TranscriptWatchState,
    processor: TranscriptEventProcessor,
    known_files: HashSet<PathBuf>,
}

impl TranscriptWatcher {
    pub fn new(config: TranscriptWatchConfig, state_path: PathBuf, worker: WorkerClient) -> Self {
        let state = load_state(&state_path);
        Self {
            config,
            state_path,
            state,
            processor: TranscriptEventProcessor::new(worker),
            known_files: HashSet::new(),
        }
    }

    pub async fn process_once(&mut self) -> Result<WatchRunStats> {
        let mut stats = WatchRunStats::default();
        for watch in self.config.watches.clone() {
            let schema = self.resolve_schema(&watch)?;
            let files = self.resolve_watch_files(&watch)?;
            stats.files_seen += files.len();
            for file in files {
                self.process_file(&watch, &schema, &file, &mut stats)
                    .await?;
            }
        }
        save_state(&self.state_path, &self.state)?;
        Ok(stats)
    }

    pub async fn watch_forever(&mut self) -> Result<()> {
        loop {
            let _ = self.process_once().await?;
            tokio::time::sleep(Duration::from_millis(self.min_interval_ms())).await;
        }
    }

    fn min_interval_ms(&self) -> u64 {
        self.config
            .watches
            .iter()
            .filter_map(|watch| watch.rescan_interval_ms)
            .min()
            .unwrap_or(5000)
            .max(250)
    }

    fn resolve_schema(&self, watch: &WatchTarget) -> Result<TranscriptSchema> {
        match &watch.schema {
            WatchSchema::Inline(schema) => Ok(schema.clone()),
            WatchSchema::Named(name) => self
                .config
                .schemas
                .get(name)
                .cloned()
                .ok_or_else(|| anyhow!("watch {} references missing schema {name}", watch.name)),
        }
    }

    fn resolve_watch_files(&self, watch: &WatchTarget) -> Result<Vec<PathBuf>> {
        let path = expand_home_path(&watch.path);
        // The `glob` crate's pattern syntax is POSIX-style: backslashes are
        // treated as escape characters even on Windows, so a literal Windows
        // path like `C:\Users\me\.codex\sessions\**\*.jsonl` would never
        // match. Normalise the pattern to forward-slashes for glob expansion
        // (the `glob` crate accepts forward-slashes on Windows and yields
        // canonical `PathBuf`s back).
        let path_string = path_to_glob_pattern(&path);
        if has_glob(&path_string) {
            let mut files = Vec::new();
            for entry in glob::glob(&path_string)? {
                let entry = entry?;
                if entry.is_file() {
                    files.push(entry);
                }
            }
            files.sort();
            return Ok(files);
        }
        if path.is_file() {
            return Ok(vec![path]);
        }
        if path.is_dir() {
            let mut files = Vec::new();
            collect_jsonl_files(&path, &mut files)?;
            files.sort();
            return Ok(files);
        }
        Ok(Vec::new())
    }

    async fn process_file(
        &mut self,
        watch: &WatchTarget,
        schema: &TranscriptSchema,
        file: &Path,
        stats: &mut WatchRunStats,
    ) -> Result<()> {
        let file_key = state_key_for_path(file);
        let metadata = fs::metadata(file)?;
        let len = metadata.len();
        let is_initial_discovery = self.known_files.insert(file.to_path_buf());
        let mut offset = *self.state.offsets.get(&file_key).unwrap_or(&0);
        if offset == 0 && watch.start_at_end.unwrap_or(false) && is_initial_discovery {
            self.state.offsets.insert(file_key, len);
            return Ok(());
        }
        if offset > len {
            offset = 0;
        }
        if offset == len {
            return Ok(());
        }

        let mut file_handle = fs::File::open(file)?;
        file_handle.seek(SeekFrom::Start(offset))?;
        let mut data = String::new();
        file_handle.read_to_string(&mut data)?;
        let consumed = len;
        self.state.offsets.insert(file_key, consumed);

        let session_id_override = extract_session_id_from_path(file);
        for line in data.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: Value = match serde_json::from_str(trimmed) {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let event_stats = self
                .processor
                .process_entry(entry, watch, schema, session_id_override.as_deref())
                .await?;
            merge_event_stats(stats, event_stats);
            stats.lines_processed += 1;
        }
        Ok(())
    }
}

fn merge_event_stats(stats: &mut WatchRunStats, event_stats: ProcessEntryStats) {
    stats.matched_events += event_stats.matched_events;
    stats.session_inits += event_stats.session_inits;
    stats.observations += event_stats.observations;
    stats.summaries += event_stats.summaries;
    stats.completions += event_stats.completions;
}

fn has_glob(path: &str) -> bool {
    path.bytes()
        .any(|byte| matches!(byte, b'*' | b'?' | b'[' | b']' | b'{' | b'}'))
}

fn collect_jsonl_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    Ok(())
}

/// Render `path` as a `glob` crate pattern. On Windows the pattern is
/// normalised to forward-slashes (the `C:` drive prefix is left intact)
/// because backslashes are escape characters in `glob` syntax. On Unix the
/// path is rendered as-is.
fn path_to_glob_pattern(path: &Path) -> String {
    #[cfg(windows)]
    {
        path.display().to_string().replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        path.display().to_string()
    }
}

/// Render `path` as a state-file key. Normalising to forward-slashes on
/// Windows means a transcript file's offset survives across runs even if
/// callers thread `\` and `/` separators into the same path differently.
fn state_key_for_path(path: &Path) -> String {
    #[cfg(windows)]
    {
        path.display().to_string().replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        path.display().to_string()
    }
}

fn extract_session_id_from_path(path: &Path) -> Option<String> {
    let text = path.display().to_string();
    for part in text.split(|ch: char| !(ch.is_ascii_hexdigit() || ch == '-')) {
        if part.len() == 36
            && part.chars().enumerate().all(|(idx, ch)| {
                matches!(idx, 8 | 13 | 18 | 23) && ch == '-'
                    || !matches!(idx, 8 | 13 | 18 | 23) && ch.is_ascii_hexdigit()
            })
        {
            return Some(part.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_glob_detects_meta_chars() {
        assert!(has_glob("/a/**/*.jsonl"));
        assert!(has_glob("foo?bar"));
        assert!(!has_glob("/a/b/c.jsonl"));
    }

    #[cfg(windows)]
    #[test]
    fn path_to_glob_pattern_normalises_backslashes() {
        let p = std::path::PathBuf::from(r"C:\Users\me\.codex\sessions\**\*.jsonl");
        let pattern = super::path_to_glob_pattern(&p);
        assert_eq!(pattern, "C:/Users/me/.codex/sessions/**/*.jsonl");
        // glob crate must accept this pattern shape without panicking.
        let _ = glob::Pattern::new(&pattern).expect("normalised pattern compiles");
    }

    #[cfg(windows)]
    #[test]
    fn state_key_normalises_mixed_separators_on_windows() {
        let mixed = std::path::PathBuf::from(r"C:\Users\me/.codex\sessions/abc.jsonl");
        let key = super::state_key_for_path(&mixed);
        assert_eq!(key, "C:/Users/me/.codex/sessions/abc.jsonl");
    }

    #[cfg(not(windows))]
    #[test]
    fn path_to_glob_pattern_is_identity_on_unix() {
        let p = std::path::PathBuf::from("/home/me/.codex/sessions/**/*.jsonl");
        assert_eq!(
            super::path_to_glob_pattern(&p),
            "/home/me/.codex/sessions/**/*.jsonl"
        );
    }
}
