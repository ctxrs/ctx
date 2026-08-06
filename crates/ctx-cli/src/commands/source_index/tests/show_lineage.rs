use super::*;

#[test]
fn machine_show_contract_keeps_lineage_in_existing_nested_fields() {
    let mut direct = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 51, 1);
    let parent = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 52, 1);
    let root = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 53, 1);
    direct.parent_session_id = Some(parent.session_id);
    direct.root_session_id = root.session_id;
    direct.agent_type = "subagent".to_owned();
    direct.is_primary = false;
    direct.session_relationship = SessionRelationshipKind::Delegated;
    let event = fixture_core_event(&direct, "nested lineage event");
    let session = SessionRecord::from(&direct);

    let session_value = session_transcript_value(
        &session,
        TranscriptMode::Log,
        OutputFormat::Json,
        vec![render_event_value(&event)],
        false,
        None,
    );
    assert_eq!(
        sorted_json_keys(&session_value),
        vec![
            "ctx_session_id",
            "events",
            "format",
            "mode",
            "payload_type",
            "provider",
            "provider_session_id",
            "schema_version",
            "session",
            "target",
        ]
    );
    assert!(session_value.get("parent_ctx_session_id").is_none());
    assert!(session_value.get("root_ctx_session_id").is_none());
    assert_eq!(
        session_value["session"]["parent_ctx_session_id"],
        parent.session_id.as_uuid().to_string()
    );
    assert_eq!(
        session_value["session"]["root_ctx_session_id"],
        root.session_id.as_uuid().to_string()
    );
    assert_eq!(session_value["session"]["session_relationship"], "delegated");

    let event_value =
        event_window_value(&event, OutputFormat::Json, vec![render_event_value(&event)]).unwrap();
    assert_eq!(
        sorted_json_keys(&event_value),
        vec![
            "ctx_event_id",
            "ctx_session_id",
            "event",
            "events",
            "format",
            "payload_type",
            "schema_version",
            "target",
        ]
    );
    assert!(event_value.get("parent_ctx_session_id").is_none());
    assert!(event_value.get("root_ctx_session_id").is_none());
    assert_eq!(
        event_value["event"]["parent_ctx_session_id"],
        parent.session_id.as_uuid().to_string()
    );
    assert_eq!(
        event_value["event"]["root_ctx_session_id"],
        root.session_id.as_uuid().to_string()
    );
    assert_eq!(event_value["event"]["session_relationship"], "delegated");
    assert_eq!(event_value["event"]["event_origin"]["kind"], "unknown");
}

#[test]
fn copied_event_show_list_and_search_models_share_typed_lineage() {
    let ancestor = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 54, 1);
    let mut copied = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 55, 1);
    copied.parent_session_id = Some(ancestor.session_id);
    copied.root_session_id = ancestor.session_id;
    copied.session_relationship = SessionRelationshipKind::Forked;
    copied.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(ancestor.session_id),
        ancestor_event_id: Box::new(ancestor.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };
    let copied = fixture_core_event(&copied, "copied body remains directly visible");

    let shown = render_event_value(&copied);
    let listed = crate::commands::list::events::render_event(
        &copied,
        crate::commands::list::events::EventContentProjection::Full,
    )
    .unwrap();
    let search = SearchEventMetadata::from(&copied.event);

    for value in [&shown, &listed] {
        assert_eq!(value["session_relationship"], "forked");
        assert_eq!(value["event_origin"]["kind"], "copied_from_ancestor");
        assert_eq!(
            value["event_origin"]["ancestor_event_id"],
            ancestor.event_id.as_uuid().to_string()
        );
        assert_eq!(
            value["event_origin"]["ancestor_session_id"],
            ancestor.session_id.as_uuid().to_string()
        );
        assert_eq!(value["event_origin"]["proof"], "native_event_identity");
        assert_eq!(value["text"], "copied body remains directly visible");
    }
    assert_eq!(search.session_relationship, SessionRelationshipKind::Forked);
    assert_eq!(search.event_origin, copied.event_origin);

    let rendered = render_show_document(
        &event_window_value(&copied, OutputFormat::Json, vec![shown]).unwrap(),
        &RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
    )
    .render(&RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)));
    assert!(rendered.contains("Relationship"));
    assert!(rendered.contains("forked"));
    assert!(rendered.contains("Original event"));
    assert!(rendered.contains(&ancestor.event_id.as_uuid().to_string()));
    assert!(rendered.contains("copied body remains directly visible"));
}
