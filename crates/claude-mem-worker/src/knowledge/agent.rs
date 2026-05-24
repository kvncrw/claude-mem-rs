//! Knowledge agent — port of upstream-claude-mem
//! `src/services/worker/knowledge/KnowledgeAgent.ts`.
//!
//! Drives a real `claude` CLI process to prime/query/reprime a Q&A session
//! against a corpus. The TS version uses the in-process `@anthropic-ai/claude-agent-sdk`
//! JS package; in Rust we shell out to `claude --print --output-format stream-json`
//! and parse the JSONL stdout, which gives us the same `session_id` and
//! assistant-text values without dragging a JS runtime in.
//!
//! Compiled only when cargo feature `knowledge-agent` is on.

use std::ffi::OsString;
use std::process::Stdio;

use claude_mem_core::types::{CorpusFile, CorpusQueryResult};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::renderer::CorpusRenderer;
use super::store::{CorpusStore, CorpusStoreError};
use super::KNOWLEDGE_AGENT_DISALLOWED_TOOLS;

/// Errors raised by the knowledge agent runtime.
#[derive(Debug, Error)]
pub enum KnowledgeAgentError {
    /// The corpus has never been primed; call `prime()` first.
    #[error("corpus \"{0}\" has no session — call prime first")]
    NotPrimed(String),
    /// The `claude` CLI executable could not be found via env or PATH.
    #[error("claude executable not found; set CLAUDE_CODE_PATH or add `claude` to PATH")]
    ExecutableNotFound,
    /// The CLI ran but never emitted a session_id during priming.
    #[error("failed to capture session_id while priming corpus \"{0}\"")]
    NoSessionId(String),
    /// `claude` exited non-zero (and we did not capture useful output).
    #[error("claude exited with status {status}: {stderr}")]
    NonZeroExit { status: i32, stderr: String },
    /// Underlying IO or process error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON parse error on a stream-json line.
    #[error("json parse error on stream-json line: {0}")]
    Json(#[from] serde_json::Error),
    /// Underlying store error from persisting the session_id back.
    #[error(transparent)]
    Store(#[from] CorpusStoreError),
}

impl KnowledgeAgentError {
    /// True when the error matches the TS regex for "session resume failure"
    /// — these cases auto-trigger a reprime.
    pub fn is_session_resume_error(&self) -> bool {
        let message = self.to_string().to_lowercase();
        // Pattern: /session|resume|expired|invalid.*session|not found/i
        message.contains("session")
            || message.contains("resume")
            || message.contains("expired")
            || message.contains("not found")
    }
}

/// Whether the error text from a `claude` invocation looks like a session-resume
/// failure (separate from our error enum because the CLI's stderr is opaque
/// text, not a typed error).
pub fn looks_like_session_resume_error(text: &str) -> bool {
    let lowered = text.to_lowercase();
    lowered.contains("expired")
        || lowered.contains("invalid session")
        || lowered.contains("session not found")
        || lowered.contains("could not resume")
        || (lowered.contains("session") && lowered.contains("resume"))
        || (lowered.contains("session") && lowered.contains("invalid"))
        || (lowered.contains("session") && lowered.contains("not found"))
}

/// Output of one `claude` invocation, parsed from stream-json stdout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeInvocation {
    /// `session_id` value from the last frame that carried it.
    pub session_id: Option<String>,
    /// Concatenated text content of every `assistant`-type frame.
    pub assistant_text: String,
    /// Raw stderr (used to detect session-resume failures).
    pub stderr: String,
    /// Exit code reported by the CLI.
    pub exit_code: i32,
}

/// Backend abstraction so unit tests can substitute a fake `claude` instead
/// of spawning a real process. Production uses [`ClaudeCliBackend`].
#[async_trait::async_trait]
pub trait ClaudeBackend: Send + Sync {
    async fn invoke(
        &self,
        prompt: &str,
        resume: Option<&str>,
        disallowed: &[&str],
    ) -> Result<ClaudeInvocation, KnowledgeAgentError>;
}

/// Production backend: spawns `claude --print --output-format stream-json`
/// and parses each stdout line as a JSON frame.
#[derive(Debug, Clone)]
pub struct ClaudeCliBackend {
    executable: OsString,
    extra_args: Vec<OsString>,
}

impl ClaudeCliBackend {
    /// Auto-detect the `claude` executable from `CLAUDE_CODE_PATH` or `$PATH`.
    pub fn from_env() -> Result<Self, KnowledgeAgentError> {
        if let Ok(configured) = std::env::var("CLAUDE_CODE_PATH") {
            let path = std::path::PathBuf::from(&configured);
            if !path.exists() {
                tracing::warn!(
                    %configured,
                    "CLAUDE_CODE_PATH is set but the file does not exist; falling back to PATH"
                );
            } else {
                return Ok(Self {
                    executable: configured.into(),
                    extra_args: Vec::new(),
                });
            }
        }
        // PATH fallback — Tokio's Command resolves PATH automatically when we
        // pass a bare name, so we just trust the lookup succeeds at spawn time.
        // We could `which` here, but production callers can rescue a spawn
        // failure with a clearer error message via `Self::with_executable`.
        Ok(Self {
            executable: "claude".into(),
            extra_args: Vec::new(),
        })
    }

