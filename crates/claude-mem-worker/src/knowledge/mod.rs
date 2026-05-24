//! Knowledge corpus subsystem — port of upstream-claude-mem
//! `src/services/worker/knowledge/`.
//!
//! Always-on:
//! - [`store`]: file I/O for `~/.claude-mem/corpora/{name}.corpus.json`
//! - [`builder`]: assembles a `CorpusFile` from filtered observations via
//!   `SearchOrchestrator` + `get_observations_by_ids`
//! - [`renderer`]: deterministic markdown render + system prompt generator
//!
//! Behind cargo feature `knowledge-agent`:
//! - [`agent`]: shell-out to the `claude` CLI to prime/query/reprime a
//!   knowledge agent session

pub mod builder;
pub mod renderer;
pub mod store;

#[cfg(feature = "knowledge-agent")]
pub mod agent;

pub use builder::CorpusBuilder;
pub use renderer::CorpusRenderer;
pub use store::{corpora_dir, CorpusStore, CorpusStoreError};

#[cfg(feature = "knowledge-agent")]
pub use agent::{KnowledgeAgent, KnowledgeAgentError};

/// The 12 tools that are disallowed inside a knowledge-agent session.
/// Mirrors `KNOWLEDGE_AGENT_DISALLOWED_TOOLS` in TS
/// `src/services/worker/knowledge/KnowledgeAgent.ts:30-43`.
pub const KNOWLEDGE_AGENT_DISALLOWED_TOOLS: &[&str] = &[
    "Bash",
    "Read",
    "Write",
    "Edit",
    "Grep",
    "Glob",
    "WebFetch",
    "WebSearch",
    "Task",
    "NotebookEdit",
    "AskUserQuestion",
    "TodoWrite",
];
