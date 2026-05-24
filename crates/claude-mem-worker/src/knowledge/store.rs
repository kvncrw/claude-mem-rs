//! Corpus file I/O — port of upstream-claude-mem
//! `src/services/worker/knowledge/CorpusStore.ts`.
//!
//! Lives at `~/.claude-mem/corpora/{name}.corpus.json`. Cross-impl files MUST
//! stay interchangeable with the TS implementation. Name validation matches
//! the TS regex exactly and includes a path-traversal guard.

use std::fs;
use std::path::{Path, PathBuf};

use claude_mem_core::types::{CorpusFile, CorpusListEntry};
use thiserror::Error;

/// Errors raised by [`CorpusStore`].
#[derive(Debug, Error)]
pub enum CorpusStoreError {
    /// Name contains characters outside `[a-zA-Z0-9._-]` or fails the
    /// path-traversal guard.
    #[error("invalid corpus name: only alphanumeric characters, dots, hyphens, and underscores are allowed")]
    InvalidName,
    /// Underlying filesystem error.
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// JSON parse/serialize failure.
    #[error("corpus json error at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// Default corpora directory: `${CLAUDE_MEM_HOME:-$HOME/.claude-mem}/corpora`.
pub fn corpora_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("CLAUDE_MEM_HOME") {
        return PathBuf::from(home).join("corpora");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude-mem")
        .join("corpora")
}

/// File-backed corpus storage. Cheap to construct (no fd held); each call
/// touches disk independently.
#[derive(Debug, Clone)]
pub struct CorpusStore {
    corpora_dir: PathBuf,
}

impl Default for CorpusStore {
    fn default() -> Self {
        Self::new(corpora_dir())
    }
}

impl CorpusStore {
    /// Construct a store rooted at `dir`. The directory is created lazily on
    /// the first write — read/list/delete tolerate a missing dir.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            corpora_dir: dir.into(),
        }
    }

    /// Return the canonical corpora directory for this store.
    pub fn dir(&self) -> &Path {
        &self.corpora_dir
    }

    /// Write a corpus file, creating the corpora directory if needed.
    pub fn write(&self, corpus: &CorpusFile) -> Result<(), CorpusStoreError> {
        self.ensure_dir()?;
        let path = self.path_for(&corpus.name)?;
        let body = serde_json::to_string_pretty(corpus).map_err(|source| {
            CorpusStoreError::Json {
                path: path.clone(),
                source,
            }
        })?;
        fs::write(&path, body).map_err(|source| CorpusStoreError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(())
    }

    /// Read a corpus file. Returns `Ok(None)` if the file does not exist.
    pub fn read(&self, name: &str) -> Result<Option<CorpusFile>, CorpusStoreError> {
        let path = self.path_for(name)?;
        let body = match fs::read_to_string(&path) {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(CorpusStoreError::Io { path, source }),
        };
        let corpus: CorpusFile = serde_json::from_str(&body)
            .map_err(|source| CorpusStoreError::Json { path, source })?;
        Ok(Some(corpus))
    }

    /// Enumerate every `*.corpus.json` file. Unparseable files are skipped
    /// with a warn-level trace and do not fail the call (TS parity).
    pub fn list(&self) -> Result<Vec<CorpusListEntry>, CorpusStoreError> {
        if !self.corpora_dir.exists() {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&self.corpora_dir).map_err(|source| CorpusStoreError::Io {
            path: self.corpora_dir.clone(),
            source,
        })?;
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.ends_with(".corpus.json") {
                continue;
            }
            match fs::read_to_string(&path) {
                Ok(body) => match serde_json::from_str::<CorpusFile>(&body) {
                    Ok(corpus) => out.push(CorpusListEntry {
                        name: corpus.name,
                        description: corpus.description,
                        stats: corpus.stats,
                        session_id: corpus.session_id,
                    }),
                    Err(error) => {
                        tracing::warn!(
                            path = %path.display(),
                            %error,
                            "failed to parse corpus file; skipping"
                        );
                    }
                },
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "failed to read corpus file; skipping"
                    );
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Delete a corpus file. Returns `Ok(false)` if it did not exist.
    pub fn delete(&self, name: &str) -> Result<bool, CorpusStoreError> {
        let path = self.path_for(name)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(CorpusStoreError::Io { path, source }),
        }
    }

    fn ensure_dir(&self) -> Result<(), CorpusStoreError> {
        fs::create_dir_all(&self.corpora_dir).map_err(|source| CorpusStoreError::Io {
            path: self.corpora_dir.clone(),
            source,
        })
    }

    /// Resolve the canonical path for `name`. Enforces the TS regex AND
    /// re-checks the resolved path is inside the corpora dir so a creatively
    /// composed name (`..something`, `.`) cannot escape.
    fn path_for(&self, name: &str) -> Result<PathBuf, CorpusStoreError> {
        let trimmed = name.trim();
        if trimmed.is_empty() || !is_valid_corpus_name(trimmed) {
            return Err(CorpusStoreError::InvalidName);
        }
        let candidate = self
            .corpora_dir
            .join(format!("{trimmed}.corpus.json"));
        // Defence in depth: the regex already forbids `/` and `\`, but reject
        // any name that doesn't resolve under the corpora dir anyway.
        if candidate.parent() != Some(self.corpora_dir.as_path()) {
            return Err(CorpusStoreError::InvalidName);
        }
        Ok(candidate)
    }
}

