use super::*;

fn complete_activity() -> CoreActivity {
    CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8("native-call-呼び出し-🦀").unwrap()),
        invocation: Some(ActivityInvocation {
            protocol: Some("mcp".to_owned()),
            server: Some("source-サーバー".to_owned()),
            tool: "検索-tool".to_owned(),
            arguments: ActivityJsonCapture::Present {
                value: json!({
                    "snake_key": ["雪", null, {"camelKey": true}],
                    "nested": {"deep_null": null},
                }),
            },
            started_at_unix_ms: Some(101),
        }),
        result: Some(ActivityResult {
            status: Some("provider::ok".to_owned()),
            completed_at_unix_ms: Some(202),
            duration_ns: Some(303),
            text: ActivityTextCapture::NormalizedBody,
            structured_content: ActivityJsonCapture::Present {
                value: json!({"result_key": ["完了", null, {"mixedCase": [false, 3]}]}),
            },
        }),
        facts: [
            (LiteralFactKind::File, "src/lib.rs"),
            (LiteralFactKind::Branch, "Feature/MixedCase"),
            (LiteralFactKind::File, "src/lib.rs"),
        ]
        .into_iter()
        .map(|(kind, value)| ProviderDeclaredFact {
            kind,
            value: value.to_owned(),
        })
        .collect(),
    }
}

#[test]
fn show_and_list_preserve_complete_activity_exactly() {
    let temp = tempdir().unwrap();
    write_test_generation(temp.path());
    let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 94, 1);
    let mut stored = fixture_core_event(&event, "normalized response body");
    stored.core_record.content.activity = Some(complete_activity());
    stored.core_record.validate_contract().unwrap();
    append_fixture_session(temp.path(), std::slice::from_ref(&stored), 94);

    let exact =
        serde_json::to_value(stored.core_record.content.activity.as_ref().unwrap()).unwrap();
    let rendered = render_event_value(&stored);
    assert_eq!(rendered["activity"], exact);
    assert_eq!(rendered["activity"]["facts"].as_array().unwrap().len(), 3);
    assert_eq!(
        rendered["activity"]["facts"][0],
        rendered["activity"]["facts"][2]
    );

    let listed =
        crate::list_events::render_event(&stored, crate::list_events::EventContentProjection::Full)
            .unwrap();
    assert_eq!(listed["activity"], exact);
    let compact =
        crate::list_events::render_event(&stored, crate::list_events::EventContentProjection::None)
            .unwrap();
    assert!(compact.get("activity").is_none());

    let shown = mcp_fixture_show_event(temp.path(), &stored);
    assert_eq!(shown["event"]["activity"], exact);
    let session = SessionRecord::from(&stored.event);
    let shown_session = mcp_show_session(
        temp.path(),
        &session.session_id.as_uuid().to_string(),
        TranscriptMode::Log,
        10,
        None,
        crate::presentation_limit::CLI_PRESENTATION_MAX_OUTPUT_BYTES,
    )
    .unwrap();
    assert_eq!(shown_session["events"][0]["activity"], exact);
}

#[test]
fn absent_activity_is_omitted() {
    let event = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 95, 1),
        "no activity",
    );
    assert!(render_event_value(&event).get("activity").is_none());
}

#[test]
fn show_and_list_preserve_structured_nulls_exactly() {
    for payload in [
        json!([null, {"value": null}, [null, {"nested": null}]]),
        json!({"argument": null, "nested": {"unset": null},
            "items": [null, {"value": null}], "snake_key": 1, "snakeKey": 2}),
    ] {
        let temp = tempdir().unwrap();
        write_test_generation(temp.path());
        let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 96, 1);
        let mut stored = fixture_core_event(&event, "structured response");
        stored.core_record.content.structured_content = Some(payload.clone());
        stored.core_record.content.activity = Some(complete_activity());
        stored.core_record.validate_contract().unwrap();
        append_fixture_session(temp.path(), std::slice::from_ref(&stored), 96);
        let activity = serde_json::to_value(complete_activity()).unwrap();

        let rendered = render_event_value(&stored);
        assert!(rendered.get("parent_ctx_session_id").is_none());
        assert!(rendered.get("occurred_at").is_none());
        let listed = crate::list_events::render_event(
            &stored,
            crate::list_events::EventContentProjection::Full,
        )
        .unwrap();
        let shown = mcp_fixture_show_event(temp.path(), &stored);
        let session = mcp_show_session(
            temp.path(),
            &stored.session_id.as_uuid().to_string(),
            TranscriptMode::Log,
            10,
            None,
            crate::presentation_limit::CLI_PRESENTATION_MAX_OUTPUT_BYTES,
        )
        .unwrap();
        for value in [
            &rendered,
            &listed,
            &shown["event"],
            &shown["events"][0],
            &session["events"][0],
        ] {
            assert_eq!(value.get("structured_content"), Some(&payload));
            assert_eq!(value["activity"], activity);
            assert_eq!(value["content"]["complete"], true);
        }

        let mut buffered = Vec::new();
        super::super::render::write_show_value(
            session,
            OutputFormat::Jsonl,
            None,
            stored.event_id.as_uuid(),
            &mut buffered,
        )
        .unwrap();
        let (mut ui, stdout) = test_ui();
        stream_cli_session(
            temp.path(),
            Some(stored.session_id.to_string()),
            None,
            None,
            None,
            None,
            TranscriptMode::Log,
            OutputFormat::Jsonl,
            None,
            None,
            &mut ui,
        )
        .unwrap();
        ui.flush().unwrap();
        for bytes in [buffered, stdout.bytes()] {
            let lines = bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(lines[0]["payload_type"], "session_transcript_event");
            assert_eq!(lines[0]["event"]["structured_content"], payload);
            assert_eq!(lines[0]["event"]["activity"], activity);
            assert_eq!(lines[0]["event"]["content"]["complete"], true);
        }
    }
}

#[test]
fn show_renderer_distinguishes_explicit_null_from_absent_content() {
    let mut absent = fixture_core_event(
        &fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 97, 1),
        "no structured content",
    );
    absent.core_record.content.structured_content = None;
    assert!(render_event_value(&absent)
        .get("structured_content")
        .is_none());
    // This tests renderer input only, not a stored round trip.
    absent.core_record.content.structured_content = Some(Value::Null);
    assert_eq!(
        render_event_value(&absent).get("structured_content"),
        Some(&Value::Null)
    );
}
