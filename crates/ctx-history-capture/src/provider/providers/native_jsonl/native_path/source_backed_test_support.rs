use std::path::Path;

use ctx_history_core::{CertifiedSource, EventHydrationRequest, TypedKey};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};

use super::*;

// The fixture helper keeps all ten expected contract values explicit at call
// sites; a test-only argument struct would make failures less legible.
#[allow(clippy::too_many_arguments)]
pub(in super::super) fn assert_source_backed_fixture(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    expected_native_session_id: &str,
    expected_body: &str,
    expected_record: &[u8],
    expected_parent_provider_session_id: Option<&str>,
    expected_root_provider_session_id: &str,
    expected_agent_type: &str,
    expected_is_primary: bool,
    expected_projection_digest: &str,
) {
    use ctx_history_core::NativeRecordCoordinate;

    let opening = adapter.discover(root).unwrap();
    assert!(!opening.root_missing());
    assert!(opening.failures().is_empty());
    assert_eq!(opening.leaves().len(), 1);
    let leaf = opening.leaves()[0].clone();
    let (documents, certified) = collect_test_leaf(adapter, &leaf, None);
    assert_eq!(
        certified.certificate().counts().indexed_documents,
        documents.len() as u64
    );
    assert_eq!(
        certified.certificate().counts().certified_bytes,
        leaf.open_verified().unwrap().len()
    );
    assert!(certified.certificate().frontier().is_some());
    let document = documents
        .iter()
        .find(|document| document.body.contains(expected_body))
        .unwrap();
    assert_eq!(
        document.provider_session_id.as_deref(),
        Some(expected_native_session_id)
    );
    let expected_parent_session_id = expected_parent_provider_session_id
        .map(|parent| direct_jsonl_session_identity(adapter, parent).unwrap().1);
    let expected_root_session_id =
        direct_jsonl_session_identity(adapter, expected_root_provider_session_id)
            .unwrap()
            .1;
    assert_eq!(document.parent_session_id, expected_parent_session_id);
    assert_eq!(document.root_session_id, expected_root_session_id);
    assert_eq!(document.agent_type, expected_agent_type);
    assert_eq!(document.is_primary, expected_is_primary);
    assert_eq!(document.branch, None);
    assert_eq!(document.source_path.as_deref(), leaf.path.to_str());
    let NativeRecordCoordinate::Jsonl {
        byte_length,
        native_session_key,
        native_event_key,
        ..
    } = document.locator.coordinate()
    else {
        panic!("source-backed fixture did not emit a typed JSONL locator");
    };
    assert_eq!(*byte_length as usize, expected_record.len());
    assert_eq!(
        native_session_key.as_ref(),
        Some(&TypedKey::Utf8(expected_native_session_id.to_owned()))
    );
    assert!(native_event_key.is_some());
    let request = EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap();
    let mut hydration_catalog = adapter.open_hydration_catalog(root).unwrap();
    let hydrated = hydration_catalog
        .hydrate_group(certified.certificate(), std::slice::from_ref(&request))
        .unwrap();
    assert_eq!(hydrated[0].provider_bytes, document.body.as_bytes());
    assert_eq!(
        semantic_projection_digest(&documents),
        expected_projection_digest
    );

    let closing = adapter.discover(root).unwrap();
    let inventory = opening
        .certify_against(&closing, vec![certified.source().clone()])
        .unwrap();
    assert!(inventory.contains(certified.source()));

    super::super::reader::reset_provider_projection_count();
    let (unchanged_documents, unchanged) =
        collect_test_leaf(adapter, &closing.leaves()[0], Some(certified.certificate()));
    assert!(unchanged_documents.is_empty());
    assert_eq!(unchanged.disposition(), DirectJsonlDisposition::Unchanged);
    assert_eq!(unchanged.certificate(), certified.certificate());
    assert_eq!(super::super::reader::provider_projection_count(), 0);

    assert_incremental_final_matches_cold(adapter, root, &leaf);
}

