use claude_mem_worker::agents::session_cleanup_helper::{
    cleanup_processed_messages, ActiveSessionState, ProcessingStatusBroadcaster,
};
use std::cell::Cell;

#[derive(Default)]
struct CountingWorker {
    calls: Cell<usize>,
}

impl ProcessingStatusBroadcaster for CountingWorker {
    fn broadcast_processing_status(&self) {
        self.calls.set(self.calls.get() + 1);
    }
}

#[test]
fn resets_earliest_pending_timestamp() {
    let mut session = ActiveSessionState {
        earliest_pending_timestamp: Some(1_700_000_000_000),
    };
    let worker = CountingWorker::default();

    cleanup_processed_messages(&mut session, Some(&worker));

    assert_eq!(session.earliest_pending_timestamp, None);
}

#[test]
fn resets_timestamp_when_already_empty() {
    let mut session = ActiveSessionState {
        earliest_pending_timestamp: None,
    };

    cleanup_processed_messages(&mut session, None);

    assert_eq!(session.earliest_pending_timestamp, None);
}

#[test]
fn broadcasts_processing_status_when_worker_is_present() {
    let mut session = ActiveSessionState {
        earliest_pending_timestamp: Some(1),
    };
    let worker = CountingWorker::default();

    cleanup_processed_messages(&mut session, Some(&worker));

    assert_eq!(worker.calls.get(), 1);
}

#[test]
fn handles_missing_worker_without_crashing() {
    let mut session = ActiveSessionState {
        earliest_pending_timestamp: Some(1),
    };

    cleanup_processed_messages(&mut session, None);

    assert_eq!(session.earliest_pending_timestamp, None);
}