    pub fn with_executable(executable: impl Into<OsString>) -> Self {
        Self {
            executable: executable.into(),
            extra_args: Vec::new(),
        }
    }
}

#[async_trait::async_trait]
impl ClaudeBackend for ClaudeCliBackend {
    async fn invoke(
        &self,
        prompt: &str,
        resume: Option<&str>,
        disallowed: &[&str],
    ) -> Result<ClaudeInvocation, KnowledgeAgentError> {
        let mut cmd = Command::new(&self.executable);
        cmd.arg("--print").arg("--output-format").arg("stream-json");
        if let Some(session) = resume {
            cmd.arg("--resume").arg(session);
        }
        if !disallowed.is_empty() {
            cmd.arg("--disallowed-tools").arg(disallowed.join(","));
        }
        for arg in &self.extra_args {
            cmd.arg(arg);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                KnowledgeAgentError::ExecutableNotFound
            } else {
                KnowledgeAgentError::Io(error)
            }
        })?;

        // Pipe the prompt over stdin so we never expose it on argv.
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.flush().await?;
            drop(stdin);
        }

        let stdout = child.stdout.take().expect("stdout pipe configured");
        let stderr = child.stderr.take().expect("stderr pipe configured");

        let stdout_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            let mut invocation = ClaudeInvocation::default();
            while let Some(line) = reader.next_line().await? {
                if line.trim().is_empty() {
                    continue;
                }
                apply_stream_frame(&mut invocation, &line)?;
            }
            Ok::<ClaudeInvocation, KnowledgeAgentError>(invocation)
        });

        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            let mut buf = String::new();
            while let Some(line) = reader.next_line().await? {
                buf.push_str(&line);
                buf.push('\n');
            }
            Ok::<String, KnowledgeAgentError>(buf)
        });

        let status = child.wait().await?;
        let mut invocation = stdout_task.await.expect("stdout task panicked")?;
        invocation.stderr = stderr_task.await.expect("stderr task panicked")?;
        invocation.exit_code = status.code().unwrap_or(-1);
        Ok(invocation)
    }
}

/// Apply a single stream-json frame to the running invocation snapshot. Frames
/// look like `{"type":"assistant","session_id":"...","message":{"content":[...]}}`
/// or `{"type":"result","session_id":"..."}`. We tolerate unknown fields and
/// unknown frame types.
fn apply_stream_frame(
    invocation: &mut ClaudeInvocation,
    line: &str,
) -> Result<(), KnowledgeAgentError> {
    let frame: Value = serde_json::from_str(line)?;
    if let Some(session) = frame.get("session_id").and_then(Value::as_str) {
        invocation.session_id = Some(session.to_owned());
    }
    let frame_type = frame.get("type").and_then(Value::as_str).unwrap_or("");
    if frame_type == "assistant" {
        if let Some(content) = frame
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        {
            for block in content {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        invocation.assistant_text.push_str(text);
                    }
                }
            }
        }
    }
    Ok(())
}

/// High-level knowledge agent — same prime/query/reprime surface as TS.
pub struct KnowledgeAgent {
    backend: Box<dyn ClaudeBackend>,
    renderer: CorpusRenderer,
}

impl KnowledgeAgent {
    pub fn new(backend: Box<dyn ClaudeBackend>) -> Self {
        Self {
            backend,
            renderer: CorpusRenderer::new(),
        }
    }

    /// Construct using the real `claude` CLI on `$PATH`.
    pub fn with_cli() -> Result<Self, KnowledgeAgentError> {
        Ok(Self::new(Box::new(ClaudeCliBackend::from_env()?)))
    }

