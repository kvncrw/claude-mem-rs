# claude-mem-rs

Native Rust port of [claude-mem](https://github.com/thedotmack/claude-mem) v12.

## Architecture

Multi-crate workspace:

- **`claude-mem-core`** — types, SQLite schema (rusqlite + FTS5), context compiler,
  shared utils. No HTTP.
- **`claude-mem-worker`** — axum HTTP API (mcp tool surface), search strategies
  (FTS5 + BM25, optional Qdrant), session/queue managers. Depends on core.
- **`claude-mem-supervisor`** — process lifecycle, health monitor, graceful
  shutdown, hook pipeline. Depends on worker.
- **`claude-mem-sdk`** — LLM-facing parser (`ParsedObservation`, `ParsedSummary`)
  and prompt builders. Zero service I/O deps.
- **`claude-mem-mcp`** — stdout MCP server via `rmcp`, thin HTTP wrapper over
  worker. Depends on core + worker.

## Data compatibility

The Rust port reads the existing TypeScript database at
`~/.claude-mem/claude-mem.db` in-place. No schema migration on cutover. The
schema mirror is in `crates/claude-mem-core/src/db/migrations.rs`.

## Build & test

```bash
cargo build --workspace
cargo test --workspace
cargo test -p claude-mem-worker --features qdrant
cargo test -p claude-mem-worker --features chroma  # ignored tests
cargo run -p claude-mem-worker                      # HTTP worker
cargo run -p claude-mem-mcp                         # stdio MCP server
cargo run -p claude-mem-supervisor --bin hook -- claude-code session-init
```

The public runtime/process docs live in `README.md`. The graph/vector follow-up
plan lives in `ROADMAP.md`.

## Development notes

- SQLite remains authoritative. Qdrant is optional behind `feature = "qdrant"`
  and `CLAUDE_MEM_QDRANT_*` env vars; it must fall back cleanly to SQLite.
- The old Chroma vector layer is still only represented by compatibility names
  and cleanup tests. Do not revive it; Qdrant is the replacement path.
- The hook pipeline: **stdin → adapter → handler → worker HTTP → HookResult →
  stdout JSON + exit code**. Adapters cover Claude, Cursor, Gemini CLI, Codex,
  and raw payloads.
- Browser/admin parity routes live in the worker: `/`, `/stream`,
  `/api/admin/doctor`, `/api/export`, `/api/import`, `/api/settings`,
  `/api/logs`, `/api/branch/status`, and guarded branch mutation routes.
- Session completion auto-generates one fallback searchable summary when a
  session has no explicit summary yet.
- Dual session IDs: `content_session_id` (user-visible, immutable) and
  `memory_session_id` (NULL at create, populated async by the worker).
  `ObservationRow` and `SdkSessionRow` both carry both; FK with `ON UPDATE CASCADE`.
- BM25 ranking: `ORDER BY <table>_fts.rank ASC` (smaller = more relevant).
- `PendingMessageStore` is a persistent claim-confirm queue, NOT a compression
  queue. Retries up to 3× via `retry_count`, then permanent `failed`.
