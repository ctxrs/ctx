use ctx_history_index::CoreEventRecord;
use serde_json::Value;

use super::{EventContentProjection, EventQueryError};

pub fn render_event(
    event: &CoreEventRecord,
    projection: EventContentProjection,
) -> std::result::Result<Value, EventQueryError> {
    ctx_history_read_application::render_event_read_model(event, projection, &Default::default())
        .map_err(Into::into)
}
