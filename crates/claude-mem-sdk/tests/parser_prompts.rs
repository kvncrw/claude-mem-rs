use claude_mem_sdk::{
    build_continuation_prompt, build_init_prompt, build_observation_prompt, build_summary_prompt,
    parse_observations, parse_summary, ObservationPromptInput, SummaryPromptInput,
};

#[test]
fn parser_extracts_observations_and_summaries() {
    let observations = parse_observations(
        r#"
        <observation>
          <type>decision</type>
          <title>Use Qdrant</title>
          <facts><fact>Qdrant replaces Chroma.</fact></facts>
          <narrative>Self-hosted Qdrant is enough.</narrative>
          <concepts><concept>decision</concept><concept>qdrant</concept></concepts>
          <files_read><file>README.md</file></files_read>
          <files_modified></files_modified>
        </observation>
        "#,
        Some("test"),
    );
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].r#type, "decision");
    assert_eq!(observations[0].title, "Use Qdrant");
    assert_eq!(observations[0].concepts, vec!["qdrant"]);

    let summary = parse_summary(
        r#"<summary><request>Port memory</request><learned>Prompt builders matter.</learned></summary>"#,
        Some("session"),
    )
    .unwrap();
    assert_eq!(summary.request.as_deref(), Some("Port memory"));
    assert_eq!(summary.learned.as_deref(), Some("Prompt builders matter."));
    assert!(parse_summary(r#"<skip_summary reason="empty" />"#, None).is_none());
}

#[test]
fn prompt_builders_emit_non_empty_contracts() {
    let init = build_init_prompt("claude-mem-rs", "session-1", "Remember port parity.");
    assert!(init.contains("<observed_from_primary_session>"));
    assert!(init.contains("<observation>"));

    let observation = build_observation_prompt(&ObservationPromptInput {
        tool_name: "Read".into(),
        tool_input: r#"{"file_path":"src/lib.rs"}"#.into(),
        tool_output: r#"{"content":"important"}"#.into(),
        created_at_epoch: 1_748_000_000_000,
        cwd: Some("/repo".into()),
    });
    assert!(observation.contains(
        "Return either one or more <observation>...</observation> blocks, or an empty response"
    ));
    assert!(observation.contains("Concrete debugging findings from logs, queue state, database rows, session routing, or code-path inspection"));
    assert!(observation.contains(
        "Never reply with prose such as \"Skipping\", \"No substantive tool executions\""
    ));

    let summary = build_summary_prompt(&SummaryPromptInput {
        session_db_id: 42,
        memory_session_id: Some("memory-42".into()),
        project: "claude-mem-rs".into(),
        user_prompt: "Finish the port".into(),
        last_assistant_message: "Implemented parser.".into(),
    });
    assert!(summary.contains("<summary>"));
    assert!(summary.contains("MODE SWITCH"));

    let continuation = build_continuation_prompt("Next prompt", 2, "session-1");
    assert!(continuation.contains("<prompt_number>2</prompt_number>"));
}
