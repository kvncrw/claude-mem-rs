# Roadmap: Neo4j And Qdrant

This roadmap tracks storage/search work beyond the current SQLite/FTS5 runtime.

## Neo4j Graph Memory

Goal: add optional graph-backed memory for entities, relationships, decisions, files, and sessions.

- Define a graph model for projects, sessions, prompts, observations, files, concepts, decisions, and summaries.
- Add a Rust `GraphStore` trait with a no-op implementation and a Neo4j implementation behind a feature flag.
- Build idempotent migration jobs from SQLite rows to Neo4j nodes/relationships.
- Preserve SQLite as the source of truth during initial rollout.
- Add graph lookup APIs for related files, related decisions, concept neighborhoods, and session lineage.
- Add e2e tests that seed SQLite, run migration, and verify Neo4j relationship queries.

## Qdrant Vector Search

Status: optional self-hosted Qdrant support exists behind the worker `qdrant` feature.

Implemented:

- Observation point IDs are stable SQLite observation IDs.
- Qdrant is feature-gated and runtime-gated by `CLAUDE_MEM_QDRANT_*` env vars.
- Collection bootstrap is automatic.
- New observations, prompts, and generated/stored summaries are indexed opportunistically after writes.
- `/api/vector/qdrant/reindex` backfills recent/project-scoped observations, prompts, and summaries.
- `/api/search?strategy=qdrant` searches Qdrant, resolves typed memory refs back through SQLite, and falls back to SQLite.
- Payloads include schema/version metadata and typed refs for observation, prompt, and summary points.
- Unit/integration/e2e coverage uses a fake Qdrant server, plus optional `QDRANT_URL` smoke coverage.

Remaining:

- Add hybrid ranking that merges Qdrant scores with SQLite FTS5/BM25 results.
- Add migration tooling for Chroma-to-Qdrant if an old Chroma directory is present.
- Add containerized CI coverage for real Qdrant.

## Runtime Parity Follow-Ups

Implemented in Rust:

- Browser viewer root and initial SSE stream endpoint.
- Claude, Cursor, Gemini CLI, Codex, and raw hook adapters.
- Import/export, doctor, stats/projects, processing-status, settings, logs, and guarded branch routes.
- Session summary generation on explicit summarize calls and completion fallback.
- Pending-message queue processing through native Rust observer runners.
- Claude CLI, Gemini REST, OpenRouter REST, fake, and deterministic local observer providers.
- Gemini CLI and Codex CLI observer providers.
- Tier model selection for queued simple-tool and summary work.
- Persistent SSE broadcaster for live observation, summary, session, queue, and manual-memory events.
- Claude Stop/summarize transcript extraction with system-reminder stripping and session completion.
- Rich browser UI for feed/search/timeline/context/admin/queue/logs/settings workflows.
- Cross-platform installer/uninstaller CLI for Claude Code, Cursor, Gemini CLI, Codex transcript setup, opencode MCP/plugin setup, and persistent worker/transcript watcher services.
- Generic background transcript watcher daemon with schema config, offset state, tool pairing, summaries, and AGENTS context updates.
- Real-provider smoke tests for Claude CLI, Gemini CLI, and Codex CLI behind `CLAUDE_MEM_LIVE_PROVIDER_SMOKE=1`.
- OpenRouter live smoke is available behind `CLAUDE_MEM_LIVE_OPENROUTER_SMOKE=1`; it requires a valid OpenRouter key.
- Folder `CLAUDE.md` generation and cleanup with managed-block preservation.
- MCP smart file search, outline, and unfold helper tools.
- Vulcan MCP SDK detection is exposed from `/api/mcp/status`; the runtime remains `rmcp`-backed until Vulcan can be adopted without a local path dependency.

Remaining:

- Expand provider fallback telemetry and retry reporting in admin routes.
- Promote Vulcan MCP from detected/evaluated to an optional build/runtime backend once the SDK can be consumed as a public crate or stable git dependency.

## Migration Principles

- Keep SQLite authoritative until graph/vector migrations are repeatable and observable.
- Make migrations resumable and idempotent.
- Store migration checkpoints under `CLAUDE_MEM_HOME`.
- Keep feature flags off by default until CI covers the integration path.
- Preserve the current worker HTTP and hook contracts while adding new capabilities.
