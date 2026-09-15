use super::*;

fn numbered_events(count: usize) -> Vec<Value> {
    (0..count).map(|index| json!({ "index": index })).collect()
}

#[test]
fn delivery_observation_is_closed_bucketed_and_content_free() {
    let occurred_at = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:34:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let event = serialize_delivery_observation(
        AnalyticsDeliveryObservationV1::new(
            3,
            4,
            1,
            std::time::Duration::from_secs(7 * 60),
            AnalyticsDeliveryFailureClass::Transport,
        ),
        occurred_at,
    );
    let mut expected: Value = serde_json::from_str(include_str!(
        "../../../../../contracts/telemetry-v1/fixtures/analytics_delivery_observation.valid.json"
    ))
    .unwrap();
    expected["event_id"] = event["event_id"].clone();

    assert_eq!(event, expected);
    let diagnosed = serialize_delivery_observation(
        AnalyticsDeliveryObservationV1::new(
            3,
            4,
            1,
            std::time::Duration::from_secs(7 * 60),
            AnalyticsDeliveryFailureClass::Transport,
        )
        .with_failure_reason(Some(
            super::super::AnalyticsDeliveryFailureReason::RequestTimeout,
        )),
        occurred_at,
    );
    let mut fixture: Value = serde_json::from_str(include_str!(
        "../../../../../contracts/telemetry-v1/fixtures/analytics_delivery_reason.valid.json"
    ))
    .unwrap();
    fixture["event_id"] = diagnosed["event_id"].clone();
    assert_eq!(diagnosed, fixture);
    let encoded = event.to_string();
    for forbidden in ["endpoint", "response", "error_message", "path", "command"] {
        assert!(!encoded.contains(forbidden));
    }
}

#[test]
fn delivery_reason_wire_is_cli_outbox_closed_and_absent_on_recovery() {
    use super::super::AnalyticsDeliveryFailureReason as Reason;
    let at = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:34:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    for reason in [
        Reason::RequestDns,
        Reason::RequestConnect,
        Reason::RequestTimeout,
        Reason::RequestIo,
        Reason::ResponseStatus408,
        Reason::ResponseBodyTimeout,
        Reason::ResponseBodyIo,
        Reason::FileOpen,
        Reason::FileWrite,
        Reason::FileFlush,
        Reason::OutboxCorrupt,
        Reason::OutboxExpired,
        Reason::OutboxCapacity,
        Reason::OutboxClock,
        Reason::OutboxOversized,
    ] {
        for class in [
            AnalyticsDeliveryFailureClass::None,
            AnalyticsDeliveryFailureClass::Transport,
            AnalyticsDeliveryFailureClass::LocalIo,
            AnalyticsDeliveryFailureClass::Server,
            AnalyticsDeliveryFailureClass::RateLimited,
            AnalyticsDeliveryFailureClass::ClientRejection,
            AnalyticsDeliveryFailureClass::Configuration,
            AnalyticsDeliveryFailureClass::Unknown,
        ] {
            let mut observation =
                AnalyticsDeliveryObservationV1::new(0, 1, 0, std::time::Duration::ZERO, class);
            observation.failure_reason = Some(reason); // Exercise the serializer guard, not just the setter.
            let event = serialize_delivery_observation(observation, at);
            assert_eq!(event["surface"], "cli");
            assert_eq!(event["operation"], "outbox");
            assert_eq!(
                event["properties"]
                    .get("delivery_failure_reason")
                    .and_then(Value::as_str),
                reason.permits(class).then_some(reason.as_str())
            );
            assert_eq!(
                event["properties"].as_object().unwrap().len(),
                if reason.permits(class) { 6 } else { 5 }
            );
            if class == AnalyticsDeliveryFailureClass::None {
                assert_eq!(event["outcome"], "success");
            }
        }
    }
}

#[test]
fn outbound_payloads_never_exceed_fifty_events_and_preserve_order() {
    for (event_count, expected_chunk_sizes) in [
        (1, vec![1]),
        (49, vec![49]),
        (50, vec![50]),
        (51, vec![50, 1]),
        (100, vec![50, 50]),
        (101, vec![50, 50, 1]),
        (123, vec![50, 50, 23]),
    ] {
        let events = numbered_events(event_count);
        let mut payloads = Vec::new();

        post_event_chunks(
            &events,
            false,
            |chunk| {
                let body = serialize_batch_body("1.0.0", "client", "root", chunk)?;
                payloads.push(serde_json::from_slice::<Value>(&body)?);
                Ok(())
            },
            || panic!("a batch without a capability snapshot must not acknowledge one"),
        )
        .unwrap();

        assert_eq!(
            payloads
                .iter()
                .map(|payload| payload["events"].as_array().unwrap().len())
                .collect::<Vec<_>>(),
            expected_chunk_sizes
        );
        assert!(payloads.iter().all(|payload| {
            payload["events"].as_array().unwrap().len() <= MAX_EVENTS_PER_REQUEST
        }));
        assert_eq!(
            payloads
                .iter()
                .flat_map(|payload| payload["events"].as_array().unwrap())
                .map(|event| event["index"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            (0..event_count as u64).collect::<Vec<_>>()
        );
    }
}

#[test]
fn capability_ack_tracks_the_snapshot_bearing_chunk_not_later_chunks() {
    for (failure_on_post, expected_posts, expected_acks, should_succeed) in [
        (Some(1), 1, 0, false),
        (Some(2), 2, 1, false),
        (None, 3, 1, true),
    ] {
        let events = numbered_events(101);
        let mut posts = 0;
        let mut acknowledgements = 0;
        let result = post_event_chunks(
            &events,
            true,
            |_chunk| {
                posts += 1;
                if failure_on_post == Some(posts) {
                    return Err(anyhow::anyhow!("injected post failure"));
                }
                Ok(())
            },
            || {
                acknowledgements += 1;
                Ok(())
            },
        );

        assert_eq!(result.is_ok(), should_succeed);
        assert_eq!(posts, expected_posts);
        assert_eq!(acknowledgements, expected_acks);
    }
}
