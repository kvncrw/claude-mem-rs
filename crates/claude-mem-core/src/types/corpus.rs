//! Corpus types — port of upstream-claude-mem
//! `src/services/worker/knowledge/types.ts` (LOC 1-56).
//!
//! These are the persisted shape of a knowledge corpus: a named, filtered
//! slice of observations stored at `~/.claude-mem/corpora/{name}.corpus.json`.
//! The on-disk JSON layout MUST stay byte-compatible with the TypeScript
//! implementation — same field names, same nesting, `version: 1`. Cross-impl
//! corpus files must be readable by either runtime.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Filter parameters used to build / rebuild a corpus.
///
/// Mirrors TS `CorpusFilter` — every field is optional. `types` is constrained
/// at the route layer (see `CorpusRoutes` in TS, our `routes/mod.rs`) to the
/// set `{decision, bugfix, feature, refactor, discovery, change}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CorpusFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concepts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

/// Per-corpus stats. `observation_count` and `type_breakdown` are derived from
/// the assembled observation set; `token_estimate` is updated after rendering
/// (mirrors TS two-pass build).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CorpusStats {
    pub observation_count: i64,
    pub token_estimate: i64,
    pub date_range: CorpusDateRange,
    pub type_breakdown: HashMap<String, i64>,
}

/// Inclusive ISO-8601 date range for the corpus' observations. Both fields
/// default to "now" when the corpus is empty (TS parity).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CorpusDateRange {
    pub earliest: String,
    pub latest: String,
}

/// One observation as embedded in the corpus JSON. This is a flattened,
/// renderer-friendly projection of `ObservationRow`; arrays are stored as
/// real JSON arrays (not stringified JSON).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CorpusObservation {
    pub id: i64,
    pub r#type: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub narrative: Option<String>,
    pub facts: Vec<String>,
    pub concepts: Vec<String>,
    pub files_read: Vec<String>,
    pub files_modified: Vec<String>,
    pub project: String,
    pub created_at: String,
    pub created_at_epoch: i64,
}

/// The persisted corpus file. The `version: 1` literal is asserted at
/// deserialization time via a custom serde wrapper — see `CorpusVersion`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CorpusFile {
    pub version: CorpusVersion,
    pub name: String,
    pub description: String,
    pub created_at: String,
    pub updated_at: String,
    pub filter: CorpusFilter,
    pub stats: CorpusStats,
    pub system_prompt: String,
    /// Resume token for a primed `claude` session. `None` when the corpus
    /// has never been primed or has been reprimed.
    pub session_id: Option<String>,
    pub observations: Vec<CorpusObservation>,
}

/// Phantom-typed version field that only accepts the integer `1`. Future
/// versions get a new variant; readers reject unknown versions instead of
/// silently coercing them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorpusVersion;

impl Default for CorpusVersion {
    fn default() -> Self {
        Self
    }
}

impl Serialize for CorpusVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(1)
    }
}

impl<'de> Deserialize<'de> for CorpusVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value: i64 = i64::deserialize(deserializer)?;
        if value != 1 {
            return Err(serde::de::Error::custom(format!(
                "unsupported corpus file version: {value}; expected 1"
            )));
        }
        Ok(Self)
    }
}

/// Metadata returned by `CorpusStore::list` — same shape as TS' anonymous
/// object literal so the HTTP/MCP surface stays identical.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CorpusListEntry {
    pub name: String,
    pub description: String,
    pub stats: CorpusStats,
    pub session_id: Option<String>,
}

/// Successful `query()` result from a primed knowledge agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CorpusQueryResult {
    pub answer: String,
    pub session_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip_matches_ts_layout() {
        // Locked: must match the TS `CorpusFile` JSON layout byte-for-byte
        // (modulo HashMap key order, which serde_json sorts by insertion).
        let mut breakdown = HashMap::new();
        breakdown.insert("decision".to_owned(), 2);
        breakdown.insert("bugfix".to_owned(), 1);
        let corpus = CorpusFile {
            version: CorpusVersion,
            name: "hooks".to_owned(),
            description: "All hook lifecycle work".to_owned(),
            created_at: "2026-05-24T00:00:00Z".to_owned(),
            updated_at: "2026-05-24T00:00:00Z".to_owned(),
            filter: CorpusFilter {
                project: Some("claude-mem".to_owned()),
                limit: Some(50),
                ..Default::default()
            },
            stats: CorpusStats {
                observation_count: 3,
                token_estimate: 1234,
                date_range: CorpusDateRange {
                    earliest: "2026-01-01T00:00:00Z".to_owned(),
                    latest: "2026-05-01T00:00:00Z".to_owned(),
                },
                type_breakdown: breakdown,
            },
            system_prompt: "You are a knowledge agent...".to_owned(),
            session_id: None,
            observations: vec![CorpusObservation {
                id: 7,
                r#type: "decision".to_owned(),
                title: "Use exit code 2 for blocking".to_owned(),
                subtitle: None,
                narrative: Some("Long form text...".to_owned()),
                facts: vec!["exit 0 = success".to_owned()],
                concepts: vec!["hooks".to_owned()],
                files_read: vec![],
                files_modified: vec!["src/hooks/session-start.ts".to_owned()],
                project: "claude-mem".to_owned(),
                created_at: "2026-04-01T00:00:00Z".to_owned(),
                created_at_epoch: 1_711_929_600_000,
            }],
        };
        let json = serde_json::to_value(&corpus).unwrap();
        // version must serialize as the integer 1, not a struct or string.
        assert_eq!(json.get("version"), Some(&json!(1)));
        // session_id must serialize as JSON null, not be omitted (TS keeps the
        // key present for prime/reprime nullability semantics).
        assert!(json.as_object().unwrap().contains_key("session_id"));
        // round-trip preserves equality.
        let back: CorpusFile = serde_json::from_value(json).unwrap();
        assert_eq!(back, corpus);
    }

    #[test]
    fn rejects_unknown_version() {
        let raw = json!({
            "version": 2,
            "name": "x",
            "description": "",
            "created_at": "",
            "updated_at": "",
            "filter": {},
            "stats": {
                "observation_count": 0,
                "token_estimate": 0,
                "date_range": {"earliest": "", "latest": ""},
                "type_breakdown": {}
            },
            "system_prompt": "",
            "session_id": null,
            "observations": []
        });
        let err = serde_json::from_value::<CorpusFile>(raw).unwrap_err();
        assert!(err.to_string().contains("unsupported corpus file version"));
    }
}
