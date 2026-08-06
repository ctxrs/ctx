use std::{fs, path::Path};

use ctx_history_core::{AgentType, CoreRecord, EventOrigin, SessionRelationshipKind, TypedKey};
use ctx_history_index::{VerifiedIndex, WriterOptions};

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, register_landed_source_backed_route,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

fn write_session(path: &Path, session_id: &str, parent: Option<&Path>, body: &str) {
    let header = serde_json::json!({
        "type": "session",
        "version": 3,
        "id": session_id,
        "timestamp": "2026-01-02T03:04:05Z",
        "cwd": "/tmp/pi",
        "parentSession": parent.map(|path| path.to_string_lossy().into_owned()),
    });
    let message = serde_json::json!({
        "type": "message",
        "id": "copied-entry",
        "timestamp": "2026-01-02T03:04:06Z",
        "message": {"role": "user", "content": body},
    });
    fs::write(path, format!("{header}\n{message}\n")).unwrap();
}

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::Pi,
            path: root.to_path_buf(),
            exists: true,
            source_format: PI_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn published_session(index: &Path, provider_session_id: &str) -> Vec<CoreRecord> {
    let source = source_key(provider_session_id).unwrap();
    VerifiedIndex::open(index)
        .unwrap()
        .core_source_event_page(&source, None, 64)
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.core_record)
        .collect()
}

#[test]
fn parent_session_path_resolves_header_identity_and_publishes_forked_unknown_copy() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let parent_path = temp.path().join("parent.jsonl");
    let child_path = temp.path().join("child.jsonl");
    write_session(&parent_path, "pi-parent", None, "parent retained message");
    let parent_path = fs::canonicalize(parent_path).unwrap();
    write_session(
        &child_path,
        "pi-child",
        Some(&parent_path),
        "child retained message",
    );

    let index = temp.path().join("index");
    refresh_source_backed_generation(
        &index,
        &registry(temp.path()),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    let parent = published_session(&index, "pi-parent").remove(0);
    let child = published_session(&index, "pi-child").remove(0);

    assert_eq!(parent.session_relationship, SessionRelationshipKind::Root);
    assert_eq!(parent.event_origin, EventOrigin::Unknown);
    assert_eq!(child.session_relationship, SessionRelationshipKind::Forked);
    assert_eq!(child.parent_session_id, Some(parent.session_id));
    assert_eq!(child.root_session_id, parent.session_id);
    assert!(child.is_primary);
    assert_eq!(child.agent_type, AgentType::Primary.as_str());
    assert_eq!(child.event_origin, EventOrigin::Unknown);
    assert_eq!(
        parent.native_event_id,
        Some(TypedKey::utf8("copied-entry").unwrap())
    );
    assert_eq!(parent.native_event_id, child.native_event_id);
    assert_ne!(parent.event_id, child.event_id);

    let path_as_native_id = session_identity_for_native(parent_path.to_str().unwrap()).unwrap();
    assert_ne!(child.parent_session_id, Some(path_as_native_id));
}
