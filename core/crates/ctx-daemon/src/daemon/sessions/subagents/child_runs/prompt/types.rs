use ctx_core::ids::RunId;
use ctx_core::models::Message;

pub(in crate::daemon) struct PersistedSubagentPrompt {
    pub(in crate::daemon) run_id: RunId,
    pub(in crate::daemon) saved_message: Message,
    pub(in crate::daemon) last_event_seq: i64,
}
