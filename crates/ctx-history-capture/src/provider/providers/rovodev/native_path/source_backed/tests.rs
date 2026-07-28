use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use chrono::{TimeZone, Utc};
use ctx_history_core::{NativeRecordCoordinate, TypedKey};
use serde_json::{json, Value};

use super::*;
use crate::{
    provider_sources::{
        discover_provider_sources_for_provider_with_context, DiscoveryContext, DiscoveryPlatform,
        DiscoveryPlatformDirs,
    },
    test_support_paths::tempdir,
};

fn adapter_context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "rovodev-source-backed-test".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: Some(root.to_path_buf()),
        imported_at: Utc.timestamp_millis_opt(1_700_000_000_000).unwrap(),
    }
}

fn write_session(
    root: &Path,
    directory_session_id: &str,
    provider_session_id: &str,
    parent_session_id: Option<&str>,
    messages: &[Value],
) -> PathBuf {
    let directory = root.join(directory_session_id);
    fs::create_dir_all(&directory).unwrap();
    let context = directory.join("session_context.json");
    fs::write(
        &context,
        serde_json::to_vec(&json!({
            "session_id": provider_session_id,
            "message_history": messages,
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        directory.join("metadata.json"),
        serde_json::to_vec(&json!({
            "session_id": provider_session_id,
            "parent_session_id": parent_session_id,
            "workspace_path": "/workspace/rovo",
            "git_branch": "feature/source-backed",
        }))
        .unwrap(),
    )
    .unwrap();
    context
}

fn collect_scan(
    leaf: &RovoDevSourceBackedLeaf,
    context: ProviderAdapterContext,
    previous: Option<&CertifiedSource>,
) -> (
    RovoDevSourceBackedScan,
    Vec<LexicalDocument>,
    Vec<RovoDevSourceBackedPage>,
) {
    let mut reader = RovoDevSourceBackedReader::new(leaf, context, previous).unwrap();
    let mut documents = Vec::new();
    let mut pages = Vec::new();
    while let Some(mut page) = reader.next_page().unwrap() {
        documents.append(&mut page.documents);
        pages.push(page);
    }
    (reader.finish().unwrap(), documents, pages)
}

#[test]
fn cold_scan_emits_stable_bounded_documents_tree_coordinates_and_exact_counts() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".rovodev/sessions");
    write_session(
        &root,
        "session-a",
        "session-a",
        None,
        &[
            json!({
                "id": "message-a",
                "role": "user",
                "timestamp": "2026-01-01T00:00:00Z",
                "content": "bounded ".repeat(MAX_BODY_PREVIEW_CHARS)
            }),
            json!({
                "id": "tool-success",
                "role": "tool_result",
                "status": "success",
                "content": "successful output is intentionally not retained"
            }),
            json!("malformed-message"),
        ],
    );
    let context = adapter_context(&root);
    let inventory = discover_rovodev_source_backed(&root, context.clone()).unwrap();
    assert_eq!(inventory.leaves().len(), 1);
    assert_eq!(inventory.certify().unwrap().observed_sources(), 1);

    let leaf = &inventory.leaves()[0];
    let (cold, documents, pages) = collect_scan(leaf, context.clone(), None);
    assert_eq!(cold.disposition, RovoDevSourceBackedDisposition::Cold);
    assert_eq!(documents.len(), 1);
    assert_eq!(cold.source.counts().complete_records, 3);
    assert_eq!(cold.source.counts().retained_records, 1);
    assert_eq!(cold.source.counts().rejected_records, 1);
    assert_eq!(cold.source.counts().ignored_records, 1);
    assert_eq!(cold.source.counts().indexed_documents, 1);
    assert!(pages.last().is_some_and(|page| page.terminal));
    assert!(documents[0].body.chars().count() <= MAX_BODY_PREVIEW_CHARS);
    assert_eq!(documents[0].session_id, leaf.session_id());
    assert_eq!(documents[0].parent_session_id, None);
    assert_eq!(documents[0].root_session_id, leaf.session_id());
    assert_eq!(documents[0].source, *leaf.source_key());
    assert_eq!(
        documents[0].provider_session_id.as_deref(),
        Some("session-a")
    );
    assert_eq!(
        documents[0].branch.as_deref(),
        Some("feature/source-backed")
    );
    assert_eq!(
        documents[0].source_path.as_deref(),
        Some(
            root.join("session-a/session_context.json")
                .to_str()
                .unwrap()
        )
    );
    assert_eq!(documents[0].agent_type, AgentType::Primary.as_str());
    assert!(documents[0].is_primary);
    assert_eq!(
        documents[0].locator.certified_source_revision_digest(),
        Some(cold.source.content_digest())
    );
    assert_eq!(
        cold.source.frontier().unwrap().checkpoint_kind(),
        FRONTIER_KIND
    );
    match documents[0].locator.coordinate() {
        NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::Utf8(relative),
            record_coordinate: TypedKey::Composite(parts),
        } => {
            assert_eq!(relative, RELATIVE_CONTEXT_FILE);
            assert_eq!(
                parts,
                &[
                    TypedKey::Utf8(MESSAGE_OBJECT_KIND.to_owned()),
                    TypedKey::U64(0),
                    TypedKey::Utf8("message-a".to_owned()),
                ]
            );
        }
        coordinate => panic!("unexpected coordinate: {coordinate:?}"),
    }

    let (noop, noop_documents, noop_pages) = collect_scan(leaf, context, Some(&cold.source));
    assert_eq!(noop.disposition, RovoDevSourceBackedDisposition::Unchanged);
    assert!(noop_documents.is_empty());
    assert!(noop_pages.is_empty());
    assert_eq!(noop.source, cold.source);
}

