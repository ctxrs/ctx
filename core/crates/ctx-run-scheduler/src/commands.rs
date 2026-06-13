use std::time::Instant;

use ctx_core::ids::MessageId;
use ctx_core::models::Message;
use ctx_session_tools::interrupt_telemetry::InterruptTelemetryContext;

#[derive(Debug)]
pub struct QueuedMessage {
    pub message: Message,
    pub enqueued_at: Instant,
    pub run_id: Option<String>,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum SchedulerCommand {
    Enqueue(QueuedMessage),
    RemoveQueued(MessageId),
    Cancel,
    Interrupt(InterruptTelemetryContext),
    StorageEmergency,
}
