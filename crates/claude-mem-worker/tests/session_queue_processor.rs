use claude_mem_core::types::pending_message::PendingMessageRow;
use claude_mem_worker::queue::{QueueError, QueueRunOptions, QueueStore, SessionQueueProcessor};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

#[derive(Default)]
struct MockStore {
    claims: Mutex<VecDeque<Result<Option<PendingMessageRow>, QueueError>>>,
    call_count: Mutex<usize>,
}

impl MockStore {
    fn push(&self, value: Result<Option<PendingMessageRow>, QueueError>) {
        self.claims.lock().unwrap().push_back(value);
    }

    fn call_count(&self) -> usize {
        *self.call_count.lock().unwrap()
    }
}

impl QueueStore for MockStore {
    fn claim_next_message(
        &self,
        _session_db_id: i64,
    ) -> Result<Option<PendingMessageRow>, QueueError> {
        *self.call_count.lock().unwrap() += 1;
        Ok(self
            .claims
            .lock()
            .unwrap()
            .pop_front()
            .transpose()?
            .flatten())
    }
}

fn message(id: i64) -> PendingMessageRow {
    PendingMessageRow {
        id,
        session_db_id: 123,
        content_session_id: "content-session".to_owned(),
        message_type: "observation".to_owned(),
        tool_name: Some("Read".to_owned()),
        tool_input: Some(json!({ "file": "test.ts" })),
        tool_response: Some(json!({ "content": "file contents" })),
        cwd: Some("/test".to_owned()),
        last_user_message: None,
        last_assistant_message: Some("assistant".to_owned()),
        prompt_number: Some(5),
        status: "pending".to_owned(),
        retry_count: 0,
        created_at_epoch: 1_704_067_200_000,
        started_processing_at_epoch: None,
        completed_at_epoch: None,
        failed_at_epoch: None,
        agent_type: None,
        agent_id: None,
    }
}

fn options(
    session_db_id: i64,
    abort_rx: watch::Receiver<bool>,
    idle_timeout: Duration,
) -> QueueRunOptions {
    QueueRunOptions {
        session_db_id,
        abort: abort_rx,
        idle_timeout,
        error_backoff: Duration::from_millis(25),
    }
}

#[tokio::test]
async fn exits_immediately_when_aborted() {
    let store = Arc::new(MockStore::default());
    let processor = SessionQueueProcessor::new(Arc::clone(&store));
    let (abort_tx, abort_rx) = watch::channel(false);
    abort_tx.send(true).unwrap();

    let mut seen = Vec::new();
    let mut idle_calls = 0;
    processor
        .run(
            options(123, abort_rx, Duration::from_millis(50)),
            |message| seen.push(message),
            || idle_calls += 1,
        )
        .await;

    assert!(seen.is_empty());
    assert_eq!(idle_calls, 0);
    assert_eq!(store.call_count(), 0);
}

#[tokio::test]
async fn yields_claimed_message_with_persistent_metadata() {
    let store = Arc::new(MockStore::default());
    store.push(Ok(Some(message(42))));
    store.push(Ok(None));

    let processor = SessionQueueProcessor::new(Arc::clone(&store));
    let (abort_tx, abort_rx) = watch::channel(false);
    let mut seen = Vec::new();

    processor
        .run(
            options(123, abort_rx, Duration::from_millis(50)),
            |message| {
                seen.push(message);
                abort_tx.send(true).unwrap();
            },
            || {},
        )
        .await;

    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].persistent_id, 42);
    assert_eq!(seen[0].original_timestamp, 1_704_067_200_000);
    assert_eq!(seen[0].tool_name.as_deref(), Some("Read"));
    assert_eq!(seen[0].prompt_number, Some(5));
}

#[tokio::test]
async fn wakes_when_message_notification_arrives() {
    let store = Arc::new(MockStore::default());
    store.push(Ok(None));
    store.push(Ok(Some(message(1))));

    let processor = SessionQueueProcessor::new(Arc::clone(&store));
    let notify = processor.notifier();
    let (abort_tx, abort_rx) = watch::channel(false);
    let mut seen = Vec::new();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(25)).await;
        notify.notify_one();
    });

    processor
        .run(
            options(123, abort_rx, Duration::from_secs(1)),
            |message| {
                seen.push(message);
                abort_tx.send(true).unwrap();
            },
            || {},
        )
        .await;

    assert_eq!(seen.len(), 1);
    assert!(store.call_count() >= 2);
}

#[tokio::test]
async fn invokes_idle_timeout_callback_when_queue_stays_empty() {
    let store = Arc::new(MockStore::default());
    store.push(Ok(None));
    store.push(Ok(None));

    let processor = SessionQueueProcessor::new(store);
    let (_abort_tx, abort_rx) = watch::channel(false);
    let mut idle_calls = 0;

    processor
        .run(
            options(123, abort_rx, Duration::from_millis(30)),
            |_| {},
            || idle_calls += 1,
        )
        .await;

    assert_eq!(idle_calls, 1);
}

#[tokio::test]
async fn recovers_after_store_error_with_backoff() {
    let store = Arc::new(MockStore::default());
    store.push(Err(QueueError::new("database error")));
    store.push(Ok(Some(message(7))));

    let processor = SessionQueueProcessor::new(store);
    let (abort_tx, abort_rx) = watch::channel(false);
    let mut seen = Vec::new();

    processor
        .run(
            options(123, abort_rx, Duration::from_secs(1)),
            |message| {
                seen.push(message);
                abort_tx.send(true).unwrap();
            },
            || {},
        )
        .await;

    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].persistent_id, 7);
}