fn collect_test_leaf(
    adapter: DirectJsonlSourceAdapter,
    leaf: &DirectJsonlInventoryLeaf,
    previous: Option<&CertifiedSource>,
) -> (Vec<LexicalDocument>, DirectJsonlScanReceipt) {
    let mut reader = adapter
        .open_leaf(leaf, "2026-07-28T12:00:00Z".parse().unwrap(), previous)
        .unwrap();
    let mut documents = Vec::new();
    reader
        .visit_documents(&mut |document| {
            documents.push(document);
            Ok(())
        })
        .unwrap();
    (documents, reader.finish().unwrap())
}

fn assert_incremental_final_matches_cold(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    fixture_leaf: &DirectJsonlInventoryLeaf,
) {
    use std::{fs, io::Write};

    let fixture = fs::read(&fixture_leaf.path).unwrap();
    let previous_newline = fixture[..fixture.len().saturating_sub(1)]
        .iter()
        .rposition(|byte| *byte == b'\n');
    let (prefix, suffix) = previous_newline.map_or_else(
        || (fixture.clone(), fixture.clone()),
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
        .path
        .strip_prefix(root)
        .unwrap_or_else(|_| Path::new(fixture_leaf.path.file_name().unwrap()));
    let incremental_path = incremental_root.join(relative);
    fs::create_dir_all(incremental_path.parent().unwrap()).unwrap();
    fs::write(&incremental_path, &prefix).unwrap();

    let initial_inventory = adapter.discover(&incremental_root).unwrap();
    assert_eq!(initial_inventory.leaves().len(), 1);
    let (mut incremental_documents, initial) =
        collect_test_leaf(adapter, &initial_inventory.leaves()[0], None);

    fs::OpenOptions::new()
        .append(true)
        .open(&incremental_path)
        .unwrap()
        .write_all(&suffix)
        .unwrap();
    let final_inventory = adapter.discover(&incremental_root).unwrap();
    let appended_physical_records = suffix.iter().filter(|byte| **byte == b'\n').count();
    assert_ne!(appended_physical_records, 0);
    super::super::reader::reset_provider_projection_count();
    let (appended_documents, appended) = collect_test_leaf(
        adapter,
        &final_inventory.leaves()[0],
        Some(initial.certificate()),
    );
    assert_eq!(appended.disposition(), DirectJsonlDisposition::Append);
    assert_eq!(
        super::super::reader::provider_projection_count(),
        appended_physical_records
    );
    incremental_documents.extend(appended_documents);

    let (cold_final_documents, cold_final) =
        collect_test_leaf(adapter, &final_inventory.leaves()[0], None);
    assert_eq!(cold_final.disposition(), DirectJsonlDisposition::Cold);
    assert_eq!(
        semantic_projection_digest(&incremental_documents),
        semantic_projection_digest(&cold_final_documents)
    );
    assert_eq!(
        appended.certificate().counts(),
        cold_final.certificate().counts()
    );
    assert_eq!(
        appended.certificate().content_digest(),
        cold_final.certificate().content_digest()
    );
}

fn semantic_projection_digest(documents: &[LexicalDocument]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx.direct-jsonl.semantic-projection-test-v1\0");
    digest.update((documents.len() as u64).to_be_bytes());
    for document in documents {
        let encoded = serde_json::to_vec(&serde_json::json!({
            "coordinate": document.locator.coordinate(),
            "record_digest": document.locator.record_digest(),
            "provider_session_id": document.provider_session_id,
            "branch": document.branch,
            "agent_type": document.agent_type,
            "is_primary": document.is_primary,
            "event_sequence": document.event_sequence,
            "occurred_at_unix_ms": document.occurred_at_unix_ms,
            "event_type": document.event_type,
            "role": document.role,
            "body": document.body,
            "workspace": document.workspace,
            "cwd": document.cwd,
            "touched_files": document.touched_files,
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