#[test]
fn lineage_fields_bind_parent_root_thread_and_agent_semantics() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".rovodev/sessions");
    for (provider_session_id, parent_session_id) in [
        ("root-thread", None),
        ("child-thread", Some("root-thread")),
        ("grandchild-thread", Some("child-thread")),
    ] {
        write_session(
            &root,
            provider_session_id,
            provider_session_id,
            parent_session_id,
            &[json!({
                "id": format!("{provider_session_id}-message"),
                "role": "assistant",
                "content": provider_session_id,
            })],
        );
    }
    let context = adapter_context(&root);
    let inventory = discover_rovodev_source_backed(&root, context.clone()).unwrap();
    let mut documents = HashMap::new();
    for leaf in inventory.leaves() {
        let (_, mut leaf_documents, _) = collect_scan(leaf, context.clone(), None);
        assert_eq!(leaf_documents.len(), 1);
        let document = leaf_documents.pop().unwrap();
        documents.insert(
            document.provider_session_id.clone().unwrap(),
            (leaf.session_id(), document),
        );
    }

    let (root_session_id, root_document) = &documents["root-thread"];
    let (child_session_id, child_document) = &documents["child-thread"];
    let (grandchild_session_id, grandchild_document) = &documents["grandchild-thread"];
    assert_eq!(root_document.parent_session_id, None);
    assert_eq!(root_document.root_session_id, *root_session_id);
    assert_eq!(root_document.agent_type, AgentType::Primary.as_str());
    assert!(root_document.is_primary);
    assert_eq!(child_document.parent_session_id, Some(*root_session_id));
    assert_eq!(child_document.root_session_id, *root_session_id);
    assert_eq!(child_document.agent_type, AgentType::Subagent.as_str());
    assert!(!child_document.is_primary);
    assert_eq!(
        grandchild_document.parent_session_id,
        Some(*child_session_id)
    );
    assert_eq!(grandchild_document.root_session_id, *root_session_id);
    assert_eq!(grandchild_document.agent_type, AgentType::Subagent.as_str());
    assert!(!grandchild_document.is_primary);
    assert_ne!(grandchild_session_id, root_session_id);
}

#[test]
fn exact_route_reopens_full_body_and_replacement_rejects_stale_revision() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".rovodev/sessions");
    let original_text = format!("original exact body {}", "content ".repeat(500));
    let path = write_session(
        &root,
        "session-a",
        "session-a",
        None,
        &[
            json!({"id": "stable-message", "role": "user", "content": original_text.clone()}),
            json!({"role": "assistant", "content": "positional"}),
        ],
    );
    let context = adapter_context(&root);
    let original_inventory = discover_rovodev_source_backed(&root, context.clone()).unwrap();
    let original_leaf = &original_inventory.leaves()[0];
    let (cold, original_documents, _) = collect_scan(original_leaf, context.clone(), None);
    assert_eq!(original_documents.len(), 2);
    let hydrated = hydrate_rovodev_source_record(
        &original_inventory,
        original_documents[0].event_id,
        &original_documents[0].locator,
    )
    .unwrap();
    assert_eq!(
        hydrated.decoded_display_text.as_deref(),
        Some(original_text.as_str())
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&hydrated.provider_bytes).unwrap()["message_history"][0]
            ["id"],
        "stable-message"
    );

    let replacement_text = format!("replaced exact body {}", "content ".repeat(500));
    assert_eq!(replacement_text.len(), original_text.len());
    let replacement_document = json!({
        "session_id": "session-a",
        "message_history": [
            {"id": "stable-message", "role": "user", "content": replacement_text},
            {"role": "assistant", "content": "positional"}
        ]
    });
    fs::write(&path, serde_json::to_vec(&replacement_document).unwrap()).unwrap();

    let replacement_inventory = discover_rovodev_source_backed(&root, context.clone()).unwrap();
    let stale = hydrate_rovodev_source_record(
        &replacement_inventory,
        original_documents[0].event_id,
        &original_documents[0].locator,
    )
    .unwrap_err();
    assert!(matches!(
        stale,
        RovoDevSourceBackedError::LocatorSourceChanged
    ));

    let replacement_leaf = &replacement_inventory.leaves()[0];
    let (replacement, replacement_documents, _) =
        collect_scan(replacement_leaf, context, Some(&cold.source));
    assert_eq!(
        replacement.disposition,
        RovoDevSourceBackedDisposition::Replacement
    );
    assert_eq!(replacement_documents.len(), 2);
    assert_eq!(
        replacement_documents[0].event_id, original_documents[0].event_id,
        "native message identity survives a document replacement"
    );
    assert_ne!(
        replacement_documents[1].event_id, original_documents[1].event_id,
        "positional message identity is revision scoped"
    );
    let hydrated = hydrate_rovodev_source_record(
        &replacement_inventory,
        replacement_documents[0].event_id,
        &replacement_documents[0].locator,
    )
    .unwrap();
    assert_eq!(
        hydrated.decoded_display_text.as_deref(),
        Some(replacement_text.as_str())
    );
}

