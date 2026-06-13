mod event;

use axum::http::StatusCode;
use ctx_observability::telemetry::TelemetryEvent;
use serde::Deserialize;

use self::event::SemanticTelemetryEventReq;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::api) struct SemanticTelemetryBatch {
    events: Vec<SemanticTelemetryEventReq>,
}

impl SemanticTelemetryBatch {
    pub(super) fn event_count(&self) -> usize {
        self.events.len()
    }

    pub(super) fn into_events(self) -> Result<Vec<TelemetryEvent>, StatusCode> {
        let mut events = Vec::with_capacity(self.events.len());
        for event in self.events {
            events.push(event.into_event()?);
        }
        Ok(events)
    }
}