/// `^[a-zA-Z0-9._-]+$` — explicit per-byte check avoids pulling in `regex`
/// for one pattern.
fn is_valid_corpus_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_mem_core::types::{
        CorpusDateRange, CorpusFilter, CorpusObservation, CorpusStats, CorpusVersion,
    };
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn sample_corpus(name: &str) -> CorpusFile {
        CorpusFile {
            version: CorpusVersion,
            name: name.to_owned(),
            description: format!("test corpus {name}"),
            created_at: "2026-05-24T00:00:00Z".to_owned(),
            updated_at: "2026-05-24T00:00:00Z".to_owned(),
            filter: CorpusFilter {
                project: Some("p".into()),
                ..Default::default()
            },
            stats: CorpusStats {
                observation_count: 1,
                token_estimate: 100,
                date_range: CorpusDateRange {
                    earliest: "2026-05-24T00:00:00Z".into(),
                    latest: "2026-05-24T00:00:00Z".into(),
                },
                type_breakdown: HashMap::new(),
            },
            system_prompt: "p".into(),
            session_id: None,
            observations: vec![CorpusObservation {
                id: 1,
                r#type: "decision".into(),
                title: "t".into(),
                subtitle: None,
                narrative: None,
                facts: vec![],
                concepts: vec![],
                files_read: vec![],
                files_modified: vec![],
                project: "p".into(),
                created_at: "2026-05-24T00:00:00Z".into(),
                created_at_epoch: 1,
            }],
        }
    }

    #[test]
    fn write_read_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = CorpusStore::new(tmp.path());
        let corpus = sample_corpus("alpha");
        store.write(&corpus).unwrap();
        let loaded = store.read("alpha").unwrap().unwrap();
        assert_eq!(loaded, corpus);
    }

    #[test]
    fn read_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        let store = CorpusStore::new(tmp.path());
        assert!(store.read("nope").unwrap().is_none());
    }

    #[test]
    fn list_skips_non_corpus_files() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path()).unwrap();
        fs::write(tmp.path().join("README.md"), "hi").unwrap();
        let store = CorpusStore::new(tmp.path());
        store.write(&sample_corpus("alpha")).unwrap();
        store.write(&sample_corpus("beta")).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "alpha");
        assert_eq!(list[1].name, "beta");
    }

    #[test]
    fn list_skips_unparseable() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path()).unwrap();
        fs::write(tmp.path().join("broken.corpus.json"), "not json").unwrap();
        let store = CorpusStore::new(tmp.path());
        store.write(&sample_corpus("alpha")).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "alpha");
    }

    #[test]
    fn list_missing_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let store = CorpusStore::new(tmp.path().join("not-yet"));
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn delete_reports_existence() {
        let tmp = TempDir::new().unwrap();
        let store = CorpusStore::new(tmp.path());
        store.write(&sample_corpus("alpha")).unwrap();
        assert!(store.delete("alpha").unwrap());
        assert!(!store.delete("alpha").unwrap());
    }

    #[test]
    fn rejects_invalid_names() {
        // The TS regex `^[a-zA-Z0-9._-]+$` does the heavy lifting; anything
        // with a slash, backslash, or space is rejected before path resolution.
        // Names made only of dots (e.g. `.`, `..`) match the regex but resolve
        // to files literally named `.corpus.json` / `..corpus.json` INSIDE the
        // corpora dir, so they're not traversals. We leave those to the OS.
        let tmp = TempDir::new().unwrap();
        let store = CorpusStore::new(tmp.path());
        for bad in [
            "../escape",
            "foo/bar",
            "a\\b",
            "  ",
            "",
            "with space",
            "weird$",
            "a%b",
            "name\0null",
        ] {
            let err = store.read(bad).unwrap_err();
            assert!(
                matches!(err, CorpusStoreError::InvalidName),
                "expected InvalidName for {bad:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn accepts_dotted_and_underscored_names() {
        let tmp = TempDir::new().unwrap();
        let store = CorpusStore::new(tmp.path());
        for good in ["alpha", "alpha.v2", "alpha_beta", "alpha-1", "1.2.3", "A_B"] {
            store.write(&sample_corpus(good)).unwrap();
            assert!(store.read(good).unwrap().is_some());
        }
    }
}
