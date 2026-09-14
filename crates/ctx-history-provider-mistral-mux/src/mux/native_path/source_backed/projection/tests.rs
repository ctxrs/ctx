use super::*;
use std::path::Path;

#[derive(Clone)]
struct EmptyLookup;

impl ctx_history_capture_runtime::BaseEventLookup for EmptyLookup {
    type Error = std::convert::Infallible;

    fn contains(&self, _event_id: uuid::Uuid) -> std::result::Result<bool, Self::Error> {
        Ok(false)
    }
}

fn project_relationship_fixture(parent: Option<&str>) -> CoreRecord {
    project_lineage_fixture(parent, None)
}

fn project_lineage_fixture(parent: Option<&str>, root: Option<&str>) -> CoreRecord {
    let temp = tempfile::tempdir().unwrap();
    let provider_session_id = if parent.is_some() || root.is_some() {
        "mux-child"
    } else {
        "mux-root"
    };
    project_metadata_fixture(
        temp.path(),
        crate::mux::metadata::MuxBoundedSessionMetadata {
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: parent.map(str::to_owned),
            root_provider_session_id: root.map(str::to_owned),
            lineage_ambiguous: false,
            started_at: "2026-08-05T12:00:00Z".to_owned(),
            cwd: Some("/workspace/mux".to_owned()),
            model: Some("mux-test".to_owned()),
            metadata_revision: "mux-test-metadata-v1".to_owned(),
            metadata_failure: None,
        },
        [7; 32],
    )
}

