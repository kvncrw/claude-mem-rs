use claude_mem_core::db::observations::store_observation;
use claude_mem_core::db::open_or_create;
use claude_mem_core::db::sessions::{create_session, update_memory_session_id};
use claude_mem_core::types::observation::ObservationInput;
use claude_mem_core::types::session::CreateSessionInput;
use claude_mem_supervisor::claude_md::{clean, generate, ClaudeMdOptions};

#[test]
fn generate_and_clean_folder_claude_md_context() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("cloudy-project");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='cloudy-project'\n",
    )
    .unwrap();
    std::fs::write(src.join("thermal.rs"), "pub fn cap() {}\n").unwrap();

    let db_path = tmp.path().join("claude-mem.db");
    let conn = open_or_create(&db_path).unwrap();
    create_session(
        &conn,
        &CreateSessionInput {
            content_session_id: "session-1".into(),
            project: "cloudy-project".into(),
            user_prompt: Some("remember thermal notes".into()),
            started_at: "2026-05-23T00:00:00Z".into(),
            started_at_epoch: 1,
        },
    )
    .unwrap();
    update_memory_session_id(&conn, "session-1", "memory-1").unwrap();
    store_observation(
        &conn,
        &ObservationInput {
            memory_session_id: "memory-1".into(),
            project: "cloudy-project".into(),
            r#type: "decision".into(),
            title: Some("Reduce package wattage".into()),
            narrative: Some("Fans are not enough for tiny Dynatron coolers.".into()),
            files_read: Some(vec!["src/thermal.rs".into()]),
            created_at: "2026-05-23T00:00:00Z".into(),
            created_at_epoch: 1,
            ..Default::default()
        },
    )
    .unwrap();

    let report = generate(ClaudeMdOptions {
        dry_run: false,
        project_root: root.clone(),
        db_path: Some(db_path),
        project: None,
        target_file: None,
        limit: 10,
    })
    .unwrap();
    assert_eq!(report.written, 1);
    let claude_md = src.join("CLAUDE.md");
    let content = std::fs::read_to_string(&claude_md).unwrap();
    assert!(content.contains("<claude-mem-context>"));
    assert!(content.contains("Reduce package wattage"));

    let cleaned = clean(ClaudeMdOptions {
        dry_run: false,
        project_root: root,
        db_path: None,
        project: None,
        target_file: None,
        limit: 10,
    })
    .unwrap();
    assert_eq!(cleaned.deleted, 1);
    assert!(!claude_md.exists());
}