    /// Load the full corpus into a new agent session. Updates `corpus.session_id`
    /// and persists via `store`.
    pub async fn prime(
        &self,
        corpus: &mut CorpusFile,
        store: &CorpusStore,
    ) -> Result<String, KnowledgeAgentError> {
        let rendered = self.renderer.render_corpus(corpus);
        let prompt = format!(
            "{}\n\nHere is your complete knowledge base:\n\n{}\n\nAcknowledge what you've received. Summarize the key themes and topics you can answer questions about.",
            corpus.system_prompt, rendered,
        );
        let invocation = self
            .backend
            .invoke(&prompt, None, KNOWLEDGE_AGENT_DISALLOWED_TOOLS)
            .await?;
        let session_id = invocation
            .session_id
            .ok_or_else(|| KnowledgeAgentError::NoSessionId(corpus.name.clone()))?;
        corpus.session_id = Some(session_id.clone());
        store.write(corpus)?;
        Ok(session_id)
    }

    /// Query a primed agent. Auto-reprimes once on session-resume failure and
    /// retries the question.
    pub async fn query(
        &self,
        corpus: &mut CorpusFile,
        store: &CorpusStore,
        question: &str,
    ) -> Result<CorpusQueryResult, KnowledgeAgentError> {
        let session_id = corpus
            .session_id
            .clone()
            .ok_or_else(|| KnowledgeAgentError::NotPrimed(corpus.name.clone()))?;

        match self.execute_query(&session_id, question).await {
            Ok(result) => {
                if result.session_id != session_id {
                    corpus.session_id = Some(result.session_id.clone());
                    store.write(corpus)?;
                }
                Ok(result)
            }
            Err(error) if error.is_session_resume_error() => {
                tracing::info!(
                    corpus = %corpus.name,
                    "session expired; auto-repriming"
                );
                self.prime(corpus, store).await?;
                let new_session = corpus
                    .session_id
                    .clone()
                    .ok_or_else(|| KnowledgeAgentError::NoSessionId(corpus.name.clone()))?;
                let result = self.execute_query(&new_session, question).await?;
                if result.session_id != new_session {
                    corpus.session_id = Some(result.session_id.clone());
                    store.write(corpus)?;
                }
                Ok(result)
            }
            Err(error) => Err(error),
        }
    }

    /// Clear the existing session and prime again. Mirrors TS' `reprime` —
    /// no `claude --logout`, just a fresh session.
    pub async fn reprime(
        &self,
        corpus: &mut CorpusFile,
        store: &CorpusStore,
    ) -> Result<String, KnowledgeAgentError> {
        corpus.session_id = None;
        self.prime(corpus, store).await
    }

