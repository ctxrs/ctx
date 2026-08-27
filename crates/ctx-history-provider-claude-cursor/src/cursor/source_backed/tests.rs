use super::super::layout::CursorTranscriptPath;
use super::*;
use ctx_history_core::EventRole;
use std::collections::BTreeSet;

fn cursor_record(timestamp: &str, text: &str) -> String {
    format!(
        concat!(
            r#"{{"timestamp":"{}","role":"user","#,
            r#""message":{{"content":[{{"type":"text","text":"{}"}}]}}}}"#,
            "\n"
        ),
        timestamp, text
    )
}

fn duplicate_cursor_routes(
    copies: &[(&str, Vec<String>)],
) -> (tempfile::TempDir, Vec<CursorTranscriptPath>) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("projects");
    for (project, records) in copies {
        let session = root
            .join(project)
            .join("agent-transcripts")
            .join("duplicate-session");
        fs::create_dir_all(&session).unwrap();
        fs::write(session.join("duplicate-session.jsonl"), records.concat()).unwrap();
    }
    let inventory = discover_cursor_transcripts(&root);
    assert!(inventory.completed, "{:#?}", inventory.issues);
    (temp, inventory.transcripts)
}

fn selected_project(
    routes: &[CursorTranscriptPath],
    selection: CursorTranscriptSelection,
) -> String {
    routes[selection.selected_index]
        .path()
        .strip_prefix(routes[selection.selected_index].authority().named_path())
        .unwrap()
        .components()
        .next()
        .unwrap()
        .as_os_str()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn cursor_duplicate_selection_collapses_equal_semantics_and_prefers_strict_extension() {
    let (_equal_temp, equal) = duplicate_cursor_routes(&[
        (
            "a-project",
            vec![cursor_record("2026-06-24T12:00:00Z", "same")],
        ),
        (
            "z-project",
            vec![cursor_record("2026-06-24T12:00:00Z", "same")],
        ),
    ]);
    let equal_selection = select_cursor_transcript(&equal).unwrap();
    assert!(equal_selection.divergent_indices.is_empty());
    assert_eq!(selected_project(&equal, equal_selection), "a-project");

    let (_malformed_temp, malformed) = duplicate_cursor_routes(&[
        (
            "a-project",
            vec![
                cursor_record("2026-06-24T12:00:00Z", "same"),
                "not json\n".to_owned(),
            ],
        ),
        (
            "z-project",
            vec![cursor_record("2026-06-24T12:00:00Z", "same")],
        ),
    ]);
    let malformed_selection = select_cursor_transcript(&malformed).unwrap();
    assert!(malformed_selection.divergent_indices.is_empty());
    assert_eq!(
        selected_project(&malformed, malformed_selection),
        "a-project"
    );

    let first = cursor_record("2026-06-24T12:00:00Z", "first");
    let (_extension_temp, extension) = duplicate_cursor_routes(&[
        ("a-project", vec![first.clone()]),
        (
            "z-project",
            vec![
                first,
                cursor_record("2026-06-24T12:01:00Z", "strict extension"),
            ],
        ),
    ]);
    let extension_selection = select_cursor_transcript(&extension).unwrap();
    assert!(extension_selection.divergent_indices.is_empty());
    assert_eq!(
        selected_project(&extension, extension_selection),
        "z-project"
    );
}

#[test]
fn cursor_duplicate_selection_authenticates_the_selected_file_observation() {
    let (_temp, routes) = duplicate_cursor_routes(&[
        (
            "a-project",
            vec![cursor_record("2026-06-24T12:00:00Z", "selected")],
        ),
        (
            "z-project",
            vec![cursor_record("2026-06-24T12:00:00Z", "other")],
        ),
    ]);
    let selection = select_cursor_transcript(&routes).unwrap();
    let selected = &routes[selection.selected_index];
    let source =
        source_key_scoped(selected.native_session_id(), SourceAnchorScope::Unqualified).unwrap();
    let binding = CursorBinding {
        native_session_id: selected.native_session_id().to_owned(),
        logical_transcript_sha256: Some(selection.selected_signature),
        selected_route_sha256: cursor_route_sha256(selected.path()),
        alias_route_sha256: routes
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != selection.selected_index)
            .map(|(_, route)| cursor_route_sha256(route.path()))
            .collect(),
    };

    fs::write(
        selected.path(),
        [
            cursor_record("2026-06-24T12:00:00Z", "selected"),
            cursor_record("2026-06-24T12:01:00Z", "changed after selection"),
        ]
        .concat(),
    )
    .unwrap();
    let leaf = ProviderJsonlLeaf::observe(
        source,
        selected.path().to_path_buf(),
        Arc::new(selected.authority().clone()),
        selected.authority_relative_path().to_path_buf(),
        TypedKey::bytes(serde_json::to_vec(&binding).unwrap()).unwrap(),
    )
    .unwrap();

    assert!(matches!(
        authenticate_selected_cursor_leaf(leaf, Some(&selection.selected_observation)),
        Err(CaptureError::SourceChangedDuringCapture)
    ));
}

