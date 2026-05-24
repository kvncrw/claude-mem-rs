use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use claude_mem_core::db::observations::get::get_observations_by_ids;
use claude_mem_core::db::observations::store::{StoreObservationResult, store_observation};
use claude_mem_core::db::prompts::{PromptInput, get_user_prompts_by_ids, save_user_prompt};
use claude_mem_core::db::sessions::{create_session, update_memory_session_id};
use claude_mem_core::db::summaries::{SummaryInput, get_summaries_by_ids, store_summary};
use claude_mem_core::db::{self, open_or_create};
use claude_mem_core::types::ObservationInput;
use claude_mem_core::types::session::CreateSessionInput;
use claude_mem_worker::http::router::{AppState, build_router_with_state};
use claude_mem_worker::search::result_formatter::{ResultFormatter, SearchResults};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use tower::ServiceExt;

const PROJECT: &str = "fixture-project";
const CWD: &str = "/home/kcrawley/projects/fixture-project";

#[derive(Debug, Clone, Copy)]
struct FixtureIds {
    observations: [i64; 3],
    summary: i64,
    prompt: i64,
}

async fn get_text(app: axum::Router, uri: &str) -> (StatusCode, String) {
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

fn seed_boot_context_fixture(conn: &Connection) -> FixtureIds {
    create_session(
        conn,
        &CreateSessionInput {
            content_session_id: "fixture-content-1".into(),
            project: PROJECT.into(),
            user_prompt: Some("Can you verify the boot memory lifecycle fixture?".into()),
            started_at: "2024-06-01T09:00:00.000Z".into(),
            started_at_epoch: 1_717_232_400_000,
        },
    )
    .unwrap();
    update_memory_session_id(conn, "fixture-content-1", "fixture-memory-1").unwrap();

    let first = inserted_id(store_observation(
        conn,
        &ObservationInput {
            memory_session_id: "fixture-memory-1".into(),
            project: PROJECT.into(),
            r#type: "feature".into(),
            text: Some("Qdrant vector search replacement was implemented.".into()),
            title: Some("Build qdrant index population".into()),
            subtitle: Some("Chroma replacement stores compact point refs".into()),
            narrative: Some(
                "Qdrant replaces Chroma for vector search while preserving SQLite as source of truth."
                    .into(),
            ),
            facts: Some(vec![
                "Payload refs stay compact.".into(),
                "SQLite rows remain authoritative.".into(),
            ]),
            concepts: Some(vec!["what-changed".into(), "pattern".into()]),
            files_read: Some(vec!["crates/worker/src/search/qdrant.rs".into()]),
            files_modified: Some(vec!["crates/worker/src/search/qdrant.rs".into()]),
            prompt_number: Some(1),
            discovery_tokens: Some(1_300),
            relevance_count: Some(0),
            created_at: "2024-05-31T15:15:00.000Z".into(),
            created_at_epoch: 1_717_168_500_000,
            generated_by_model: None,
            merged_into_project: None,
            agent_type: None,
            agent_id: None,
            content_hash: Some("fixture-qdrant".into()),
        },
    )
    .unwrap());

    let second = inserted_id(
        store_observation(
            conn,
            &ObservationInput {
                memory_session_id: "fixture-memory-1".into(),
                project: PROJECT.into(),
                r#type: "bugfix".into(),
                text: Some("Boot memory table escaping was fixed.".into()),
                title: Some("Escape boot memory tables".into()),
                subtitle: Some("Pipe characters are escaped".into()),
                narrative: Some("Escaping pipes keeps markdown tables stable.".into()),
                facts: Some(vec!["Titles can contain pipe characters.".into()]),
                concepts: Some(vec!["problem-solution".into(), "gotcha".into()]),
                files_read: Some(vec!["crates/worker/src/search/result_formatter.rs".into()]),
                files_modified: Some(vec!["crates/worker/src/search/qdrant.rs".into()]),
                prompt_number: Some(1),
                discovery_tokens: Some(300),
                relevance_count: Some(0),
                created_at: "2024-05-31T15:15:00.000Z".into(),
                created_at_epoch: 1_717_168_500_000,
                generated_by_model: None,
                merged_into_project: None,
                agent_type: None,
                agent_id: None,
                content_hash: Some("fixture-escape".into()),
            },
        )
        .unwrap(),
    );

    let summary = store_summary(
        conn,
        &SummaryInput {
            memory_session_id: "fixture-memory-1".into(),
            project: PROJECT.into(),
            request: Some("Port boot memory lifecycle".into()),
            investigated: Some("TS v12 context-generator and Rust formatter".into()),
            learned: Some("Rust intentionally uses compact markdown tables".into()),
            completed: Some("Added boot memory parity fixtures".into()),
            next_steps: Some("Keep deltas documented when output changes".into()),
            files_read: Some("[\"scripts/context-generator.cjs\"]".into()),
            files_edited: Some(
                "[\"crates/claude-mem-worker/src/search/result_formatter.rs\"]".into(),
            ),
            notes: Some(
                "Fixture covers observations, prompts, summaries, files, concepts, and timestamps."
                    .into(),
            ),
            prompt_number: Some(1),
            discovery_tokens: Some(1_800),
            created_at: "2024-06-01T09:00:00.000Z".into(),
            created_at_epoch: 1_717_232_400_000,
            merged_into_project: None,
        },
    )
    .unwrap();

    let third = inserted_id(
        store_observation(
            conn,
            &ObservationInput {
                memory_session_id: "fixture-memory-1".into(),
                project: PROJECT.into(),
                r#type: "decision".into(),
                text: Some("Document TS and Rust boot memory deltas.".into()),
                title: Some("Keep TS | Rust parity explicit".into()),
                subtitle: Some("Golden fixture documents intentional differences".into()),
                narrative: Some(
                    "Golden tests should make every formatting delta reviewable.".into(),
                ),
                facts: Some(vec![
                    "Rust tables are denser than TS lines.".into(),
                    "TS v12 still provides the source comparison.".into(),
                ]),
                concepts: Some(vec!["trade-off".into(), "why-it-exists".into()]),
                files_read: Some(vec!["docs/boot.md".into()]),
                files_modified: None,
                prompt_number: Some(1),
                discovery_tokens: Some(800),
                relevance_count: Some(0),
                created_at: "2024-06-01T09:30:00.000Z".into(),
                created_at_epoch: 1_717_234_200_000,
                generated_by_model: None,
                merged_into_project: None,
                agent_type: None,
                agent_id: None,
                content_hash: Some("fixture-parity".into()),
            },
        )
        .unwrap(),
    );

    let prompt = save_user_prompt(
        conn,
        &PromptInput {
            content_session_id: "fixture-content-1".into(),
            prompt_number: 1,
            prompt_text: "Can you verify the boot memory lifecycle fixture?".into(),
            created_at: "2024-06-01T09:45:00.000Z".into(),
            created_at_epoch: 1_717_235_100_000,
        },
    )
    .unwrap();

    create_session(
        conn,
        &CreateSessionInput {
            content_session_id: "other-content".into(),
            project: "other-project".into(),
            user_prompt: Some("This should not appear".into()),
            started_at: "2024-06-01T10:00:00.000Z".into(),
            started_at_epoch: 1_717_236_000_000,
        },
    )
    .unwrap();
    update_memory_session_id(conn, "other-content", "other-memory").unwrap();
    let _ = store_observation(
        conn,
        &ObservationInput {
            memory_session_id: "other-memory".into(),
            project: "other-project".into(),
            r#type: "discovery".into(),
            text: Some("Other project memory must not leak into boot context.".into()),
            title: Some("Other project leak sentinel".into()),
            subtitle: None,
            narrative: Some("This row should be filtered out.".into()),
            facts: None,
            concepts: Some(vec!["gotcha".into()]),
            files_read: None,
            files_modified: None,
            prompt_number: Some(1),
            discovery_tokens: Some(999),
            relevance_count: Some(0),
            created_at: "2024-06-01T10:00:00.000Z".into(),
            created_at_epoch: 1_717_236_000_000,
            generated_by_model: None,
            merged_into_project: None,
            agent_type: None,
            agent_id: None,
            content_hash: Some("fixture-other".into()),
        },
    )
    .unwrap();

    FixtureIds {
        observations: [first, second, third],
        summary,
        prompt,
    }
}

fn inserted_id(result: StoreObservationResult) -> i64 {
    match result {
        StoreObservationResult::Inserted(id) | StoreObservationResult::Duplicate(id) => id,
    }
}

fn normalize_context_timestamp(text: &str) -> String {
    let mut lines = text
        .trim_end()
        .lines()
        .skip_while(|line| !line.starts_with("# ["));
    let Some(first) = lines.next() else {
        return String::new();
    };
    let mut normalized = String::new();
    if let Some((prefix, _timestamp)) = first.rsplit_once(", ") {
        normalized.push_str(prefix);
        normalized.push_str(", <TIMESTAMP>");
    } else {
        normalized.push_str(first);
    }
    for line in lines {
        normalized.push('\n');
        normalized.push_str(line);
    }
    normalized
}

#[tokio::test]
async fn rust_boot_context_matches_checked_in_golden() {
    let state = AppState::in_memory().unwrap();
    {
        let conn = state.conn.lock().unwrap();
        seed_boot_context_fixture(&conn);
    }
    let app = build_router_with_state(state);

    let (status, body) =
        get_text(app, "/api/context/inject?project=fixture-project&limit=20").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        normalize_context_timestamp(&body),
        include_str!("fixtures/boot_context_rust_expected.md").trim_end()
    );
}