#[test]
fn configured_root_wins_and_unsafe_replacement_suppresses_default_fallback() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let custom = temp.path().join("custom-rovo-sessions");
    let default = home.join(".rovodev/sessions");
    fs::create_dir_all(&cwd).unwrap();
    write_session(
        &custom,
        "custom",
        "custom",
        None,
        &[json!({"id": "custom-message", "role": "user", "content": "custom"})],
    );
    write_session(
        &default,
        "stale",
        "stale",
        None,
        &[json!({"id": "stale-message", "role": "user", "content": "stale"})],
    );
    fs::create_dir_all(home.join(".rovodev")).unwrap();
    let discovery_context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs {
            data: Some(home.join(".local/share")),
            config: Some(home.join(".config")),
            state: Some(home.join(".local/state")),
            local_data: Some(home.join(".local/share")),
        },
    );
    let selected_default = discover_provider_sources_for_provider_with_context(
        &discovery_context,
        CaptureProvider::RovoDev,
    );
    assert_eq!(
        selected_default
            .sources
            .iter()
            .map(|source| source.path.clone())
            .collect::<Vec<_>>(),
        vec![default.clone()]
    );

    fs::write(
        home.join(".rovodev/config.yml"),
        format!(
            "sessions:\n  persistenceDir: {}\n",
            serde_json::to_string(custom.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    let selected = discover_provider_sources_for_provider_with_context(
        &discovery_context,
        CaptureProvider::RovoDev,
    );
    assert_eq!(
        selected
            .sources
            .iter()
            .map(|source| source.path.clone())
            .collect::<Vec<_>>(),
        vec![custom.clone()]
    );
    discover_rovodev_source_backed(&selected.sources[0].path, adapter_context(&custom)).unwrap();

    fs::write(
        home.join(".rovodev/config.yml"),
        "sessions:\n  persistenceDir: relative/unsafe\n",
    )
    .unwrap();
    let blocked = discover_provider_sources_for_provider_with_context(
        &discovery_context,
        CaptureProvider::RovoDev,
    );
    assert!(blocked.sources.is_empty());
    assert_eq!(blocked.issues.len(), 1);

    let direct_leaf = custom.join("custom/session_context.json");
    let error = discover_rovodev_source_backed(&direct_leaf, adapter_context(&custom)).unwrap_err();
    assert!(matches!(
        error,
        RovoDevSourceBackedError::NonAuthoritativeRoot
    ));
}

#[test]
fn compound_authority_rovodev_rejects_missing_auxiliary_and_sibling_swap() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".rovodev/sessions");
    let context_path = write_session(
        &root,
        "session-a",
        "session-a",
        None,
        &[json!({"id": "message-a", "role": "user", "content": "hello"})],
    );
    let metadata_path = context_path.parent().unwrap().join("metadata.json");
    fs::remove_file(&metadata_path).unwrap();
    let context = adapter_context(&root);
    let inventory = discover_rovodev_source_backed(&root, context.clone()).unwrap();
    fs::write(&metadata_path, br#"{"session_id":"session-a"}"#).unwrap();
    assert!(inventory.certify().is_err());

    let inventory = discover_rovodev_source_backed(&root, context.clone()).unwrap();
    let leaf = &inventory.leaves()[0];
    let mut reader = RovoDevSourceBackedReader::new(leaf, context, None).unwrap();
    while reader.next_page().unwrap().is_some() {}
    fs::write(
        &metadata_path,
        br#"{"session_id":"session-a","git_branch":"replacement"}"#,
    )
    .unwrap();
    assert!(reader.finish().is_err());
}

#[cfg(unix)]
#[test]
fn compound_authority_rovodev_rejects_ancestor_swap_and_stale_locator() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".rovodev/sessions");
    write_session(
        &root,
        "session-a",
        "session-a",
        None,
        &[json!({"id": "message-a", "role": "user", "content": "hello"})],
    );
    let context = adapter_context(&root);
    let inventory = discover_rovodev_source_backed(&root, context.clone()).unwrap();
    let (_, documents, _) = collect_scan(&inventory.leaves()[0], context, None);

    let retired = temp.path().join("retired-sessions");
    fs::rename(&root, &retired).unwrap();
    write_session(
        &root,
        "session-a",
        "session-a",
        None,
        &[json!({"id": "message-a", "role": "user", "content": "hello"})],
    );

    assert!(hydrate_rovodev_source_record(
        &inventory,
        documents[0].event_id,
        &documents[0].locator,
    )
    .is_err());
}