#[test]
fn cursor_duplicate_selection_bounds_oversized_rejected_records() {
    let valid = cursor_record("2026-06-24T12:00:00Z", "same valid event");
    let oversized = format!("{}\n", "x".repeat(MAX_PROVIDER_JSONL_LINE_BYTES + 1));
    let (_temp, routes) = duplicate_cursor_routes(&[
        ("a-project", vec![oversized, valid.clone()]),
        ("z-project", vec![valid]),
    ]);

    let selection = select_cursor_transcript(&routes).unwrap();
    assert!(selection.divergent_indices.is_empty());
    assert_eq!(selected_project(&routes, selection), "a-project");
}

#[test]
fn cursor_divergent_selection_ranks_valid_events_then_timestamp_then_path() {
    let (_count_temp, count_ranked) = duplicate_cursor_routes(&[
        (
            "a-project",
            vec![cursor_record("2026-06-24T12:05:00Z", "one newer event")],
        ),
        (
            "z-project",
            vec![
                cursor_record("2026-06-24T12:00:00Z", "first divergent event"),
                cursor_record("2026-06-24T12:01:00Z", "second divergent event"),
            ],
        ),
    ]);
    let count_selection = select_cursor_transcript(&count_ranked).unwrap();
    assert_eq!(count_selection.divergent_indices, BTreeSet::from([0]));
    assert_eq!(
        selected_project(&count_ranked, count_selection),
        "z-project"
    );

    let (_time_temp, time_ranked) = duplicate_cursor_routes(&[
        (
            "a-project",
            vec![cursor_record("2026-06-24T12:00:00Z", "older copy")],
        ),
        (
            "z-project",
            vec![cursor_record("2026-06-24T12:01:00Z", "newer copy")],
        ),
    ]);
    let time_selection = select_cursor_transcript(&time_ranked).unwrap();
    assert_eq!(time_selection.divergent_indices, BTreeSet::from([0]));
    assert_eq!(selected_project(&time_ranked, time_selection), "z-project");

    let (_path_temp, path_ranked) = duplicate_cursor_routes(&[
        (
            "a-project",
            vec![cursor_record("2026-06-24T12:00:00Z", "copy a")],
        ),
        (
            "z-project",
            vec![cursor_record("2026-06-24T12:00:00Z", "copy z")],
        ),
    ]);
    let path_selection = select_cursor_transcript(&path_ranked).unwrap();
    assert_eq!(path_selection.divergent_indices, BTreeSet::from([1]));
    assert_eq!(selected_project(&path_ranked, path_selection), "a-project");
}

#[test]
fn source_and_session_identities_are_root_scoped() {
    let released = source_key("same-session").unwrap();
    let compatibility = source_key_scoped("same-session", SourceAnchorScope::Unqualified).unwrap();
    let first = source_key_scoped("same-session", SourceAnchorScope::Lineage([1; 32])).unwrap();
    let second = source_key_scoped("same-session", SourceAnchorScope::Lineage([2; 32])).unwrap();

    assert!(released.exact_descriptor_eq(&compatibility));
    assert_ne!(first.identity(), second.identity());
    assert_ne!(
        session_id(&first, "same-session").unwrap(),
        session_id(&second, "same-session").unwrap()
    );
}

fn event(event_type: EventType, body: CursorEventBody) -> CursorNativeEvent {
    CursorNativeEvent {
        native_order: super::super::projection::CursorNativeOrder {
            semantic_ordinal: 7,
            physical_ordinal: 11,
            part_ordinal: 0,
        },
        event_type,
        role: EventRole::Tool,
        occurred_at: None,
        body,
        record_byte_start: 0,
        record_byte_end_exclusive: 1,
        record_sha256: [0; 32],
        provider_event_hash: [1; 32],
    }
}