#[tokio::test]
async fn rust_boot_context_empty_state_matches_checked_in_golden() {
    let app = build_router_with_state(AppState::in_memory().unwrap());

    let (status, body) = get_text(app, "/api/context/inject?project=empty-project").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        normalize_context_timestamp(&body),
        include_str!("fixtures/boot_context_empty_expected.md").trim_end()
    );
}

#[test]
fn search_golden_covers_summary_and_prompt_rows_from_shared_fixture() {
    let conn = db::open_in_memory().unwrap();
    let ids = seed_boot_context_fixture(&conn);
    let observations = get_observations_by_ids(&conn, &ids.observations).unwrap();
    let sessions = get_summaries_by_ids(&conn, &[ids.summary]).unwrap();
    let prompts = get_user_prompts_by_ids(&conn, &[ids.prompt]).unwrap();

    let formatted = ResultFormatter::new().format_search_results(
        &SearchResults {
            observations,
            sessions,
            prompts,
        },
        "fixture lifecycle",
        false,
    );

    assert_eq!(
        formatted,
        include_str!("fixtures/search_results_expected.md").trim_end()
    );
}

#[test]
fn documented_ts_v12_boot_context_fixture_matches_when_source_runtime_is_available() {
    let Some(ts) = TsRuntime::discover() else {
        eprintln!("skipping TS v12 context fixture: Bun or context-generator.cjs not found");
        return;
    };

    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let conn = open_or_create(data_dir.join("claude-mem.db")).unwrap();
    seed_boot_context_fixture(&conn);
    drop(conn);

    let output = ts.generate_context(&data_dir).unwrap();
    assert_eq!(
        normalize_context_timestamp(&output),
        include_str!("fixtures/boot_context_ts_v12_expected.md").trim_end()
    );
    assert!(
        include_str!("fixtures/boot_context_ts_v12_deltas.md")
            .contains("Rust uses a compact markdown table")
    );
}

