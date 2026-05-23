//! claude-mem-worker — axum HTTP API, search strategies (FTS5 + BM25;
//! embedding/Chroma is gated behind `feature = "chroma"` and off by default),
//! session/queue management.

pub mod agents;
pub mod http;
pub mod queue;
pub mod search;