#[test]
fn activity_preserves_exact_cursor_channels_without_flattened_mcp_inference() {
    let invocation = cursor_annotation(&event(
        EventType::ToolCall,
        CursorEventBody::ToolCall {
            native_content: serde_json::json!({
                "type": "tool_use",
                "id": " call-1 ",
                "name": "mcp__forge__read",
                "input": {"command": "  git status  ", "path": "./a"},
            }),
            call_id: Some(" call-1 ".to_owned()),
            tool_name: Some("mcp__forge__read".to_owned()),
            arguments: Some(serde_json::json!({"command": "  git status  ", "path": "./a"})),
            protocol: None,
            server: None,
            explicit_tool: None,
            call_id_unavailable: false,
            tool_name_unavailable: false,
            arguments_unavailable: false,
            mcp_identity_unavailable: false,
            native_content_unavailable: false,
            literal_facts: Vec::new(),
        },
    ))
    .expect("invocation annotation");
    let invocation = invocation.activity.expect("invocation activity");
    assert_eq!(
        invocation.provider_call_id,
        Some(TypedKey::utf8(" call-1 ").expect("call id"))
    );
    let invocation = invocation.invocation.expect("invocation channel");
    assert_eq!(invocation.protocol, None);
    assert_eq!(invocation.server, None);
    assert_eq!(invocation.tool, "mcp__forge__read");
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: serde_json::json!({"command": "  git status  ", "path": "./a"}),
        }
    );

    let result_value = serde_json::json!({
        "type": "tool_result",
        "tool_use_id": " call-1 ",
        "content": [" exact ", {"ok": false}],
    });
    let result = cursor_annotation(&event(
        EventType::ToolOutput,
        CursorEventBody::ToolOutput {
            native_content: result_value.clone(),
            call_id: Some(" call-1 ".to_owned()),
            call_id_unavailable: false,
            content_unavailable: false,
            native_content_unavailable: false,
            literal_facts: Vec::new(),
        },
    ))
    .expect("result annotation");
    let result = result.activity.expect("result activity");
    assert_eq!(
        result.provider_call_id,
        Some(TypedKey::utf8(" call-1 ").expect("call id"))
    );
    let result = result.result.expect("result channel");
    assert_eq!(result.status, None);
    assert_eq!(result.text, ActivityTextCapture::Absent);
    assert_eq!(
        result.structured_content,
        ActivityJsonCapture::Present {
            value: result_value,
        }
    );
}

#[test]
fn cursor_parser_preserves_result_content_and_explicit_capture_states() {
    let exact = br#"{"type":"message","role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":" exact text ","unknown":{"kept":true}}]}}"#;
    let events = project_cursor_jsonl_record(exact, 0, 0, 0, exact.len() as u64)
        .unwrap()
        .unwrap();
    let annotation = cursor_annotation(&events[0]).unwrap();
    assert_eq!(
        annotation.structured_content,
        Some(serde_json::json!({
            "type":"tool_result",
            "tool_use_id":"call",
            "content":" exact text ",
            "unknown":{"kept":true},
        }))
    );
    let result = annotation.activity.unwrap().result.unwrap();
    assert_eq!(
        result.text,
        ActivityTextCapture::Present {
            value: " exact text ".to_owned(),
        }
    );

    let absent = br#"{"type":"message","role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call"}]}}"#;
    let events = project_cursor_jsonl_record(absent, 0, 0, 0, absent.len() as u64)
        .unwrap()
        .unwrap();
    let result = cursor_annotation(&events[0])
        .unwrap()
        .activity
        .unwrap()
        .result
        .unwrap();
    assert_eq!(result.text, ActivityTextCapture::Absent);
}

#[test]
fn cursor_duplicate_selectors_withhold_channels_and_facts_keep_raw_order() {
    let duplicate = br#"{"type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"tool","input":{"path":"one"},"input":{"path":"two"}}]}}"#;
    let events = project_cursor_jsonl_record(duplicate, 0, 0, 0, duplicate.len() as u64)
        .unwrap()
        .unwrap();
    let annotation = cursor_annotation(&events[0]).unwrap();
    assert!(annotation.structured_content.is_none());
    let invocation = annotation.activity.unwrap().invocation.unwrap();
    assert_eq!(invocation.arguments, ActivityJsonCapture::Unavailable);

    let ordered = br#"{"type":"message","role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"tool","input":{"command":" c ","path":" p ","url":" u "}}]}}"#;
    let events = project_cursor_jsonl_record(ordered, 0, 0, 0, ordered.len() as u64)
        .unwrap()
        .unwrap();
    let activity = cursor_annotation(&events[0]).unwrap().activity.unwrap();
    assert_eq!(
        activity
            .facts
            .iter()
            .map(|fact| (fact.kind, fact.value.as_str()))
            .collect::<Vec<_>>(),
        [
            (ctx_history_core::LiteralFactKind::Command, " c "),
            (ctx_history_core::LiteralFactKind::File, " p "),
            (ctx_history_core::LiteralFactKind::Url, " u "),
        ]
    );
}
