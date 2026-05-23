//! Session state cleanup after response processing.
//!
//! Port of `src/services/worker/agents/SessionCleanupHelper.ts`.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActiveSessionState {
    pub earliest_pending_timestamp: Option<i64>,
}

pub trait ProcessingStatusBroadcaster {
    fn broadcast_processing_status(&self);
}

pub fn cleanup_processed_messages(
    session: &mut ActiveSessionState,
    worker: Option<&dyn ProcessingStatusBroadcaster>,
) {
    session.earliest_pending_timestamp = None;

    if let Some(worker) = worker {
        worker.broadcast_processing_status();
    }
}