    async fn execute_query(
        &self,
        session_id: &str,
        question: &str,
    ) -> Result<CorpusQueryResult, KnowledgeAgentError> {
        let invocation = self
            .backend
            .invoke(question, Some(session_id), KNOWLEDGE_AGENT_DISALLOWED_TOOLS)
            .await?;
        // CLI sometimes exits non-zero after a fully streamed response — TS
        // tolerates this when an answer was captured. Honour that.
        if invocation.exit_code != 0 && invocation.assistant_text.is_empty() {
            if looks_like_session_resume_error(&invocation.stderr) {
                return Err(KnowledgeAgentError::NonZeroExit {
                    status: invocation.exit_code,
                    stderr: format!("session resume failed: {}", invocation.stderr.trim()),
                });
            }
            return Err(KnowledgeAgentError::NonZeroExit {
                status: invocation.exit_code,
                stderr: invocation.stderr.trim().to_owned(),
            });
        }
        let session = invocation
            .session_id
            .unwrap_or_else(|| session_id.to_owned());
        Ok(CorpusQueryResult {
            answer: invocation.assistant_text,
            session_id: session,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_mem_core::types::{
        CorpusDateRange, CorpusFile, CorpusFilter, CorpusObservation, CorpusStats, CorpusVersion,
    };
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    #[derive(Default)]
    struct FakeBackend {
        calls: Mutex<Vec<(String, Option<String>)>>,
        call_count: AtomicUsize,
        prime_session: String,
        query_response: String,
        query_session: String,
        // when set, the Nth call (0-indexed) returns this error
        failures: Mutex<HashMap<usize, KnowledgeAgentError>>,
    }

    impl FakeBackend {
        fn new(prime: &str, query_session: &str, response: &str) -> Self {
            Self {
                calls: Mutex::default(),
                call_count: AtomicUsize::new(0),
                prime_session: prime.into(),
                query_response: response.into(),
                query_session: query_session.into(),
                failures: Mutex::default(),
            }
        }

        fn fail_call(&self, index: usize, err: KnowledgeAgentError) {
            self.failures.lock().unwrap().insert(index, err);
        }

        fn call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl ClaudeBackend for FakeBackend {
        async fn invoke(
            &self,
            prompt: &str,
            resume: Option<&str>,
            _disallowed: &[&str],
        ) -> Result<ClaudeInvocation, KnowledgeAgentError> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
            self.calls
                .lock()
                .unwrap()
                .push((prompt.to_owned(), resume.map(str::to_owned)));
            if let Some(err) = self.failures.lock().unwrap().remove(&idx) {
                return Err(err);
            }
            if resume.is_some() {
                Ok(ClaudeInvocation {
                    session_id: Some(self.query_session.clone()),
                    assistant_text: self.query_response.clone(),
                    stderr: String::new(),
                    exit_code: 0,
                })
            } else {
                Ok(ClaudeInvocation {
                    session_id: Some(self.prime_session.clone()),
                    assistant_text: "primed".to_owned(),
                    stderr: String::new(),
                    exit_code: 0,
                })
            }
        }
    }

    fn fixture_corpus() -> CorpusFile {
        CorpusFile {
            version: CorpusVersion,
            name: "fix".into(),
            description: "fix".into(),
            created_at: "2026-05-24T00:00:00Z".into(),
            updated_at: "2026-05-24T00:00:00Z".into(),
            filter: CorpusFilter::default(),
            stats: CorpusStats {
                observation_count: 1,
                token_estimate: 100,
                date_range: CorpusDateRange {
                    earliest: "2026-05-24T00:00:00Z".into(),
                    latest: "2026-05-24T00:00:00Z".into(),
                },
                type_breakdown: HashMap::new(),
            },
            system_prompt: "you are an agent".into(),
            session_id: None,
            observations: vec![CorpusObservation {
                id: 1,
                r#type: "decision".into(),
                title: "t".into(),
                subtitle: None,
                narrative: None,
                facts: vec![],
                concepts: vec![],
                files_read: vec![],
                files_modified: vec![],
                project: "p".into(),
                created_at: "2026-05-24T00:00:00Z".into(),
                created_at_epoch: 1,
            }],
        }
    }

    #[tokio::test]
    async fn prime_captures_session_and_persists() {
        let tmp = TempDir::new().unwrap();
        let store = CorpusStore::new(tmp.path());
        let backend = Arc::new(FakeBackend::new("sess-1", "sess-1", "hi"));
        let agent = KnowledgeAgent {
            backend: Box::new(BackendHandle(backend.clone())),
            renderer: CorpusRenderer::new(),
        };
        let mut corpus = fixture_corpus();
        let sid = agent.prime(&mut corpus, &store).await.unwrap();
        assert_eq!(sid, "sess-1");
        assert_eq!(corpus.session_id.as_deref(), Some("sess-1"));
        let loaded = store.read("fix").unwrap().unwrap();
        assert_eq!(loaded.session_id.as_deref(), Some("sess-1"));
        assert_eq!(backend.call_count(), 1);
    }

    #[tokio::test]
    async fn query_requires_prior_prime() {
        let tmp = TempDir::new().unwrap();
        let store = CorpusStore::new(tmp.path());
        let backend = Arc::new(FakeBackend::new("sess-1", "sess-1", "hi"));
        let agent = KnowledgeAgent {
            backend: Box::new(BackendHandle(backend.clone())),
            renderer: CorpusRenderer::new(),
        };
        let mut corpus = fixture_corpus();
        let err = agent.query(&mut corpus, &store, "what?").await.unwrap_err();
        matches!(err, KnowledgeAgentError::NotPrimed(_));
    }

    #[tokio::test]
    async fn query_auto_reprimes_on_session_failure() {
        let tmp = TempDir::new().unwrap();
        let store = CorpusStore::new(tmp.path());
        let backend = Arc::new(FakeBackend::new("sess-2", "sess-2", "answer"));
        // Call 0 = initial prime, call 1 = first query (fails with session-resume
        // text), call 2 = reprime, call 3 = retried query.
        backend.fail_call(
            1,
            KnowledgeAgentError::NonZeroExit {
                status: 1,
                stderr: "session expired".into(),
            },
        );
        let agent = KnowledgeAgent {
            backend: Box::new(BackendHandle(backend.clone())),
            renderer: CorpusRenderer::new(),
        };
        let mut corpus = fixture_corpus();
        agent.prime(&mut corpus, &store).await.unwrap();
        let result = agent.query(&mut corpus, &store, "what?").await.unwrap();
        assert_eq!(result.answer, "answer");
        assert_eq!(result.session_id, "sess-2");
        // 1 prime + 1 failed query + 1 reprime + 1 retried query = 4.
        assert_eq!(backend.call_count(), 4);
    }

    #[tokio::test]
    async fn query_rethrows_non_session_errors() {
        let tmp = TempDir::new().unwrap();
        let store = CorpusStore::new(tmp.path());
        let backend = Arc::new(FakeBackend::new("sess-3", "sess-3", "hi"));
        backend.fail_call(
            1,
            KnowledgeAgentError::Io(std::io::Error::other("network down")),
        );
        let agent = KnowledgeAgent {
            backend: Box::new(BackendHandle(backend.clone())),
            renderer: CorpusRenderer::new(),
        };
        let mut corpus = fixture_corpus();
        agent.prime(&mut corpus, &store).await.unwrap();
        let err = agent.query(&mut corpus, &store, "what?").await.unwrap_err();
        assert!(matches!(err, KnowledgeAgentError::Io(_)));
        // 1 prime + 1 failed query = 2 (no reprime, no retry).
        assert_eq!(backend.call_count(), 2);
    }

    #[tokio::test]
    async fn reprime_clears_session_then_primes_again() {
        let tmp = TempDir::new().unwrap();
        let store = CorpusStore::new(tmp.path());
        let backend = Arc::new(FakeBackend::new("sess-new", "sess-new", "ok"));
        let agent = KnowledgeAgent {
            backend: Box::new(BackendHandle(backend.clone())),
            renderer: CorpusRenderer::new(),
        };
        let mut corpus = fixture_corpus();
        corpus.session_id = Some("sess-old".into());
        let sid = agent.reprime(&mut corpus, &store).await.unwrap();
        assert_eq!(sid, "sess-new");
        // Prime call (no resume) — verify the first call carried `None`.
        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].1.is_none());
    }

    #[test]
    fn stream_json_frame_parser_extracts_session_and_text() {
        let mut inv = ClaudeInvocation::default();
        apply_stream_frame(
            &mut inv,
            r#"{"type":"system","session_id":"abc","subtype":"start"}"#,
        )
        .unwrap();
        apply_stream_frame(
            &mut inv,
            r#"{"type":"assistant","session_id":"abc","message":{"content":[{"type":"text","text":"hello "},{"type":"text","text":"world"}]}}"#,
        )
        .unwrap();
        apply_stream_frame(
            &mut inv,
            r#"{"type":"result","session_id":"abc","subtype":"final"}"#,
        )
        .unwrap();
        assert_eq!(inv.session_id.as_deref(), Some("abc"));
        assert_eq!(inv.assistant_text, "hello world");
    }

    #[test]
    fn stream_json_frame_parser_ignores_unknown_blocks() {
        let mut inv = ClaudeInvocation::default();
        apply_stream_frame(
            &mut inv,
            r#"{"type":"assistant","session_id":"abc","message":{"content":[{"type":"tool_use","id":"x"}]}}"#,
        )
        .unwrap();
        assert!(inv.assistant_text.is_empty());
    }

    #[test]
    fn session_resume_error_detection() {
        assert!(looks_like_session_resume_error("session expired"));
        assert!(looks_like_session_resume_error("Invalid session token"));
        assert!(looks_like_session_resume_error("could not resume"));
        assert!(looks_like_session_resume_error("session not found"));
        assert!(!looks_like_session_resume_error("network unavailable"));
        assert!(!looks_like_session_resume_error("rate limited"));
    }

    /// Adapter that lets us hand the same `Arc<FakeBackend>` to multiple
    /// places (the agent and the test assertions) without unsafe sharing.
    struct BackendHandle(Arc<FakeBackend>);

    #[async_trait::async_trait]
    impl ClaudeBackend for BackendHandle {
        async fn invoke(
            &self,
            prompt: &str,
            resume: Option<&str>,
            disallowed: &[&str],
        ) -> Result<ClaudeInvocation, KnowledgeAgentError> {
            self.0.invoke(prompt, resume, disallowed).await
        }
    }
}