fn project_metadata_fixture(
    authority_path: &Path,
    metadata: crate::mux::metadata::MuxBoundedSessionMetadata,
    source_revision_digest: [u8; 32],
) -> CoreRecord {
    let provider_session_id = metadata.provider_session_id.clone();
    let source = super::super::source_key(&provider_session_id).unwrap();
    let session_id = super::super::session_identity(&source, &provider_session_id).unwrap();
    let parent_session_id = metadata
        .parent_provider_session_id
        .as_deref()
        .map(|parent| {
            super::super::related_session_identity(
                parent,
                ctx_history_core::SourceAnchorScope::Unqualified,
            )
        })
        .transpose()
        .unwrap();
    let root_session_id = metadata
        .root_provider_session_id
        .as_deref()
        .map(|root| {
            super::super::related_session_identity(
                root,
                ctx_history_core::SourceAnchorScope::Unqualified,
            )
        })
        .transpose()
        .unwrap();
    let binding = MuxBinding {
        metadata,
        session_id,
        parent_session_id,
        root_session_id,
        primary_stream: MuxStreamKind::Chat,
        archive: None,
        chat: None,
        partial: None,
        metadata_file: None,
        source_revision_digest,
    };
    let authority = Arc::new(ProviderSourceRoot::open(authority_path).unwrap());
    let mut projector = MuxProjector::<EmptyLookup>::new(
        source,
        authority,
        binding,
        JsonlFamilyProjectionMode::Cold,
        None,
    )
    .unwrap();
    let value = serde_json::json!({
        "id": "mux-child-event",
        "workspaceId": provider_session_id,
        "role": "user",
        "createdAt": "2026-08-05T12:00:01Z",
        "parts": [{"type": "text", "text": "exact child-owned Mux event"}]
    });
    let bytes = serde_json::to_vec(&value).unwrap();
    let mut emitted = Vec::new();
    projector
        .project_record(
            MuxStreamKind::Chat,
            JsonlRecordRef::for_test(&bytes, 0),
            &mut |record| {
                emitted.push(record);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(emitted.len(), 1);
    emitted.pop().unwrap()
}

#[test]
fn optional_dynamic_tool_metadata_abstains_without_losing_valid_result_content() {
    let oversized = "x".repeat(64 * 1024 + 1);
    let invalid_id = serde_json::json!({
        "parts": [{
            "type": "dynamic-tool",
            "toolCallId": oversized,
            "toolName": "shell",
            "output": {"exact": true},
        }]
    });
    assert_eq!(mux_activity(&invalid_id, Vec::new()), None);

    let invalid_tool = serde_json::json!({
        "parts": [{
            "type": "dynamic-tool",
            "toolCallId": "call-1",
            "toolName": "x".repeat(64 * 1024 + 1),
            "output": {"exact": true},
        }]
    });
    let activity = mux_activity(&invalid_tool, Vec::new()).unwrap();
    assert!(activity.invocation.is_none());
    assert_eq!(
        activity.result.unwrap().structured_content,
        ActivityJsonCapture::Present {
            value: serde_json::json!({"exact": true}),
        }
    );
}

#[test]
fn delegated_tasks_are_unique_while_root_events_are_primary() {
    let child = project_relationship_fixture(Some("mux-parent"));
    assert_eq!(
        child.session_relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );
    assert_eq!(child.agent_scope, Some(AgentScope::Subagent));
    assert!(child.parent_session_id.is_some());
    assert_eq!(child.root_session_id, None);
    assert_eq!(
        child.content.meaningful_text(),
        "exact child-owned Mux event"
    );
    assert!(child.native_event_id.is_some());

    let explicit_root = project_lineage_fixture(Some("mux-parent"), Some("mux-explicit-root"));
    assert_eq!(explicit_root.parent_session_id, child.parent_session_id);
    assert!(explicit_root.root_session_id.is_some());

    let root = project_relationship_fixture(None);
    assert_eq!(root.session_relationship, None);
    assert_eq!(root.agent_scope, Some(AgentScope::Primary));
    assert_eq!(root.parent_session_id, None);
    assert_eq!(root.root_session_id, None);
    assert_eq!(
        root.content.meaningful_text(),
        "exact child-owned Mux event"
    );
    assert!(root.native_event_id.is_some());

    let unresolved_child = project_lineage_fixture(None, Some("mux-foreign-root"));
    assert_eq!(unresolved_child.session_relationship, None);
    assert_eq!(unresolved_child.agent_scope, None);
    assert!(unresolved_child.root_session_id.is_some());
}

#[test]
fn contradictory_lineage_aliases_omit_relationship_claim() {
    let temp = tempfile::tempdir().unwrap();
    let native = crate::mux::source::MuxSessionSource {
        session_dir: temp.path().join("mux-child"),
        archive_path: None,
        chat_path: None,
        partial_path: None,
        metadata_path: None,
        provider_session_id: "mux-child".to_owned(),
        parent_provider_session_id: None,
    };
    let metadata = crate::mux::metadata::mux_bounded_session_metadata_from_bytes(
        &native,
        "mux-test-metadata-v2",
        "2026-08-05T12:00:00Z".parse().unwrap(),
        Some(
            &serde_json::to_vec(&serde_json::json!({
                "workspaceId": "mux-child",
                "parentWorkspaceId": "mux-parent",
                "parentTaskId": "contradictory-parent",
                "rootWorkspaceId": "mux-parent",
                "rootTaskId": "contradictory-root"
            }))
            .unwrap(),
        ),
    )
    .unwrap();
    assert!(metadata.lineage_ambiguous);
    assert_eq!(
        metadata.parent_provider_session_id.as_deref(),
        Some("mux-parent")
    );
    assert_eq!(
        metadata.root_provider_session_id.as_deref(),
        Some("mux-parent")
    );

    let record = project_metadata_fixture(temp.path(), metadata, [8; 32]);
    assert_eq!(record.session_relationship, None);
    assert_eq!(record.agent_scope, None);
    assert_eq!(record.parent_session_id, None);
    assert_eq!(record.root_session_id, None);
}

#[test]
fn duplicate_lineage_keys_project_unknown_without_edges() {
    let temp = tempfile::tempdir().unwrap();
    let native = crate::mux::source::MuxSessionSource {
        session_dir: temp.path().join("mux-child"),
        archive_path: None,
        chat_path: None,
        partial_path: None,
        metadata_path: None,
        provider_session_id: "mux-child".to_owned(),
        parent_provider_session_id: None,
    };
    let metadata = crate::mux::metadata::mux_bounded_session_metadata_from_bytes(
        &native,
        "mux-test-metadata-v3",
        "2026-08-05T12:00:00Z".parse().unwrap(),
        Some(
            br#"{
                "workspaceId": "mux-child",
                "parentSessionId": "mux-parent",
                "parentSessionId": "conflicting-parent",
                "rootSessionId": "mux-root"
            }"#,
        ),
    )
    .unwrap();
    assert!(metadata.lineage_ambiguous);

    let record = project_metadata_fixture(temp.path(), metadata, [9; 32]);
    assert_eq!(record.session_relationship, None);
    assert_eq!(record.agent_scope, None);
    assert_eq!(record.parent_session_id, None);
    assert_eq!(record.root_session_id, None);
}

fn duplicate_then_depth_exhausted_metadata() -> Vec<u8> {
    let depth = 256;
    let mut raw = br#"{
        "workspaceId": "mux-child",
        "parentSessionId": "metadata-parent",
        "parentSessionId": "duplicate-parent",
        "unrelated":
    "#
    .to_vec();
    raw.extend(std::iter::repeat_n(b'[', depth));
    raw.extend_from_slice(b"null");
    raw.extend(std::iter::repeat_n(b']', depth));
    raw.push(b'}');
    raw
}

#[test]
fn failed_raw_lineage_audit_projects_unknown_with_or_without_path_parent() {
    let malformed = br#"{
        "workspaceId": "mux-child",
        "parentSessionId": "metadata-parent",
        "parentSessionId": "duplicate-parent",
        "unrelated":
    "#
    .to_vec();
    let depth_exhausted = duplicate_then_depth_exhausted_metadata();

    for (failure_kind, raw) in [
        ("malformed", malformed.as_slice()),
        ("depth-exhausted", depth_exhausted.as_slice()),
    ] {
        for path_parent in [None, Some("mux-path-parent")] {
            let temp = tempfile::tempdir().unwrap();
            let native = crate::mux::source::MuxSessionSource {
                session_dir: temp.path().join("mux-child"),
                archive_path: None,
                chat_path: None,
                partial_path: None,
                metadata_path: None,
                provider_session_id: "mux-child".to_owned(),
                parent_provider_session_id: path_parent.map(str::to_owned),
            };
            let metadata = crate::mux::metadata::mux_bounded_session_metadata_from_bytes(
                &native,
                &format!("mux-test-{failure_kind}"),
                "2026-08-05T12:00:00Z".parse().unwrap(),
                Some(raw),
            )
            .unwrap();
            assert!(metadata.lineage_ambiguous, "{failure_kind} {path_parent:?}");
            assert!(metadata.metadata_failure.is_some());
            assert_eq!(
                metadata.parent_provider_session_id.as_deref(),
                path_parent,
                "{failure_kind}"
            );

            let record = project_metadata_fixture(temp.path(), metadata, [10; 32]);
            assert_eq!(record.agent_scope, None, "{failure_kind} {path_parent:?}");
            assert_eq!(record.session_relationship, None);
            assert_eq!(record.parent_session_id, None);
            assert_eq!(record.root_session_id, None);
            assert_eq!(
                record.content.meaningful_text(),
                "exact child-owned Mux event"
            );
        }
    }
}

#[test]
fn provider_textual_result_over_16k_is_complete() {
    let tail = "mux_success_result_tail_complete";
    let output = format!("{} {tail}", "successful mux output ".repeat(800));
    assert!(output.len() > 16_000);
    let value = serde_json::json!({
        "role": "assistant",
        "parts": [{
            "type": "dynamic-tool",
            "toolName": "shell",
            "toolCallId": "complete-success",
            "state": "output-available",
            "output": output,
        }]
    });

    assert_eq!(mux_exact_logical_content(&value).unwrap(), output);
    assert!(mux_output_content_omission(&value, mux_output_projection(&value).as_ref()).is_none());
}

#[test]
fn explicit_redaction_has_truthful_omission_reason() {
    let value = serde_json::json!({
        "role": "assistant",
        "parts": [{
            "type": "dynamic-tool",
            "toolName": "shell",
            "toolCallId": "redacted",
            "state": "output-redacted",
        }]
    });
    assert_eq!(
        mux_output_content_omission(&value, mux_output_projection(&value).as_ref()),
        Some((
            "explicit_redaction",
            "Mux provider marked the tool output as redacted"
        ))
    );
}
