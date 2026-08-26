use super::*;

#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [
        include_str!("../source_backed.rs"),
        include_str!("document.rs"),
    ];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("body = lexical_body"));
    assert!(production.contains("let body = event.body.clone()"));
    for removed_api in [
        concat!("Lexical", "Document"),
        concat!("SourceRecord", "Locator"),
        concat!("hyd", "rate_"),
        concat!("resol", "ver"),
    ] {
        assert!(!production.contains(removed_api), "found {removed_api}");
    }
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
}

#[test]
fn direct_parent_claim_is_conservative_and_does_not_require_a_present_target() {
    let source = rovodev_source_key("child").unwrap();
    let child = rovodev_session_identity(&source, "child").unwrap();
    let parent = provider_thread_session_identity("missing-parent").unwrap();
    let native_item_key =
        NativeItemKey::native_id(EVENT_KEY_NAMESPACE, TypedKey::utf8("child-event").unwrap())
            .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id: child,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        child,
        source,
        0,
        "message",
        PARSER_REVISION,
        "body",
    )
    .unwrap();

    apply_direct_session_relationship(&mut record, Some(parent)).unwrap();

    assert_eq!(record.session_relationship, None);
    assert_eq!(record.parent_session_id, Some(parent));
    assert_eq!(record.root_session_id, None);
    record.validate_contract().unwrap();
}

#[test]
fn root_scope_distinguishes_native_sessions_and_parent_lineage() {
    let legacy = rovodev_source_key("same-session").unwrap();
    let unqualified =
        rovodev_source_key_scoped("same-session", SourceAnchorScope::Unqualified).unwrap();
    let first =
        rovodev_source_key_scoped("same-session", SourceAnchorScope::Lineage([1; 32])).unwrap();
    let second =
        rovodev_source_key_scoped("same-session", SourceAnchorScope::Lineage([2; 32])).unwrap();

    assert!(legacy.exact_descriptor_eq(&unqualified));
    assert_ne!(first.identity(), second.identity());
    assert_ne!(
        rovodev_session_identity(&first, "same-session").unwrap(),
        rovodev_session_identity(&second, "same-session").unwrap()
    );
    assert_ne!(
        provider_thread_session_identity_scoped(
            "same-parent",
            SourceAnchorScope::Lineage([1; 32]),
        )
        .unwrap(),
        provider_thread_session_identity_scoped(
            "same-parent",
            SourceAnchorScope::Lineage([2; 32]),
        )
        .unwrap()
    );
}

#[test]
fn parent_lifecycle_does_not_change_child_leaf_ownership() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_session(&root, "root-a", None);
    write_session(&root, "root-b", None);
    write_session(&root, "middle", Some("root-a"));
    write_session(&root, "child", Some("middle"));

    let cold = discover_rovodev_source_backed(&root).unwrap();
    let cold = complete(cold);
    let child_fingerprint = leaf_fingerprint(&cold, "child");
    let middle_fingerprint = leaf_fingerprint(&cold, "middle");

    write_session(&root, "middle", Some("root-b"));
    let mutated = complete(discover_rovodev_source_backed(&root).unwrap());
    assert_eq!(leaf_fingerprint(&mutated, "child"), child_fingerprint);
    assert_ne!(leaf_fingerprint(&mutated, "middle"), middle_fingerprint);

    std::fs::remove_dir_all(root.join("middle")).unwrap();
    let deleted = complete(discover_rovodev_source_backed(&root).unwrap());
    assert_eq!(leaf_fingerprint(&deleted, "child"), child_fingerprint);
    assert_eq!(leaf_parent(&deleted, "child"), Some("middle"));

    write_session(&root, "middle", Some("root-a"));
    let reappeared = complete(discover_rovodev_source_backed(&root).unwrap());
    assert_eq!(leaf_fingerprint(&reappeared, "child"), child_fingerprint);
    assert_eq!(leaf_parent(&reappeared, "child"), Some("middle"));
}

fn complete(disposition: RovoDevSourceBackedDisposition) -> Box<RovoDevDocumentTree> {
    match disposition {
        RovoDevSourceBackedDisposition::Complete(tree) => tree,
        RovoDevSourceBackedDisposition::Unavailable => panic!("Rovo Dev fixture unavailable"),
    }
}

fn leaf_fingerprint(
    tree: &RovoDevDocumentTree,
    provider_session_id: &str,
) -> DocumentLeafFingerprint {
    tree.leaves
        .iter()
        .find(|leaf| leaf.provider_leaf.header.provider_session_id == provider_session_id)
        .unwrap()
        .fingerprint
}

fn leaf_parent<'tree>(
    tree: &'tree RovoDevDocumentTree,
    provider_session_id: &str,
) -> Option<&'tree str> {
    tree.leaves
        .iter()
        .find(|leaf| leaf.provider_leaf.header.provider_session_id == provider_session_id)
        .unwrap()
        .provider_leaf
        .header
        .parent_provider_session_id
        .as_deref()
}

fn write_session(root: &std::path::Path, session: &str, parent: Option<&str>) {
    let directory = root.join(session);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("session_context.json"),
        serde_json::to_vec(&serde_json::json!({
            "session_id": session,
            "message_history": [{
                "id": format!("message-{session}"),
                "timestamp": "2026-08-09T12:00:00Z",
                "role": "assistant",
                "content": format!("body {session}"),
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        directory.join("metadata.json"),
        serde_json::to_vec(&serde_json::json!({
            "session_id": session,
            "parent_session_id": parent,
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn duplicate_and_conflicting_rovodev_selectors_fail_closed() {
    assert!(json_has_duplicate_key(
        br#"{"session_id":"same","session_id":"same","message_history":[]}"#
    )
    .unwrap());
    assert!(json_has_duplicate_key(
        br#"{"message_history":[{"role":"assistant","role":"assistant","content":"body"}]}"#
    )
    .unwrap());

    let document = PreparedDocument {
        metadata: serde_json::Value::Null,
        context_branch: None,
        messages: Vec::new(),
        provider_session_id: "session".to_owned(),
        parent_provider_session_id: None,
        started_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        cwd: None,
        initial_failure_count: 0,
    };
    let ambiguous = serde_json::json!({
        "kind": "assistant",
        "type": "tool_result",
        "content": "must not acquire a selected kind"
    });
    assert!(document::project_message(&ambiguous, 0, &document).is_err());
}