struct TsRuntime {
    bun: PathBuf,
    context_generator: PathBuf,
}

impl TsRuntime {
    fn discover() -> Option<Self> {
        let bun = std::env::var_os("BUN")
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .or_else(|| {
                let path = PathBuf::from("/home/kcrawley/.bun/bin/bun");
                path.exists().then_some(path)
            })?;
        let context_generator = std::env::var_os("CLAUDE_MEM_TS_V12_CONTEXT_GENERATOR")
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .or_else(|| {
                let path = PathBuf::from(
                    "/home/kcrawley/.claude/plugins/marketplaces/thedotmack/plugin/scripts/context-generator.cjs",
                );
                path.exists().then_some(path)
            })?;
        Some(Self {
            bun,
            context_generator,
        })
    }

    fn generate_context(&self, data_dir: &Path) -> Result<String, String> {
        let home = data_dir.parent().unwrap().join("home");
        std::fs::create_dir_all(&home).map_err(|error| error.to_string())?;
        let script = r#"
const fs = require("fs");
const Module = require("module");
const path = require("path");
const file = process.env.TS_CONTEXT_GENERATOR;
let code = fs.readFileSync(file, "utf8");
code = code.replace(
  "module.exports=kt(ns);",
  "module.exports=kt(ns); Object.defineProperty(module.exports, \"__loadMode\", { value: (mode) => A.getInstance().loadMode(mode) });"
);
const mod = new Module(file);
mod.filename = file;
mod.paths = Module._nodeModulePaths(path.dirname(file));
mod._compile(code, file);
mod.exports.__loadMode(process.env.CLAUDE_MEM_MODE || "code");
mod.exports.generateContext({
  cwd: process.env.FIXTURE_CWD,
  session_id: "fixture-memory-1"
}).then((context) => {
  process.stdout.write(context);
}).catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
"#;
        let output = Command::new(&self.bun)
            .arg("-e")
            .arg(script)
            .env("TS_CONTEXT_GENERATOR", &self.context_generator)
            .env("FIXTURE_CWD", CWD)
            .env("CLAUDE_MEM_DATA_DIR", data_dir)
            .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
            .env("HOME", &home)
            .env("TZ", "UTC")
            .env("CLAUDE_MEM_MODE", "code")
            .env("CLAUDE_MEM_CONTEXT_OBSERVATIONS", "20")
            .env("CLAUDE_MEM_CONTEXT_FULL_COUNT", "2")
            .env("CLAUDE_MEM_CONTEXT_SESSION_COUNT", "10")
            .env("CLAUDE_MEM_CONTEXT_SHOW_READ_TOKENS", "true")
            .env("CLAUDE_MEM_CONTEXT_SHOW_WORK_TOKENS", "true")
            .env("CLAUDE_MEM_CONTEXT_SHOW_SAVINGS_PERCENT", "false")
            .env("CLAUDE_MEM_CONTEXT_SHOW_SAVINGS_AMOUNT", "false")
            .env("CLAUDE_MEM_CONTEXT_SHOW_LAST_SUMMARY", "true")
            .env("CLAUDE_MEM_CONTEXT_SHOW_LAST_MESSAGE", "false")
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}
