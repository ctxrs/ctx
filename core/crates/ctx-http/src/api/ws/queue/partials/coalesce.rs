use super::super::super::*;

fn is_partial_event(event: &SessionEvent) -> bool {
    matches!(
        event.event_type,
        SessionEventType::AssistantChunk | SessionEventType::ThoughtChunk
    )
}

fn is_same_partial_type(prev: &SessionEvent, next: &SessionEvent) -> bool {
    matches!(
        (&prev.event_type, &next.event_type),
        (
            &SessionEventType::AssistantChunk,
            &SessionEventType::AssistantChunk
        ) | (
            &SessionEventType::ThoughtChunk,
            &SessionEventType::ThoughtChunk
        )
    )
}

fn extract_fragment(event: &SessionEvent) -> Option<&str> {
    event
        .payload_json
        .get("content_fragment")
        .and_then(Value::as_str)
}

pub(in crate::api::ws::queue) fn merge_partial_fragment(prev: &str, next: &str) -> String {
    if prev.is_empty() {
        return next.to_string();
    }
    if next.is_empty() {
        return prev.to_string();
    }
    if next.starts_with(prev) {
        return next.to_string();
    }
    if prev.ends_with(next) {
        return prev.to_string();
    }
    format!("{prev}{next}")
}

pub(in crate::api::ws::queue) fn try_coalesce_partial_delta_tail(
    prev: &mut SessionHeadDelta,
    next: &SessionHeadDelta,
) -> bool {
    if prev.turn.is_some()
        || prev.message.is_some()
        || next.turn.is_some()
        || next.message.is_some()
    {
        return false;
    }
    let (Some(prev_event), Some(next_event)) = (prev.event.as_ref(), next.event.as_ref()) else {
        return false;
    };
    if !is_partial_event(prev_event) || !is_partial_event(next_event) {
        return false;
    }
    if !is_same_partial_type(prev_event, next_event) {
        return false;
    }
    if prev_event.turn_id != next_event.turn_id || prev_event.turn_id.is_none() {
        return false;
    }
    let (Some(prev_fragment), Some(next_fragment)) =
        (extract_fragment(prev_event), extract_fragment(next_event))
    else {
        return false;
    };
    let merged_fragment = merge_partial_fragment(prev_fragment, next_fragment);
    let mut merged_event = next_event.clone();
    match merged_event.payload_json {
        Value::Object(ref mut map) => {
            map.insert(
                "content_fragment".to_string(),
                Value::String(merged_fragment),
            );
        }
        _ => return false,
    }
    prev.event = Some(merged_event);
    prev.last_event_seq = prev.last_event_seq.max(next.last_event_seq);
    prev.projection_rev = prev.projection_rev.max(next.projection_rev);
    prev.state_rev = prev.state_rev.max(next.state_rev);
    true
}

#[cfg(test)]
mod tests {
    use super::merge_partial_fragment;

    #[test]
    fn merge_partial_fragment_keeps_longer_overlapping_suffix() {
        assert_eq!(
            merge_partial_fragment("hello", "hello world"),
            "hello world"
        );
        assert_eq!(
            merge_partial_fragment("hello world", "world"),
            "hello world"
        );
    }
}
