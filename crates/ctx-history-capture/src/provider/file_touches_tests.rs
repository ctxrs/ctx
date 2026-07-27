use ctx_history_core::{EventRole, Fidelity};

use super::*;

#[test]
fn nested_high_cardinality_touches_stream_to_counting_sink() {
    const PATH_COUNT: usize = 4_096;
    let paths = (0..PATH_COUNT)
        .map(|index| {
            json!({
                "tool": "write_file",
                "nested": { "path": format!("src/generated/{index}.rs") },
            })
        })
        .chain(std::iter::once(json!({
            "tool": "write_file",
            "nested": { "path": "src/generated/0.rs" },
        })))
        .collect();
    let raw_value = Value::Array(paths);
    let event = ProviderEventEnvelope {
        provider_event_index: 17,
        provider_event_hash: None,
        cursor: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: "2026-07-18T00:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        idempotency_key: None,
        artifacts: Vec::new(),
        payload: Value::Null,
        metadata: Value::Null,
    };
    let mut count = 0;
    let mut first = None;
    let mut last = None;

    let emitted = visit_provider_file_touches_from_raw_value(
        ProviderFileTouchSourceContext::new(
            CaptureProvider::Claude,
            "streaming-session",
            "streaming-test-v1",
            Some("/tmp/streaming.jsonl"),
            Some("/tmp/streaming.jsonl"),
        ),
        &raw_value,
        &event,
        9,
        |(line, touch)| {
            assert_eq!(line, 9);
            if first.is_none() {
                first = Some((touch.provider_touch_index, touch.path.clone()));
            }
            last = Some((touch.provider_touch_index, touch.path));
            count += 1;
            Ok::<(), Infallible>(())
        },
    )
    .unwrap();

    assert_eq!(emitted.emitted(), PATH_COUNT);
    assert!(!emitted.limit_exceeded());
    assert_eq!(count, PATH_COUNT);
    assert_eq!(
        first,
        Some(((17_u64 << 16), "src/generated/0.rs".to_owned()))
    );
    assert_eq!(
        last,
        Some((
            (17_u64 << 16) | (PATH_COUNT as u64 - 1),
            format!("src/generated/{}.rs", PATH_COUNT - 1),
        ))
    );
}

#[test]
fn unique_touch_limit_prevents_sixteen_bit_identity_aliasing() {
    let paths = (0..=MAX_PROVIDER_FILE_TOUCHES_PER_EVENT)
        .map(|index| json!({ "path": format!(".p{index}") }))
        .collect();
    let raw_value = Value::Array(paths);
    let event = ProviderEventEnvelope {
        provider_event_index: 17,
        provider_event_hash: None,
        cursor: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: "2026-07-18T00:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        idempotency_key: None,
        artifacts: Vec::new(),
        payload: Value::Null,
        metadata: Value::Null,
    };
    let mut count = 0_usize;
    let mut last_touch_index = None;

    let result = visit_provider_file_touches_from_raw_value(
        ProviderFileTouchSourceContext::new(
            CaptureProvider::Claude,
            "streaming-session",
            "streaming-test-v1",
            None,
            None,
        ),
        &raw_value,
        &event,
        1,
        |(_, touch)| {
            count += 1;
            last_touch_index = Some(touch.provider_touch_index);
            Ok::<(), Infallible>(())
        },
    );

    let outcome = result.unwrap();
    assert!(outcome.limit_exceeded());
    assert_eq!(outcome.emitted(), MAX_PROVIDER_FILE_TOUCHES_PER_EVENT);
    assert_eq!(count, MAX_PROVIDER_FILE_TOUCHES_PER_EVENT);
    assert_eq!(
        last_touch_index,
        Some((17_u64 << 16) | (u64::try_from(MAX_PROVIDER_FILE_TOUCHES_PER_EVENT).unwrap() - 1))
    );
}

