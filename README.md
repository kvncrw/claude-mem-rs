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

The Rust port currently covers the normal Claude lifecycle path:

- session start / prompt persistence
- PostToolUse observation capture
- file path tracking for tool observations
- manual memory save
- search across observations and prompts
- search helpers by file, concept, and type
- timeline expansion around a search result
- semantic context lookup through SQLite FTS5
- Claude context injection
- session completion hook
- worker health, readiness, version, PID file, and graceful HTTP shutdown
- MCP save/search/timeline/fetch tools over the worker

The original TypeScript project also includes broader UI, installer, multi-editor, and admin surfaces. Those are not the primary runtime surface here yet.

## Build And Test

```bash
cargo build --workspace
cargo test --workspace
```

Known warning: `bon::Builder` currently emits `unexpected cfg condition name: rust_analyzer` warnings during builds. The test suite is otherwise green.

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

## Worker HTTP

Start the worker:

```bash
cargo run -p claude-mem-worker
```

Useful endpoints:

```bash
curl http://127.0.0.1:37777/api/health
curl http://127.0.0.1:37777/api/readiness
curl http://127.0.0.1:37777/api/version

curl -X POST http://127.0.0.1:37777/api/memory/save \
  -H 'content-type: application/json' \
  -d '{"project":"my-project","title":"Important memory","text":"Remember this."}'

curl 'http://127.0.0.1:37777/api/search?query=important&project=my-project'
curl 'http://127.0.0.1:37777/api/search/by-file?filePath=/repo/src/lib.rs'
curl 'http://127.0.0.1:37777/api/search/by-concept?concept=tool-use&project=my-project'
curl 'http://127.0.0.1:37777/api/search/by-type?type=discovery&project=my-project'
curl 'http://127.0.0.1:37777/api/timeline?anchor=1&project=my-project'

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

Supported events are `session-init`, `observation`, `context`, `user-message`, and `session-complete`.

## MCP

Start the stdio MCP server:

```bash
cargo run -p claude-mem-mcp
```

The MCP process expects the worker to be reachable through `CLAUDE_MEM_WORKER_URL` or `CLAUDE_MEM_WORKER_PORT`.

## Data Compatibility

The port mirrors the TypeScript SQLite schema and opens the same default database path. Existing `~/.claude-mem/claude-mem.db` data should be readable in place. Keep a database backup before switching active runtimes.

## Verification

The current end-to-end demo transcript was generated from live `target/debug/claude-mem-worker`, `target/debug/hook`, and `curl` executions:

```text
/home/kcrawley/.agents/notes/claude-mem-rs-http-cli-demo-2026-05-23.md
```
