use super::*;
use ctx_history_jsonl::FallbackEventIdentityMode;

#[test]
fn source_and_related_session_identities_are_root_scoped() {
    let released = source_key("same-session").unwrap();
    let compatibility = source_key_scoped("same-session", SourceAnchorScope::Unqualified).unwrap();
    let first = source_key_scoped("same-session", SourceAnchorScope::Lineage([1; 32])).unwrap();
    let second = source_key_scoped("same-session", SourceAnchorScope::Lineage([2; 32])).unwrap();

    assert!(released.exact_descriptor_eq(&compatibility));
    assert_ne!(first.identity(), second.identity());
    assert_ne!(
        provider_session_identity("parent", SourceAnchorScope::Lineage([1; 32])).unwrap(),
        provider_session_identity("parent", SourceAnchorScope::Lineage([2; 32])).unwrap()
    );
}

#[derive(Clone)]
struct EmptyLookup;

impl BaseEventLookup for EmptyLookup {
    type Error = std::convert::Infallible;

    fn contains(&self, _event_id: uuid::Uuid) -> std::result::Result<bool, Self::Error> {
        Ok(false)
    }
}

fn binding() -> (SourceKey, Binding) {
    let source = source_key("session").unwrap();
    let session_id = session_identity(&source, "session").unwrap();
    (
        source,
        Binding {
            metadata_relative_path: PathBuf::from("meta.json"),
            provider_session_id: "session".to_owned(),
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            started_at_unix_ms: 0,
            cwd: None,
            branch: None,
            revision_digest: [0; 32],
        },
    )
}

fn fallback_identities(
    source: &SourceKey,
    binding: &Binding,
) -> FallbackEventIdentityState<EmptyLookup, CaptureError> {
    FallbackEventIdentityState::new(
        source.clone(),
        binding.session_id,
        LOGICAL_EVENT_KIND,
        "mistral-vibe.message.fallback",
        EVENT_IDENTITY_REVISION,
        FallbackEventIdentityMode::Cold,
        None,
    )
    .unwrap()
}

#[test]
fn native_parent_metadata_classifies_child_and_root_scope() {
    let (source, root_binding) = binding();
    let project = |binding: &Binding| {
        let mut fallback_identities = fallback_identities(&source, binding);
        let mut native_identities = MistralNativeIdentityTracker::default();
        let bytes = br#"{"role":"user","content":"Mistral scope fixture"}"#;
        core_record(
            &source,
            binding,
            &mut fallback_identities,
            &mut native_identities,
            JsonlRecordRef::for_test(bytes, 0),
        )
        .unwrap()
        .unwrap()
    };

    let root = project(&root_binding);
    assert_eq!(root.agent_scope, Some(AgentScope::Primary));

    let parent_session_id = session_identity(&source, "parent").unwrap();
    let child_binding = Binding {
        parent_session_id: Some(parent_session_id),
        root_session_id: parent_session_id,
        ..root_binding
    };
    let child = project(&child_binding);
    assert_eq!(child.agent_scope, Some(AgentScope::Subagent));
}

#[test]
fn tool_results_keep_native_statuses_statusless_activity_and_large_content() {
    let (source, binding) = binding();
    let mut fallback_identities = fallback_identities(&source, &binding);
    let mut native_identities = MistralNativeIdentityTracker::default();
    for (status, expected) in [
        (Some("success"), "success"),
        (Some("failure"), "failure"),
        (None, "unknown"),
    ] {
        let mut value = serde_json::json!({
            "role": "tool",
            "content": format!("complete-{expected}"),
            "tool_call_id": "call-1",
            "name": "write_file",
        });
        if let Some(status) = status {
            value["status"] = Value::String(status.to_owned());
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        let record = core_record(
            &source,
            &binding,
            &mut fallback_identities,
            &mut native_identities,
            JsonlRecordRef::for_test(&bytes, 0),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            record.content.meaningful_text(),
            format!("complete-{expected}")
        );
        assert_eq!(
            record
                .content
                .structured_content
                .as_ref()
                .and_then(|value| value.get("tool_call_id"))
                .and_then(Value::as_str),
            Some("call-1")
        );
        assert_eq!(
            record
                .content
                .activity
                .as_ref()
                .and_then(|activity| activity.result.as_ref())
                .and_then(|result| result.status.as_deref()),
            None
        );
    }

    let large = format!("{}tail", "x".repeat(9 * 1024 * 1024));
    let bytes = serde_json::to_vec(&serde_json::json!({
        "role": "tool",
        "content": large,
        "tool_call_id": "large",
    }))
    .unwrap();
    let record = core_record(
        &source,
        &binding,
        &mut fallback_identities,
        &mut native_identities,
        JsonlRecordRef::for_test(&bytes, 1),
    )
    .unwrap()
    .unwrap();
    assert_eq!(record.content.meaningful_text().len(), 9 * 1024 * 1024 + 4);
    assert!(record.content.meaningful_text().ends_with("tail"));

    let bytes = serde_json::to_vec(&serde_json::json!({
        "role": "tool",
        "content": "one",
        "output": "two",
    }))
    .unwrap();
    assert!(core_record(
        &source,
        &binding,
        &mut fallback_identities,
        &mut native_identities,
        JsonlRecordRef::for_test(&bytes, 2),
    )
    .is_err());
}

#[test]
fn composite_name_and_transport_metadata_always_abstain_without_changing_result_capture() {
    let (source, binding) = binding();
    let project = |transport: &str| {
        let mut fallback_identities = fallback_identities(&source, &binding);
        let mut native_identities = MistralNativeIdentityTracker::default();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "role": "tool",
            "content": "terminal result",
            "name": "docs_server_read_document",
            "tool_call_id": "call-exact",
            "status": "success",
            "tool_result": {
                "output": {
                    "ok": true,
                    "server": transport,
                    "tool": "read_document",
                },
                "cancelled": false,
            },
        }))
        .unwrap();
        core_record(
            &source,
            &binding,
            &mut fallback_identities,
            &mut native_identities,
            JsonlRecordRef::for_test(&bytes, 1),
        )
        .unwrap()
        .unwrap()
    };

    let url = project("https://mcp.example.test/mcp");
    let stdio = project("uvx mcp-server-filesystem /tmp");

    for record in [&url, &stdio] {
        assert_eq!(
            record
                .content
                .activity
                .as_ref()
                .and_then(|activity| activity.result.as_ref())
                .and_then(|result| result.status.as_deref()),
            None
        );
        assert_eq!(record.content.meaningful_text(), "terminal result");
        assert_eq!(record.event_type, EventType::ToolOutput.as_str());
        assert_eq!(record.parser_revision, PARSER_REVISION);
        let linkage = record.content.structured_content.as_ref().unwrap();
        assert_eq!(
            linkage.get("tool_call_id").and_then(Value::as_str),
            Some("call-exact")
        );
        assert_eq!(
            linkage.get("name").and_then(Value::as_str),
            Some("docs_server_read_document")
        );
        assert_eq!(
            linkage.get("status").and_then(Value::as_str),
            Some("success")
        );
    }

    assert_eq!(url.event_id, stdio.event_id);
    assert_ne!(
        url.content.structured_content,
        stdio.content.structured_content
    );
}