#[test]
fn production_collection_rejects_touch_that_would_alias_next_event() {
    let payload = Value::Array(
        (0..=MAX_PROVIDER_FILE_TOUCHES_PER_EVENT)
            .map(|index| json!({ "path": format!(".p{index}") }))
            .collect(),
    );
    let mut event = ProviderEventEnvelope {
        provider_event_index: 0,
        provider_event_hash: None,
        cursor: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: "2026-07-18T00:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        idempotency_key: None,
        artifacts: Vec::new(),
        payload,
        metadata: Value::Null,
    };

    let first_event = provider_file_touches_from_event(
        CaptureProvider::Claude,
        "bounded-session",
        "bounded-test-v1",
        None,
        None,
        &event,
        1,
    );
    let (first_event_touches, first_event_outcome) = first_event.into_parts();
    assert!(first_event_outcome.limit_exceeded());
    assert_eq!(
        first_event_outcome.emitted(),
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT
    );
    assert_eq!(
        first_event_touches
            .last()
            .map(|(_, touch)| touch.provider_touch_index),
        Some(u64::try_from(MAX_PROVIDER_FILE_TOUCHES_PER_EVENT).unwrap() - 1)
    );

    event.provider_event_index = 1;
    event.payload = json!({ "path": ".next-event" });
    let second_event = provider_file_touches_from_event(
        CaptureProvider::Claude,
        "bounded-session",
        "bounded-test-v1",
        None,
        None,
        &event,
        2,
    );
    let (second_event_touches, second_event_outcome) = second_event.into_parts();
    assert!(!second_event_outcome.limit_exceeded());
    assert_eq!(second_event_outcome.emitted(), 1);
    assert_eq!(second_event_touches[0].1.provider_touch_index, 1_u64 << 16);
    assert!(first_event_touches
        .iter()
        .all(|(_, touch)| touch.provider_touch_index != 1_u64 << 16));
}

#[test]
fn production_collection_preserves_packed_index_compatibility_at_boundary() {
    let event_with_index = |provider_event_index| ProviderEventEnvelope {
        provider_event_index,
        provider_event_hash: None,
        cursor: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: "2026-07-18T00:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        idempotency_key: None,
        artifacts: Vec::new(),
        payload: json!({ "path": ".bounded-index" }),
        metadata: Value::Null,
    };
    let collect = |event: &ProviderEventEnvelope| {
        provider_file_touches_from_event(
            CaptureProvider::Claude,
            "bounded-session",
            "bounded-test-v1",
            None,
            None,
            event,
            1,
        )
        .into_parts()
    };

    let (zero_touches, zero_outcome) = collect(&event_with_index(0));
    assert!(!zero_outcome.limit_exceeded());
    assert_eq!(zero_touches[0].1.provider_touch_index, 0);

    let (max_touches, max_outcome) = collect(&event_with_index(MAX_PACKED_PROVIDER_EVENT_INDEX));
    assert!(!max_outcome.limit_exceeded());
    assert_eq!(
        max_touches[0].1.provider_touch_index,
        MAX_PACKED_PROVIDER_EVENT_INDEX << 16
    );

    let (extended_touches, extended_outcome) = collect(&event_with_index(1_u64 << 48));
    assert!(!extended_outcome.limit_exceeded());
    assert_eq!(extended_outcome.emitted(), 1);
    assert_eq!(
        extended_touches[0].1.provider_event_index,
        Some(1_u64 << 48)
    );
    assert_eq!(extended_touches[0].1.provider_touch_index, 0);
}

#[test]
fn production_collection_preserves_full_hash_event_index_and_touch_ordinal() {
    let provider_event_index = 0xfedc_ba98_7654_3210;
    let event = ProviderEventEnvelope {
        provider_event_index,
        provider_event_hash: None,
        cursor: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: "2026-07-18T00:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        idempotency_key: None,
        artifacts: Vec::new(),
        payload: json!([
            { "path": ".openhands-first" },
            { "path": ".openhands-second" },
        ]),
        metadata: Value::Null,
    };

    let (touches, outcome) = provider_file_touches_from_event(
        CaptureProvider::OpenHands,
        "hash-indexed-session",
        "openhands-event-stream-v1",
        None,
        None,
        &event,
        1,
    )
    .into_parts();

    assert!(!outcome.limit_exceeded());
    assert_eq!(outcome.emitted(), 2);
    assert_eq!(
        touches[0].1.provider_event_index,
        Some(provider_event_index)
    );
    assert_eq!(touches[0].1.provider_touch_index, 0);
    assert_eq!(
        touches[1].1.provider_event_index,
        Some(provider_event_index)
    );
    assert_eq!(touches[1].1.provider_touch_index, 1);
}
