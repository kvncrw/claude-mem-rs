# Changelog

All notable changes to `claude-mem-rs` are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **Persistent installer services** — `claude-mem install` now writes a stable
  `claude-mem` launcher, configures persistent worker + Codex transcript
  watcher services, and covers Linux systemd user units, macOS LaunchAgents,
  and Windows ONLOGON Scheduled Tasks. The docs now describe all three install
  paths and the service-management escape hatches.

- **Corpus / knowledge-agent subsystem** — ports the TypeScript v12 corpus
  surface to Rust. ([#1](https://github.com/kvncrw/claude-mem-rs/issues/1))
  - `CorpusFile` / `CorpusFilter` / `CorpusStats` / `CorpusObservation`
    types in `claude-mem-core`. Persistence layout (`version: 1`, field
    names, `~/.claude-mem/corpora/{name}.corpus.json` path) is byte-compatible
    with the TS implementation, so corpora are interchangeable.
  - `CorpusStore`, `CorpusBuilder`, `CorpusRenderer` in `claude-mem-worker`
    (always-on, default features). Builder composes the existing
    `SearchOrchestrator` path — no new SQL.
  - `KnowledgeAgent` (prime / query / reprime) behind a new
    `knowledge-agent` cargo feature on both `claude-mem-worker` and
    `claude-mem-mcp` (default OFF). Prime shells out to the `claude` CLI
    with `--output-format stream-json` and parses the JSONL response;
    `--disallowed-tools` blocklist matches TS verbatim. Backend is split
    behind a `ClaudeBackend` trait so the production CLI shell-out is
    fully mockable in tests. Prompt is passed via stdin to keep it out
    of `/proc/<pid>/cmdline`.
  - 8 new HTTP routes registered unconditionally:
    `GET/POST /api/corpus`, `GET/DELETE /api/corpus/:name`,
    `POST /api/corpus/:name/{rebuild,prime,query,reprime}`. The three
    knowledge-agent routes return `501 Not Implemented` with a JSON body
    explaining the disabled feature when the feature flag is off.
  - 6 new MCP tools, names matching TS verbatim: `build_corpus`,
    `list_corpora`, `prime_corpus`, `query_corpus`, `rebuild_corpus`,
    `reprime_corpus`.
  - Unit coverage: 27 tests across the new modules (storage round-trip,
    name + path-traversal guard, renderer snapshot, JSONL parser for the
    `claude` CLI stream, builder against an in-memory database). E2E
    coverage: 7 tests across `corpus_http_e2e.rs` + `corpus_mcp_e2e.rs`
    covering build / list / get / rebuild / delete round-trip, name and
    type validation, and the 501-when-feature-off path.

### Changed

- Pre-existing clippy warnings cleared in nine non-test files across
  `claude-mem-core`, `claude-mem-worker`, and `claude-mem-supervisor`
  (mechanical only — `sort_by` → `sort_by_key`, manual `Default` impls
  → `#[derive(Default)]`, `repeat().take(N)` → `repeat_n`, etc.).
  Behavior preserved. This unblocks the `-D warnings` clippy gate for
  the new corpus modules.

### Tests

- **Installer fixture parity** — pinned the output shape of every
  non-Claude installer integration (`.cursor/mcp.json`, `.gemini/settings.json`,
  `.codex/AGENTS.md` + transcript-watch sample, `.config/opencode/opencode.json`
  + generated lifecycle plugin JS). 18 tests across 4 new files under
  `crates/claude-mem-supervisor/tests/installer_{cursor,gemini,codex,opencode}_fixture.rs`.
  Stale Gemini `Stop` hook removal, opencode plugin event names, dedupe on
  rerun, and preservation of user-customized config keys are all covered.
  ([#2](https://github.com/kvncrw/claude-mem-rs/issues/2))
- Installer service rendering is covered by supervisor tests, and fixture tests
  redirect Linux systemd, macOS LaunchAgent, and Windows Scheduled Task outputs
  so install tests can run without mutating the host service manager.
