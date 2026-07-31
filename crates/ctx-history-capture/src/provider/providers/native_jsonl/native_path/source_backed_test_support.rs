use std::path::Path;

use ctx_history_core::{
    BatchHydrationRequest, ContentSourceResolver, EventHydrationRequest, SourceKey, TypedKey,
};
use ctx_history_index::{EventRecord, VerifiedIndex, WriterOptions, MAX_SOURCE_EVENT_PAGE_ITEMS};
use sha2::{Digest, Sha256};

use super::*;
use crate::{
    provider::source_backed::{
        family::jsonl::{jsonl_family_work, reset_jsonl_family_work},
        refresh_source_backed_generation, register_landed_source_backed_route,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    provider_sources::provider_source_for_path,
};

const COMPLETE_BODY_REGRESSION_BOUNDARY_CHARS: usize = 16 * 1024;

// The fixture helper keeps all eleven expected contract values explicit at call
// sites; a test-only argument struct would make failures less legible.
#[allow(clippy::too_many_arguments)]
pub(in super::super) fn assert_source_backed_fixture(
    adapter: DirectJsonlFamilyAdapter,
    root: &Path,
    expected_native_session_id: &str,
    expected_body: &str,
    expected_tail_term: &str,
    expected_record: &[u8],
    expected_parent_provider_session_id: Option<&str>,
    expected_root_provider_session_id: &str,
    expected_agent_type: &str,
    expected_is_primary: bool,
    expected_projection_digest: &str,
) {
    use ctx_history_core::NativeRecordCoordinate;

    let body_prefix = expected_body
        .split_once(expected_tail_term)
        .map(|(prefix, _)| prefix)
        .expect("complete-body tail term is absent from the expected body");
    assert!(body_prefix.chars().count() > COMPLETE_BODY_REGRESSION_BOUNDARY_CHARS);
    serde_json::from_str::<serde_json::Value>(expected_body)
        .expect("structured complete-body fixture must remain valid JSON");

    let opening = adapter.discover(root).unwrap();
    assert!(!opening.root_missing());
    assert!(opening.rejected_leaves().is_empty());
    assert_eq!(opening.leaves().len(), 1);
    let leaf = opening.leaves()[0].clone();

    let capture_temp = crate::test_support_paths::tempdir().unwrap();
    let index_root = capture_temp.path().join("index");
    let registry = production_registry(adapter, root);
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    let certified = &cold.sources[0];
    assert!(certified
        .observation()
        .source()
        .exact_descriptor_eq(leaf.source()));
    assert_eq!(
        certified.counts().certified_bytes,
        leaf.open_verified().unwrap().len()
    );
    assert!(certified.frontier().is_some());

    let documents = captured_documents(&index_root, &registry, certified.observation().source());
    assert_eq!(certified.counts().indexed_documents, documents.len() as u64);
    let document = documents
        .iter()
        .find(|document| document.body.contains(expected_tail_term))
        .unwrap();
    assert_eq!(document.body, expected_body);
    let tail_matches = VerifiedIndex::open(&index_root)
        .unwrap()
        .search_event_candidates(expected_tail_term, 10)
        .unwrap();
    assert!(tail_matches
        .iter()
        .any(|candidate| candidate.event.event_id == document.event.event_id));
    assert_eq!(
        document.event.provider,
        JsonlFamilyAdapter::provider(&adapter).as_str()
    );
    assert_eq!(
        document.event.source_format,
        JsonlFamilyAdapter::source_format(&adapter)
    );
    assert_eq!(
        document.event.provider_session_id.as_deref(),
        Some(expected_native_session_id)
    );
    assert_eq!(
        document.event.session_id,
        adapter
            .session_identity(expected_native_session_id)
            .unwrap()
            .1
    );
    let expected_parent_session_id = expected_parent_provider_session_id
        .map(|parent| adapter.session_identity(parent).unwrap().1);
    let expected_root_session_id = adapter
        .session_identity(expected_root_provider_session_id)
        .unwrap()
        .1;
    assert_eq!(document.event.parent_session_id, expected_parent_session_id);
    assert_eq!(document.event.root_session_id, expected_root_session_id);
    assert_eq!(document.event.agent_type, expected_agent_type);
    assert_eq!(document.event.is_primary, expected_is_primary);
    assert_eq!(document.event.branch, None);
    assert_eq!(
        document.event.source_path.as_deref(),
        leaf.source_path().to_str()
    );
    let NativeRecordCoordinate::Jsonl {
        byte_length,
        native_session_key,
        native_event_key,
        ..
    } = document.event.locator.coordinate()
    else {
        panic!("source-backed fixture did not emit a typed JSONL locator");
    };
    assert_eq!(*byte_length as usize, expected_record.len());
    assert_eq!(
        native_session_key.as_ref(),
        Some(&TypedKey::Utf8(expected_native_session_id.to_owned()))
    );
    assert!(native_event_key.is_some());
    assert_eq!(
        semantic_projection_digest(&documents),
        expected_projection_digest
    );

    super::super::reader::reset_provider_projection_count();
    reset_jsonl_family_work();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);
    assert_eq!(super::super::reader::provider_projection_count(), 0);
    assert_eq!(jsonl_family_work().provider_projections, 0);

    assert_incremental_final_matches_cold(adapter, root, &leaf);
}

