# claude-mem-rs

Native Rust port of [`claude-mem`](https://github.com/thedotmack/claude-mem), focused on the Claude Code memory lifecycle on Unix-like systems.

This repository is a Rust workspace that replaces the TypeScript/Bun runtime with native binaries:

- `claude-mem-worker`: long-running Axum HTTP worker backed by SQLite/FTS5.
- `hook`: Claude Code hook dispatcher that reads hook JSON from stdin, calls the worker, and writes Claude-compatible JSON to stdout.
- `claude-mem-mcp`: stdio MCP server that exposes memory tools over the worker HTTP API.
- `claude-mem-core`: schema, migrations, storage, context formatting, and shared types.
- `claude-mem-sdk`: parser and prompt-building helpers with no service I/O dependencies.
- `claude-mem-supervisor`: process, health, PID, shutdown, and hook support code.

Windows is not a supported target. The runtime assumes Linux/macOS/POSIX process behavior.

## Status

The Rust port covers the storage, search, hook-normalization, and HTTP/MCP surfaces needed to run a native Rust memory worker:

- session start / prompt persistence
- PostToolUse observation capture
- file path tracking for tool observations
- manual memory save
- session summary storage, fallback generation, and searchable summary recall
- search across observations, prompts, and summaries
- search helpers by file, concept, and type
- timeline expansion around a search result
- semantic context lookup through SQLite FTS5
- optional self-hosted Qdrant indexing/search for observations, prompts, and summaries
- Claude, Cursor, Gemini CLI, Codex, and raw hook adapter normalization
- context injection
- session completion hook
- browser viewer and initial SSE snapshot stream
- worker health, readiness, version, doctor, PID file, and graceful HTTP shutdown
- import/export, settings, logs, project/stats, processing-status, and guarded branch admin routes
- MCP save/search/timeline/fetch tools over the worker
- native observer queue processing for queued observations and summaries
- local/fake deterministic observer runners plus Claude CLI, Gemini REST/CLI, Codex CLI, and OpenRouter REST provider runners
- tier model selection metadata for queued simple-tool and summary work
- browser viewer shell with live SSE events for session, observation, summary, queue, and manual-memory lifecycle changes
- Claude Stop/summarize transcript JSONL extraction for summary generation, with system-reminder stripping and completion cleanup
- rich built-in browser viewer for feed/search/timeline/context/admin/queue/logs/settings workflows
- POSIX installer/uninstaller CLI for Claude Code, Cursor, Gemini CLI, and Codex transcript integration
- generic JSONL transcript watcher daemon with v12-compatible schema config, offset state, tool pairing, summaries, and AGENTS context updates
- folder `CLAUDE.md` memory-context generation and cleanup
- MCP smart file search/outline/unfold helpers backed by the local filesystem

Queued observation and summary routes now drain through the Rust observer processor. The default provider is `local`, which deterministically converts hook payloads into recallable XML-backed memory without external credentials. Set `CLAUDE_MEM_PROVIDER=claude`, `gemini`, `gemini-cli`, `codex`, `openrouter`, or `fake` to use the corresponding runner.

## Build And Test

```bash
cargo build --workspace
cargo test --workspace
cargo test -p claude-mem-worker --features qdrant
```

Optional live provider smoke coverage is gated because it calls real CLIs/APIs:

```bash
CLAUDE_MEM_LIVE_PROVIDER_SMOKE=1 cargo test -p claude-mem-worker --test live_provider_smoke -- --nocapture
CLAUDE_MEM_LIVE_PROVIDER_SMOKE=1 CLAUDE_MEM_LIVE_OPENROUTER_SMOKE=1 cargo test -p claude-mem-worker --test live_provider_smoke -- --nocapture
```

## Runtime Layout

By default, the worker stores data under:

```text
~/.claude-mem/claude-mem.db
~/.claude-mem/worker.pid
```

Override the data directory and worker port with:

```bash
export CLAUDE_MEM_HOME=/path/to/data-dir
export CLAUDE_MEM_WORKER_PORT=37777
export CLAUDE_MEM_WORKER_HOST=127.0.0.1
export CLAUDE_MEM_WORKER_URL=http://127.0.0.1:37777
```

## Optional Qdrant

Qdrant support is optional and self-hosted. No commercial Qdrant account is required.

Run Qdrant locally:

```bash
docker run --rm -p 6333:6333 qdrant/qdrant
```

Build the worker with Qdrant support:

```bash
cargo run -p claude-mem-worker --features qdrant
```

Enable Qdrant at runtime:

```bash
export CLAUDE_MEM_QDRANT_ENABLED=true
export CLAUDE_MEM_QDRANT_URL=http://127.0.0.1:6333
export CLAUDE_MEM_QDRANT_COLLECTION=claude_mem_observations
```

The Rust worker uses a deterministic local hash embedding, so Qdrant does not require an embedding API key. SQLite remains the source of truth; if Qdrant is disabled or unavailable, memory writes and search fall back to SQLite. Qdrant payloads include schema metadata and distinguish observation, prompt, and summary points.

Qdrant endpoints:

```bash
curl http://127.0.0.1:37777/api/vector/qdrant/health

curl -X POST http://127.0.0.1:37777/api/vector/qdrant/reindex \
  -H 'content-type: application/json' \
  -d '{"project":"my-project","limit":1000}'

curl 'http://127.0.0.1:37777/api/search?strategy=qdrant&query=important&project=my-project'
```

Optional real-Qdrant smoke coverage:

```bash
QDRANT_URL=http://127.0.0.1:6333 cargo test -p claude-mem-worker --features qdrant real_qdrant_smoke
```

## Observer Providers

The worker processes pending observations and summaries through the observer queue. Provider selection is controlled with:

```bash
export CLAUDE_MEM_PROVIDER=local        # default deterministic local runner
export CLAUDE_MEM_PROVIDER=claude       # shells out to claude
export CLAUDE_MEM_PROVIDER=gemini       # uses Gemini REST API
export CLAUDE_MEM_PROVIDER=gemini-cli   # shells out to gemini
export CLAUDE_MEM_PROVIDER=codex        # shells out to codex exec
export CLAUDE_MEM_PROVIDER=openrouter   # uses OpenRouter REST API
```

Useful provider settings:

```bash
export CLAUDE_MEM_MODEL=sonnet
export CLAUDE_MEM_TIER_SIMPLE_MODEL=haiku
export CLAUDE_MEM_TIER_SUMMARY_MODEL=opus
export CLAUDE_MEM_CLAUDE_COMMAND=claude
export CLAUDE_MEM_CLAUDE_ARGS='-p --output-format json --tools "" --permission-mode dontAsk'
export CLAUDE_MEM_GEMINI_COMMAND=gemini
export CLAUDE_MEM_CODEX_COMMAND=codex
export CLAUDE_MEM_GEMINI_API_KEY=...
export CLAUDE_MEM_OPENROUTER_API_KEY=...
```

## Worker HTTP

Start the worker:

```bash
cargo run -p claude-mem-worker
```

Useful endpoints:

```bash
curl http://127.0.0.1:37777/
curl http://127.0.0.1:37777/stream
curl http://127.0.0.1:37777/api/health
curl http://127.0.0.1:37777/api/readiness
curl http://127.0.0.1:37777/api/version
curl http://127.0.0.1:37777/api/admin/doctor
curl http://127.0.0.1:37777/api/stats
curl http://127.0.0.1:37777/api/projects

curl -X POST http://127.0.0.1:37777/api/memory/save \
  -H 'content-type: application/json' \
  -d '{"project":"my-project","title":"Important memory","text":"Remember this."}'

curl 'http://127.0.0.1:37777/api/search?query=important&project=my-project'
curl 'http://127.0.0.1:37777/api/search/by-file?filePath=/repo/src/lib.rs'
curl 'http://127.0.0.1:37777/api/search/by-concept?concept=tool-use&project=my-project'
curl 'http://127.0.0.1:37777/api/search/by-type?type=discovery&project=my-project'
curl 'http://127.0.0.1:37777/api/timeline?anchor=1&project=my-project'

curl -X POST http://127.0.0.1:37777/api/sessions/summarize \
  -H 'content-type: application/json' \
  -d '{"contentSessionId":"demo","summary":"<summary><request>Demo</request><completed>Stored searchable summary.</completed></summary>"}'

curl http://127.0.0.1:37777/api/export
curl http://127.0.0.1:37777/api/settings
curl http://127.0.0.1:37777/api/logs
curl http://127.0.0.1:37777/api/branch/status
curl -X POST http://127.0.0.1:37777/api/admin/shutdown
```

## Claude Hook CLI

The hook binary is intentionally simple: the first argument is the platform, the second is the event, and stdin is the hook payload.

```bash
printf '%s' '{"session_id":"demo","cwd":"/repo/my-project","prompt":"Remember the Rust port."}' \
  | cargo run -p claude-mem-supervisor --bin hook -- claude-code session-init

printf '%s' '{"session_id":"demo","cwd":"/repo/my-project","tool_name":"Read","tool_input":{"file_path":"/repo/src/lib.rs"},"tool_response":{"content":"important result"}}' \
  | cargo run -p claude-mem-supervisor --bin hook -- claude-code observation

printf '%s' '{"session_id":"demo","cwd":"/repo/my-project"}' \
  | cargo run -p claude-mem-supervisor --bin hook -- claude-code context

printf '%s' '{"session_id":"demo","cwd":"/repo/my-project"}' \
  | cargo run -p claude-mem-supervisor --bin hook -- claude-code session-complete
```

Supported events are `session-init`, `observation`, `context`, `user-message`, `summarize`, and `session-complete`.

Supported platform adapters are:

- `claude-code` / `claude`
- `cursor` / `cursor-agent`
- `gemini` / `gemini-cli`
- `codex`
- `raw`

## MCP

Start the stdio MCP server:

```bash
cargo run -p claude-mem-mcp
```

The MCP process expects the worker to be reachable through `CLAUDE_MEM_WORKER_URL` or `CLAUDE_MEM_WORKER_PORT`.

The MCP server exposes memory save/search/timeline/fetch tools plus smart file helpers:

- `smart_search`
- `smart_outline`
- `smart_unfold`

## Folder CLAUDE.md Context

Generate folder-local Claude context files from the SQLite memory store:

```bash
cargo run -p claude-mem-supervisor --bin claude-mem -- generate --root /repo/my-project --project my-project --dry-run
cargo run -p claude-mem-supervisor --bin claude-mem -- generate --root /repo/my-project --project my-project
cargo run -p claude-mem-supervisor --bin claude-mem -- clean --root /repo/my-project
```

The generated block is tagged with `claude-mem-context`, so cleanup removes only Rust-port managed content and preserves user-authored `CLAUDE.md` text.

## Data Compatibility

The port mirrors the TypeScript SQLite schema and opens the same default database path. Existing `~/.claude-mem/claude-mem.db` data should be readable in place. Keep a database backup before switching active runtimes.

## Verification

The current end-to-end demo transcript was generated from live `target/debug/claude-mem-worker`, `target/debug/hook`, and `curl` executions:

```text
/home/kcrawley/.agents/notes/claude-mem-rs-http-cli-demo-2026-05-23.md
```
