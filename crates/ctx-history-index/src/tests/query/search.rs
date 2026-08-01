use super::*;

#[test]
fn filtered_search_covers_relationship_and_public_metadata_contracts() {
    let temp = tempdir().unwrap();
    let codex_root = source("codex-root");
    let codex_child = source("codex-child");
    let claude = source_for_provider(
        "claude_code",
        "claude_projects_jsonl_tree",
        "claude-sessions",
    );

    let mut root = document_for_session(&codex_root, "root-thread", 1, "shared needle");
    root.workspace = Some("Ctx-Rich-Fixture".to_owned());
    root.cwd = Some("/work/ctx-root".to_owned());
    root.occurred_at_unix_ms = Some(100);
    let root_session_id = root.session_id;
    root.root_session_id = root_session_id;

    let mut child = document_for_session(&codex_child, "child-thread", 2, "shared needle");
    child.parent_session_id = Some(root_session_id);
    child.root_session_id = root_session_id;
    child.branch = Some("feature/query-seam".to_owned());
    child.workspace = Some("ChildSpace".to_owned());
    child.cwd = Some("/work/child".to_owned());
    child.agent_type = "subagent".to_owned();
    child.is_primary = false;
    child.event_type = "tool_call".to_owned();
    child.role = Some("assistant".to_owned());
    child.occurred_at_unix_ms = Some(200);
    let child_session_id = child.session_id;

    let mut other = document_for_session(&claude, "other-thread", 3, "shared needle");
    other.workspace = Some("Elsewhere".to_owned());
    other.branch = Some("release".to_owned());
    other.occurred_at_unix_ms = Some(300);
    let other_session_id = other.session_id;
    other.root_session_id = other_session_id;

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(codex_root.clone()).unwrap();
    writer.add_core_record(root).unwrap();
    writer
        .certify_source(certificate(&codex_root, 1, 1))
        .unwrap();
    writer.begin_source(codex_child.clone()).unwrap();
    writer.add_core_record(child).unwrap();
    writer
        .certify_source(certificate(&codex_child, 1, 1))
        .unwrap();
    writer.begin_source(claude.clone()).unwrap();
    writer.add_core_record(other).unwrap();
    writer.certify_source(certificate(&claude, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();

    let all = sorted_uuids(vec![
        root_session_id.as_uuid(),
        child_session_id.as_uuid(),
        other_session_id.as_uuid(),
    ]);
    assert_eq!(
        filtered_session_ids(&index, EventSearchFilters::default()),
        all
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                provider: Some("claude_code".to_owned()),
                ..EventSearchFilters::default()
            }
        ),
        vec![other_session_id.as_uuid()]
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                workspace: Some("RICH".to_owned()),
                ..EventSearchFilters::default()
            }
        ),
        vec![root_session_id.as_uuid()]
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                since_unix_ms: Some(250),
                ..EventSearchFilters::default()
            }
        ),
        vec![other_session_id.as_uuid()]
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                event_type: Some("tool_call".to_owned()),
                role: Some("assistant".to_owned()),
                agent_type: Some("subagent".to_owned()),
                ..EventSearchFilters::default()
            }
        ),
        vec![child_session_id.as_uuid()]
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                agent_scope: AgentScope::Primary,
                ..EventSearchFilters::default()
            }
        ),
        sorted_uuids(vec![root_session_id.as_uuid(), other_session_id.as_uuid()])
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                session_id: Some(child_session_id.as_uuid()),
                agent_scope: AgentScope::Primary,
                ..EventSearchFilters::default()
            }
        ),
        vec![child_session_id.as_uuid()]
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                parent_session_id: Some(root_session_id.as_uuid()),
                root_session_id: Some(root_session_id.as_uuid()),
                provider_session_id: Some("child-thread".to_owned()),
                branch: Some("feature/query-seam".to_owned()),
                ..EventSearchFilters::default()
            }
        ),
        vec![child_session_id.as_uuid()]
    );
    assert_eq!(
        filtered_session_ids(
            &index,
            EventSearchFilters {
                exclude_session_tree: Some(ExcludedSessionTree {
                    provider: "codex".to_owned(),
                    provider_session_id: "root-thread".to_owned(),
                    session_id: Some(root_session_id.as_uuid()),
                }),
                ..EventSearchFilters::default()
            }
        ),
        vec![other_session_id.as_uuid()]
    );

    let child = index
        .session_by_id(child_session_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(child.parent_session_id, Some(root_session_id));
    assert_eq!(child.root_session_id, root_session_id);
    assert_eq!(child.provider_session_id.as_deref(), Some("child-thread"));
    assert_eq!(child.branch.as_deref(), Some("feature/query-seam"));
    assert_eq!(child.agent_type, "subagent");
    assert!(!child.is_primary);
}