fn production_registry(
    adapter: DirectJsonlFamilyAdapter,
    root: &Path,
) -> SourceBackedProviderRegistry {
    let source =
        provider_source_for_path(JsonlFamilyAdapter::provider(&adapter), root.to_path_buf());
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    assert_eq!(registry.executable_route_count(), 1);
    registry
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FixtureDocument {
    event: EventRecord,
    body: String,
}

fn captured_documents(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
    source: &SourceKey,
) -> Vec<FixtureDocument> {
    let index = VerifiedIndex::open(index_root).unwrap();
    let mut page = index
        .source_event_page(source, None, MAX_SOURCE_EVENT_PAGE_ITEMS)
        .unwrap();
    assert!(page.terminal);
    page.items.sort_by_key(|event| event.event_sequence);
    if page.items.is_empty() {
        return Vec::new();
    }
    let requests = page
        .items
        .iter()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(requests).unwrap())
        .unwrap()
        .into_records();
    assert_eq!(hydrated.len(), page.items.len());
    page.items
        .into_iter()
        .zip(hydrated)
        .map(|(event, hydrated)| {
            assert_eq!(hydrated.event_id, event.event_id);
            FixtureDocument {
                event,
                body: String::from_utf8(hydrated.provider_bytes).unwrap(),
            }
        })
        .collect()
}

fn assert_incremental_final_matches_cold(
    adapter: DirectJsonlFamilyAdapter,
    root: &Path,
    fixture_leaf: &JsonlFamilyLeaf,
) {
    use std::{fs, io::Write};

    let fixture = fs::read(fixture_leaf.source_path()).unwrap();
    let previous_newline = fixture[..fixture.len().saturating_sub(1)]
        .iter()
        .rposition(|byte| *byte == b'\n');
    let (prefix, suffix) = previous_newline.map_or_else(
        || (fixture.clone(), b"\n".to_vec()),
        |split| {
            (
                fixture[..=split].to_vec(),
                fixture[split.saturating_add(1)..].to_vec(),
            )
        },
    );
    let temp = crate::test_support_paths::tempdir().unwrap();
    let incremental_root = temp.path().join("incremental");
    let relative = fixture_leaf
        .source_path()
        .strip_prefix(root)
        .unwrap_or_else(|_| Path::new(fixture_leaf.source_path().file_name().unwrap()));
    let incremental_path = incremental_root.join(relative);
    fs::create_dir_all(incremental_path.parent().unwrap()).unwrap();
    fs::write(&incremental_path, &prefix).unwrap();

    let incremental_registry = production_registry(adapter, &incremental_root);
    let incremental_index = temp.path().join("incremental-index");
    let initial = refresh_source_backed_generation(
        &incremental_index,
        &incremental_registry,
        writer_options(),
    )
    .unwrap();
    assert_eq!(initial.sources.len(), 1);
    let initial_documents = captured_documents(
        &incremental_index,
        &incremental_registry,
        initial.sources[0].observation().source(),
    );

    fs::OpenOptions::new()
        .append(true)
        .open(&incremental_path)
        .unwrap()
        .write_all(&suffix)
        .unwrap();
    let appended_physical_records = suffix.iter().filter(|byte| **byte == b'\n').count();
    assert_ne!(appended_physical_records, 0);
    super::super::reader::reset_provider_projection_count();
    reset_jsonl_family_work();
    let appended = refresh_source_backed_generation(
        &incremental_index,
        &incremental_registry,
        writer_options(),
    )
    .unwrap();
    assert_eq!(appended.sources.len(), 1);
    assert_eq!(
        super::super::reader::provider_projection_count(),
        appended_physical_records
    );
    assert_eq!(
        jsonl_family_work().provider_projections,
        appended_physical_records
    );
    let incremental_final_documents = captured_documents(
        &incremental_index,
        &incremental_registry,
        appended.sources[0].observation().source(),
    );
    assert!(incremental_final_documents.starts_with(&initial_documents));

    let cold_registry = production_registry(adapter, &incremental_root);
    let cold_index = temp.path().join("cold-index");
    let cold_final =
        refresh_source_backed_generation(&cold_index, &cold_registry, writer_options()).unwrap();
    assert_eq!(cold_final.sources.len(), 1);
    let cold_final_documents = captured_documents(
        &cold_index,
        &cold_registry,
        cold_final.sources[0].observation().source(),
    );
    assert_eq!(
        semantic_projection_digest(&incremental_final_documents),
        semantic_projection_digest(&cold_final_documents)
    );
    assert_eq!(appended.sources[0].counts(), cold_final.sources[0].counts());
    assert_eq!(
        appended.sources[0].content_digest(),
        cold_final.sources[0].content_digest()
    );
}

fn semantic_projection_digest(documents: &[FixtureDocument]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx.direct-jsonl.semantic-projection-test-v1\0");
    digest.update((documents.len() as u64).to_be_bytes());
    for document in documents {
        let encoded = serde_json::to_vec(&serde_json::json!({
            "coordinate": document.event.locator.coordinate(),
            "record_digest": document.event.locator.record_digest(),
            "provider_session_id": document.event.provider_session_id,
            "branch": document.event.branch,
            "agent_type": document.event.agent_type,
            "is_primary": document.event.is_primary,
            "event_sequence": document.event.event_sequence,
            "occurred_at_unix_ms": document.event.occurred_at_unix_ms,
            "event_type": document.event.event_type,
            "role": document.event.role,
            "body": document.body,
            "workspace": document.event.workspace,
            "cwd": document.event.cwd,
            "touched_files": document.event.touched_files,
        }))
        .unwrap();
        digest.update((encoded.len() as u64).to_be_bytes());
        digest.update(encoded);
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
