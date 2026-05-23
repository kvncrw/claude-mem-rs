//! Per-session pending-message queue processor.
//!
//! Port of `src/services/queue/SessionQueueProcessor.ts`. The TypeScript
//! implementation exposes an async iterator; the Rust port exposes a
//! callback-driven async loop over the same claim/wait/abort lifecycle.

use claude_mem_core::types::pending_message::PendingMessageRow;
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{watch, Notify};

pub const IDLE_TIMEOUT: Duration = Duration::from_secs(3 * 60);
const ERROR_BACKOFF: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq)]
pub struct PendingMessageWithId {
    pub persistent_id: i64,
    pub original_timestamp: i64,
    pub message_type: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub tool_response: Option<Value>,
    pub prompt_number: Option<i64>,
    pub cwd: Option<String>,
    pub last_assistant_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueError {
    message: String,
}

impl QueueError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QueueError {}

impl From<rusqlite::Error> for QueueError {
    fn from(value: rusqlite::Error) -> Self {
        Self::new(value.to_string())
    }
}

pub trait QueueStore {
    fn claim_next_message(
        &self,
        session_db_id: i64,
    ) -> Result<Option<PendingMessageRow>, QueueError>;
}

impl<T> QueueStore for Arc<T>
where
    T: QueueStore + ?Sized,
{
    fn claim_next_message(
        &self,
        session_db_id: i64,
    ) -> Result<Option<PendingMessageRow>, QueueError> {
        (**self).claim_next_message(session_db_id)
    }
}

#[derive(Debug, Clone)]
pub struct QueueRunOptions {
    pub session_db_id: i64,
    pub abort: watch::Receiver<bool>,
    pub idle_timeout: Duration,
    pub error_backoff: Duration,
}

impl QueueRunOptions {
    pub fn new(session_db_id: i64, abort: watch::Receiver<bool>) -> Self {
        Self {
            session_db_id,
            abort,
            idle_timeout: IDLE_TIMEOUT,
            error_backoff: ERROR_BACKOFF,
        }
    }
}

pub struct SessionQueueProcessor<S> {
    store: S,
    notify: Arc<Notify>,
}

impl<S> SessionQueueProcessor<S>
where
    S: QueueStore,
{
    pub fn new(store: S) -> Self {
        Self {
            store,
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn notifier(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    pub fn notify_message(&self) {
        self.notify.notify_one();
    }

    pub async fn run<F, I>(
        &self,
        mut options: QueueRunOptions,
        mut on_message: F,
        mut on_idle_timeout: I,
    ) where
        F: FnMut(PendingMessageWithId),
        I: FnMut(),
    {
        let mut last_activity = Instant::now();

        while !*options.abort.borrow() {
            match self.store.claim_next_message(options.session_db_id) {
                Ok(Some(message)) => {
                    last_activity = Instant::now();
                    on_message(to_pending_message_with_id(message));
                }
                Ok(None) => {
                    let received_message =
                        wait_for_message(&self.notify, &mut options.abort, options.idle_timeout)
                            .await;

                    if !received_message && !*options.abort.borrow() {
                        if last_activity.elapsed() >= options.idle_timeout {
                            on_idle_timeout();
                            return;
                        }
                        last_activity = Instant::now();
                    }
                }
                Err(_) => {
                    if *options.abort.borrow() {
                        return;
                    }
                    wait_for_abort_or_sleep(&mut options.abort, options.error_backoff).await;
                }
            }
        }
    }
}

pub fn to_pending_message_with_id(msg: PendingMessageRow) -> PendingMessageWithId {
    PendingMessageWithId {
        persistent_id: msg.id,
        original_timestamp: msg.created_at_epoch,
        message_type: msg.message_type,
        tool_name: msg.tool_name,
        tool_input: msg.tool_input,
        tool_response: msg.tool_response,
        prompt_number: msg.prompt_number,
        cwd: msg.cwd,
        last_assistant_message: msg.last_assistant_message,
    }
}

async fn wait_for_message(
    notify: &Notify,
    abort: &mut watch::Receiver<bool>,
    timeout: Duration,
) -> bool {
    tokio::select! {
        _ = notify.notified() => true,
        _ = abort.changed() => false,
        _ = tokio::time::sleep(timeout) => false,
    }
}

async fn wait_for_abort_or_sleep(abort: &mut watch::Receiver<bool>, duration: Duration) {
    tokio::select! {
        _ = abort.changed() => {},
        _ = tokio::time::sleep(duration) => {},
    }
}