#[test]
fn complete_core_body_beyond_16k_round_trips_reopens_and_has_no_stored_preview() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let body = format!("{} tailonlyneedle", "界".repeat(16_384));
    let expected = document(&source, 1, &body);
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(expected.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let record = index
        .event_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert!(record.source.exact_descriptor_eq(&expected.source));
    let core_record = index
        .core_record_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        core_record.content.normalized_body.as_deref(),
        Some(body.as_str())
    );
    assert!(core_record.repository_bindings.is_empty());
    assert!(core_record.repository_abstentions.is_empty());
    let source_page = index.core_source_event_page(&source, None, 1).unwrap();
    let semantic_page = index.core_semantic_event_page(None, 1).unwrap();
    let session_events = index
        .core_events_for_session(expected.session_id.as_uuid())
        .unwrap();
    let bounded_session_events = index
        .core_events_for_session_if_bounded(expected.session_id.as_uuid(), 1)
        .unwrap()
        .unwrap();
    assert!(source_page.terminal);
    assert!(semantic_page.terminal);
    assert_eq!(semantic_page.eligible_total, 1);
    for record in [
        &source_page.items[0],
        &semantic_page.items[0],
        &session_events[0],
        &bounded_session_events[0],
    ] {
        assert_eq!(record.event_id, expected.event_id);
        assert_eq!(
            record.core_record.content.normalized_body.as_deref(),
            Some(body.as_str())
        );
    }
    assert_eq!(
        index.search_event_candidates("tailonlyneedle", 10).unwrap()[0]
            .event
            .event_id,
        expected.event_id
    );

    let fields = fields_from_schema(index.searcher.schema()).unwrap();
    assert!(index.searcher.schema().get_field("body_preview").is_err());
    assert!(index.searcher.schema().get_field("body").is_err());
    let address = index
        .searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let stored: TantivyDocument = index.searcher.doc(address).unwrap();
    assert!(stored.get_first(fields.body_search).is_none());
    assert!(stored.get_first(fields.core_record).is_some());

    drop(index);
    let reopened = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let reopened_records = reopened
        .core_events_for_session(expected.session_id.as_uuid())
        .unwrap();
    assert_eq!(
        reopened_records[0]
            .core_record
            .content
            .normalized_body
            .as_deref(),
        Some(body.as_str())
    );
}

#[test]
fn empty_or_invalid_programmatic_queries_are_safe() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "body"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();

    assert!(index.search_event_candidates("", 10).unwrap().is_empty());
    assert!(index.search_event_candidates("body", 0).unwrap().is_empty());
    assert!(matches!(
        index.search_event_candidates_with_filters(
            "body",
            &EventSearchFilters {
                provider: Some("  ".to_owned()),
                ..EventSearchFilters::default()
            },
            10,
        ),
        Err(IndexError::EmptyQueryFilter { field: "provider" })
    ));
    assert!(matches!(
        index.events_by_id_prefix("not-a-uuid"),
        Err(IndexError::InvalidIdPrefix)
    ));
    assert!(matches!(
        index.sessions_by_id_prefix(""),
        Err(IndexError::InvalidIdPrefix)
    ));
}
