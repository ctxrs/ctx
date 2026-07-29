use std::fs;

use super::*;
use crate::test_support_paths::tempdir;
use serde_json::json;

fn context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "auggie-source-backed-test".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: None,
        imported_at: "2026-07-28T12:00:00Z".parse().unwrap(),
    }
}

fn project_one(
    root: &AuggieSourceBackedRoot,
    context: ProviderAdapterContext,
) -> AuggieSourceBackedSource {
    let inventory = discover_auggie_source_backed(root).unwrap();
    let mut sources = project_auggie_source_backed_inventory(&inventory, &context).unwrap();
    assert_eq!(sources.len(), 1);
    sources.remove(0)
}

fn write_session(path: &Path, request_text: &str, response_text: &str) {
    write_history(path, &[("request-stable-id", request_text, response_text)]);
}

fn write_history(path: &Path, exchanges: &[(&str, &str, &str)]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let chat_history = exchanges
        .iter()
        .enumerate()
        .map(|(index, (request_id, request_text, response_text))| {
            json!({
                "exchange": {
                    "request_id": request_id,
                    "request_message": request_text,
                    "response_text": response_text,
                },
                "finishedAt": format!("2026-07-28T11:{:02}:00Z", index + 1),
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        path,
        serde_json::to_vec(&json!({
            "sessionId": "auggie-source-session",
            "created": "2026-07-28T11:00:00Z",
            "workspaceRoot": "/workspace/auggie",
            "chatHistory": chat_history,
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn cold_projection_is_stable_full_body_and_document_located() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("home/.augment/sessions");
    let path = sessions.join("session.json");
    let request_text = format!("full-prefix-{}-auggie-tail", "x".repeat(3_000));
    write_session(&path, &request_text, "bounded response");
    let root = AuggieSourceBackedRoot::explicit(&sessions);
    let inventory = discover_auggie_source_backed(&root).unwrap();
    assert_eq!(
        inventory.status,
        AuggieSourceBackedInventoryStatus::Complete
    );
    assert_eq!(inventory.paths.len(), 1);

    let first = project_one(&root, context(&sessions));
    let second = project_one(&root, context(&sessions));
    assert_eq!(first.certified_source, second.certified_source);
    assert_eq!(first.documents.len(), 2);
    assert_eq!(
        first
            .documents
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>(),
        second
            .documents
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(first.documents[0].body, request_text);
    assert!(first.documents[0].body.ends_with("auggie-tail"));
    for document in &first.documents {
        assert_eq!(document.parent_session_id, None);
        assert_eq!(document.root_session_id, document.session_id);
        assert_eq!(
            document.provider_session_id.as_deref(),
            Some("auggie-source-session")
        );
        assert_eq!(
            document.source_path.as_deref(),
            fs::canonicalize(&path).unwrap().to_str()
        );
        assert_eq!(document.agent_type, "primary");
        assert!(document.is_primary);
        assert_eq!(document.branch, None);
        assert_eq!(
            document.locator.revision_policy(),
            LocatorRevisionPolicy::ExactSourceRevision
        );
        assert_eq!(
            document.locator.certified_source_revision_digest(),
            Some(first.certified_source.content_digest())
        );
        assert!(matches!(
            document.locator.coordinate(),
            NativeRecordCoordinate::Document {
                object_key: TypedKey::Composite(_),
                json_pointer: Some(pointer),
            } if pointer.starts_with("/chatHistory/0")
        ));
    }
}

#[test]
fn exact_hydration_fails_closed_and_replacement_keeps_native_ids() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let path = sessions.join("session.json");
    let root = AuggieSourceBackedRoot::explicit(&sessions);
    write_session(&path, "before replacement", "stable response");
    let before = project_one(&root, context(&sessions));
    let old_request = &before.documents[0];
    let hydrated = hydrate_auggie_source_backed(&path, &old_request.locator).unwrap();
    assert_eq!(hydrated.decoded_display_text, "before replacement");
    assert_eq!(hydrated.provider_bytes, b"before replacement");

    write_session(&path, "after replacement", "stable response");
    let after = project_one(&root, context(&sessions));
    assert_eq!(
        before.documents[0].session_id,
        after.documents[0].session_id
    );
    assert_eq!(before.documents[0].event_id, after.documents[0].event_id);
    assert_ne!(
        before.certified_source.content_digest(),
        after.certified_source.content_digest()
    );
    assert!(matches!(
        hydrate_auggie_source_backed(&path, &old_request.locator),
        Err(AuggieSourceBackedError::SourceRevisionChanged)
            | Err(AuggieSourceBackedError::LocatorDigestMismatch)
    ));
    assert_eq!(
        hydrate_auggie_source_backed(&path, &after.documents[0].locator)
            .unwrap()
            .decoded_display_text,
        "after replacement"
    );
}

#[test]
fn provider_b_source_backed_body_architecture_has_no_preview_or_store_contract() {
    let forbidden_preview_cap = ["MAX_BODY_PREVIEW", "_CHARS"].concat();
    let forbidden_legacy_field = ["lexical_", "preview"].concat();
    let forbidden_store = ["ctx_history_", "store::Store"].concat();
    let sources = [
        ("auggie", include_str!("source_backed.rs")),
        (
            "codebuddy",
            include_str!("../../codebuddy/native_path/source_backed.rs"),
        ),
        (
            "continue_cli",
            include_str!("../../continue_cli/native_path/source_backed.rs"),
        ),
        (
            "crush",
            include_str!("../../crush/native_path/source_backed.rs"),
        ),
        ("cursor", include_str!("../../cursor/source_backed.rs")),
        (
            "deepagents",
            include_str!("../../deepagents/native_path/source_backed.rs"),
        ),
        (
            "firebender",
            include_str!("../../firebender/native_path/source_backed.rs"),
        ),
        ("goose", include_str!("../../goose/source_backed.rs")),
        ("hermes", include_str!("../../hermes/source_backed.rs")),
        (
            "kimi",
            include_str!("../../kimi/native_path/source_backed.rs"),
        ),
        ("kiro", include_str!("../../kiro/source_backed.rs")),
    ];
    for (provider, source) in sources {
        assert!(
            !source.contains(&forbidden_preview_cap),
            "{provider} restored the index preview cap"
        );
        assert!(
            !source.contains(&forbidden_legacy_field),
            "{provider} restored lexical-preview construction"
        );
        assert!(
            !source.contains(&forbidden_store),
            "{provider} restored the legacy Store path"
        );
    }
}

#[test]
fn explicit_cache_root_selects_direct_sessions_child_without_recursing() {
    let temp = tempdir().unwrap();
    let cache_root = temp.path().join("one-shot-augment-cache");
    let cache_sessions = cache_root.join("sessions");
    write_session(
        &cache_sessions.join("nested/ignored.json"),
        "nested request",
        "nested response",
    );
    write_session(
        &cache_sessions.join("explicit.json"),
        "explicit request",
        "explicit response",
    );

    let explicit =
        discover_auggie_source_backed(&AuggieSourceBackedRoot::explicit(&cache_root)).unwrap();
    assert_eq!(explicit.paths.len(), 1);
    assert_eq!(
        explicit.paths[0],
        fs::canonicalize(cache_sessions.join("explicit.json")).unwrap()
    );
}

#[test]
fn inventory_and_projection_signal_append_rewrite_truncate_delete_and_unavailable() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let root = AuggieSourceBackedRoot::explicit(&sessions);

    let missing = discover_auggie_source_backed(&root).unwrap();
    assert_eq!(
        missing.status,
        AuggieSourceBackedInventoryStatus::Unavailable
    );
    assert!(missing.paths.is_empty());

    fs::create_dir_all(&sessions).unwrap();
    let empty = discover_auggie_source_backed(&root).unwrap();
    assert_eq!(empty.status, AuggieSourceBackedInventoryStatus::Complete);
    assert!(empty.paths.is_empty());

    let path = sessions.join("session.json");
    write_history(
        &path,
        &[("stable-request-1", "initial request", "initial response")],
    );
    let initial_inventory = discover_auggie_source_backed(&root).unwrap();
    let initial =
        project_auggie_source_backed_inventory(&initial_inventory, &context(&sessions)).unwrap();
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0].documents.len(), 2);
    let initial_ids = initial[0]
        .documents
        .iter()
        .map(|document| document.event_id)
        .collect::<Vec<_>>();

    write_history(
        &path,
        &[
            (
                "stable-request-1",
                "rewritten request with a longer body",
                "rewritten response",
            ),
            ("stable-request-2", "appended request", "appended response"),
        ],
    );
    let appended = project_auggie_source_backed_inventory(
        &discover_auggie_source_backed(&root).unwrap(),
        &context(&sessions),
    )
    .unwrap();
    assert_eq!(appended[0].documents.len(), 4);
    assert_eq!(appended[0].documents[0].event_id, initial_ids[0]);
    assert_eq!(appended[0].documents[1].event_id, initial_ids[1]);
    assert_eq!(
        appended[0].documents[0].body,
        "rewritten request with a longer body"
    );

    write_history(
        &path,
        &[(
            "stable-request-1",
            "truncated generation request",
            "truncated generation response",
        )],
    );
    let truncated = project_auggie_source_backed_inventory(
        &discover_auggie_source_backed(&root).unwrap(),
        &context(&sessions),
    )
    .unwrap();
    assert_eq!(truncated[0].documents.len(), 2);
    assert_eq!(truncated[0].documents[0].event_id, initial_ids[0]);
    assert_eq!(truncated[0].documents[1].event_id, initial_ids[1]);

    let stale_inventory = discover_auggie_source_backed(&root).unwrap();
    fs::remove_file(&path).unwrap();
    assert!(project_auggie_source_backed_inventory(&stale_inventory, &context(&sessions)).is_err());
    let deleted = discover_auggie_source_backed(&root).unwrap();
    assert_eq!(deleted.status, AuggieSourceBackedInventoryStatus::Complete);
    assert!(deleted.paths.is_empty());

    fs::remove_dir(&sessions).unwrap();
    let unavailable = discover_auggie_source_backed(&root).unwrap();
    assert_eq!(
        unavailable.status,
        AuggieSourceBackedInventoryStatus::Unavailable
    );
    assert!(unavailable.paths.is_empty());
}
